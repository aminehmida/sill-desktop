//! Plays two seconds of a tone (left 1000 Hz, right 1500 Hz) through `MacSound`, as Sill for Mac
//! sends it: the fixture's kind 43 messages (AAC-ELD from the Mac's own encoder), fed at the pace the
//! Mac makes them, with a 60 fps picture's time stamps beside them.
//!
//!     audio_tone [--null] [--512] [--seconds N] [--quiet]
//!
//! `--null`: the same path with no output device (for CI); `--512`: the 512-frame fixture;
//! `--seconds N`: the tone N times over, as one stream; `--quiet`: no log lines. Prints what was
//! played and exits 1 if it was not the tone.
use sill_media::audio::{now, AudioMessage, MacSound, Sink};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const FIXTURE_480: &[u8] = include_bytes!("../tests/data/audio_eld_480.sill43");
const FIXTURE_512: &[u8] = include_bytes!("../tests/data/audio_eld_512.sill43");

struct Stderr;
impl log::Log for Stderr {
    fn enabled(&self, _: &log::Metadata) -> bool {
        true
    }
    fn log(&self, record: &log::Record) {
        eprintln!("{:8.3} {}", now() - 1000.0, record.args());
    }
    fn flush(&self) {}
}

/// The fixture's records: each message's stamp (seconds from the tone's first sample) and payload.
fn records(file: &[u8]) -> Vec<(f64, Vec<u8>)> {
    assert_eq!(&file[..8], b"SILL43\0\x01", "a fixture");
    let mut out = Vec::new();
    let mut i = 8;
    while i + 12 <= file.len() {
        let n = u32::from_be_bytes(file[i..i + 4].try_into().unwrap()) as usize;
        let stamp = f64::from_be_bytes(file[i + 4..i + 12].try_into().unwrap());
        out.push((stamp, file[i + 12..i + 12 + n].to_vec()));
        i += 12 + n;
    }
    out
}

/// The power of `x` at `hz`, 10 ms at a time (a frame gained or lost here and there does not matter),
/// averaged; a full-scale sine's is 0.25.
fn power(x: &[f32], hz: f64) -> f64 {
    let blocks: Vec<f64> = x
        .chunks_exact(480)
        .map(|b| {
            let (mut re, mut im) = (0.0, 0.0);
            for (n, v) in b.iter().enumerate() {
                let a = 2.0 * std::f64::consts::PI * hz * n as f64 / 48_000.0;
                re += *v as f64 * a.cos();
                im += *v as f64 * a.sin();
            }
            (re * re + im * im) / (480.0 * 480.0)
        })
        .collect();
    blocks.iter().sum::<f64>() / blocks.len().max(1) as f64
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let null = args.iter().any(|a| a == "--null");
    let file = if args.iter().any(|a| a == "--512") { FIXTURE_512 } else { FIXTURE_480 };
    let times = args.iter().position(|a| a == "--seconds").and_then(|i| args.get(i + 1)).and_then(|s| s.parse::<f64>().ok()).map_or(1, |s| (s / 2.0).ceil().max(1.0) as u32);
    if !args.iter().any(|a| a == "--quiet") {
        static LOGGER: Stderr = Stderr;
        let _ = log::set_logger(&LOGGER);
        log::set_max_level(log::LevelFilter::Debug);
    }

    let all = records(file);
    let Some(AudioMessage::Format(format)) = AudioMessage::parse(&all[0].1) else { panic!("the fixture's format") };
    let (f, p) = (format.frames_per_packet.unwrap() as f64, format.priming_frames.unwrap() as f64);
    let packets: Vec<Vec<u8>> = all[1..]
        .iter()
        .map(|(_, payload)| match AudioMessage::parse(payload) {
            Some(AudioMessage::Packets(p)) => p.packets[0].clone(),
            _ => panic!("the fixture's packets"),
        })
        .collect();

    let sound = MacSound::with_sink(if null { Sink::Null } else { Sink::Device });
    println!("codecs: {:?}; sink: {}", sound.codecs(), if null { "null" } else { "the default output device" });
    let heard: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::with_capacity(48_000 * 2 * 3)));
    let tap = heard.clone();
    sound.set_tap(Some(Box::new(move |_, frames| {
        if let Ok(mut h) = tap.try_lock() {
            h.extend_from_slice(frames);
        }
    })));
    sound.set_stream_fps(60);
    sound.set_rtt(Some(2));

    // The Mac's wall clock, and ours: a packet is sent once its last frame is captured and encoded
    // (4 ms), a picture's frame 8 ms after it is captured.
    let wall = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(1.79e9, |d| d.as_secs_f64());
    let begun = now();
    let mut frame = 0u64;
    let mut seconds = Vec::new();
    let mut next_second = 1.0;
    let mut tick = |until: f64| loop {
        let t = now() - begun;
        while frame as f64 / 60.0 + 0.008 <= t.min(until) {
            sound.on_video_frame(wall + frame as f64 / 60.0);
            frame += 1;
        }
        if t >= next_second {
            next_second += 1.0;
            seconds.push(sound.take_second());
        }
        if t >= until {
            break;
        }
        std::thread::sleep(Duration::from_secs_f64((until - t).min(0.002)));
    };
    sound.on_message(&all[0].1, wall);
    let mut seq = 0u32;
    for round in 0..times {
        for (n, packet) in packets.iter().enumerate() {
            // Each round is a segment of its own: a new start, the decoder afresh.
            let at = round as f64 * (packets.len() as f64 * f / 48_000.0);
            let stamp = at + (n as f64 * f - p) / 48_000.0;
            tick(at + ((n + 1) as f64 * f - p) / 48_000.0 + 0.004);
            let message = sill_media::audio::AudioPackets { epoch: format.epoch.unwrap() as u16, seq, segment_start: n == 0, packets: vec![packet.clone()] };
            sound.on_message(&message.serialized(), wall + stamp);
            seq += 1;
        }
    }
    let end = now() - begun + 0.6;
    tick(end);
    seconds.push(sound.take_second());

    let stats = sound.stats();
    let last = sound.latest_second();
    sound.set_tap(None);
    sound.stop();
    std::thread::sleep(Duration::from_millis(150));
    drop(sound);

    let pcm = heard.lock().unwrap();
    let left: Vec<f32> = pcm.chunks(2).filter(|f| f[0] != 0.0 || f[1] != 0.0).map(|f| f[0]).collect();
    let right: Vec<f32> = pcm.chunks(2).filter(|f| f[0] != 0.0 || f[1] != 0.0).map(|f| f[1]).collect();
    let full = 0.4 * 0.4 / 4.0;
    let (l1000, l1500, r1000, r1500) = (power(&left, 1000.0) / full, power(&left, 1500.0) / full, power(&right, 1000.0) / full, power(&right, 1500.0) / full);
    println!("sent: {} packets of {} frames, {:.1} s", seq, f, seq as f64 * f / 48_000.0);
    println!("outputs started: {}; frames taken: {} ({:.2} s); with sound: {} ({:.2} s); peak {:.3}; rms {:.3}; underruns {}", stats.engines, stats.frames, stats.frames as f64 / 48_000.0, stats.sounding, stats.sounding as f64 / 48_000.0, stats.peak, stats.rms, stats.xruns);
    println!("left: {:.1}% of the tone's power at 1000 Hz, {:.2}% at 1500 Hz; right: {:.1}% at 1500 Hz, {:.2}% at 1000 Hz", l1000 * 100.0, l1500 * 100.0, r1500 * 100.0, r1000 * 100.0);
    println!("each second's (audioBehindMs, audioLate): {seconds:?}");
    if let Some(s) = last {
        println!("the last second: need {:.1} ms (jitter cover {:.1} ms), delay {:.1} ms, error {:?} ms, frames +{} -{}, jumps {}, late {}, joins {}", s.need * 1000.0, s.cover * 1000.0, s.delay * 1000.0, s.error.map(|e| (e * 1e4).round() / 10.0), s.frames_added, s.frames_dropped, s.jumps, s.late, s.joins);
    }
    // All of it but what came while the output was being opened (a device takes a fifth of a second).
    let wanted = seq as f64 * f * if null { 0.95 } else { 0.8 };
    let ok = stats.sounding as f64 > wanted && l1000 > 0.8 && r1500 > 0.8 && l1500 < 0.02 && r1000 < 0.02;
    if stats.engines == 0 {
        println!("no output started (no device?): nothing was played, nothing broke");
        std::process::exit(if null { 1 } else { 0 });
    }
    println!("{}", if ok { "OK: the tone was played" } else { "FAILED: what was played is not the tone" });
    std::process::exit(if ok { 0 } else { 1 });
}
