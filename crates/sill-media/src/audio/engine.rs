//! One output stream and what feeds it (what SoundEngine, AVAudioEngine and its player node, are to
//! iOSClient's AudioOutput): the default output device through cpal at the device's own rate and
//! channels, low latency, or no device at all (`Sink::Null`). The output's callback pulls the
//! timeline, sound or silence, so the stream never runs dry and the frames it has taken ARE the
//! timeline's samples; each callback's time stamp says when its first frame is heard, which is the
//! output's clock.
//!
//! The device's rate is never changed (another app's sound would glitch): a device that does not run
//! at 48 kHz gets the timeline resampled, a mono device both channels mixed, a device with more than
//! two channels the sound on its first two.

use super::playout::RATE;
use super::resample::Resampler;
use super::timeline::{OutputClock, PlayerTimeline};
use super::{now, Sink, Tap};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The ramp to silence before a stop (the iOS mixer's volume ramp: 25 ms).
pub(crate) const FADE_FRAMES: usize = 1200;
/// `Sink::Null`'s stand-ins for a device (iOSClient's manual rendering): it takes 5 ms at a time,
/// and what it takes would be heard 10 ms after.
pub(crate) const STAND_IN_CHUNK: usize = 240;
pub(crate) const STAND_IN_OUTPUT: f64 = 0.010;
/// The frames asked of the device at a time: 5 ms where the OS mixes for low latency by itself,
/// 10 ms elsewhere.
const WANTED_BUFFER: f64 = if cfg!(any(target_os = "macos", target_os = "windows")) { 0.005 } else { 0.010 };
/// Before the first callback has said how much the device takes at a time.
const IO_GUESS: f64 = 0.012;
/// The model's IO buffer is what the device takes at a time and this: the tail's deadline timer may
/// run this late.
const TIMER_SLACK: f64 = 0.002;

/// What has been rendered since the sound was made, whatever the engine: for a HUD and the checks.
#[derive(Default)]
pub(crate) struct Meter {
    frames: AtomicU64,
    sounding: AtomicU64,
    /// f32 and f64 bits, written by the one output callback at a time.
    peak: AtomicU32,
    energy: AtomicU64,
    xruns: AtomicU64,
    engines: AtomicU64,
}

/// What the outputs have rendered since the sound was made.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SoundStats {
    /// Outputs started (one per start of the sound, route loss or idle stop).
    pub engines: u64,
    /// Frames of the 48 kHz timeline the outputs took.
    pub frames: u64,
    /// Those that were not silence.
    pub sounding: u64,
    /// The largest sample, and the root mean square of the frames that were not silence.
    pub peak: f32,
    pub rms: f32,
    /// Underruns the device reported.
    pub xruns: u64,
}

impl Meter {
    pub(crate) fn stats(&self) -> SoundStats {
        let sounding = self.sounding.load(Ordering::Relaxed);
        let energy = f64::from_bits(self.energy.load(Ordering::Relaxed));
        SoundStats {
            engines: self.engines.load(Ordering::Relaxed),
            frames: self.frames.load(Ordering::Relaxed),
            sounding,
            peak: f32::from_bits(self.peak.load(Ordering::Relaxed)),
            rms: if sounding > 0 { (energy / (2.0 * sounding as f64)).sqrt() as f32 } else { 0.0 },
            xruns: self.xruns.load(Ordering::Relaxed),
        }
    }

    fn add(&self, stereo: &[f32]) {
        let (mut sounding, mut energy) = (0u64, 0f64);
        let mut peak = f32::from_bits(self.peak.load(Ordering::Relaxed));
        for f in stereo.chunks_exact(2) {
            if f[0] != 0.0 || f[1] != 0.0 {
                sounding += 1;
                energy += (f[0] * f[0] + f[1] * f[1]) as f64;
                peak = peak.max(f[0].abs()).max(f[1].abs());
            }
        }
        self.frames.fetch_add(stereo.len() as u64 / 2, Ordering::Relaxed);
        if sounding > 0 {
            self.sounding.fetch_add(sounding, Ordering::Relaxed);
            self.peak.store(peak.to_bits(), Ordering::Relaxed);
            let e = f64::from_bits(self.energy.load(Ordering::Relaxed)) + energy;
            self.energy.store(e.to_bits(), Ordering::Relaxed);
        }
    }
}

/// When timeline sample 0 is heard, from each callback's (sample, when it is heard): the stamps of
/// some outputs jitter by a callback's wake-up, and the playout wants a steady clock (rule 11
/// corrects by a frame at a time). A slow average, started afresh after a step of over 20 ms (an
/// underrun, a new route).
#[derive(Default)]
pub(crate) struct Steady {
    zero: f64,
    count: u32,
}

impl Steady {
    const STEP: f64 = 0.020;
    const SLOWEST: u32 = 32;

    /// `sample` is heard at `ear`: when it is heard on the steady clock.
    pub(crate) fn stamp(&mut self, sample: i64, ear: f64) -> f64 {
        let at = sample as f64 / RATE;
        let zero = ear - at;
        if self.count == 0 || (zero - self.zero).abs() > Self::STEP {
            self.zero = zero;
            self.count = 1;
        } else {
            self.count = (self.count + 1).min(Self::SLOWEST);
            self.zero += (zero - self.zero) / self.count as f64;
        }
        self.zero + at
    }

    pub(crate) fn forget(&mut self) {
        self.count = 0;
    }
}

/// What the callback and the audio thread share of one engine.
#[derive(Default)]
struct Shared {
    /// The stream cannot go on (its device went, its configuration changed): another is needed.
    dead: AtomicBool,
    /// The stream moved to another device by itself.
    rerouted: AtomicBool,
    /// The callback's steady clock starts afresh (a new route).
    forget: AtomicBool,
    /// The most device frames one callback took.
    most_frames: AtomicU32,
}

/// What an output callback does, whatever calls it: pulls the timeline, stamps the clock.
struct Puller {
    timeline: Arc<PlayerTimeline>,
    clock: Arc<OutputClock>,
    shared: Arc<Shared>,
    meter: Arc<Meter>,
    tap: Arc<Mutex<Option<Tap>>>,
    resampler: Option<Resampler>,
    steady: Steady,
    /// The timeline's frames for this callback, and the same at the device's rate.
    pulled: Vec<f32>,
    made: Vec<f32>,
}

impl Puller {
    /// The next `frames` stereo frames at the device's rate, taken at device time `taken`, the first
    /// of them heard at `ear`.
    fn render(&mut self, frames: usize, taken: f64, ear: f64) -> &[f32] {
        if self.shared.forget.swap(false, Ordering::Relaxed) {
            self.steady.forget();
        }
        self.shared.most_frames.fetch_max(frames as u32, Ordering::Relaxed);
        let (needed, lag) = match &self.resampler {
            Some(r) => (r.input_needed(frames), r.lag()),
            None => (frames, 0.0),
        };
        self.pulled.resize(needed * 2, 0.0);
        let from = self.timeline.read(&mut self.pulled, needed);
        // The first frame pulled is heard once the resampler's taps have passed it.
        let heard = self.steady.stamp(from, ear.max(taken) + lag / RATE);
        self.clock.stamp(from, heard);
        self.clock.wrote(from, taken);
        self.meter.add(&self.pulled);
        if let Ok(mut tap) = self.tap.try_lock() {
            if let Some(tap) = tap.as_mut() {
                tap(from, &self.pulled);
            }
        }
        match &mut self.resampler {
            Some(r) => {
                self.made.resize(frames * 2, 0.0);
                r.process(&self.pulled, &mut self.made);
                &self.made
            }
            None => &self.pulled,
        }
    }
}

enum Backend {
    Device(cpal::Stream),
    Null { stop: Arc<AtomicBool>, thread: Option<std::thread::JoinHandle<()>> },
}

pub(crate) struct SoundEngine {
    pub(crate) timeline: Arc<PlayerTimeline>,
    pub(crate) clock: Arc<OutputClock>,
    shared: Arc<Shared>,
    backend: Option<Backend>,
    /// The output, for the console.
    pub(crate) route: String,
    device_rate: f64,
}

impl SoundEngine {
    /// A running engine, or the reason there is none. `wake` is called (from any thread) when the
    /// engine has something to say: `take_dead`, `take_rerouted`.
    pub(crate) fn start(sink: Sink, meter: Arc<Meter>, tap: Arc<Mutex<Option<Tap>>>, wake: impl Fn() + Send + Clone + 'static) -> Result<SoundEngine, String> {
        let timeline = Arc::new(PlayerTimeline::new(2));
        let clock = Arc::new(OutputClock::default());
        let shared = Arc::new(Shared::default());
        let mut puller = Puller {
            timeline: timeline.clone(),
            clock: clock.clone(),
            shared: shared.clone(),
            meter: meter.clone(),
            tap,
            resampler: None,
            steady: Steady::default(),
            pulled: Vec::with_capacity(8192),
            made: Vec::with_capacity(8192),
        };
        let (backend, route, device_rate) = match sink {
            #[cfg(test)]
            Sink::Missing => return Err("no output device".into()),
            Sink::Null => {
                let stop = Arc::new(AtomicBool::new(false));
                let stopped = stop.clone();
                let thread = std::thread::Builder::new()
                    .name("sill.audio.null".into())
                    .spawn(move || {
                        // Sample 0 is taken now and would be heard STAND_IN_OUTPUT later.
                        let t0 = now();
                        let mut rendered = 0u64;
                        while !stopped.load(Ordering::Relaxed) {
                            let taken = now();
                            while (rendered + STAND_IN_CHUNK as u64) as f64 <= (taken - t0) * RATE + STAND_IN_CHUNK as f64 {
                                puller.render(STAND_IN_CHUNK, taken, t0 + rendered as f64 / RATE + STAND_IN_OUTPUT);
                                rendered += STAND_IN_CHUNK as u64;
                            }
                            std::thread::sleep(Duration::from_millis(2));
                        }
                    })
                    .map_err(|e| format!("no thread: {e}"))?;
                (Backend::Null { stop, thread: Some(thread) }, "no output device (null sink)".to_string(), RATE)
            }
            Sink::Device => {
                let host = cpal::default_host();
                let device = host.default_output_device().ok_or("no output device")?;
                let supported = device.default_output_config().map_err(|e| format!("the output's format: {e}"))?;
                let format = supported.sample_format();
                let mut config = supported.config();
                let rate = config.sample_rate as f64;
                let channels = config.channels as usize;
                if channels == 0 || !(8000.0..=768_000.0).contains(&rate) {
                    return Err(format!("an output of {channels} channels at {rate} Hz"));
                }
                if rate != RATE {
                    puller.resampler = Some(Resampler::new(RATE, rate));
                }
                let name = device.description().map(|d| d.name().to_string()).unwrap_or_else(|_| "an output".into());
                let wanted = match supported.buffer_size() {
                    cpal::SupportedBufferSize::Range { min, max } if min <= max => cpal::BufferSize::Fixed(((rate * WANTED_BUFFER).round() as u32).clamp(*min, *max)),
                    _ => cpal::BufferSize::Default,
                };
                config.buffer_size = wanted;
                // The stream's clock on ours: set once the stream is made, before it plays.
                let offset = Arc::new(AtomicU64::new(f64::NAN.to_bits()));
                let puller = Arc::new(Mutex::new(puller));
                let made = build(&device, config, format, puller.clone(), offset.clone(), shared.clone(), meter.clone(), wake.clone());
                let stream = match made {
                    Ok(s) => s,
                    Err(e) if wanted != cpal::BufferSize::Default => {
                        log::info!("audio: the output refused {wanted:?} ({e}): its own buffer size");
                        config.buffer_size = cpal::BufferSize::Default;
                        build(&device, config, format, puller, offset.clone(), shared.clone(), meter.clone(), wake).map_err(|e| format!("the output: {e}"))?
                    }
                    Err(e) => return Err(format!("the output: {e}")),
                };
                offset.store((now() - stream.now().as_nanos() as f64 / 1e9).to_bits(), Ordering::Relaxed);
                stream.play().map_err(|e| format!("the output did not start: {e}"))?;
                let route = format!("{name}, {rate} Hz, {channels} ch, {format}");
                (Backend::Device(stream), route, rate)
            }
        };
        meter.engines.fetch_add(1, Ordering::Relaxed);
        Ok(SoundEngine { timeline, clock, shared, backend: Some(backend), route, device_rate })
    }

    /// The model's IO buffer: the output takes a callback's frames at once (a sample is taken up to
    /// that long before the time the clock gives it), and the tail's deadline timer may run 2 ms late.
    pub(crate) fn io_buffer(&self) -> f64 {
        match self.shared.most_frames.load(Ordering::Relaxed) {
            0 => IO_GUESS,
            n => n as f64 / self.device_rate + TIMER_SLACK,
        }
    }

    /// One part of a packet at `at` on the timeline, or right after the one before (None).
    pub(crate) fn schedule(&self, left: &[f32], right: &[f32], at: Option<f64>) {
        let n = left.len().min(right.len());
        let mut pcm = Vec::with_capacity(n * 2);
        for i in 0..n {
            pcm.push(left[i]);
            pcm.push(right[i]);
        }
        self.timeline.schedule(pcm, at);
    }

    /// Down to silence over 25 ms, before a stop.
    pub(crate) fn turn_down(&self) {
        self.timeline.fade_out(FADE_FRAMES);
    }

    /// The stream died (its device went away, its configuration changed), once.
    pub(crate) fn take_dead(&self) -> bool {
        self.shared.dead.swap(false, Ordering::Relaxed)
    }

    /// The stream moved to another device by itself, once: what the clock measured was the old one's.
    pub(crate) fn take_rerouted(&self) -> bool {
        let moved = self.shared.rerouted.swap(false, Ordering::Relaxed);
        if moved {
            self.clock.forget_latency();
            self.shared.forget.store(true, Ordering::Relaxed);
        }
        moved
    }

    /// Stops the output and lets it go. On the audio thread.
    pub(crate) fn stop(&mut self) {
        match self.backend.take() {
            Some(Backend::Device(stream)) => {
                let _ = stream.pause();
                drop(stream);
            }
            Some(Backend::Null { stop, thread }) => {
                stop.store(true, Ordering::Relaxed);
                if let Some(t) = thread {
                    let _ = t.join();
                }
            }
            None => {}
        }
    }
}

impl Drop for SoundEngine {
    fn drop(&mut self) {
        self.stop();
    }
}

#[allow(clippy::too_many_arguments)]
fn build(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    format: cpal::SampleFormat,
    puller: Arc<Mutex<Puller>>,
    offset: Arc<AtomicU64>,
    shared: Arc<Shared>,
    meter: Arc<Meter>,
    wake: impl Fn() + Send + 'static,
) -> Result<cpal::Stream, cpal::Error> {
    use cpal::SampleFormat as F;
    macro_rules! with {
        ($t:ty) => {
            stream::<$t>(device, config, puller, offset, shared, meter, wake)
        };
    }
    match format {
        F::F32 => with!(f32),
        F::I16 => with!(i16),
        F::U16 => with!(u16),
        F::I32 => with!(i32),
        F::I24 => with!(cpal::I24),
        F::I8 => with!(i8),
        F::U8 => with!(u8),
        F::U32 => with!(u32),
        F::I64 => with!(i64),
        F::U64 => with!(u64),
        F::F64 => with!(f64),
        other => Err(cpal::Error::with_message(cpal::ErrorKind::UnsupportedConfig, format!("sample format {other}"))),
    }
}

fn stream<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    puller: Arc<Mutex<Puller>>,
    offset: Arc<AtomicU64>,
    shared: Arc<Shared>,
    meter: Arc<Meter>,
    wake: impl Fn() + Send + 'static,
) -> Result<cpal::Stream, cpal::Error>
where
    T: cpal::SizedSample + cpal::FromSample<f32>,
{
    let channels = config.channels as usize;
    device.build_output_stream(
        config,
        move |data: &mut [T], info: &cpal::OutputCallbackInfo| {
            let taken = now();
            let stamp = info.timestamp();
            // When the first frame is heard: the stream's own playback time on our clock, or, where
            // that is not to be believed, as long after now as the stream says.
            let lead = stamp.playback.saturating_duration_since(stamp.callback).as_secs_f64();
            let offset = f64::from_bits(offset.load(Ordering::Relaxed));
            let direct = stamp.playback.as_nanos() as f64 / 1e9 + offset;
            let ear = if direct.is_finite() && (-0.002..5.0).contains(&(direct - taken)) { direct } else { taken + lead.min(5.0) };
            // The lock is this callback's alone but for a refused buffer size's second try.
            let Ok(mut puller) = puller.try_lock() else {
                data.fill(T::EQUILIBRIUM);
                return;
            };
            let stereo = puller.render(data.len() / channels, taken, ear);
            for (frame, s) in data.chunks_exact_mut(channels).zip(stereo.chunks_exact(2)) {
                let (l, r) = (s[0].clamp(-1.0, 1.0), s[1].clamp(-1.0, 1.0));
                if channels == 1 {
                    frame[0] = T::from_sample((l + r) * 0.5);
                } else {
                    frame[0] = T::from_sample(l);
                    frame[1] = T::from_sample(r);
                    for x in &mut frame[2..] {
                        *x = T::EQUILIBRIUM;
                    }
                }
            }
        },
        move |e: cpal::Error| match e.kind() {
            cpal::ErrorKind::Xrun => {
                meter.xruns.fetch_add(1, Ordering::Relaxed);
            }
            cpal::ErrorKind::RealtimeDenied => log::info!("audio: {e}"),
            cpal::ErrorKind::DeviceChanged => {
                shared.rerouted.store(true, Ordering::Relaxed);
                wake();
            }
            _ => {
                log::warn!("audio: the output failed: {e}");
                shared.dead.store(true, Ordering::Relaxed);
                wake();
            }
        },
        None,
    )
}
