//! The Mac's sound on this device (iOSClient's AudioOutput, docs/audio-plan.md §7.3–7.5): kind 43's
//! formats and packets in, through the playout model (`AudioPlayout`, which decides) and the decoder
//! (`EldDecoder`), onto the timeline an output plays (`SoundEngine`). Every line of Sill's runs on the
//! `sill.audio` thread, which owns the model, the decoder and the output stream; the output's
//! callback has the device's own real-time thread.
//!
//! The engine runs while a format has come, the sound is not muted, the app is in the foreground (as
//! the client says) and a packet came within 10 s. It stops (after a 25 ms ramp) on mute, in the
//! background, at `stop()` and after 10 s without a packet, and the next packet starts it again.
//! While the sound cannot be heard nothing is decoded, and each packet still counts for the floor and
//! the cover. The output is opened shared, at the device's own rate: another app's sound keeps
//! playing. An output that goes away (unplugged, the default changed) takes the engine with it, and
//! the next packet starts another on the default of the moment; with no output at all one is looked
//! for every 5 s, and nothing else changes.
//!
//! `stop()` forgets the stream's timing (the model's session reset), the decoder and the stats, and
//! keeps the format: the Mac sends no new format when a locked Mac unlocks or the app comes back
//! within the session's grace, so the next packet of that epoch starts the sound again (half a second
//! after the stop at the earliest, as a join: decoded, dropped whole, the next faded in). A new
//! format replaces it.
//!
//! Threads: the client hands formats, packets and frames over from its network thread (stamped
//! there), everything else from any thread; `take_second` reads under its own lock.

use super::decoder::EldDecoder;
use super::engine::{Meter, SoundEngine, SoundStats};
use super::message::{codec, AudioFormat, AudioMessage, AudioPackets};
use super::playout::{self, AudioPlayout, Output, Part, Reset, Schedule, Second, RATE};
use super::timeline::ClockFeed;
use super::{eld_config, now, Sink, Tap};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// About half a second of frames at 60 fps: the median the guard starts from.
const FRAMES_KEPT_BEFORE_FORMAT: usize = 32;
/// Idle: no packet for this long stops the engine (rule 14).
const IDLE_AFTER: f64 = 10.0;
/// A fading stop: the 25 ms ramp and a little more, plus the output's latency.
const FADE_WAIT: f64 = 0.05;
const QUIET_AFTER_STOP: f64 = 0.5;
const ENGINE_RETRY: f64 = 5.0;

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

enum Cmd {
    Format { format: AudioFormat, before: Vec<(f64, f64)> },
    Packets { packets: AudioPackets, stamp: f64, came: f64, before: Vec<(f64, f64)> },
    Frame { stamp: f64, arrival: f64 },
    Stop,
    Mute(bool),
    Foreground(bool),
    StreamFps(i64),
    Rtt(Option<i32>),
    Away(bool),
    Refresh(f64),
    /// The engine has something to say.
    Wake,
    Release,
}

/// The network thread's: until a format comes, frames are only kept here (the last 32).
#[derive(Default)]
struct Gate {
    format_seen: bool,
    frames_before_format: VecDeque<(f64, f64)>,
    kept_epoch: Option<u16>,
}

/// The last closed second, for the stats.
#[derive(Default)]
struct Stats {
    last_second: Option<Second>,
    late_since_taken: i32,
    any_played: bool,
}

/// The Mac's sound on this device. `Send + Sync`: every method may be called from any thread.
pub struct MacSound {
    tx: mpsc::Sender<Cmd>,
    gate: Mutex<Gate>,
    stats: Arc<Mutex<Stats>>,
    muted_shown: AtomicBool,
    released: AtomicBool,
    meter: Arc<Meter>,
    tap: Arc<Mutex<Option<Tap>>>,
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
}

const _: () = {
    const fn send_sync<T: Send + Sync>() {}
    send_sync::<MacSound>()
};

impl Default for MacSound {
    fn default() -> Self {
        Self::new()
    }
}

impl MacSound {
    /// The sound on the default output device.
    pub fn new() -> Self {
        Self::with_sink(Sink::Device)
    }

    pub fn with_sink(sink: Sink) -> Self {
        let (tx, rx) = mpsc::channel();
        let stats = Arc::new(Mutex::new(Stats::default()));
        let meter = Arc::new(Meter::default());
        let tap: Arc<Mutex<Option<Tap>>> = Arc::new(Mutex::new(None));
        let mut worker = Worker::new(sink, tx.clone(), stats.clone(), meter.clone(), tap.clone());
        let thread = std::thread::Builder::new().name("sill.audio".into()).spawn(move || worker.run(rx)).ok();
        if thread.is_none() {
            log::warn!("audio: no sound: its thread did not start");
        }
        MacSound {
            tx,
            gate: Mutex::new(Gate::default()),
            stats,
            muted_shown: AtomicBool::new(false),
            released: AtomicBool::new(false),
            meter,
            tap,
            thread: Mutex::new(thread),
        }
    }

    /// The codecs of the Mac's sound this device plays, best first (the hello's `audio`); empty: none.
    pub fn codecs(&self) -> Vec<String> {
        if EldDecoder::available() {
            vec![codec::AAC_ELD.to_string()]
        } else {
            Vec::new()
        }
    }

    /// Mute on this device alone: the Mac keeps sending, nothing is heard.
    pub fn set_muted(&self, muted: bool) {
        self.muted_shown.store(muted, Ordering::Relaxed);
        self.post(Cmd::Mute(muted));
    }

    pub fn muted(&self) -> bool {
        self.muted_shown.load(Ordering::Relaxed)
    }

    /// One kind 43 payload as it came, with its header's time stamp (host wall clock, s). The
    /// network thread's; never blocks.
    pub fn on_message(&self, payload: &[u8], host_timestamp: f64) {
        if self.released.load(Ordering::Relaxed) {
            return;
        }
        let came = now();
        match AudioMessage::parse(payload) {
            None => {}
            Some(AudioMessage::Format(format)) => {
                // The session's first format hands the frames kept before it to the model first.
                let before = {
                    let mut g = lock(&self.gate);
                    let before = if g.format_seen { Vec::new() } else { g.frames_before_format.iter().copied().collect() };
                    g.format_seen = true;
                    g.frames_before_format.clear();
                    g.kept_epoch = format.epoch.and_then(|e| u16::try_from(e).ok());
                    before
                };
                self.post(Cmd::Format { format, before });
            }
            Some(AudioMessage::Packets(packets)) => {
                // After a stop(): packets of the kept format's epoch start it again, its frames first.
                let before = {
                    let mut g = lock(&self.gate);
                    if g.format_seen || g.kept_epoch != Some(packets.epoch) {
                        Vec::new()
                    } else {
                        g.format_seen = true;
                        g.frames_before_format.drain(..).collect()
                    }
                };
                self.post(Cmd::Packets { packets, stamp: host_timestamp, came, before });
            }
        }
    }

    /// One video frame as it came, with its header's time stamp (host wall clock, s): for the
    /// picture's lag (rule 3). The network thread's; never blocks.
    pub fn on_video_frame(&self, host_timestamp: f64) {
        if self.released.load(Ordering::Relaxed) {
            return;
        }
        let arrival = now();
        let seen = {
            let mut g = lock(&self.gate);
            if !g.format_seen {
                if g.frames_before_format.len() >= FRAMES_KEPT_BEFORE_FORMAT {
                    g.frames_before_format.pop_front();
                }
                g.frames_before_format.push_back((host_timestamp, arrival));
            }
            g.format_seen
        };
        if seen {
            self.post(Cmd::Frame { stamp: host_timestamp, arrival });
        }
    }

    /// Stop playing and forget the stream's timing: the session ended, the app left the screen, the
    /// Mac locked, Send Audio went off. The format stays for its epoch's next packet.
    pub fn stop(&self) {
        {
            let mut g = lock(&self.gate);
            g.format_seen = false;
            g.frames_before_format.clear();
        }
        self.post(Cmd::Stop);
    }

    /// ClientStats' sound fields for the second just ended: `audioBehindMs` (the median ms the sound
    /// trailed the picture in the last closed second, -1 for a second without sound) and `audioLate`
    /// (packets too late to play since the last call); None while no sound has played this session.
    pub fn take_second(&self) -> Option<(i32, i32)> {
        let mut s = lock(&self.stats);
        if !s.any_played {
            return None;
        }
        let r = (s.last_second.as_ref().and_then(|x| x.behind_ms).unwrap_or(-1), s.late_since_taken);
        s.late_since_taken = 0;
        Some(r)
    }

    /// The stream's frame rate, for the guard's one frame (rule 4): the host's `stream.fps`.
    pub fn set_stream_fps(&self, fps: i64) {
        self.post(Cmd::StreamFps(fps));
    }

    /// The pings' median round trip of the last second, ms (rule 13's threshold); None: none.
    pub fn set_rtt(&self, median_ms: Option<i32>) {
        self.post(Cmd::Rtt(median_ms));
    }

    /// The session is away from home (the jitter cover's start and bounds, rule 2).
    pub fn set_away(&self, away: bool) {
        self.post(Cmd::Away(away));
    }

    /// The display's refresh rate, Hz (the picture's lag counts 1.5 refreshes, rule 3); 60 until said.
    pub fn set_refresh_hz(&self, hz: f64) {
        self.post(Cmd::Refresh(hz));
    }

    /// The app can be seen (true, as it starts) or not: out of sight nothing is decoded or played,
    /// as on a phone in the background. A client that plays on while hidden never calls this.
    pub fn set_foreground(&self, on: bool) {
        self.post(Cmd::Foreground(on));
    }

    /// The last second's numbers, for a HUD.
    pub fn latest_second(&self) -> Option<Second> {
        lock(&self.stats).last_second.clone()
    }

    /// What the outputs have rendered since this was made.
    pub fn stats(&self) -> SoundStats {
        self.meter.stats()
    }

    /// Hears what the output takes (a meter, a loopback check); None: no more.
    pub fn set_tap(&self, tap: Option<Tap>) {
        *lock(&self.tap) = tap;
    }

    /// The app is going away for good: the output stops, the thread ends. Also on drop.
    pub fn release(&self) {
        if self.released.swap(true, Ordering::Relaxed) {
            return;
        }
        let _ = self.tx.send(Cmd::Release);
        if let Some(t) = lock(&self.thread).take() {
            let _ = t.join();
        }
    }

    fn post(&self, cmd: Cmd) {
        if !self.released.load(Ordering::Relaxed) {
            let _ = self.tx.send(cmd);
        }
    }
}

impl Drop for MacSound {
    fn drop(&mut self) {
        self.release();
    }
}

/// The epoch playing: its packet size, and whether this device plays it.
#[derive(Clone, Copy)]
struct Playing {
    epoch: u16,
    frames_per_packet: usize,
    playable: bool,
}

/// Everything on `sill.audio`.
struct Worker {
    sink: Sink,
    tx: mpsc::Sender<Cmd>,
    stats: Arc<Mutex<Stats>>,
    meter: Arc<Meter>,
    tap: Arc<Mutex<Option<Tap>>>,

    model: AudioPlayout,
    decoder: Option<EldDecoder>,
    playing: Option<Playing>,
    /// The last format, kept across stop(): its epoch's next packet starts the sound again.
    kept: Option<AudioFormat>,
    /// Each kept packet's frames (after its priming), by the model's id.
    frames: HashMap<i32, (Vec<f32>, Vec<f32>)>,
    engine: Option<SoundEngine>,
    /// Engines turned down, and when each is let go.
    fading: Vec<(f64, SoundEngine)>,
    /// No new engine before this (one failed to start).
    engine_retry_at: f64,
    muted: bool,
    foreground: bool,
    /// Packets of the kept format start nothing before this (half a second after a stop()).
    quiet_until: f64,
    last_packet_at: Option<f64>,
    /// When this thread last finished starting or stopping an engine or making a decoder (rule 6).
    busy_until: f64,
    /// The tail's deadline as last armed, and the second's end.
    deadline_at: Option<f64>,
    second_at: Option<f64>,
    refresh: f64,
    stream_fps: f64,
    /// The output's clock to the model.
    feed: ClockFeed,
}

impl Worker {
    fn new(sink: Sink, tx: mpsc::Sender<Cmd>, stats: Arc<Mutex<Stats>>, meter: Arc<Meter>, tap: Arc<Mutex<Option<Tap>>>) -> Self {
        let mut model = AudioPlayout::new();
        model.latency(0.0, 0.005, 1.0 / 60.0, 60.0);
        Worker {
            sink,
            tx,
            stats,
            meter,
            tap,
            model,
            decoder: None,
            playing: None,
            kept: None,
            frames: HashMap::new(),
            engine: None,
            fading: Vec::new(),
            engine_retry_at: 0.0,
            muted: false,
            foreground: true,
            quiet_until: 0.0,
            last_packet_at: None,
            busy_until: 0.0,
            deadline_at: None,
            second_at: None,
            refresh: 1.0 / 60.0,
            stream_fps: 60.0,
            feed: ClockFeed::new(),
        }
    }

    fn run(&mut self, rx: mpsc::Receiver<Cmd>) {
        loop {
            let next = [self.deadline_at, self.second_at, self.fading.iter().map(|f| f.0).reduce(f64::min)].into_iter().flatten().reduce(f64::min);
            let cmd = match next {
                // Never early (an early tick fades a tail the next packet would have followed); the
                // IO buffer's 2 ms of slack covers the lateness.
                Some(at) => match rx.recv_timeout(Duration::from_secs_f64((at - now()).clamp(0.0, 3600.0))) {
                    Ok(c) => Some(c),
                    Err(RecvTimeoutError::Timeout) => None,
                    Err(RecvTimeoutError::Disconnected) => break,
                },
                None => match rx.recv() {
                    Ok(c) => Some(c),
                    Err(_) => break,
                },
            };
            match cmd {
                Some(Cmd::Release) => break,
                Some(c) => self.handle(c),
                None => {}
            }
            self.engine_events();
            self.timers();
        }
        self.stop_engine("released", false);
        self.fading.clear();
        self.decoder = None;
    }

    fn handle(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Format { format, before } => {
                for (stamp, arrival) in before {
                    self.model.frame(stamp, arrival);
                }
                self.receive_format(&format, false);
            }
            Cmd::Packets { packets, stamp, came, before } => {
                for (s, arrival) in before {
                    self.model.frame(s, arrival);
                }
                self.receive_packets(&packets, stamp, came);
            }
            Cmd::Frame { stamp, arrival } => self.model.frame(stamp, arrival),
            Cmd::Stop => {
                self.stop_engine("stopped", true);
                self.model.reset(Reset::Session);
                self.feed.forget();
                self.frames.clear();
                self.decoder = None;
                self.playing = None;
                self.last_packet_at = None;
                self.second_at = None;
                self.quiet_until = now() + QUIET_AFTER_STOP;
                *lock(&self.stats) = Stats::default();
            }
            Cmd::Mute(on) => {
                if self.muted != on {
                    self.muted = on;
                    // Unmuting is the person asking for sound: an output that failed to start is tried again.
                    if !on {
                        self.engine_retry_at = 0.0;
                    }
                    self.update_silence(now());
                    if on {
                        self.stop_engine("muted", true)
                    } else {
                        log::info!("audio: unmuted")
                    }
                }
            }
            Cmd::Foreground(on) => {
                if self.foreground != on {
                    self.foreground = on;
                    self.update_silence(now());
                    if !on {
                        self.stop_engine("in the background", true);
                    }
                }
            }
            Cmd::StreamFps(fps) => {
                if (1..=1000).contains(&fps) {
                    self.stream_fps = fps as f64;
                    self.feed.forget();
                    self.feed_reading(now());
                }
            }
            Cmd::Rtt(ms) => self.model.rtt(ms.filter(|m| *m >= 0).map(|m| m as f64 / 1000.0)),
            Cmd::Away(away) => self.model.away(away),
            Cmd::Refresh(hz) => {
                if hz.is_finite() && hz > 0.0 {
                    self.refresh = 1.0 / hz;
                    self.feed.forget();
                    if self.engine.is_none() {
                        self.model.latency(self.model.output_latency(), self.model.io_buffer(), self.refresh, self.stream_fps);
                    }
                    self.feed_reading(now());
                }
            }
            Cmd::Wake | Cmd::Release => {}
        }
    }

    // MARK: - Formats and packets

    fn receive_format(&mut self, f: &AudioFormat, again: bool) {
        let Some(epoch) = f.epoch.and_then(|e| u16::try_from(e).ok()) else { return };
        let Some(fpp) = f.frames_per_packet else { return };
        let packet_frames = fpp.clamp(1, 1 << 20) as usize;
        let priming = f.priming_frames.unwrap_or(0).clamp(0, 1 << 20);
        let cookie = f.cookie.as_deref().unwrap_or(&[]);
        if !again {
            self.kept = Some(f.clone());
            self.quiet_until = 0.0;
        }
        if !self.model.format(epoch as i32, packet_frames as i64, priming, cookie) {
            return;
        }
        self.decoder = None;
        let asc = eld_config::audio_specific_config(cookie);
        let playable = f.codec.as_deref() == Some(codec::AAC_ELD)
            && f.sample_rate.unwrap_or(48_000) == 48_000
            && f.channels.unwrap_or(2) == 2
            && (1..=8192).contains(&fpp)
            && EldDecoder::available();
        let mut refused = None;
        if let (true, Some(asc)) = (playable, &asc) {
            match EldDecoder::new(asc, packet_frames) {
                Ok(d) => self.decoder = Some(d),
                Err(e) => refused = Some(e),
            }
            self.busy_until = now();
        }
        self.playing = Some(Playing { epoch, frames_per_packet: packet_frames, playable: self.decoder.is_some() });
        if self.decoder.is_some() {
            let info = asc.as_deref().and_then(eld_config::parse);
            let what = f.app.as_deref().unwrap_or(match f.source.as_deref() {
                Some(AudioFormat::SOURCE_DESKTOP) => "the whole Mac",
                Some(AudioFormat::SOURCE_TEST) => "a test tone",
                _ => "?",
            });
            log::info!(
                "audio: format {} {} Hz {} ch, {packet_frames} frames (priming {priming}), epoch {epoch}, {what}{}; config {info:?}",
                f.codec.as_deref().unwrap_or("?"),
                f.sample_rate.unwrap_or(48_000),
                f.channels.unwrap_or(2),
                if again { " (again, after a stop)" } else { "" }
            );
            if let Some(i) = info.filter(|i| i.frame_length as usize != packet_frames) {
                log::warn!("audio: the config says {} frames a packet, the format {packet_frames}", i.frame_length);
            }
        } else {
            let why = match refused {
                Some(e) => format!(" (the decoder refused it: {e})"),
                None => String::new(),
            };
            log::info!("audio: cannot play {}{why}", f.codec.as_deref().unwrap_or("an unnamed codec"));
        }
    }

    fn receive_packets(&mut self, p: &AudioPackets, stamp: f64, came: f64) {
        if self.playing.is_none() {
            // After a stop(): the kept format's epoch starts again.
            let Some(k) = self.kept.clone() else { return };
            if k.epoch != Some(p.epoch as i64) {
                return;
            }
            self.receive_format(&k, true);
        }
        let Some(fmt) = self.playing else { return };
        if p.epoch != fmt.epoch || !fmt.playable {
            return;
        }
        match &self.decoder {
            None => return,
            Some(d) if d.broken() => {
                log::warn!("audio: the decoder gives nothing; no sound this epoch");
                self.playing = Some(Playing { playable: false, ..fmt });
                return;
            }
            Some(_) => {}
        }
        let t = now();
        self.last_packet_at = Some(t);
        if self.second_at.is_none() {
            self.second_at = Some(t + 1.0);
        }
        self.update_silence(t);
        if !self.model.muted() && self.engine.is_none() && t >= self.engine_retry_at {
            self.start_engine();
        }
        let step = fmt.frames_per_packet as f64 / RATE;
        for (i, data) in p.packets.iter().enumerate() {
            let t = now();
            self.feed_reading(t);
            let plan = self.model.packet(
                p.epoch as i32,
                p.seq.wrapping_add(i as u32),
                stamp + i as f64 * step,
                p.segment_start && i == 0,
                (came < self.busy_until).then_some(came),
                t,
            );
            if plan.decode {
                if let Some(dec) = self.decoder.as_mut() {
                    if plan.reset_first {
                        dec.reset();
                    }
                    let decoded = dec.decode(data);
                    if let Some(id) = plan.id {
                        let keep = fmt.frames_per_packet.saturating_sub(plan.drop_front);
                        match decoded {
                            Some((mut l, mut r)) if l.len() == fmt.frames_per_packet && r.len() == fmt.frames_per_packet => {
                                l.drain(..plan.drop_front.min(l.len()));
                                r.drain(..plan.drop_front.min(r.len()));
                                self.frames.insert(id, (l, r));
                            }
                            _ => {
                                // Silence in its place; the decoder starts afresh at the next packet.
                                self.model.decode_failed();
                                self.frames.insert(id, (vec![0.0; keep], vec![0.0; keep]));
                            }
                        }
                    }
                }
            }
            self.apply(plan.output);
        }
        self.deadline_at = self.model.deadline();
    }

    /// What the model says: schedule, or forget.
    fn apply(&mut self, out: Output) {
        for id in &out.discards {
            self.frames.remove(id);
        }
        for s in &out.schedules {
            self.schedule(s);
        }
        if let Some((by, cause)) = self.model.take_jump() {
            log::info!("audio: jump {:+.0} ms ({})", by * 1000.0, cause.words());
        }
    }

    fn schedule(&mut self, s: &Schedule) {
        let pcm = if s.part == Part::Tail { self.frames.remove(&s.id) } else { self.frames.get(&s.id).cloned() };
        let (Some((left, right)), Some(e)) = (pcm, &self.engine) else { return };
        let end = (s.offset + s.count).min(left.len()).min(right.len());
        if s.offset >= end {
            return;
        }
        let mut l = left[s.offset..end].to_vec();
        let mut r = right[s.offset..end].to_vec();
        if s.change != 0 {
            playout::change(&mut l, &mut r, s.change);
        }
        if s.fade_in {
            playout::fade(&mut l, true);
            playout::fade(&mut r, true);
        }
        if s.fade_out {
            playout::fade(&mut l, false);
            playout::fade(&mut r, false);
        }
        e.schedule(&l, &r, s.at);
    }

    /// The timeline as the output last stamped it, and its latency when it moved (ClockFeed).
    fn feed_reading(&mut self, now: f64) {
        let Some(e) = &self.engine else { return };
        let io = e.io_buffer();
        // What the output takes at a time is known from its first callback on: the need follows it.
        if self.feed.given() >= 0.0 && (io - self.model.io_buffer()).abs() > 0.0005 {
            self.model.latency(self.feed.given(), io, self.refresh, self.stream_fps);
        }
        let before = (self.feed.given(), self.model.io_buffer());
        let out = self.feed.feed(&mut self.model, &e.clock, io, self.refresh, self.stream_fps, now);
        if (self.feed.given(), self.model.io_buffer()) != before {
            log::info!("audio: the output takes {:.1} ms at a time and is heard {:.1} ms after", (io - 0.002) * 1000.0, self.feed.given() * 1000.0);
        }
        if let Some(out) = out {
            self.apply(out);
        }
    }

    // MARK: - The tail's deadline and the second

    fn timers(&mut self) {
        let t = now();
        if self.deadline_at.is_some_and(|d| t >= d) {
            self.feed_reading(t);
            // Its deadline as the fresh reading has it, and no later than now.
            let at = t.max(self.model.deadline().unwrap_or(t));
            let out = self.model.tick(at);
            self.apply(out);
            self.deadline_at = self.model.deadline();
        }
        if self.second_at.is_some_and(|s| t >= s) {
            self.second_at = Some(t + 1.0);
            self.close_second(t);
        }
        if self.fading.iter().any(|f| t >= f.0) {
            self.fading.retain(|f| t < f.0); // dropped: stopped
            self.busy_until = now();
        }
    }

    fn close_second(&mut self, t: f64) {
        let s = self.model.take_second(t);
        if self.engine.is_some() || s.placed > 0 || s.duplicates > 0 {
            log::debug!(
                "audio: {}; need {} (jitter {}); late {}; error {}; frames +{} -{}; jumps {}; duplicates {}; joins {}",
                s.behind_ms.map(|b| format!("{b} ms behind the picture")).unwrap_or_else(|| "nothing played".into()),
                ms(s.need),
                ms(s.cover),
                s.late,
                s.error.map(|e| format!("{:+.1} ms", e * 1000.0)).unwrap_or_else(|| "–".into()),
                s.frames_added,
                s.frames_dropped,
                s.jumps,
                s.duplicates,
                s.joins
            );
        }
        {
            let mut stats = lock(&self.stats);
            stats.late_since_taken = stats.late_since_taken.saturating_add(s.late as i32);
            if s.placed > 0 {
                stats.any_played = true;
            }
            stats.last_second = Some(s);
        }
        if self.last_packet_at.is_some_and(|last| t - last >= IDLE_AFTER) {
            // Rule 14: idle. The tail in hand has faded already.
            if self.engine.is_some() {
                self.stop_engine("idle", false);
            }
            self.second_at = None;
            self.last_packet_at = None;
        }
    }

    // MARK: - The engine

    fn start_engine(&mut self) {
        let tx = self.tx.clone();
        let wake = move || {
            let _ = tx.send(Cmd::Wake);
        };
        match SoundEngine::start(self.sink, self.meter.clone(), self.tap.clone(), wake) {
            Ok(e) => {
                log::info!("audio: engine on, {}", e.route);
                self.engine = Some(e);
                let out = self.model.reset(Reset::Playback); // a new timeline: what waited for the last one goes
                self.apply(out);
                self.feed.forget();
            }
            Err(why) => {
                self.engine_retry_at = now() + ENGINE_RETRY;
                log::warn!("audio: no sound: {why}");
            }
        }
        self.busy_until = now();
    }

    /// Stops the engine; `fade` turns it down first (25 ms) and lets it go once that has been heard.
    /// The model's timeline goes with it.
    fn stop_engine(&mut self, reason: &str, fade: bool) {
        self.deadline_at = None;
        let out = self.model.reset(Reset::Playback);
        self.apply(out);
        self.frames.clear();
        let Some(mut e) = self.engine.take() else { return };
        if fade {
            e.turn_down();
            let wait = FADE_WAIT + e.clock.latency().unwrap_or(0.0).clamp(0.0, 0.25);
            self.fading.push((now() + wait, e));
        } else {
            e.stop();
            self.busy_until = now();
        }
        if reason != "idle" {
            log::info!("audio: engine off ({reason})");
        }
    }

    /// What the output said since the last look: it died (the next packet starts another), or it
    /// moved to another device (the next packet is placed by time at its latency).
    fn engine_events(&mut self) {
        self.fading.retain(|f| !f.1.take_dead());
        let Some(e) = &self.engine else { return };
        if e.take_dead() {
            self.engine = None; // dropped: stopped
            let out = self.model.reset(Reset::Playback);
            self.apply(out);
            self.frames.clear();
            self.deadline_at = None;
            self.busy_until = now();
            log::warn!("audio: the output went away; the next packet starts another");
        } else if e.take_rerouted() {
            self.feed.forget();
            let out = self.model.reset(Reset::Jump);
            self.apply(out);
            log::info!("audio: the output moved to another device");
        }
    }

    /// Nothing is decoded while the sound cannot be heard: muted, in the background, or just after a
    /// stop(). Each packet still counts for the floor and the cover, and once it can be heard again
    /// the decoder's state is stale, so the next packet starts it afresh (a playback reset, rule 9).
    fn update_silence(&mut self, now: f64) {
        let silent = self.muted || !self.foreground || now < self.quiet_until;
        if silent == self.model.muted() {
            return;
        }
        self.model.mute(silent);
        if !silent {
            let out = self.model.reset(Reset::Playback);
            self.apply(out);
        }
    }
}

fn ms(s: f64) -> String {
    format!("{} ms", playout::rounded(s * 1000.0) as i64)
}
