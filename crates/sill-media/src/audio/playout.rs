//! The device's playout of the Mac's sound (iOSClient's AudioPlayout.swift, docs/audio-plan.md §7.2):
//! which packets are decoded, which are played, and where each goes on the player's timeline, so that
//! the sound is never ahead of the picture and trails it by as little as its own path allows. Pure:
//! the unit tests run it on a simulated clock, and `MacSound` on its audio thread, doing what it says.
//!
//! Clocks. Device times are seconds of one monotonic clock. Stamps are the Mac's wall clock (seconds
//! since 1970, the header's). The offset between the two is never needed: every comparison goes
//! through the floor. Samples are the player's timeline at 48 kHz; `played` says which device time a
//! sample of it goes out at (when the output takes it), and it is heard the output latency later.
//!
//! The rules, the plan's numbers:
//! 1. The floor is the smallest (arrival − stamp) of the packets of the last 10 s, kept in 1 s
//!    buckets, a packet's arrival being when the playout takes it (`packet`), or when it came for one
//!    that waited behind Sill's own engine starting or stopping (rule 6). With no packet for 10 s it
//!    keeps its last value.
//! 2. The need is what a packet needs between arriving and being heard: the jitter cover (the largest
//!    arrival − stamp − floor of the last 10 s; 40 ms at home and 150 ms away until the session's
//!    first 2 s of packets have come, which the window leaves out; home 20–150 ms, away 60–600 ms),
//!    one packet (the hand's wait, rule 10), the IO buffer, the output latency and 2 ms.
//! 3. The picture's lag is the median (arrival − stamp − floor) of the frames of the last 2 s, plus
//!    1.5 refreshes and 4 ms. No frames: its last value. It can be negative; it is never clamped.
//! 4. The delay is the need, or the picture's lag and one frame of the stream when that is more. A
//!    packet stamped T is due at the ear at T + floor + delay, and goes onto the timeline the output
//!    latency before.
//! 5. The first packet of a segment, and the first after a gap, a late packet, a fade out or a jump,
//!    is placed by time (a segment's first packet drops its P frames of priming, so from
//!    T + P ÷ rate). The rest follow the packet before them.
//! 6. Late: a packet placed by time less than the IO buffer + 2 ms ahead of its render is decoded
//!    (for the decoder's state) and not played.
//! 7. Duplicates: a packet whose (epoch, seq) is among the last 256 is dropped before it counts
//!    anywhere; a format for the epoch playing with the same cookie is ignored.
//! 8. Missing: a seq that skips resets the decoder, and the packet after the gap is a late join.
//! 9. A clean start only where the Mac's encoder started clean (the segment flag): the decoder
//!    resets, the first P decoded frames are dropped and the next 5 ms fade in. Any other first
//!    packet on a reset decoder is decoded and dropped whole, and the next one fades in.
//! 10. The newest packet's last 5 ms (its tail) stays in hand until the next packet arrives or the
//!     tail's deadline passes (its render time − the IO buffer − 2 ms), and goes out then, faded if
//!     nothing follows.
//! 11. Drift: a packet that follows another may gain a frame (ahead) or lose one (behind), one at most.
//! 12. Jumps: an error over 40 ms, or a sound that would lead the picture, is closed at once.
//! 13. A step: a second of packets all more than max(50 ms, 2 × the rtt median) above the floor, or a
//!     packet 20 ms under it, restarts the floor (and the picture's window), closed by a jump.
//!
//! The engine's idle (rule 14) is `MacSound`'s.

use std::collections::HashSet;

/// The player's timeline: 48 kHz.
pub const RATE: f64 = 48_000.0;
pub const FLOOR_SECONDS: i64 = 10;
pub const COVER_START_HOME: f64 = 0.040;
pub const COVER_START_AWAY: f64 = 0.150;
pub const COVER_GRACE: f64 = 2.0;
pub const COVER_HOME: (f64, f64) = (0.020, 0.150);
pub const COVER_AWAY: (f64, f64) = (0.060, 0.600);
/// Rule 2's last term, and rule 6's margin beyond the IO buffer.
pub const MARGIN: f64 = 0.002;
pub const PICTURE_SECONDS: f64 = 2.0;
pub const PICTURE_REFRESHES: f64 = 1.5;
pub const PICTURE_EXTRA: f64 = 0.004;
pub const CORRECT_FROM: f64 = 0.001;
pub const CORRECT_TO: f64 = 0.00025;
pub const JUMP_OVER: f64 = 0.040;
pub const STEP_ABOVE: f64 = 0.050;
pub const STEP_BELOW: f64 = 0.020;
pub const STEP_PACKETS: u32 = 50;
pub const FADE_SECONDS: f64 = 0.005;
pub const DEDUPE_COUNT: usize = 256;
/// Packets kept while the engine starts, before its first reading gives a timeline (a second's).
pub const WAITING_LIMIT: usize = 100;
/// A fade's frames at the timeline's rate: 5 ms.
pub const FADE_FRAMES: usize = 240;

/// Swift's `rounded()`: to nearest, halves away from zero.
pub fn rounded(x: f64) -> f64 {
    x.round()
}

pub fn median(sorted: &[f64]) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let mid = sorted.len() / 2;
    if sorted.len() % 2 == 1 {
        sorted[mid]
    } else {
        (sorted[mid - 1] + sorted[mid]) / 2.0
    }
}

fn insertion_index(sorted: &[f64], value: f64) -> usize {
    sorted.partition_point(|x| *x < value)
}

/// A raised-cosine fade over the first (`fade_in`) or last `frames` frames of `x`, in place.
pub fn fade_frames(x: &mut [f32], fade_in: bool, frames: usize) {
    let n = frames.min(x.len());
    let len = x.len();
    for i in 0..n {
        let g = (0.5 - 0.5 * (std::f64::consts::PI * (i + 1) as f64 / (n + 1) as f64).cos()) as f32;
        if fade_in {
            x[i] *= g
        } else {
            x[len - 1 - i] *= g
        }
    }
}

/// A 5 ms raised-cosine fade over the start (`fade_in`) or the end of `x`, in place.
pub fn fade(x: &mut [f32], fade_in: bool) {
    fade_frames(x, fade_in, FADE_FRAMES)
}

/// One frame added (+1) or dropped (−1), both channels alike, at the packet's quietest point in its
/// middle half, blended into its neighbours. Nothing for 0, or for a packet under 4 frames.
pub fn change(left: &mut Vec<f32>, right: &mut Vec<f32>, by: i32) {
    let n = left.len().min(right.len());
    if by == 0 || n < 4 {
        return;
    }
    let from = n / 4;
    let to = (from + 1).max(3 * n / 4);
    let mut q = from;
    let mut level = f32::INFINITY;
    for i in from..to.min(n - 1) {
        let l = left[i].abs() + right[i].abs() + left[i + 1].abs() + right[i + 1].abs();
        if l < level {
            level = l;
            q = i;
        }
    }
    if by > 0 {
        left.insert(q + 1, (left[q] + left[q + 1]) / 2.0);
        right.insert(q + 1, (right[q] + right[q + 1]) / 2.0);
    } else {
        left[q] = (left[q] + left[q + 1]) / 2.0;
        right[q] = (right[q] + right[q + 1]) / 2.0;
        left.remove(q + 1);
        right.remove(q + 1);
    }
}

/// Where one packet goes, as the model saw it when it placed it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Decision {
    /// When the packet's first frame is due at the ear (stamp + floor + delay).
    pub due: f64,
    /// When it will be heard where it is placed (its render time + the output latency).
    pub ear: f64,
    /// When the picture of the same stamp shows (stamp + floor + the picture's lag); −∞ before any frame.
    pub picture: f64,
}

impl Decision {
    /// due − ear: positive, the sound is ahead of where it is due; negative, behind.
    pub fn error(&self) -> f64 {
        self.due - self.ear
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    Body,
    Tail,
}

/// A part of a packet to hand to the player now: its body at once, its last 5 ms once the next says.
#[derive(Clone, Debug, PartialEq)]
pub struct Schedule {
    pub id: i32,
    pub part: Part,
    /// Which of the packet's kept frames: from `offset`, `count` of them (before `change`).
    pub offset: usize,
    pub count: usize,
    /// Its first frame's place on the player's timeline, or None: right after the buffer before it.
    pub at: Option<f64>,
    /// Where it starts either way (planned).
    pub start: f64,
    /// Its frames once `change` is made.
    pub frames: usize,
    /// The body's: +1, a frame added; −1, one dropped (rule 11); 0.
    pub change: i32,
    pub fade_in: bool,
    pub fade_out: bool,
    /// The stamp of the packet's first kept frame, its seq and epoch.
    pub stamp: f64,
    pub seq: u32,
    pub epoch: i32,
    pub decision: Decision,
    /// When its first frame goes out, as the model reads the timeline now.
    pub goes_out: f64,
}

impl Schedule {
    pub fn end(&self) -> f64 {
        self.start + self.frames as f64
    }
}

/// What to do with the player and the kept packets.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Output {
    pub schedules: Vec<Schedule>,
    /// Kept packets never to be played (late, dropped by a jump, left by a reset).
    pub discards: Vec<i32>,
}

impl Output {
    pub fn add(&mut self, other: Output) {
        self.schedules.extend(other.schedules);
        self.discards.extend(other.discards);
    }
}

/// What to do with one packet.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Plan {
    /// Decode it: every packet of the epoch playing that is not a duplicate and not muted.
    pub decode: bool,
    /// Reset the decoder before it.
    pub reset_first: bool,
    /// Frames dropped from its start once decoded: the codec's priming, or all of them.
    pub drop_front: usize,
    /// The id its kept frames go by until a Schedule or a discard names it; None when none are kept.
    pub id: Option<i32>,
    /// The hand, scheduled or discarded now, and this packet, discarded when late.
    pub output: Output,
}

/// Why a jump was owed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JumpCause {
    Step,
    Error,
    Ahead,
    Route,
}

impl JumpCause {
    /// The console's words.
    pub fn words(&self) -> &'static str {
        match self {
            JumpCause::Step => "clock step",
            JumpCause::Error => "off by over 40 ms",
            JumpCause::Ahead => "ahead of the picture",
            JumpCause::Route => "route",
        }
    }
}

/// What a reset throws away. Session: everything. Playback: the timeline (the engine restarted or
/// stopped, unmuted): the kept packets, the placement and the decoder's continuity, never the floor,
/// the cover or the dedupe. Jump: the next packet is placed by time (a route's new latency).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reset {
    Session,
    Playback,
    Jump,
}

/// One second of what happened, for the console and the stats sent to the Mac.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Second {
    /// How far the sound trailed the picture, ms, over the packets placed; None for a second with none.
    pub behind_ms: Option<i32>,
    pub placed: u32,
    pub late: u32,
    pub duplicates: u32,
    pub frames_added: u32,
    pub frames_dropped: u32,
    pub jumps: u32,
    pub joins: u32,
    pub jump_drops: u32,
    pub need: f64,
    pub cover: f64,
    pub delay: f64,
    pub lag: Option<f64>,
    pub error: Option<f64>,
}

struct Bucket {
    second: i64,
    min: f64,
    max: Option<f64>,
    #[allow(dead_code)]
    count: u32,
}

struct Run {
    since: f64,
    count: u32,
    min: f64,
    max: f64,
}

#[derive(Clone, Copy)]
struct Admitted {
    id: i32,
    epoch: i32,
    seq: u32,
    stamp: f64,
    frames: usize,
    fade_in: bool,
    segment_start: bool,
}

#[derive(Clone, Copy)]
struct Planned {
    packet: Admitted,
    start: f64,
    contiguous: bool,
    change: i32,
    fade_in: bool,
    decision: Decision,
}

impl Planned {
    fn tail_count(&self) -> usize {
        FADE_FRAMES.min(self.packet.frames)
    }
    fn body_count(&self) -> usize {
        self.packet.frames - self.tail_count()
    }
    fn body_frames(&self) -> usize {
        (self.body_count() as i64 + self.change as i64) as usize
    }
    fn tail_start(&self) -> f64 {
        self.start + self.body_frames() as f64
    }
    fn end(&self) -> f64 {
        self.tail_start() + self.tail_count() as f64
    }
    fn takes(&self, next: &Admitted) -> bool {
        next.epoch == self.packet.epoch && next.seq == self.packet.seq.wrapping_add(1) && !next.segment_start && !next.fade_in
    }
    fn body(&self, goes_out: f64) -> Option<Schedule> {
        if self.body_count() == 0 {
            return None;
        }
        Some(Schedule {
            id: self.packet.id,
            part: Part::Body,
            offset: 0,
            count: self.body_count(),
            at: if self.contiguous { None } else { Some(self.start) },
            start: self.start,
            frames: self.body_frames(),
            change: self.change,
            fade_in: self.fade_in,
            fade_out: false,
            stamp: self.packet.stamp,
            seq: self.packet.seq,
            epoch: self.packet.epoch,
            decision: self.decision,
            goes_out,
        })
    }
    fn tail(&self, fade_out: bool, goes_out: f64) -> Schedule {
        let first = self.body_count() == 0;
        Schedule {
            id: self.packet.id,
            part: Part::Tail,
            offset: self.body_count(),
            count: self.tail_count(),
            at: if first && !self.contiguous { Some(self.start) } else { None },
            start: self.tail_start(),
            frames: self.tail_count(),
            change: 0,
            fade_in: first && self.fade_in,
            fade_out,
            stamp: self.packet.stamp,
            seq: self.packet.seq,
            epoch: self.packet.epoch,
            decision: self.decision,
            goes_out,
        }
    }
}

pub struct AudioPlayout {
    epoch: Option<i32>,
    frames_per_packet: usize,
    priming_frames: usize,
    cookie: Vec<u8>,

    output_latency: f64,
    io_buffer: f64,
    refresh: f64,
    stream_fps: f64,
    rtt_median: Option<f64>,
    away: bool,
    muted: bool,

    buckets: Vec<Bucket>,
    floor_kept: Option<f64>,
    session_start: Option<f64>,
    above: Option<Run>,

    frame_arrivals: Vec<f64>,
    frame_raws: Vec<f64>,
    frame_head: usize,
    sorted_raws: Vec<f64>,
    last_picture_raw: Option<f64>,

    recent_keys: Vec<u64>,
    recent_set: HashSet<u64>,
    recent_next: usize,

    expected_seq: Option<u32>,
    priming_left: usize,
    fade_next: bool,

    /// The player's timeline: a sample and the device time it goes out at. None until the first reading.
    mapping: Option<(f64, f64)>,
    hand: Option<Planned>,
    chain_end: Option<f64>,
    waiting: Vec<Admitted>,
    next_id: i32,
    jump_owed: bool,
    owed_cause: Option<JumpCause>,
    made_jump: Option<(f64, JumpCause)>,
    correcting: i32,
    last_error: Option<f64>,

    second: Second,
    behind: Vec<f64>,
    placed_this_session: bool,
}

impl Default for AudioPlayout {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioPlayout {
    pub fn new() -> Self {
        AudioPlayout {
            epoch: None,
            frames_per_packet: 480,
            priming_frames: 240,
            cookie: Vec::new(),
            output_latency: 0.0,
            io_buffer: 0.005,
            refresh: 1.0 / 60.0,
            stream_fps: 60.0,
            rtt_median: None,
            away: false,
            muted: false,
            buckets: Vec::new(),
            floor_kept: None,
            session_start: None,
            above: None,
            frame_arrivals: Vec::new(),
            frame_raws: Vec::new(),
            frame_head: 0,
            sorted_raws: Vec::new(),
            last_picture_raw: None,
            recent_keys: Vec::new(),
            recent_set: HashSet::new(),
            recent_next: 0,
            expected_seq: None,
            priming_left: 0,
            fade_next: true,
            mapping: None,
            hand: None,
            chain_end: None,
            waiting: Vec::new(),
            next_id: 1,
            jump_owed: false,
            owed_cause: None,
            made_jump: None,
            correcting: 0,
            last_error: None,
            second: Second::default(),
            behind: Vec::new(),
            placed_this_session: false,
        }
    }

    pub fn epoch(&self) -> Option<i32> {
        self.epoch
    }
    pub fn frames_per_packet(&self) -> usize {
        self.frames_per_packet
    }
    pub fn priming_frames(&self) -> usize {
        self.priming_frames
    }
    pub fn output_latency(&self) -> f64 {
        self.output_latency
    }
    pub fn io_buffer(&self) -> f64 {
        self.io_buffer
    }
    pub fn refresh(&self) -> f64 {
        self.refresh
    }
    pub fn stream_fps(&self) -> f64 {
        self.stream_fps
    }
    pub fn rtt_median(&self) -> Option<f64> {
        self.rtt_median
    }
    pub fn is_away(&self) -> bool {
        self.away
    }
    pub fn muted(&self) -> bool {
        self.muted
    }
    pub fn last_error(&self) -> Option<f64> {
        self.last_error
    }
    pub fn placed_this_session(&self) -> bool {
        self.placed_this_session
    }

    // MARK: - Inputs

    /// A format message. True for a new epoch (a new decoder); false for the epoch playing with the
    /// same cookie.
    pub fn format(&mut self, epoch: i32, frames_per_packet: i64, priming_frames: i64, cookie: &[u8]) -> bool {
        if self.epoch == Some(epoch)
            && self.cookie == cookie
            && self.frames_per_packet as i64 == frames_per_packet
            && self.priming_frames as i64 == priming_frames
        {
            return false;
        }
        self.epoch = Some(epoch);
        self.frames_per_packet = frames_per_packet.clamp(1, 1 << 20) as usize;
        self.priming_frames = priming_frames.clamp(0, 1 << 20) as usize;
        self.cookie = cookie.to_vec();
        self.expected_seq = None;
        self.priming_left = 0;
        self.fade_next = true;
        true
    }

    /// A video frame, stamped at arrival: for the picture's lag (rule 3).
    pub fn frame(&mut self, stamp: f64, arrival: f64) {
        let raw = arrival - stamp;
        if !raw.is_finite() {
            return;
        }
        self.frame_arrivals.push(arrival);
        self.frame_raws.push(raw);
        let i = insertion_index(&self.sorted_raws, raw);
        self.sorted_raws.insert(i, raw);
        self.expire_frames(arrival);
    }

    /// The output's latencies as read back once it runs, the panel's refresh, and the stream's rate.
    pub fn latency(&mut self, output: f64, io: f64, refresh: f64, stream_fps: f64) {
        self.output_latency = if output.is_finite() { output.max(0.0) } else { 0.0 };
        self.io_buffer = if io.is_finite() { io.max(0.0) } else { 0.0 };
        self.refresh = if refresh.is_finite() { refresh.max(0.0) } else { 0.0 };
        self.stream_fps = if stream_fps > 0.0 && stream_fps.is_finite() { stream_fps } else { 60.0 };
    }

    /// The pings' median round trip of the last second, for the step test (rule 13).
    pub fn rtt(&mut self, median: Option<f64>) {
        self.rtt_median = median.filter(|m| m.is_finite() && *m >= 0.0);
    }

    /// Whether this session is away from home: the cover's start and bounds (rule 2).
    pub fn away(&mut self, away: bool) {
        self.away = away;
    }

    /// Muted (or hidden): each packet still counts for the floor and the cover, none is decoded.
    pub fn mute(&mut self, muted: bool) {
        self.muted = muted;
    }

    /// The player's timeline: `sample` goes out at device time `time`. The first reading places what
    /// waited.
    pub fn played(&mut self, sample: f64, time: f64, now: f64) -> Output {
        if !sample.is_finite() || !time.is_finite() {
            return Output::default();
        }
        let first = self.mapping.is_none();
        self.mapping = Some((sample, time));
        if !first || self.waiting.is_empty() {
            return Output::default();
        }
        let mut out = Output::default();
        let list = std::mem::take(&mut self.waiting);
        for a in list {
            out.add(self.place(a, now));
        }
        out
    }

    /// One packet, with its own stamp (the message's + its place × F ÷ rate), taken `now`. A packet
    /// that waited behind Sill's own engine starting or stopping gives `came`, when it came: the
    /// floor and the cover take that, the late test and the placement `now` (rule 6).
    pub fn packet(&mut self, epoch: i32, seq: u32, stamp: f64, segment_start: bool, came: Option<f64>, now: f64) -> Plan {
        let arrival = came.unwrap_or(now);
        if self.epoch != Some(epoch) || !stamp.is_finite() || !arrival.is_finite() {
            return Plan::default();
        }
        // Rule 7: before anything counts.
        let key = ((epoch as u64 & 0xFFFF) << 32) | seq as u64;
        if self.recent_set.contains(&key) {
            self.second.duplicates += 1;
            return Plan::default();
        }
        self.remember(key);
        self.record(arrival - stamp, arrival);
        if self.muted {
            self.expected_seq = None;
            return Plan::default();
        }
        let mut plan = Plan { decode: true, ..Plan::default() };
        let continuous = !segment_start && self.expected_seq == Some(seq);
        self.expected_seq = Some(seq.wrapping_add(1));
        if segment_start {
            // Rule 9: a clean start.
            plan.reset_first = true;
            self.priming_left = self.priming_frames;
            self.fade_next = true;
        } else if !continuous {
            // Rules 8 and 9: decoded for the decoder's state and dropped whole; the next fades in.
            plan.reset_first = true;
            plan.drop_front = self.frames_per_packet;
            self.priming_left = 0;
            self.fade_next = true;
            self.second.joins += 1;
            plan.output = self.hand_over(true);
            return plan;
        }
        let drop = self.priming_left.min(self.frames_per_packet);
        self.priming_left -= drop;
        plan.drop_front = drop;
        let kept = self.frames_per_packet - drop;
        if kept == 0 {
            return plan;
        }
        let a = Admitted { id: self.next_id, epoch, seq, stamp: stamp + drop as f64 / RATE, frames: kept, fade_in: self.fade_next, segment_start };
        self.next_id = self.next_id.wrapping_add(1);
        self.fade_next = false;
        plan.id = Some(a.id);
        if self.mapping.is_none() {
            self.waiting.push(a);
            if self.waiting.len() > WAITING_LIMIT {
                plan.output.discards.push(self.waiting.remove(0).id);
                // What follows the first kept one no longer follows anything: placed by time.
                if let Some(first) = self.waiting.first_mut() {
                    first.fade_in = true;
                }
            }
            return plan;
        }
        plan.output.add(self.place(a, now));
        plan
    }

    /// The decoder could not decode a packet the plan kept: silence in its place, and afresh at the next.
    pub fn decode_failed(&mut self) {
        self.expected_seq = None;
        self.priming_left = 0;
        self.fade_next = true;
    }

    /// Time passing: the tail in hand goes out, faded, once its deadline has passed (rule 10).
    pub fn tick(&mut self, now: f64) -> Output {
        match self.deadline() {
            Some(d) if now >= d => self.hand_over(true),
            _ => Output::default(),
        }
    }

    /// When `tick` next has something to do, or None.
    pub fn deadline(&self) -> Option<f64> {
        let h = self.hand.as_ref()?;
        Some(self.time(h.tail_start()) - self.io_buffer - MARGIN)
    }

    pub fn reset(&mut self, kind: Reset) -> Output {
        if kind == Reset::Jump {
            self.owe(JumpCause::Route);
            return Output::default();
        }
        let mut out = Output::default();
        if let Some(h) = &self.hand {
            out.discards.push(h.packet.id);
        }
        out.discards.extend(self.waiting.iter().map(|a| a.id));
        self.hand = None;
        self.waiting.clear();
        self.chain_end = None;
        self.mapping = None;
        self.jump_owed = false;
        self.owed_cause = None;
        self.made_jump = None;
        self.correcting = 0;
        self.expected_seq = None;
        self.priming_left = 0;
        self.fade_next = true;
        if kind == Reset::Session {
            self.epoch = None;
            self.cookie.clear();
            self.buckets.clear();
            self.floor_kept = None;
            self.session_start = None;
            self.above = None;
            self.frame_arrivals.clear();
            self.frame_raws.clear();
            self.frame_head = 0;
            self.sorted_raws.clear();
            self.last_picture_raw = None;
            self.recent_keys.clear();
            self.recent_set.clear();
            self.recent_next = 0;
            self.last_error = None;
            self.placed_this_session = false;
            self.second = Second::default();
            self.behind.clear();
        }
        out
    }

    /// The second that ends now, and the next one starts.
    pub fn take_second(&mut self, now: f64) -> Second {
        self.expire_frames(now);
        let mut s = std::mem::take(&mut self.second);
        s.behind_ms = if self.behind.is_empty() {
            None
        } else {
            let mut sorted = std::mem::take(&mut self.behind);
            sorted.sort_by(|a, b| a.total_cmp(b));
            Some(rounded(median(&sorted) * 1000.0) as i32)
        };
        s.need = self.need(now);
        s.cover = self.cover(now);
        let lag = self.picture_lag(now);
        s.lag = lag;
        s.delay = self.delay(now, lag);
        s.error = self.last_error;
        s
    }

    // MARK: - The numbers

    /// Rule 1.
    pub fn floor(&self) -> Option<f64> {
        if self.buckets.is_empty() {
            self.floor_kept
        } else {
            self.buckets.iter().map(|b| b.min).reduce(f64::min)
        }
    }

    /// Rule 2's cover.
    pub fn cover(&self, now: f64) -> f64 {
        let start = if self.away { COVER_START_AWAY } else { COVER_START_HOME };
        let (Some(begun), Some(floor)) = (self.session_start, self.floor()) else { return start };
        if now - begun < COVER_GRACE {
            return start;
        }
        let Some(top) = self.buckets.iter().filter_map(|b| b.max).reduce(f64::max) else { return start };
        let (lo, hi) = if self.away { COVER_AWAY } else { COVER_HOME };
        (top - floor).max(lo).min(hi)
    }

    /// Rule 2.
    pub fn need(&self, now: f64) -> f64 {
        self.cover(now) + self.frames_per_packet as f64 / RATE + self.io_buffer + self.output_latency + MARGIN
    }

    /// Rule 3: None before any frame came (or before the floor).
    pub fn picture_lag(&self, now: f64) -> Option<f64> {
        let floor = self.floor()?;
        let raw = if !self.sorted_raws.is_empty() && self.live_frames_after_expiry(now) > 0 {
            Some(median(&self.sorted_raws))
        } else {
            self.last_picture_raw
        };
        raw.map(|r| r - floor + PICTURE_REFRESHES * self.refresh + PICTURE_EXTRA)
    }

    /// Rule 4.
    pub fn delay(&self, now: f64, lag: Option<f64>) -> f64 {
        let guard = lag.map(|l| l + 1.0 / self.stream_fps).unwrap_or(f64::NEG_INFINITY);
        self.need(now).max(guard)
    }

    pub fn holding(&self) -> bool {
        self.hand.is_some()
    }

    // MARK: - Placement

    fn place(&mut self, a: Admitted, now: f64) -> Output {
        let mut out = Output::default();
        // Rule 10: a tail whose deadline has passed goes out faded before anything follows it.
        if let Some(d) = self.deadline() {
            if now >= d {
                out.add(self.hand_over(true));
            }
        }
        let Some(floor) = self.floor() else { return out };
        self.expire_frames(now);
        let lag = self.picture_lag(now);
        let delay = self.delay(now, lag);
        let due = a.stamp + floor + delay;
        let picture = lag.map(|l| a.stamp + floor + l).unwrap_or(f64::NEG_INFINITY);
        let mut placed: Option<Planned> = None;
        if let Some(h) = self.hand.filter(|h| !self.jump_owed && h.takes(&a)) {
            // Rule 5: it follows the packet in hand.
            let start = h.end();
            let ear = self.time(start) + self.output_latency;
            let decision = Decision { due, ear, picture };
            self.last_error = Some(decision.error());
            if ear >= picture && decision.error().abs() <= JUMP_OVER {
                let change = if a.frames - FADE_FRAMES.min(a.frames) >= 4 { self.correction(decision.error()) } else { 0 };
                placed = Some(Planned { packet: a, start, contiguous: true, change, fade_in: false, decision });
            } else {
                // Rule 12: too far off, or ahead of the picture: closed at once.
                self.owe(if ear < picture { JumpCause::Ahead } else { JumpCause::Error });
            }
        }
        let p = match placed {
            Some(p) => p,
            None => {
                // Rule 5: by time.
                let mut start = self.sample(due - self.output_latency);
                let chain = self.hand.map(|h| h.end()).or(self.chain_end);
                if let (true, Some(cause), Some(chain)) = (self.jump_owed, self.owed_cause, chain) {
                    self.made_jump = Some(((start - chain) / RATE, cause));
                    self.owed_cause = None;
                }
                if let Some(chain) = chain.filter(|c| start < *c) {
                    if self.jump_owed && chain - start > self.frames_per_packet as f64 / 2.0 {
                        // Behind: dropped, until a packet is due within half a packet of what is scheduled.
                        self.second.jump_drops += 1;
                        out.add(self.hand_over(true));
                        out.discards.push(a.id);
                        return out;
                    }
                    // A new segment a hair early, or a jump's last few ms: right after what is scheduled.
                    start = chain;
                }
                if self.time(start) - now < self.io_buffer + MARGIN {
                    // Rule 6.
                    self.second.late += 1;
                    out.add(self.hand_over(true));
                    out.discards.push(a.id);
                    return out;
                }
                self.jump_owed = false;
                self.correcting = 0;
                let ear = self.time(start) + self.output_latency;
                Planned { packet: a, start, contiguous: false, change: 0, fade_in: true, decision: Decision { due, ear, picture } }
            }
        };
        if let Some(h) = &self.hand {
            out.schedules.push(h.tail(!p.contiguous, self.time(h.tail_start())));
        }
        if let Some(body) = p.body(self.time(p.start)) {
            out.schedules.push(body);
        }
        self.hand = Some(p);
        self.chain_end = None;
        self.second.placed += 1;
        self.placed_this_session = true;
        if p.change > 0 {
            self.second.frames_added += 1
        } else if p.change < 0 {
            self.second.frames_dropped += 1
        }
        if p.decision.picture > f64::NEG_INFINITY {
            self.behind.push(p.decision.ear - p.decision.picture);
        }
        out
    }

    /// The tail in hand goes to the player now.
    fn hand_over(&mut self, fade_out: bool) -> Output {
        let Some(h) = self.hand.take() else { return Output::default() };
        self.chain_end = Some(h.end());
        Output { schedules: vec![h.tail(fade_out, self.time(h.tail_start()))], discards: Vec::new() }
    }

    /// A jump is owed (rules 12 and 13, a route), counted once, and only where something plays.
    fn owe(&mut self, cause: JumpCause) {
        if self.jump_owed {
            return;
        }
        self.jump_owed = true;
        if self.hand.is_some() || self.chain_end.is_some() {
            self.second.jumps += 1;
            self.owed_cause = Some(cause);
        }
    }

    /// The jump the last placement by time made, once: how far (s; positive: later) and why.
    pub fn take_jump(&mut self) -> Option<(f64, JumpCause)> {
        self.made_jump.take()
    }

    /// Rule 11: the direction for this packet, with a band so a steady stream does not flutter.
    fn correction(&mut self, error: f64) -> i32 {
        if self.correcting != 0 && error.abs() < CORRECT_TO {
            self.correcting = 0;
        } else if error > CORRECT_FROM {
            self.correcting = 1;
        } else if error < -CORRECT_FROM {
            self.correcting = -1;
        } else if self.correcting != 0 && (error > 0.0) != (self.correcting > 0) {
            self.correcting = 0;
        }
        self.correcting
    }

    fn time(&self, sample: f64) -> f64 {
        match self.mapping {
            None => f64::NEG_INFINITY,
            Some((s, t)) => t + (sample - s) / RATE,
        }
    }

    fn sample(&self, time: f64) -> f64 {
        match self.mapping {
            None => 0.0,
            Some((s, t)) => rounded(s + (time - t) * RATE),
        }
    }

    // MARK: - The windows

    /// Rules 1, 2 and 13: one packet's (arrival − stamp).
    fn record(&mut self, raw: f64, arrival: f64) {
        if self.session_start.is_none() {
            self.session_start = Some(arrival);
        }
        let now = arrival.floor() as i64;
        self.buckets.retain(|b| b.second > now - FLOOR_SECONDS);
        if let Some(floor) = self.floor() {
            // Rule 13, below: a packet 20 ms under the floor.
            if raw < floor - STEP_BELOW {
                let max = self.cover_value(raw, arrival);
                self.restart(Bucket { second: now, min: raw, max, count: 1 });
                return;
            }
            // Rule 13, above: a second of packets, every one of them above the floor by the threshold.
            if raw - floor > STEP_ABOVE.max(2.0 * self.rtt_median.unwrap_or(0.0)) {
                let run = self.above.get_or_insert(Run { since: arrival, count: 0, min: raw, max: raw });
                run.count += 1;
                run.min = run.min.min(raw);
                run.max = run.max.max(raw);
                if arrival - run.since >= 1.0 && run.count >= STEP_PACKETS {
                    let (min, top, count) = (run.min, run.max, run.count);
                    let max = self.cover_value(top, arrival);
                    self.restart(Bucket { second: now, min, max, count });
                    return;
                }
            } else {
                self.above = None;
            }
        }
        if self.buckets.last().map(|b| b.second) != Some(now) {
            self.buckets.push(Bucket { second: now, min: raw, max: None, count: 0 });
        }
        let cover = self.cover_value(raw, arrival);
        if let Some(b) = self.buckets.last_mut() {
            b.min = b.min.min(raw);
            b.count += 1;
            if let Some(m) = cover {
                b.max = Some(b.max.unwrap_or(m).max(m));
            }
        }
        self.floor_kept = self.floor();
    }

    /// A packet's value for the cover's window: none in the session's first 2 s.
    fn cover_value(&self, raw: f64, arrival: f64) -> Option<f64> {
        let begun = self.session_start?;
        (arrival - begun >= COVER_GRACE).then_some(raw)
    }

    /// Rule 13: the windows start again from `bucket` (the picture's too), and a jump closes the
    /// difference.
    fn restart(&mut self, bucket: Bucket) {
        self.floor_kept = Some(bucket.min);
        self.buckets.clear();
        self.buckets.push(bucket);
        self.above = None;
        self.frame_arrivals.clear();
        self.frame_raws.clear();
        self.frame_head = 0;
        self.sorted_raws.clear();
        self.last_picture_raw = None;
        self.owe(JumpCause::Step);
    }

    fn remember(&mut self, key: u64) {
        if self.recent_keys.len() < DEDUPE_COUNT {
            self.recent_keys.push(key);
        } else {
            self.recent_set.remove(&self.recent_keys[self.recent_next]);
            self.recent_keys[self.recent_next] = key;
            self.recent_next = (self.recent_next + 1) % DEDUPE_COUNT;
        }
        self.recent_set.insert(key);
    }

    fn expire_frames(&mut self, now: f64) {
        let from = now - PICTURE_SECONDS;
        while self.frame_head < self.frame_arrivals.len() && self.frame_arrivals[self.frame_head] < from {
            let raw = self.frame_raws[self.frame_head];
            if self.sorted_raws.len() == 1 {
                self.last_picture_raw = Some(self.sorted_raws[0]);
            }
            let i = insertion_index(&self.sorted_raws, raw);
            if i < self.sorted_raws.len() && self.sorted_raws[i] == raw {
                self.sorted_raws.remove(i);
            } else if i > 0 && self.sorted_raws[i - 1] == raw {
                self.sorted_raws.remove(i - 1);
            }
            self.frame_head += 1;
        }
        if self.frame_head > 512 {
            self.frame_arrivals.drain(..self.frame_head);
            self.frame_raws.drain(..self.frame_head);
            self.frame_head = 0;
        }
        if !self.sorted_raws.is_empty() {
            self.last_picture_raw = Some(median(&self.sorted_raws));
        }
    }

    /// The frames of the window that are still in it at `now` (without changing it).
    fn live_frames_after_expiry(&self, now: f64) -> usize {
        let from = now - PICTURE_SECONDS;
        self.frame_arrivals[self.frame_head..].iter().rev().take_while(|a| **a >= from).count()
    }
}
