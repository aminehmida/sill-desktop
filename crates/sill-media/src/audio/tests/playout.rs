//! The playout policy (playout.rs) against iOSClient's own check, Tests/checks/audio-playout: the same
//! hand-worked numbers, and its simulation ported line for line (a Mac making packets and frames, a
//! link, the decoder's state, the player's timeline on an output clock read in IO-sized renders) with
//! the same invariants and scenarios. AudioPlayoutTest.kt, ported.

use super::Rng;
use crate::audio::playout::{self, AudioPlayout, Decision, JumpCause, Output, Part, Plan, Reset, Schedule, Second, FADE_FRAMES, MARGIN, RATE, WAITING_LIMIT};
use std::collections::{HashMap, HashSet};

fn near(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}
fn near_eps(a: f64, b: f64, eps: f64) -> bool {
    (a - b).abs() < eps
}
fn ms(s: f64) -> String {
    format!("{:.1} ms", s * 1000.0)
}
fn packet(m: &mut AudioPlayout, epoch: i32, seq: u32, stamp: f64, start: bool, now: f64) -> Plan {
    m.packet(epoch, seq, stamp, start, None, now)
}

// MARK: - The need, the floor, the cover, the picture's lag: by hand

#[test]
fn formats_and_numbers() {
    let mut m = AudioPlayout::new();
    assert!(m.format(3, 480, 240, &[9]), "a format: a new epoch");
    assert!(!m.format(3, 480, 240, &[9]), "the same epoch and cookie again (a move's connection): ignored");
    assert!(m.format(3, 480, 240, &[8]), "the same epoch, another cookie: a new decoder");
    assert!(m.format(4, 512, 256, &[8]), "another epoch: a new decoder");
    m.latency(0.011, 0.005, 1.0 / 60.0, 60.0);
    assert!(m.floor().is_none() && near_eps(m.cover(0.0), 0.040, 1e-12), "no packet yet: no floor, the cover's start 40 ms");
    m.away(true);
    assert!(near_eps(m.cover(0.0), 0.150, 1e-12), "away: the cover's start 150 ms");
    m.away(false);
    packet(&mut m, 4, 0, 99.995, true, 100.0);
    assert!(near(m.floor().unwrap(), 0.005), "the first packet sets the floor");
    assert!(near_eps(m.need(100.0), 0.040 + 512.0 / 48_000.0 + 0.005 + 0.011 + 0.002, 1e-12), "the need: 40 + 10.7 + 5 + 11 + 2 ms");
    packet(&mut m, 4, 1, 100.5 - 0.0053, false, 100.6);
    assert!(near_eps(m.cover(100.6), 0.040, 1e-12), "100 ms over the floor in the first 2 s: the cover stays 40 ms");
    packet(&mut m, 4, 2, 102.3 - 0.035, false, 102.3);
    assert!(near(m.cover(102.3), 0.030), "past 2 s, 30 ms over the floor: the cover 30 ms at once");
    packet(&mut m, 4, 3, 102.4 - 0.0051, false, 102.4);
    assert!(near(m.cover(102.4), 0.030), "the largest of the window stays");

    let mut lone = AudioPlayout::new();
    lone.format(1, 480, 240, &[]);
    packet(&mut lone, 1, 0, 9.990, true, 10.0);
    packet(&mut lone, 1, 1, 12.5 - 0.011, false, 12.5);
    assert!(near(lone.cover(12.5), 0.020), "1 ms over: home's lower bound, 20 ms");
    packet(&mut lone, 1, 2, 12.6 - 0.310, false, 12.6);
    assert!(near(lone.cover(12.6), 0.150), "300 ms over at home: held at 150 ms");
    lone.away(true);
    assert!(near(lone.cover(12.6), 0.300), "the same away: 300 ms");
    packet(&mut lone, 1, 3, 12.7 - 0.910, false, 12.7);
    assert!(near(lone.cover(12.7), 0.600), "900 ms over away: held at 600 ms");

    // Rule 6: a packet that waited behind the engine starting gives when it came.
    let mut waited = AudioPlayout::new();
    waited.format(1, 480, 240, &[]);
    packet(&mut waited, 1, 0, 9.990, true, 10.0);
    packet(&mut waited, 1, 1, 12.5 - 0.011, false, 12.5);
    waited.packet(1, 2, 12.6 - 0.011, false, Some(12.6), 12.68);
    assert!(near(waited.cover(12.68), 0.020) && near(waited.floor().unwrap(), 0.010), "the cover and the floor take when it came");

    let mut fl = AudioPlayout::new();
    fl.format(1, 480, 240, &[]);
    packet(&mut fl, 1, 0, 0.990, true, 1.0);
    for k in 1..=9 {
        packet(&mut fl, 1, k, k as f64 + 1.5 - 0.015, false, k as f64 + 1.5);
    }
    assert!(near(fl.floor().unwrap(), 0.010), "the floor: the smallest of the last 10 s");
    packet(&mut fl, 1, 10, 11.5 - 0.015, false, 11.5);
    assert!(near(fl.floor().unwrap(), 0.015), "10 s on, the smallest has left the window");

    let mut pic = AudioPlayout::new();
    pic.format(1, 480, 240, &[]);
    pic.latency(0.010, 0.005, 1.0 / 60.0, 60.0);
    packet(&mut pic, 1, 0, 9.980, true, 10.0);
    for (k, raw) in [0.030, 0.010, 0.050, 0.012, 0.040].into_iter().enumerate() {
        let a = 10.0 + k as f64 * 0.1;
        pic.frame(a - raw, a);
    }
    let lag = pic.picture_lag(10.5).unwrap();
    assert!(near(lag, 0.030 - 0.020 + 1.5 / 60.0 + 0.004), "the picture's lag: median 30 − floor 20 + 25 + 4");
    assert!(near_eps(pic.delay(10.5, Some(lag)), pic.need(10.5), 1e-12), "the need decides while it is the larger");
    for k in 0..5 {
        let a = 12.1 + k as f64 * 0.1;
        pic.frame(a - 0.300, a);
    }
    let late = pic.picture_lag(12.5).unwrap();
    assert!(near(late, 0.300 - 0.020 + 1.5 / 60.0 + 0.004), "frames 300 ms late: the median follows them");
    assert!(near_eps(pic.delay(12.5, Some(late)), late + 1.0 / 60.0, 1e-12), "the guard decides: the lag + one frame");
    assert!(near(pic.picture_lag(20.0).unwrap(), late), "a still window: the lag keeps its last value");
    assert!(AudioPlayout::new().picture_lag(0.0).is_none(), "no frame and no floor: no lag");
}

// MARK: - Rule 10: a packet past the hand's deadline, before its timer ran

#[test]
fn packet_past_the_hands_deadline() {
    let mut m = AudioPlayout::new();
    m.format(1, 480, 0, &[]);
    m.latency(0.010, 0.005, 1.0 / 60.0, 60.0);
    m.played(0.0, 100.0, 100.0);
    let mut t = 100.0;
    for k in 0..50 {
        packet(&mut m, 1, k, t - 0.023, k == 0, t);
        t += 0.010;
    }
    let now = m.deadline().unwrap() + 0.004;
    let out = packet(&mut m, 1, 50, t - 0.023, false, now).output;
    assert_eq!(out.schedules.iter().find(|s| s.part == Part::Tail && s.seq == 49).map(|s| s.fade_out), Some(true), "the tail in hand goes out faded");
    let next: Vec<&Schedule> = out.schedules.iter().filter(|s| s.seq == 50).collect();
    assert!(!out.discards.is_empty() || (!next.is_empty() && next.iter().all(|s| s.at.is_some() && s.fade_in)), "…the packet placed by time, faded in (or late)");
    assert!(next.iter().all(|s| s.goes_out - now >= 0.005 + MARGIN - 1e-9), "…never less than the IO buffer + 2 ms before it renders");
}

// MARK: - Fades and a frame gained or lost

#[test]
fn fades_and_a_frame_changed() {
    let mut x = vec![1f32; 480];
    playout::fade(&mut x, true);
    assert!(x[0] < 0.001 && x[239] > 0.99 && x[240] == 1.0 && (0..240).all(|i| x[i] <= x[i + 1]), "a fade in: 240 frames rising");
    let mut y = vec![1f32; 480];
    playout::fade(&mut y, false);
    assert!(y[479] < 0.001 && y[240] > 0.99 && y[239] == 1.0, "a fade out: the last 5 ms falling");
    let mut short = vec![1f32; 100];
    playout::fade(&mut short, true);
    assert!(short[0] < 0.01 && short[99] > 0.99, "shorter than a fade: over all of it");
    assert_eq!(FADE_FRAMES, playout::rounded(playout::FADE_SECONDS * RATE) as usize);
    let sine: Vec<f32> = (0..480).map(|i| (2.0 * std::f64::consts::PI * 440.0 * i as f64 / 48_000.0).sin() as f32).collect();
    let (mut l, mut r) = (sine.clone(), sine.clone());
    playout::change(&mut l, &mut r, 1);
    assert!(l.len() == 481 && r.len() == 481 && l == r, "a frame gained: 481, both alike");
    let step = |v: &[f32]| v.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0f32, f32::max);
    let natural = step(&sine);
    assert!(step(&l) <= natural * 1.01, "…no step larger than the sine's own");
    let (mut l2, mut r2) = (sine.clone(), sine.clone());
    playout::change(&mut l2, &mut r2, -1);
    assert!(l2.len() == 479 && r2.len() == 479, "a frame lost: 479");
    assert!(step(&l2) <= natural * 1.6, "…no step much larger");
    let first_diff = (0..480).find(|i| sine[*i] != l[*i]).unwrap();
    let q = (120..360).min_by(|a, b| (sine[*a].abs() + sine[a + 1].abs()).total_cmp(&(sine[*b].abs() + sine[b + 1].abs()))).unwrap();
    assert!((120..=360).contains(&first_diff) && first_diff == q + 1, "the change in the middle half, at its quietest point");
    let (mut n0, mut n1) = (sine.clone(), sine.clone());
    playout::change(&mut n0, &mut n1, 0);
    assert!(n0 == sine, "no change for 0");
    let (mut tiny, mut tiny2) = (vec![1f32; 3], vec![1f32; 3]);
    playout::change(&mut tiny, &mut tiny2, -1);
    assert_eq!(tiny.len(), 3, "nor for a packet under 4 frames");
}

// MARK: - Resets, duplicates, mute

#[test]
fn resets_duplicates_and_mute() {
    let mut m = AudioPlayout::new();
    m.format(5, 480, 240, &[1]);
    packet(&mut m, 5, 0, 0.99, true, 1.0);
    m.reset(Reset::Session);
    assert!(m.floor().is_none() && m.epoch().is_none(), "a session reset: no floor, no epoch");
    assert!(m.format(5, 480, 240, &[1]), "…so the same epoch's format is a new decoder");
    let p = packet(&mut m, 5, 0, 0.99, true, 1.5);
    assert!(p.decode && p.id.is_some(), "…and a packet it saw before is no duplicate now");

    let mut d = AudioPlayout::new();
    d.format(1, 480, 240, &[]);
    packet(&mut d, 1, 0, 0.99, true, 1.0);
    for k in 1..=300u32 {
        packet(&mut d, 1, k, k as f64 * 0.01 + 0.99, false, 1.0 + k as f64 * 0.01);
    }
    assert!(packet(&mut d, 1, 1, 1.0, false, 5.0).decode, "older than the last 256: no longer a duplicate");
    assert!(!packet(&mut d, 1, 299, 3.98, false, 5.0).decode, "one among them is");

    let mut wrap = AudioPlayout::new();
    wrap.format(65535, 480, 240, &[]);
    let a = packet(&mut wrap, 65535, 0xFFFF_FFFF, 0.99, true, 1.0);
    let b = packet(&mut wrap, 65535, 0, 1.0, false, 1.01);
    assert!(a.id.is_some() && b.id.is_some() && !b.reset_first, "the seq wraps without a reset");
    assert!(!packet(&mut wrap, 3, 5, 1.0, false, 1.0).decode, "another epoch than the format's is skipped");

    let mut mu = AudioPlayout::new();
    mu.format(1, 480, 240, &[]);
    mu.mute(true);
    let q = packet(&mut mu, 1, 0, 0.99, true, 1.0);
    assert!(!q.decode && q.id.is_none() && mu.floor().is_some(), "muted: the floor counts it, nothing decoded");

    // Nothing the network can say breaks it.
    let mut odd = AudioPlayout::new();
    odd.format(1, 0, -5, &[]);
    assert_eq!((odd.frames_per_packet(), odd.priming_frames()), (1, 0));
    odd.format(2, i64::MAX, i64::MAX, &[]);
    for (stamp, now) in [(f64::NAN, 1.0), (f64::INFINITY, 1.0), (1.0, f64::NAN), (-1e300, 1e300), (1e300, -1e300)] {
        let _ = packet(&mut odd, 2, 0, stamp, true, now);
        odd.frame(stamp, now);
        let _ = odd.played(stamp, now, now);
        let _ = odd.tick(now);
        let _ = odd.take_second(now);
    }
}

#[test]
fn segment_start_drops_the_priming_and_a_join_drops_all() {
    let mut m = AudioPlayout::new();
    m.format(1, 480, 240, &[]);
    let first = packet(&mut m, 1, 0, 10.0, true, 10.01);
    assert!(first.decode && first.reset_first && first.drop_front == 240 && first.id.is_some(), "a segment's first packet: reset, its 240 frames of priming dropped");
    let second = packet(&mut m, 1, 1, 10.01, false, 10.02);
    assert!(second.decode && !second.reset_first && second.drop_front == 0, "the next: no reset, nothing dropped");
    let gap = packet(&mut m, 1, 5, 10.05, false, 10.06);
    assert!(gap.decode && gap.reset_first && gap.drop_front == 480 && gap.id.is_none(), "after a gap: reset, decoded and dropped whole");
    let mut joined = AudioPlayout::new();
    joined.format(1, 480, 240, &[]);
    let j = packet(&mut joined, 1, 777, 10.0, false, 10.01);
    assert!(j.reset_first && j.drop_front == 480 && j.id.is_none(), "joining mid-segment: the same");
}

// MARK: - The simulation (Tests/checks/audio-playout's World and Run)

const WALL_BASE: f64 = 1_790_000_000.0;

struct Move {
    register: f64,
    hand_over: f64,
    old_transit: f64,
    new_transit: f64,
}

struct World {
    seconds: f64,
    f: usize,
    p: usize,
    output_latency: f64,
    io: f64,
    refresh: f64,
    stream_fps: f64,
    video_fps: f64,
    away: bool,
    host_drift: f64,
    host_steps: Vec<(f64, f64)>,
    out_drift: f64,
    transit: Box<dyn Fn(f64) -> f64>,
    video_transit: Option<Box<dyn Fn(f64) -> f64>>,
    video_encode: f64,
    pauses: Vec<(f64, f64)>,
    epochs: Vec<f64>,
    skipped: HashSet<i64>,
    /// from, to, extra.
    duplicate: Option<(i64, i64, f64)>,
    mv: Option<Move>,
    join_at: Option<i64>,
    engine_start: f64,
    reading_jitter: f64,
    mutes: Vec<(f64, bool)>,
    routes: Vec<(f64, f64)>,
    drifts: Vec<(f64, f64)>,
    io_at: Vec<(f64, f64)>,
    home_at: Option<f64>,
    rtt: Option<f64>,
    processing: f64,
    decode_fails: HashSet<i64>,
}

impl Default for World {
    fn default() -> Self {
        World {
            seconds: 20.0,
            f: 480,
            p: 240,
            output_latency: 0.010,
            io: 0.005,
            refresh: 1.0 / 60.0,
            stream_fps: 60.0,
            video_fps: 60.0,
            away: false,
            host_drift: 0.0,
            host_steps: Vec::new(),
            out_drift: 0.0,
            transit: Box::new(|_| 0.003),
            video_transit: None,
            video_encode: 0.008,
            pauses: Vec::new(),
            epochs: Vec::new(),
            skipped: HashSet::new(),
            duplicate: None,
            mv: None,
            join_at: None,
            engine_start: 0.0,
            reading_jitter: 0.0,
            mutes: Vec::new(),
            routes: Vec::new(),
            drifts: Vec::new(),
            io_at: Vec::new(),
            home_at: None,
            rtt: Some(0.005),
            processing: 0.00002,
            decode_fails: HashSet::new(),
        }
    }
}

impl World {
    fn wall(&self, t: f64) -> f64 {
        WALL_BASE + t * (1.0 + self.host_drift) + self.host_steps.iter().filter(|s| s.0 <= t).map(|s| s.1).sum::<f64>()
    }
}

#[allow(dead_code)]
struct Played {
    id: i32,
    part: Part,
    first: bool,
    seq: u32,
    epoch: i32,
    start: f64,
    frames: usize,
    stamp: f64,
    scheduled_at: f64,
    arrival: f64,
    fade_in: bool,
    fade_out: bool,
    change: i32,
    decision: Decision,
    ear: f64,
    contiguous: bool,
}

struct Kept {
    arrival: f64,
    frames: usize,
    good: bool,
    fade_in: bool,
    first_stamp: f64,
}

enum Event {
    Packet { epoch: i32, seq: u32, stamp: f64, segment_start: bool, arrival: f64 },
    Format(i32),
    Frame(f64),
    Mute(bool),
    Route(f64),
    Drift(f64),
    Io(f64),
    Home,
    RttTick,
    Second,
}

struct Run {
    w: World,
    model: AudioPlayout,
    played: Vec<Played>,
    played_ids: HashSet<i32>,
    parts_done: HashSet<i64>,
    played_keys: HashSet<u64>,
    delivered: HashSet<u64>,
    discarded: HashSet<i32>,
    kept: HashMap<i32, Kept>,
    seconds: Vec<(f64, Second)>,
    errors: Vec<(f64, f64)>,
    violations: Vec<String>,
    duplicates_sent: u32,
    dup_changed_windows: u32,
    formats_ignored: u32,
    jumps_made: Vec<(f64, f64, JumpCause)>,
    t0: Option<f64>,
    player_end: Option<f64>,
    output_latency: f64,
    io: f64,
    dec_epoch: i32,
    dec_next: u32,
    dec_broken: bool,
    dec_noise: u32,
    rng: Rng,
    last_now: f64,
    staleness: f64,
    last_reading_at: f64,
}

impl Run {
    fn new(w: World, seed: u64) -> Run {
        Run {
            model: AudioPlayout::new(),
            played: Vec::new(),
            played_ids: HashSet::new(),
            parts_done: HashSet::new(),
            played_keys: HashSet::new(),
            delivered: HashSet::new(),
            discarded: HashSet::new(),
            kept: HashMap::new(),
            seconds: Vec::new(),
            errors: Vec::new(),
            violations: Vec::new(),
            duplicates_sent: 0,
            dup_changed_windows: 0,
            formats_ignored: 0,
            jumps_made: Vec::new(),
            t0: None,
            player_end: None,
            output_latency: w.output_latency,
            io: w.io,
            dec_epoch: -1,
            dec_next: 0,
            dec_broken: true,
            dec_noise: 0,
            rng: Rng::new(seed),
            last_now: 0.0,
            staleness: 0.0,
            last_reading_at: 0.0,
            w,
        }
    }

    fn violation(&mut self, s: String) {
        if self.violations.len() < 20 {
            self.violations.push(s);
        }
    }

    /// When sample `s` of the timeline goes out.
    fn t(&self, s: f64) -> f64 {
        self.t0.unwrap() + s / (RATE * (1.0 + self.w.out_drift))
    }

    fn reading(&mut self, now: f64) -> Option<(f64, f64)> {
        let t0 = self.t0?;
        if now < t0 - self.io {
            return None;
        }
        let block = playout::rounded(self.io * RATE).max(1.0);
        let k = ((now + self.io - t0) * RATE * (1.0 + self.w.out_drift) / block).floor();
        if k < 0.0 {
            return None;
        }
        let s = k * block;
        let jitter = if self.w.reading_jitter > 0.0 { self.rng.range(-self.w.reading_jitter, self.w.reading_jitter) } else { 0.0 };
        Some((s, self.t(s) + jitter))
    }

    fn feed_reading(&mut self, now: f64) {
        let Some(r) = self.reading(now) else { return };
        self.last_reading_at = now;
        let out = self.model.played(r.0, r.1, now);
        self.apply(out, now);
    }

    fn apply(&mut self, out: Output, now: f64) {
        if let Some((by, cause)) = self.model.take_jump() {
            self.jumps_made.push((now, by, cause));
        }
        for d in out.discards {
            if !self.discarded.insert(d) {
                self.violation(format!("discarded twice: {d}"));
            }
        }
        for s in &out.schedules {
            self.schedule(s, now);
        }
    }

    fn schedule(&mut self, s: &Schedule, now: f64) {
        let Some(k) = self.kept.get(&s.id) else {
            self.violation(format!("scheduled an id never kept: {}", s.id));
            return;
        };
        let (k_frames, k_arrival, k_good, k_fade_in, k_first_stamp) = (k.frames, k.arrival, k.good, k.fade_in, k.first_stamp);
        if self.t0.is_none() {
            self.violation(format!("scheduled with no engine running: seq {}", s.seq));
            return;
        }
        if self.discarded.contains(&s.id) {
            self.violation(format!("scheduled a discarded packet: seq {}", s.seq));
        }
        let tail = s.part == Part::Tail;
        let part_key = ((s.id as i64) << 1) | tail as i64;
        if !self.parts_done.insert(part_key) {
            self.violation(format!("id {}'s {:?} scheduled twice", s.id, s.part));
        }
        let first = self.played_ids.insert(s.id);
        if !tail && !first {
            self.violation(format!("seq {}'s body after its tail", s.seq));
        }
        if tail && first && s.offset != 0 {
            self.violation(format!("seq {}'s tail before its body", s.seq));
        }
        let tail_count = FADE_FRAMES.min(k_frames);
        let wrong = if !tail { s.offset != 0 || s.count != k_frames - tail_count } else { s.offset != k_frames - tail_count || s.count != tail_count };
        if wrong {
            self.violation(format!("seq {}'s {:?}: frames {}+{} of {}", s.seq, s.part, s.offset, s.count, k_frames));
        }
        if first && !self.played_keys.insert(((s.epoch as u64) << 32) | s.seq as u64) {
            self.violation(format!("seq {} of epoch {} played twice", s.seq, s.epoch));
        }
        let start;
        if let Some(at) = s.at {
            start = at;
            if let Some(end) = self.player_end {
                if at < end - 0.5 {
                    self.violation(format!("seq {} overlaps: at {at} before the end {end}", s.seq));
                }
            }
            if !first {
                self.violation(format!("seq {}'s tail placed apart from its body", s.seq));
            }
        } else {
            let Some(end) = self.player_end else {
                self.violation(format!("seq {} follows nothing", s.seq));
                return;
            };
            start = end;
            if (end - s.start).abs() > 0.5 {
                self.violation(format!("seq {} planned at {}, follows at {end}", s.seq, s.start));
            }
        }
        let (io, w) = (self.io, &self.w);
        let seen = s.goes_out - now;
        let moved = w.out_drift.abs() * (self.staleness + io + MARGIN) + 2.0 * w.reading_jitter + 1e-6;
        let ahead = self.t(start) - now;
        let slack = 2e-5 + w.out_drift.abs() * (ahead.max(0.0) + self.staleness + io) + 2.0 * w.reading_jitter;
        let renders = self.t(start) - io;
        let ear = self.t(start) + self.output_latency;
        if seen < io + MARGIN - moved {
            self.violation(format!("seq {}'s {:?} scheduled {:.4} ms before it goes out as the model sees it", s.seq, s.part, seen * 1000.0));
        }
        if ahead < io + MARGIN - slack {
            self.violation(format!("seq {}'s {:?} scheduled {:.4} ms before it goes out", s.seq, s.part, ahead * 1000.0));
        }
        if renders < k_arrival {
            self.violation(format!("seq {} renders before it arrives", s.seq));
        }
        if s.change.abs() > 1 || (tail && s.change != 0) {
            self.violation(format!("seq {}: {} frames changed in its {:?}", s.seq, s.change, s.part));
        }
        if s.frames as i64 != s.count as i64 + s.change as i64 {
            self.violation(format!("seq {}: {} frames, {} and changed {}", s.seq, s.frames, s.count, s.change));
        }
        if !k_good {
            self.violation(format!("seq {} played though decoded on a reset decoder mid-segment", s.seq));
        }
        if (s.stamp - k_first_stamp).abs() > 1e-7 {
            self.violation(format!("seq {} placed by {}, its first kept frame's stamp is {k_first_stamp}", s.seq, s.stamp));
        }
        if first && k_fade_in && !s.fade_in {
            self.violation(format!("seq {} after a join played without a fade in", s.seq));
        }
        if s.fade_in && !first {
            self.violation(format!("seq {}: a fade in on its tail after its body", s.seq));
        }
        if s.fade_out && !tail {
            self.violation(format!("seq {}: a fade out on its body", s.seq));
        }
        if first && s.decision.ear < s.decision.picture - 1e-9 {
            self.violation(format!("seq {} ahead of the picture by {}", s.seq, s.decision.picture - s.decision.ear));
        }
        self.player_end = Some(start + s.frames as f64);
        self.played.push(Played {
            id: s.id,
            part: s.part,
            first,
            seq: s.seq,
            epoch: s.epoch,
            start,
            frames: s.frames,
            stamp: s.stamp,
            scheduled_at: now,
            arrival: k_arrival,
            fade_in: s.fade_in,
            fade_out: s.fade_out,
            change: s.change,
            decision: s.decision,
            ear,
            contiguous: s.at.is_none(),
        });
    }

    fn run(mut self) -> Run {
        let mut events: Vec<(f64, usize, Event)> = Vec::new();
        fn add(events: &mut Vec<(f64, usize, Event)>, t: f64, e: Event) {
            let order = events.len();
            events.push((t, order, e));
        }
        let (f, p) = (self.w.f as f64, self.w.p as f64);
        let seg_f = f / RATE;
        let mut breaks: Vec<(f64, f64, bool)> = self.w.pauses.iter().map(|x| (x.0, x.1, false)).chain(self.w.epochs.iter().map(|x| (*x, 0.0, true))).collect();
        breaks.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut epoch = 1;
        let mut seq: i64 = 0;
        let mut seg_start = 0.0;
        let mut last_old = f64::NEG_INFINITY;
        let mut last_new = f64::NEG_INFINITY;
        let mut last_dup = f64::NEG_INFINITY;
        let mut n: i64 = 0;
        let mut bi = 0;
        let first_seq = self.w.join_at.unwrap_or(0);
        add(&mut events, 0.0, Event::Format(epoch));
        loop {
            let produce = seg_start + (n + 1) as f64 * seg_f - p / RATE + 0.010;
            if produce > self.w.seconds {
                break;
            }
            if bi < breaks.len() && produce >= breaks[bi].0 {
                let (at, pause, new_epoch) = breaks[bi];
                bi += 1;
                seg_start = at + pause;
                n = 0;
                if new_epoch {
                    epoch += 1;
                    seq = 0;
                    add(&mut events, seg_start + 0.001, Event::Format(epoch));
                }
                continue;
            }
            let content = seg_start + (n as f64 * f - p) / RATE;
            let stamp = self.w.wall(content);
            if !(epoch == 1 && (self.w.skipped.contains(&seq) || seq < first_seq)) {
                let processing = self.w.processing;
                let (e, sq, start) = (epoch, seq as u32, n == 0);
                let deliver = |events: &mut Vec<(f64, usize, Event)>, arrival: f64| {
                    add(events, arrival + processing, Event::Packet { epoch: e, seq: sq, stamp, segment_start: start, arrival })
                };
                if let Some(m) = &self.w.mv {
                    last_old = last_old.max(produce + m.old_transit);
                    if last_old < m.hand_over {
                        deliver(&mut events, last_old);
                    }
                    if produce >= m.register {
                        last_new = last_new.max(produce + m.new_transit).max(m.hand_over);
                        deliver(&mut events, last_new);
                    }
                } else {
                    last_old = last_old.max(produce + (self.w.transit)(produce));
                    deliver(&mut events, last_old);
                }
                if let Some(d) = self.w.duplicate {
                    if epoch == 1 && seq >= d.0 && seq <= d.1 {
                        last_dup = last_dup.max(produce + (self.w.transit)(produce) + d.2);
                        deliver(&mut events, last_dup);
                    }
                }
            }
            seq += 1;
            n += 1;
        }
        if let Some(m) = &self.w.mv {
            add(&mut events, m.hand_over + self.w.processing - 0.0001, Event::Format(1));
        }
        if self.w.video_fps > 0.0 {
            let mut t = 0.0;
            let mut last = f64::NEG_INFINITY;
            let vt = self.w.video_transit.as_ref().unwrap_or(&self.w.transit);
            while t < self.w.seconds {
                let out = t + self.w.video_encode;
                last = last.max(out + vt(out));
                add(&mut events, last, Event::Frame(self.w.wall(out)));
                t += 1.0 / self.w.video_fps;
            }
        }
        for m in &self.w.mutes {
            add(&mut events, m.0, Event::Mute(m.1));
        }
        for r in &self.w.routes {
            add(&mut events, r.0, Event::Route(r.1));
        }
        for d in &self.w.drifts {
            add(&mut events, d.0, Event::Drift(d.1));
        }
        for x in &self.w.io_at {
            add(&mut events, x.0, Event::Io(x.1));
        }
        if let Some(h) = self.w.home_at {
            add(&mut events, h, Event::Home);
        }
        let mut s = 1.0;
        while s < self.w.seconds + 1.0 {
            add(&mut events, s, Event::RttTick);
            add(&mut events, s + 0.0005, Event::Second);
            s += 1.0;
        }
        events.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));

        self.model.latency(self.output_latency, self.io, self.w.refresh, self.w.stream_fps);
        self.model.away(self.w.away);
        self.model.rtt(self.w.rtt);
        let mut engine_at: Option<f64> = None;
        let mut muted = false;
        let end = self.w.seconds + 1.0;
        events.retain(|e| e.0 <= end);
        let order = events.len();
        events.push((end, order, Event::Second));
        let mut i = 0;
        while i < events.len() {
            let t = events[i].0;
            if let Some(at) = engine_at {
                if self.t0.is_none() && at <= t {
                    self.t0 = Some(at);
                    self.player_end = None;
                    self.feed_reading(at);
                    continue;
                }
            }
            if let Some(d) = self.model.deadline().filter(|d| *d < t) {
                let mut now = d.max(self.last_now);
                self.staleness = now - self.last_reading_at;
                self.feed_reading(now);
                now = now.max(self.model.deadline().unwrap_or(now));
                self.last_now = now;
                let out = self.model.tick(now);
                self.apply(out, now);
                continue;
            }
            let e = &events[i].2;
            i += 1;
            self.last_now = t;
            self.staleness = 0.0;
            match *e {
                Event::Format(epoch) => {
                    if !self.model.format(epoch, self.w.f as i64, self.w.p as i64, &[1, 2, (epoch & 0xFF) as u8]) {
                        self.formats_ignored += 1;
                    }
                }
                Event::Frame(stamp) => self.model.frame(stamp, t),
                Event::Packet { epoch, seq, stamp, segment_start, arrival } => {
                    let dup = !self.delivered.insert(((epoch as u64) << 32) | seq as u64);
                    if dup {
                        self.duplicates_sent += 1;
                    }
                    let before = (self.model.floor(), self.model.cover(t), self.model.need(t));
                    if engine_at.is_none() && !muted {
                        engine_at = Some(t + self.w.engine_start);
                    }
                    self.feed_reading(t);
                    let plan = self.model.packet(epoch, seq, stamp, segment_start, None, t);
                    if dup && (self.model.floor() != before.0 || self.model.cover(t) != before.1 || self.model.need(t) != before.2) {
                        self.dup_changed_windows += 1;
                    }
                    if dup && plan.decode {
                        self.violation(format!("a duplicate (seq {seq}) decoded"));
                    }
                    self.decode(&plan, epoch, seq, segment_start, arrival, stamp);
                    self.apply(plan.output, t);
                }
                Event::Mute(on) => {
                    muted = on;
                    self.model.mute(on);
                    let out = self.model.reset(Reset::Playback);
                    self.apply(out, t);
                    self.t0 = None;
                    self.player_end = None;
                    engine_at = if on { None } else { Some(t + self.w.engine_start) };
                }
                Event::Route(latency) => {
                    self.output_latency = latency;
                    self.model.latency(latency, self.io, self.w.refresh, self.w.stream_fps);
                    let out = self.model.reset(Reset::Jump);
                    self.apply(out, t);
                }
                Event::Drift(latency) => {
                    self.output_latency = latency;
                    self.model.latency(latency, self.io, self.w.refresh, self.w.stream_fps);
                }
                Event::Io(io) => {
                    self.io = io;
                    self.model.latency(self.output_latency, io, self.w.refresh, self.w.stream_fps);
                }
                Event::Home => self.model.away(false),
                Event::RttTick => self.model.rtt(self.w.rtt),
                Event::Second => {
                    let sec = self.model.take_second(t);
                    if let Some(e) = sec.error {
                        self.errors.push((t, e));
                    }
                    self.seconds.push((t, sec));
                }
            }
        }
        while let Some(d) = self.model.deadline().filter(|d| *d <= end) {
            let now = d.max(self.last_now);
            self.staleness = now - self.last_reading_at;
            self.feed_reading(now);
            let at = now.max(self.model.deadline().unwrap_or(now));
            self.last_now = at;
            let out = self.model.tick(at);
            self.apply(out, at);
        }
        let tails_done: HashSet<i32> = self.played.iter().filter(|p| p.part == Part::Tail).map(|p| p.id).collect();
        let held = self.kept.keys().filter(|k| !self.discarded.contains(k) && !tails_done.contains(k)).count();
        if held > 1 + WAITING_LIMIT {
            self.violation(format!("{held} kept packets never played nor discarded"));
        }
        self
    }

    /// The decoder: what the plan asks, and whether what it gives is the signal.
    fn decode(&mut self, plan: &Plan, epoch: i32, seq: u32, segment_start: bool, arrival: f64, stamp: f64) {
        if !plan.decode {
            if plan.id.is_some() {
                self.violation(format!("seq {seq} kept without being decoded"));
            }
            return;
        }
        let (f, p) = (self.w.f, self.w.p);
        let mut good;
        if plan.reset_first {
            self.dec_epoch = epoch;
            self.dec_next = seq.wrapping_add(1);
            self.dec_broken = false;
            if segment_start {
                good = true;
                self.dec_noise = 0;
                if plan.drop_front != p.min(f) {
                    self.violation(format!("seq {seq}: a segment's first packet drops {}, not {p}", plan.drop_front));
                }
            } else {
                good = false;
                self.dec_noise = 1;
            }
        } else if segment_start {
            self.dec_broken = true;
            good = false;
        } else if !self.dec_broken && self.dec_epoch == epoch && self.dec_next == seq {
            self.dec_next = seq.wrapping_add(1);
            good = true;
        } else {
            self.dec_broken = true;
            good = false;
        }
        let fade_required = self.dec_noise == 1 && good && !plan.reset_first;
        if good && !plan.reset_first && self.dec_noise > 0 {
            self.dec_noise = 0;
        }
        if epoch == 1 && self.w.decode_fails.contains(&(seq as i64)) && plan.id.is_some() {
            self.model.decode_failed();
            good = true;
            self.dec_broken = true;
        }
        if let Some(id) = plan.id {
            self.kept.insert(id, Kept { arrival, frames: f - plan.drop_front, good, fade_in: fade_required, first_stamp: stamp + plan.drop_front as f64 / RATE });
        } else if plan.drop_front != f && !(segment_start && p >= f) {
            self.violation(format!("seq {seq} decoded and neither kept nor dropped whole"));
        }
        if plan.reset_first && !segment_start && plan.id.is_some() {
            self.violation(format!("seq {seq} on a reset decoder mid-segment was kept"));
        }
    }

    fn packets(&self) -> impl DoubleEndedIterator<Item = &Played> {
        self.played.iter().filter(|p| p.first)
    }
    fn tails(&self) -> impl DoubleEndedIterator<Item = &Played> {
        self.played.iter().filter(|p| p.part == Part::Tail)
    }
    fn late_after(&self, t: f64) -> u32 {
        self.seconds.iter().filter(|s| s.0 > t).map(|s| s.1.late).sum()
    }
    fn jumps_after(&self, after: f64) -> u32 {
        self.seconds.iter().filter(|s| s.0 > after).map(|s| s.1.jumps).sum()
    }
    fn jumps(&self, a: f64, b: f64) -> u32 {
        self.seconds.iter().filter(|s| s.0 > a && s.0 <= b + 1.0).map(|s| s.1.jumps).sum()
    }
    fn late(&self, a: f64, b: f64) -> u32 {
        self.seconds.iter().filter(|s| s.0 > a && s.0 <= b + 1.0).map(|s| s.1.late).sum()
    }
    fn played_after(&self, t: f64) -> usize {
        self.packets().filter(|p| p.scheduled_at > t).count()
    }
    fn worst_ear_off(&self) -> f64 {
        self.packets().map(|p| (p.ear - p.decision.ear).abs()).fold(0.0, f64::max)
    }
    fn underruns(&self, after: f64) -> usize {
        self.tails().filter(|p| p.scheduled_at > after && p.scheduled_at < self.w.seconds - 0.5 && p.fade_out).count()
    }
    fn packet(&self, seq: u32) -> Option<&Played> {
        self.packets().find(|p| p.seq == seq && p.epoch == 1)
    }
    fn tail(&self, seq: u32) -> Option<&Played> {
        self.tails().find(|p| p.seq == seq && p.epoch == 1)
    }
    fn joins(&self) -> u32 {
        self.seconds.iter().map(|s| s.1.joins).sum()
    }
    fn worst_error(&self, after: f64) -> f64 {
        self.errors.iter().filter(|e| e.0 > after).map(|e| e.1.abs()).reduce(f64::max).unwrap_or(1.0)
    }
}

fn simulate(w: World) -> Run {
    Run::new(w, 7).run()
}

fn no_violations(r: &Run, what: &str) {
    assert!(r.violations.is_empty(), "{what}: {:?}", &r.violations[..r.violations.len().min(3)]);
}

// MARK: - Scenarios

#[test]
fn steady_links() {
    for (ol, io) in [(0.010, 0.005), (0.180, 0.005), (2.0, 0.005), (0.010, 0.023), (0.180, 0.023), (2.0, 0.023)] {
        let r = simulate(World { seconds: 30.0, output_latency: ol, io, transit: Box::new(|t| 0.003 + 0.0005 * (t * 7.3).sin()), ..World::default() });
        let what = format!("steady, output {}, IO {}", ms(ol), ms(io));
        no_violations(&r, &what);
        assert_eq!(r.late_after(2.0), 0, "{what}: nothing late after 2 s");
        assert!(r.played_after(3.0) >= 2600, "{what}: every packet after 3 s played");
        assert!(r.jumps_after(3.0) == 0 && r.underruns(3.0) == 0, "{what}: no jump or fade out after 3 s");
        let settled = r.worst_error(20.0);
        assert!(settled < 0.001, "{what}: error under 1 ms once settled ({})", ms(settled));
        assert!(r.worst_ear_off() < 0.0005, "{what}: heard where placed ({})", ms(r.worst_ear_off()));
    }
    let r512 = simulate(World { f: 512, p: 256, seconds: 25.0, ..World::default() });
    no_violations(&r512, "steady, 512 frames a packet");
    assert!(r512.late_after(2.0) == 0 && r512.played_after(3.0) >= 2000, "steady, 512: nothing late, everything played");
    let r120 = simulate(World { stream_fps: 120.0, video_fps: 120.0, refresh: 1.0 / 120.0, ..World::default() });
    no_violations(&r120, "steady, 120 fps");
    assert_eq!(r120.late_after(2.0), 0);
}

#[test]
fn the_sound_trails_the_picture() {
    let r = simulate(World::default());
    let behind: Vec<i32> = r.seconds.iter().filter_map(|s| s.1.behind_ms).collect();
    let behind = &behind[behind.len().saturating_sub(10)..];
    assert!(!behind.is_empty() && behind.iter().all(|b| (1..=99).contains(b)), "at home the sound trails the picture by 0–100 ms ({behind:?})");
    let d = &r.seconds.last().unwrap().1;
    assert!(d.delay >= d.need - 1e-9, "the delay is at least the need");
    let late = simulate(World { video_transit: Some(Box::new(|_| 1.0)), ..World::default() });
    no_violations(&late, "a picture a second late");
    let s = &late.seconds.last().unwrap().1;
    assert!(s.delay > 1.0 && s.lag.unwrap_or(0.0) > 0.9, "…the delay follows the picture");
    assert!(late.packets().rev().take(100).all(|p| p.decision.ear - p.decision.picture >= 1.0 / 60.0 - 0.0002), "…the sound a frame behind it");
    no_violations(
        &simulate(World { seconds: 25.0, video_transit: Some(Box::new(|t| 0.2 + if t > 10.0 { ((t - 10.0) * 0.013).min(0.040) } else { 0.0 })), ..World::default() }),
        "the picture's lag rising fast",
    );
    let still = simulate(World { video_fps: 0.0, ..World::default() });
    no_violations(&still, "no picture at all");
    assert_eq!(still.late_after(2.0), 0, "…plays by the need alone");
}

#[test]
fn keyframe_bursts_and_late_packets() {
    let r = simulate(World {
        seconds: 40.0,
        transit: Box::new(|t| {
            let into = t % 4.0;
            0.003 + if into < 0.040 { 0.040 - into } else { 0.0 }
        }),
        ..World::default()
    });
    no_violations(&r, "a 40 ms burst every 4 s");
    assert_eq!(r.late_after(2.0), 0, "…nothing late after 2 s");
    assert_eq!(r.underruns(9.0), 0, "…no fade out once the cover has it");
    assert!(r.seconds.iter().filter(|s| s.0 > 10.0).all(|s| (0.030..=0.046).contains(&s.1.cover)), "…the cover holds the burst");

    let one = simulate(World { seconds: 12.0, transit: Box::new(|t| if (t - 6.0).abs() < 0.005 { 0.073 } else { 0.003 }), ..World::default() });
    no_violations(&one, "one packet 70 ms late");
    assert_eq!(one.late_after(2.0), 0, "…nothing dropped as late: the cover rose with it");

    let first = simulate(World { transit: Box::new(|t| if t < 0.1 { 0.070 } else { 0.003 }), ..World::default() });
    no_violations(&first, "the first packets behind a keyframe");
    assert_eq!(first.late_after(2.0), 0);

    let stall = simulate(World { seconds: 30.0, transit: Box::new(|t| if (8.0..10.0).contains(&t) { 10.0 - t + 0.003 } else { 0.003 }), ..World::default() });
    no_violations(&stall, "a 2 s stall, then a burst");
    assert!(stall.late(9.9, 10.5) > 50, "…the burst's old packets dropped as late");
    assert_eq!(stall.jumps(18.0, 22.0), 1, "…a jump once the window forgets the stall");
    assert_eq!(stall.late_after(11.5), 0, "…nothing late after the burst");
}

#[test]
fn gaps_joins_pauses_epochs() {
    let gap = simulate(World { skipped: [1000, 1001, 1002].into(), ..World::default() });
    no_violations(&gap, "a seq gap");
    assert!(gap.packet(1003).is_none(), "…the packet after the gap is decoded and not played");
    assert_eq!(gap.packet(1004).map(|p| p.fade_in), Some(true), "…the next fades in");
    assert_eq!(gap.tail(999).map(|p| p.fade_out), Some(true), "…the one before it fades out");
    assert_eq!(gap.joins(), 1, "…one join");

    let join = simulate(World { join_at: Some(777), ..World::default() });
    no_violations(&join, "joining mid-segment");
    let first = join.packets().next().unwrap();
    assert!(join.packet(777).is_none() && first.seq == 778 && first.fade_in, "…its first packet dropped, the next placed by time, faded in");

    let pauses = simulate(World { seconds: 20.0, pauses: vec![(6.0, 0.05), (12.0, 0.5)], ..World::default() });
    no_violations(&pauses, "the Mac pauses 50 and 500 ms");
    let starts: Vec<&Played> = pauses.packets().filter(|p| !p.contiguous && p.scheduled_at > 3.0).collect();
    assert!(starts.len() == 2 && starts.iter().all(|p| p.fade_in), "…two segment starts placed by time, faded in");
    assert!(starts.iter().all(|p| p.decision.error().abs() < 0.0002), "…each at its stamp, after the priming");
    let before: Vec<&Played> = pauses.tails().filter(|p| starts.iter().any(|s| s.seq == p.seq + 1)).collect();
    assert!(before.len() == 2 && before.iter().all(|p| p.fade_out), "…the packet before each fades out");
    assert_eq!(pauses.late_after(2.0), 0);

    let epoch = simulate(World { seconds: 20.0, epochs: vec![10.0], ..World::default() });
    no_violations(&epoch, "a new epoch");
    assert_eq!(epoch.packets().find(|p| p.epoch == 2).map(|p| p.seq), Some(0), "…its first packet plays, from seq 0");
    assert_eq!(epoch.tails().filter(|p| p.epoch == 1).last().map(|p| p.fade_out), Some(true), "…the old epoch's last fades out");

    let fail = simulate(World { decode_fails: [500].into(), ..World::default() });
    no_violations(&fail, "a decode that fails");
    assert!(fail.packet(501).is_none() && fail.packet(502).map(|p| p.fade_in) == Some(true), "…the next dropped, the one after faded in");

    let stops = simulate(World { seconds: 5.0, transit: Box::new(|t| if t > 3.0 { 100.0 } else { 0.003 }), ..World::default() });
    no_violations(&stops, "the Mac stops sending");
    assert!(stops.tails().last().unwrap().fade_out, "…the last packet goes out at its deadline, faded");
}

#[test]
fn duplicates_and_moves() {
    let mv = simulate(World { seconds: 20.0, mv: Some(Move { register: 10.0, hand_over: 10.2, old_transit: 0.033, new_transit: 0.003 }), ..World::default() });
    no_violations(&mv, "a move's hand-over");
    let dups: u32 = mv.seconds.iter().map(|s| s.1.duplicates).sum();
    assert!(mv.duplicates_sent >= 15 && dups == mv.duplicates_sent, "…every copy dropped as a duplicate");
    assert_eq!(mv.dup_changed_windows, 0, "…none counted toward the floor, the cover or the need");
    assert_eq!(mv.formats_ignored, 1, "…the new connection's format ignored");
    assert!(mv.jumps(9.0, 13.0) <= 1, "…at most one jump");
    assert_eq!(mv.joins(), 0, "…nothing reset");
    assert_eq!(mv.late_after(2.0), 0);
    for extra in [-0.05, 0.02, 0.3] {
        let r = simulate(World { duplicate: Some((700, 760, extra)), ..World::default() });
        no_violations(&r, &format!("61 packets delivered twice, the copy {} off", ms(extra)));
        assert!(r.duplicates_sent == 61 && r.dup_changed_windows == 0, "…61 duplicates, none counted");
    }
    let home = simulate(World { seconds: 30.0, away: true, home_at: Some(15.0), transit: Box::new(|t| if t < 15.0 { 0.080 + 0.050 * (t * 3.1).sin() } else { 0.004 }), ..World::default() });
    no_violations(&home, "away at 80 ± 50 ms, then home");
    assert_eq!(home.late_after(2.0), 0, "…nothing late away");
    assert!(home.seconds.iter().filter(|s| s.0 > 5.0 && s.0 < 15.0).all(|s| s.1.cover >= 0.060), "…away's lower bound");
    assert!(home.seconds.iter().filter(|s| s.0 > 27.0).all(|s| s.1.cover <= 0.150), "…home's bounds once the window forgets");
}

#[test]
fn clocks_drift_and_step() {
    for (host, out) in [(100e-6, 0.0), (-100e-6, 0.0), (0.0, 100e-6), (100e-6, -100e-6)] {
        let r = simulate(World { seconds: 3600.0, host_drift: host, out_drift: out, video_fps: 5.0, ..World::default() });
        let what = format!("the Mac's clock {} ppm, the output's {} ppm", host * 1e6, out * 1e6);
        no_violations(&r, &what);
        let worst = r.worst_error(60.0);
        assert!(worst < 0.005, "{what}: error within ±5 ms after a minute ({})", ms(worst));
        assert!(r.jumps_after(60.0) == 0 && r.late_after(60.0) == 0, "{what}: no jump and nothing late after a minute");
    }
    for by in [1.0, -1.0] {
        let r = simulate(World { seconds: 25.0, host_steps: vec![(12.0, by)], ..World::default() });
        no_violations(&r, &format!("the Mac's clock steps {by} s"));
        assert!((1..=2).contains(&r.jumps(11.0, 16.0)), "…a jump or two");
        assert!(r.jumps_after(17.0) == 0 && r.late_after(17.0) == 0, "…nothing more after it");
        assert!(r.worst_error(18.0) < 0.005, "…the error back under 5 ms");
        assert_eq!(r.jumps(11.0, 16.0) as usize, r.jumps_made.len(), "…each jump reported once");
        if by > 0.0 {
            assert!(r.jumps_made[0].2 == JumpCause::Step && r.jumps_made[0].1.abs() < 0.005, "…forward: a clock step's jump of nothing");
        } else {
            assert!(r.jumps_made[0].1 < -0.5 && r.jumps_made.iter().any(|j| j.2 == JumpCause::Step), "…back: the second dropped, then the clock step");
        }
    }
    let slower = simulate(World { seconds: 25.0, transit: Box::new(|t| if t < 10.0 { 0.003 } else { 0.083 }), ..World::default() });
    no_violations(&slower, "the path 80 ms slower");
    assert!(slower.jumps(10.0, 13.0) >= 1, "…a jump");
    assert!(slower.late_after(13.0) == 0 && slower.jumps_after(13.0) == 0, "…then steady");
    let half = simulate(World { seconds: 20.0, transit: Box::new(|t| if (10.0..10.5).contains(&t) { 0.063 } else { 0.003 }), ..World::default() });
    no_violations(&half, "half a second 60 ms slower");
    assert_eq!(half.jumps(9.0, 20.0), 0, "…no step and no jump");
}

#[test]
fn routes_mutes_and_the_engine_starting() {
    let route = simulate(World { seconds: 25.0, routes: vec![(8.0, 0.190), (16.0, 0.010)], ..World::default() });
    no_violations(&route, "a route change to Bluetooth and back");
    assert!(route.jumps(7.5, 9.0) == 1 && route.jumps(15.5, 17.0) == 1, "…one jump each way");
    assert_eq!(route.late_after(2.0), 0);
    let worst = route.errors.iter().filter(|e| e.0 > 10.0 && e.0 < 16.0).map(|e| e.1.abs()).reduce(f64::max).unwrap_or(1.0);
    assert!(worst < 0.002, "…placed where due at once");

    let mute = simulate(World { seconds: 20.0, engine_start: 0.030, mutes: vec![(6.0, true), (12.0, false)], ..World::default() });
    no_violations(&mute, "mute and unmute");
    assert!(!mute.played.iter().any(|p| p.scheduled_at > 6.0 && p.scheduled_at < 12.0), "…nothing played while muted");
    let back = mute.packets().find(|p| p.scheduled_at > 12.0).expect("sound after unmuting");
    assert!(back.decision.ear - 12.0 < 0.2 && back.fade_in, "…sound again within 0.2 s of unmuting, faded in");

    let slow = simulate(World { engine_start: 0.080, ..World::default() });
    no_violations(&slow, "the engine takes 80 ms to start");
    assert!(slow.packets().count() > 1500 && slow.packets().next().unwrap().seq < 8, "…from a packet that waited for it");
    let slower = simulate(World { engine_start: 2.5, ..World::default() });
    no_violations(&slower, "an engine that takes 2.5 s");
    assert!(slower.discarded.len() >= 100, "…the oldest discarded");

    // The output latency measured afresh with no reset (MacSound's case): rule 11 closes the error.
    for f in [480, 512] {
        let r = simulate(World { seconds: 40.0, f, p: f / 2, io: 0.035, io_at: vec![(10.0, 0.005)], ..World::default() });
        no_violations(&r, &format!("the IO buffer shrinks with no reset, {f} frames"));
        assert!(r.jumps_after(10.0) == 0 && r.late_after(2.0) == 0 && r.errors.last().unwrap().1.abs() < 0.005, "…no jump, nothing late, the error closed");
    }
    let jitter = simulate(World { seconds: 30.0, reading_jitter: 0.0002, ..World::default() });
    no_violations(&jitter, "readings with ±0.2 ms of jitter");
    let changed: u32 = jitter.seconds.iter().filter(|s| s.0 > 15.0).map(|s| s.1.frames_added + s.1.frames_dropped).sum();
    assert!(changed < 100, "…few frames changed once settled");
}

/// Random worlds (the Swift check runs 5,000; 1,500 here, as on Android): every invariant holds.
#[test]
fn random_runs() {
    let mut rng = Rng::new(20260927);
    let mut steady = 0;
    for k in 0..1500 {
        let mut w = World { seconds: rng.range(4.0, 12.0), ..World::default() };
        if rng.chance(0.3) {
            w.f = 512;
            w.p = 256;
        }
        w.output_latency = if rng.chance(0.2) { rng.range(0.1, 2.0) } else { rng.range(0.003, 0.03) };
        w.io = rng.range(0.002, 0.030);
        w.away = rng.chance(0.25);
        w.host_drift = rng.range(-200e-6, 200e-6);
        w.out_drift = rng.range(-200e-6, 200e-6);
        w.video_fps = if rng.chance(0.2) {
            0.0
        } else if rng.chance(0.3) {
            120.0
        } else {
            60.0
        };
        w.stream_fps = if w.video_fps == 120.0 { 120.0 } else { 60.0 };
        w.refresh = if rng.chance(0.5) { 1.0 / 60.0 } else { 1.0 / 120.0 };
        w.reading_jitter = if rng.chance(0.3) { rng.range(0.0, 0.0005) } else { 0.0 };
        w.engine_start = if rng.chance(0.3) { rng.range(0.0, 0.2) } else { 0.0 };
        w.rtt = if rng.chance(0.2) { None } else { Some(rng.range(0.001, 0.2)) };
        let base = if w.away { rng.range(0.02, 0.2) } else { rng.range(0.001, 0.02) };
        let jitter = rng.range(0.0, if w.away { 0.05 } else { 0.01 });
        let seed = rng.next();
        let mut events = 0;
        let (mut burst_every, mut burst_size, mut stall_at, mut stall_for, mut step_path, mut step_by) = (0.0, 0.0, -1.0, 0.0, -1.0, 0.0);
        if rng.chance(0.3) {
            burst_every = rng.range(1.0, 5.0);
            burst_size = rng.range(0.01, 0.15);
            events += 1;
        }
        if rng.chance(0.15) {
            stall_at = rng.range(1.0, w.seconds);
            stall_for = rng.range(0.05, 2.5);
            events += 1;
        }
        if rng.chance(0.15) {
            step_path = rng.range(1.0, w.seconds);
            step_by = rng.range(-0.08, 0.12);
            events += 1;
        }
        let phase = rng.range(0.0, 100.0);
        w.transit = Box::new(move |t: f64| {
            let mut d = base + jitter * (0.5 + 0.5 * (t * 13.7 + phase).sin()) * (t * 3.1 + phase * 2.0).sin().abs();
            if burst_every > 0.0 {
                let into = t % burst_every;
                if into < burst_size {
                    d += burst_size - into;
                }
            }
            if stall_at >= 0.0 && t >= stall_at && t < stall_at + stall_for {
                d += stall_at + stall_for - t;
            }
            if step_path >= 0.0 && t >= step_path {
                d = (d + step_by).max(0.0005);
            }
            d
        });
        if rng.chance(0.3) {
            let lag = rng.range(0.0, 1.0);
            w.video_transit = Some(Box::new(move |t: f64| base + lag + 0.01 * t.sin()));
        }
        if rng.chance(0.15) {
            w.host_steps = vec![(rng.range(1.0, w.seconds), rng.range(-1.5, 1.5))];
            events += 1;
        }
        if rng.chance(0.2) {
            w.pauses = vec![(rng.range(0.5, w.seconds), rng.range(0.02, 1.0))];
            events += 1;
        }
        if rng.chance(0.1) {
            w.epochs = vec![rng.range(0.5, w.seconds)];
            events += 1;
        }
        if rng.chance(0.15) {
            let s = rng.int(10, 500);
            w.skipped = (s..s + rng.int(1, 5)).collect();
            events += 1;
        }
        if rng.chance(0.15) {
            let s = rng.int(10, 400);
            w.duplicate = Some((s, s + rng.int(0, 60), rng.range(-0.05, 0.1)));
            events += 1;
        }
        if rng.chance(0.1) {
            w.join_at = Some(rng.int(1, 200));
        }
        if rng.chance(0.15) {
            let a = rng.range(0.5, w.seconds);
            w.mutes = vec![(a, true), (a + rng.range(0.05, 3.0), false)];
            events += 1;
        }
        if rng.chance(0.15) {
            w.routes = vec![(rng.range(0.5, w.seconds), rng.range(0.003, 0.3))];
            events += 1;
        }
        if rng.chance(0.1) {
            w.drifts = vec![(rng.range(0.5, w.seconds), (w.output_latency + rng.range(-0.03, 0.03)).max(0.003))];
            events += 1;
        }
        if w.away && rng.chance(0.2) {
            w.home_at = Some(rng.range(1.0, w.seconds));
            events += 1;
        }
        if rng.chance(0.05) {
            w.decode_fails = [rng.int(20, 300)].into();
            events += 1;
        }
        let quiet = events == 0 && jitter < (if w.away { 0.045 } else { 0.009 }) && w.join_at.is_none() && w.video_transit.is_none();
        let r = Run::new(w, seed).run();
        no_violations(&r, &format!("random {k}"));
        if quiet {
            steady += 1;
            assert_eq!(r.late_after(2.0), 0, "random {k}: a steady link lost packets as late after 2 s");
        }
    }
    assert!(steady > 0, "some runs were steady ({steady})");
}
