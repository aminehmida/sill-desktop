//! The output side's pure parts: the timeline the output pulls, the clock its time stamps make, and
//! the feed that hands the clock to the playout.

use super::playout::{self, AudioPlayout, Output};
use std::collections::VecDeque;
use std::sync::Mutex;

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The player's timeline (what AVAudioPlayerNode is to iOSClient's AudioOutput): buffers scheduled at
/// a sample of the timeline or right after the one before, played in order, silence between them.
/// The output pulls it a chunk at a time, sound or silence, so its frame count IS the timeline:
/// sample s is the s-th frame the output took.
///
/// Thread-safe: the audio thread schedules, the output's callback reads.
pub struct PlayerTimeline {
    channels: usize,
    inner: Mutex<Inner>,
}

struct Item {
    start: i64,
    pcm: Vec<f32>,
    frames: i64,
}

impl Item {
    fn end(&self) -> i64 {
        self.start + self.frames
    }
}

#[derive(Default)]
struct Inner {
    items: VecDeque<Item>,
    /// The next frame the output reads: every frame before it has gone to the output.
    position: i64,
    /// Where the last buffer scheduled ends.
    end: i64,
    /// A fade to silence asked (the engine stopping): its length, how far it has gone.
    fade_frames: usize,
    fade_done: usize,
}

impl PlayerTimeline {
    pub fn new(channels: usize) -> Self {
        PlayerTimeline { channels: channels.max(1), inner: Mutex::new(Inner::default()) }
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    pub fn position(&self) -> i64 {
        lock(&self.inner).position
    }

    /// One buffer of interleaved frames, from sample `at` (rounded) or, with None, right after the
    /// buffer before it. A buffer never starts before the one scheduled before it ends, nor before
    /// what the output has already read (it would be cut: AVAudioPlayerNode plays a late buffer late
    /// too). Returns its start.
    pub fn schedule(&self, pcm: Vec<f32>, at: Option<f64>) -> i64 {
        let mut t = lock(&self.inner);
        let frames = (pcm.len() / self.channels) as i64;
        let wanted = match at {
            Some(a) if a.is_finite() => playout::rounded(a) as i64,
            _ => t.end,
        };
        let start = wanted.max(t.end).max(t.position);
        if frames > 0 {
            t.items.push_back(Item { start, pcm, frames });
            t.end = start.saturating_add(frames);
        }
        start
    }

    /// The next `frames` frames into `out` (interleaved): what is scheduled there, silence
    /// elsewhere, and the fade to silence once asked. Returns the timeline sample of the first.
    pub fn read(&self, out: &mut [f32], frames: usize) -> i64 {
        let ch = self.channels;
        let frames = frames.min(out.len() / ch);
        let mut guard = lock(&self.inner);
        let t = &mut *guard;
        let from = t.position;
        let to = from + frames as i64;
        out[..frames * ch].fill(0.0);
        while t.items.front().is_some_and(|i| i.end() <= from) {
            t.items.pop_front();
        }
        for item in t.items.iter() {
            if item.start >= to {
                break;
            }
            let s = item.start.max(from);
            let e = item.end().min(to);
            if s < e {
                let src = (s - item.start) as usize * ch;
                let dst = (s - from) as usize * ch;
                let n = (e - s) as usize * ch;
                out[dst..dst + n].copy_from_slice(&item.pcm[src..src + n]);
            }
        }
        while t.items.front().is_some_and(|i| i.end() <= to) {
            t.items.pop_front();
        }
        t.position = to;
        if t.fade_frames > 0 {
            for frame in out[..frames * ch].chunks_exact_mut(ch) {
                let g = if t.fade_done >= t.fade_frames { 0.0 } else { 1.0 - (t.fade_done + 1) as f32 / t.fade_frames as f32 };
                if t.fade_done < t.fade_frames {
                    t.fade_done += 1;
                }
                for x in frame {
                    *x *= g;
                }
            }
        }
        from
    }

    /// Down to silence over `frames` (a linear ramp, as the iOS mixer's volume ramp), and silent after.
    pub fn fade_out(&self, frames: usize) {
        let mut t = lock(&self.inner);
        if t.fade_frames > 0 {
            return;
        }
        t.fade_frames = frames.max(1);
        t.fade_done = 0;
    }

    /// The frames scheduled that are still to be read (for the checks).
    pub fn pending(&self) -> usize {
        let t = lock(&self.inner);
        t.items.iter().map(|i| (i.end() - i.start.max(t.position)).max(0) as usize).sum()
    }
}

/// The output's clock: which device time each sample of the timeline is heard at, from the output's
/// presentation time stamps (a frame and the monotonic time it reaches the output), and how long
/// before that the output takes a sample (the output latency the playout counts: the device's
/// buffers and the device, and the wait to get into them).
///
/// The playout's timeline reading is (sample, its ear time − that latency), so its ear is the
/// stamp's. Thread-safe: the output's callback feeds it, the audio thread reads it.
pub struct OutputClock {
    rate: f64,
    window: f64,
    inner: Mutex<ClockInner>,
}

#[derive(Default)]
struct ClockInner {
    ts_frame: i64,
    ts_time: f64,
    has_stamp: bool,
    stamp_is_guess: bool,
    /// Sliding maximum of (ear − when the output took it) over `window` s: (taken at, value), values falling.
    ahead: VecDeque<(f64, f64)>,
}

impl ClockInner {
    fn ear(&self, sample: i64, rate: f64) -> Option<f64> {
        self.has_stamp.then(|| self.ts_time + (sample - self.ts_frame) as f64 / rate)
    }
}

impl Default for OutputClock {
    fn default() -> Self {
        Self::new(playout::RATE, 2.0)
    }
}

impl OutputClock {
    pub fn new(rate: f64, window: f64) -> Self {
        OutputClock { rate, window, inner: Mutex::new(ClockInner::default()) }
    }

    /// A presentation time stamp: `frame` reached the output at device time `time` (s).
    pub fn stamp(&self, frame: i64, time: f64) {
        if !time.is_finite() {
            return;
        }
        let mut c = lock(&self.inner);
        c.ts_frame = frame;
        c.ts_time = time;
        c.has_stamp = true;
        c.stamp_is_guess = false;
    }

    /// A stand-in for a device that gives no time stamp (yet): only while no real one came.
    pub fn guess(&self, frame: i64, time: f64) {
        let mut c = lock(&self.inner);
        if c.has_stamp && !c.stamp_is_guess || !time.is_finite() {
            return;
        }
        c.ts_frame = frame;
        c.ts_time = time;
        c.has_stamp = true;
        c.stamp_is_guess = true;
    }

    pub fn has_stamp(&self) -> bool {
        lock(&self.inner).has_stamp
    }

    pub fn has_real_stamp(&self) -> bool {
        let c = lock(&self.inner);
        c.has_stamp && !c.stamp_is_guess
    }

    /// When sample `sample` is heard, from the last stamp; None before any.
    pub fn ear(&self, sample: i64) -> Option<f64> {
        lock(&self.inner).ear(sample, self.rate)
    }

    /// The output took the chunk starting at `sample` at device time `at`.
    pub fn wrote(&self, sample: i64, at: f64) {
        let mut c = lock(&self.inner);
        let Some(e) = c.ear(sample, self.rate) else { return };
        let v = e - at;
        if !v.is_finite() {
            return;
        }
        while c.ahead.back().is_some_and(|b| b.1 <= v) {
            c.ahead.pop_back();
        }
        c.ahead.push_back((at, v));
        while c.ahead.front().is_some_and(|f| f.0 < at - self.window) {
            c.ahead.pop_front();
        }
    }

    /// The largest (ear − taken) of the last `window` s: how long before it is heard a sample is taken.
    pub fn latency(&self) -> Option<f64> {
        lock(&self.inner).ahead.front().map(|f| f.1)
    }

    /// A new route: what the window held was the old one's.
    pub fn forget_latency(&self) {
        lock(&self.inner).ahead.clear();
    }

    /// The last stamp: a sample and when it is heard.
    pub fn reading(&self) -> Option<(i64, f64)> {
        let c = lock(&self.inner);
        c.has_stamp.then_some((c.ts_frame, c.ts_time))
    }
}

/// What `MacSound` feeds the playout from the output's clock, at each packet and at the tail's
/// deadline (iOS: the player's lastRenderTime): the reading (the last stamp's sample, and when it
/// goes out = when it is heard − the output latency), and that latency when it moved. The latency
/// goes up to the next half millisecond and only a move of more than 1 ms is passed on, so the need
/// stays put. Pure.
pub struct ClockFeed {
    given: f64,
}

impl Default for ClockFeed {
    fn default() -> Self {
        Self::new()
    }
}

impl ClockFeed {
    pub fn new() -> Self {
        ClockFeed { given: -1.0 }
    }

    /// The output latency the model has; negative: none yet.
    pub fn given(&self) -> f64 {
        self.given
    }

    /// The next reading passes the latency again (a new engine or route, another stream rate).
    pub fn forget(&mut self) {
        self.given = -1.0;
    }

    pub fn feed(&mut self, model: &mut AudioPlayout, clock: &OutputClock, io: f64, refresh: f64, stream_fps: f64, now: f64) -> Option<Output> {
        let (sample, ear) = clock.reading()?;
        let measured = clock.latency()?;
        let latency = (measured * 2000.0).ceil() / 2000.0;
        if self.given < 0.0 || (latency - self.given).abs() > 0.001 {
            self.given = latency;
            model.latency(latency, io, refresh, stream_fps);
        }
        Some(model.played(sample as f64, ear - self.given, now))
    }
}
