//! The output side's pure parts (timeline.rs): the timeline the output pulls, the clock its time
//! stamps make, the feed that hands the clock to the playout, and the three with the playout on a
//! simulated track, checked for where each sample is heard. AudioTimelineTest.kt, ported.

use crate::audio::playout::{self, AudioPlayout, Decision, Output, Part, Schedule, RATE};
use crate::audio::{ClockFeed, OutputClock, PlayerTimeline};
use std::collections::HashMap;

fn read(t: &PlayerTimeline, frames: usize) -> Vec<f32> {
    let mut out = vec![9.0; frames * 2];
    t.read(&mut out, frames);
    out
}

fn near(a: f64, b: f64, eps: f64) -> bool {
    (a - b).abs() <= eps
}

#[test]
fn timeline() {
    let t = PlayerTimeline::new(2);
    assert_eq!(t.schedule(vec![1.0, 1.0, 2.0, 2.0], Some(4.0)), 4, "a buffer at its sample");
    assert_eq!(t.schedule(vec![3.0, 3.0], None), 6, "the next right after it");
    assert_eq!(t.schedule(vec![4.0, 4.0], Some(5.0)), 7, "one asked to start before the last ends starts at its end");
    assert_eq!(t.schedule(vec![5.0, 5.0], Some(8.5)), 9, "rounded as Swift rounds (halves away from zero)");
    assert_eq!(t.pending(), 5);
    assert_eq!(
        read(&t, 10),
        [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 2.0, 2.0, 3.0, 3.0, 4.0, 4.0, 0.0, 0.0, 5.0, 5.0],
        "silence, then the buffers in order"
    );
    assert_eq!(t.position(), 10);
    assert_eq!(t.pending(), 0, "nothing left");
    assert_eq!(t.schedule(vec![6.0, 6.0], Some(2.0)), 10, "a buffer whose sample has been read starts where the output is");
    assert_eq!(read(&t, 2), [6.0, 6.0, 0.0, 0.0]);
    assert_eq!(t.schedule(vec![7.0, 7.0], None), 12, "a contiguous one after the last has played starts where the output is");
    assert_eq!(read(&t, 2), [7.0, 7.0, 0.0, 0.0]);
    assert_eq!(t.schedule(vec![8.0, 8.0], Some(f64::NAN)), 14, "no place at all: right after");
    assert_eq!(t.schedule(Vec::new(), Some(1e300)), i64::MAX, "nothing, anywhere");
    assert_eq!(read(&t, 1), [8.0, 8.0]);
    // A buffer a read takes only part of.
    t.schedule(vec![1.0, 1.0, 2.0, 2.0, 3.0, 3.0], Some(16.0));
    assert_eq!(read(&t, 2), [0.0, 0.0, 1.0, 1.0]);
    assert_eq!(t.pending(), 2);
    assert_eq!(read(&t, 3), [2.0, 2.0, 3.0, 3.0, 0.0, 0.0]);

    let f = PlayerTimeline::new(2);
    f.schedule(vec![1.0; 2000], Some(0.0));
    f.fade_out(4);
    let out = read(&f, 6);
    for (got, want) in out.iter().zip([0.75, 0.75, 0.5, 0.5, 0.25, 0.25, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]) {
        assert!((got - want).abs() < 1e-6, "the fade to silence: a linear ramp, then silence");
    }
    assert!(read(&f, 10).iter().all(|x| *x == 0.0), "…and silence after");
}

#[test]
fn clock() {
    let c = OutputClock::new(48_000.0, 2.0);
    assert_eq!(c.ear(0), None, "no stamp: no ear");
    c.wrote(0, 1.0);
    assert_eq!(c.latency(), None, "no stamp: no latency");
    c.guess(0, 1.02);
    assert!(c.has_stamp() && !c.has_real_stamp());
    c.stamp(4800, 1.2);
    assert!(near(c.ear(9600).unwrap(), 1.2 + 0.1, 1e-12), "a real stamp replaces the guess");
    c.guess(0, 5.0);
    assert!(near(c.reading().unwrap().1, 1.2, 1e-12), "…and a guess never replaces a real stamp");
    c.wrote(9600, 1.27); // heard 30 ms after it was taken
    c.wrote(9840, 1.28); // 25 ms
    assert!(near(c.latency().unwrap(), 0.030, 1e-9), "the latency: the largest of the window");
    c.wrote(48_000 + 4800, 2.08); // 120 ms
    assert!(near(c.latency().unwrap(), 0.12, 1e-9));
    c.wrote(4800 + 48_000 * 3, 4.19); // 10 ms, more than 2 s after the 120
    assert!(near(c.latency().unwrap(), 0.01, 1e-9), "the window forgets after 2 s");
    c.forget_latency();
    assert_eq!(c.latency(), None, "a new route: nothing measured yet");

    let mut m = AudioPlayout::new();
    let mut feed = ClockFeed::new();
    let k = OutputClock::default();
    k.stamp(0, 16.0);
    k.wrote(0, 16.0 - 0.03125); // heard 31.25 ms after it was taken
    feed.feed(&mut m, &k, 0.006, 1.0 / 60.0, 60.0, 16.0);
    assert!(near(m.output_latency(), 0.0315, 1e-12), "the latency to the model, up to the next half ms");
    k.wrote(0, 16.0 - 0.03173828125); // 31.7 ms: 32 ms
    feed.feed(&mut m, &k, 0.006, 1.0 / 60.0, 60.0, 16.0);
    assert!(near(m.output_latency(), 0.0315, 1e-12), "…a move of 1 ms or less is not passed on");
    k.wrote(0, 16.0 - 0.033203125); // 33.2 ms: 33.5 ms
    feed.feed(&mut m, &k, 0.006, 1.0 / 60.0, 60.0, 16.0);
    assert!(near(m.output_latency(), 0.0335, 1e-12), "…more is");
    feed.forget();
    k.forget_latency();
    k.wrote(0, 16.0 - 0.0078125);
    feed.feed(&mut m, &k, 0.006, 1.0 / 60.0, 60.0, 16.0);
    assert!(near(m.output_latency(), 0.008, 1e-12), "…and a forgotten one is passed on whatever it is");
}

/// The playout, the timeline, the clock and the feed together on a simulated track: a 15 ms buffer
/// the writer fills a 5 ms chunk at a time as the device takes it, heard 10 ms after the device takes
/// it, stamped every 100 ms. Every sample of the Mac's sound must be heard at its stamp + the floor +
/// the delay the model says, nothing late, nothing moved from where the model put it.
#[test]
fn the_chain_plays_each_sample_where_it_is_due() {
    struct Sim {
        model: AudioPlayout,
        timeline: PlayerTimeline,
        clock: OutputClock,
        feed: ClockFeed,
        kept: HashMap<i32, Vec<f32>>,
        heard: Vec<(i64, f32)>,
        decisions: Vec<(i64, Decision)>,
        moved: u32,
        next_chunk: i64,
        last_stamp: f64,
    }
    const CHUNK: usize = 240;
    const BUFFER: f64 = 720.0;
    const DEVICE: f64 = 0.010;
    const T0: f64 = 100.0;
    let packet_frames = 480usize;
    let priming = 240usize;
    let wall = 1_790_000_000.0;
    let io = CHUNK as f64 / RATE + 0.002; // the engine's IO buffer
    // The device takes frame s at t0 + s ÷ rate, the writer `buffer` frames ahead of it; heard `device` later.
    fn hear(s: i64) -> f64 {
        T0 + s as f64 / RATE + DEVICE
    }
    fn read_at(k: i64) -> f64 {
        T0 + ((k as f64 * CHUNK as f64 - BUFFER) / RATE).max(0.0)
    }
    impl Sim {
        fn schedule(&mut self, s: &Schedule) {
            let Some(pcm) = self.kept.get(&s.id) else { return };
            let end = (s.offset + s.count).min(pcm.len());
            let mut l = pcm[s.offset..end].to_vec();
            if s.change != 0 {
                let mut r = l.clone();
                playout::change(&mut l, &mut r, s.change);
            }
            let inter: Vec<f32> = l.iter().flat_map(|x| [*x, 1.0]).collect();
            let start = self.timeline.schedule(inter, s.at);
            if (start as f64 - s.start).abs() > 0.5 {
                self.moved += 1;
            }
            if s.at.is_some() {
                self.decisions.push((start, s.decision));
            }
            if s.part == Part::Tail {
                self.kept.remove(&s.id);
            }
        }
        fn apply(&mut self, o: Output) {
            for d in &o.discards {
                self.kept.remove(d);
            }
            for s in &o.schedules {
                self.schedule(s);
            }
        }
        fn writer_until(&mut self, t: f64) {
            let mut out = vec![0f32; CHUNK * 2];
            while read_at(self.next_chunk) <= t {
                let taken = read_at(self.next_chunk);
                let first = self.next_chunk * CHUNK as i64;
                if taken - self.last_stamp >= 0.1 {
                    self.clock.stamp(first, hear(first));
                    self.last_stamp = taken;
                }
                let from = self.timeline.read(&mut out, CHUNK);
                self.clock.wrote(from, taken);
                for i in 0..CHUNK {
                    if out[2 * i + 1] == 1.0 {
                        self.heard.push((from + i as i64, out[2 * i]));
                    }
                }
                self.next_chunk += 1;
            }
        }
        fn feed(&mut self, io: f64, now: f64) {
            if let Some(o) = self.feed.feed(&mut self.model, &self.clock, io, 1.0 / 60.0, 60.0, now) {
                self.apply(o);
            }
        }
    }
    let mut s = Sim {
        model: AudioPlayout::new(),
        timeline: PlayerTimeline::new(2),
        clock: OutputClock::default(),
        feed: ClockFeed::new(),
        kept: HashMap::new(),
        heard: Vec::new(),
        decisions: Vec::new(),
        moved: 0,
        next_chunk: 0,
        last_stamp: -1.0,
    };
    s.model.format(1, packet_frames as i64, priming as i64, &[]);
    s.model.latency(0.0, io, 1.0 / 60.0, 60.0);

    let seconds = 20.0;
    let mut n = 0usize;
    loop {
        let produce = T0 + (n + 1) as f64 * packet_frames as f64 / RATE - priming as f64 / RATE + 0.010;
        if produce > T0 + seconds {
            break;
        }
        let stamp = wall + (n as f64 * packet_frames as f64 - priming as f64) / RATE;
        let arrival = produce + 0.003;
        while let Some(d) = s.model.deadline().filter(|d| *d < arrival) {
            s.writer_until(d);
            s.feed(io, d);
            let at = d.max(s.model.deadline().unwrap_or(d));
            let o = s.model.tick(at);
            s.apply(o);
        }
        s.writer_until(arrival);
        s.feed(io, arrival);
        let plan = s.model.packet(1, n as u32, stamp, n == 0, None, arrival);
        // Each kept frame carries its place in the Mac's stream: frame g is the Mac's (g − P) ÷ rate.
        if let Some(id) = plan.id {
            s.kept.insert(id, (0..packet_frames - plan.drop_front).map(|i| (n * packet_frames + plan.drop_front + i) as f32).collect());
        }
        s.apply(plan.output);
        if n % 100 == 99 {
            let sec = s.model.take_second(arrival);
            if arrival > T0 + 2.5 {
                assert_eq!(sec.late, 0, "nothing late");
            }
        }
        n += 1;
    }
    s.writer_until(T0 + seconds + 1.0);

    assert_eq!(s.moved, 0, "every part played where the model put it");
    assert!(!s.decisions.is_empty(), "decisions were made");
    for (start, d) in &s.decisions {
        assert!(near(d.ear, hear(*start), 0.0002), "heard where the model says");
    }
    // The floor is (arrival − stamp), the delay the need: cover 20 ms + a packet + IO + latency + 2 ms.
    let floor = T0 + packet_frames as f64 / RATE + 0.013 - wall;
    assert!(near(s.model.output_latency(), BUFFER / RATE + DEVICE, 0.0006), "the output latency measured: the buffer and the device");
    let need = 0.020 + packet_frames as f64 / RATE + io + s.model.output_latency() + 0.002;
    let whole = |x: &&(i64, f32)| x.1 == x.1.floor();
    let late: Vec<_> = s.heard.iter().filter(|x| hear(x.0) > T0 + 16.0).filter(whole).collect();
    assert!(late.len() > 48_000);
    for (sample, g) in late {
        let heard_at = hear(*sample);
        let stamp_of = wall + (*g as f64 - priming as f64) / RATE;
        assert!(near(heard_at - stamp_of, floor + need, 0.001), "sample {sample} heard at its stamp + the floor + the need");
    }
    let order: Vec<f32> = s.heard.iter().filter(whole).map(|x| x.1).collect();
    assert!(order.windows(2).all(|w| w[1] > w[0]), "the Mac's frames in order");
}
