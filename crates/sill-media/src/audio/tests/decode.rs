//! This port's own checks, on what the Mac's encoder really makes: `tests/data/audio_eld_*.sill43`
//! are two seconds of a tone (left 1000 Hz, right 1500 Hz, 0.4 of full scale) encoded by AudioToolbox's
//! AAC-ELD encoder as Sill for Mac configures it (tests/data/audio_fixture.swift), as kind 43 payloads.
//! FFmpeg's decoder must give the tone back, frame for frame where the format says; the resampler and
//! the output's steady clock must hold; and the whole path must play it through `MacSound` on the null
//! sink, at the pace the Mac sends it.

use super::Rng;
use crate::audio::engine::Steady;
use crate::audio::playout::RATE;
use crate::audio::resample::Resampler;
use crate::audio::{eld_config, now, AudioMessage, EldDecoder, MacSound, Sink};
use std::f64::consts::PI;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub(crate) const FIXTURE_480: &[u8] = include_bytes!("../../../tests/data/audio_eld_480.sill43");
pub(crate) const FIXTURE_512: &[u8] = include_bytes!("../../../tests/data/audio_eld_512.sill43");
/// The first ten packets of each as AudioToolbox's decoder decodes them (interleaved f32, little endian).
const APPLE_480: &[u8] = include_bytes!("../../../tests/data/audio_eld_480.at.f32");
const APPLE_512: &[u8] = include_bytes!("../../../tests/data/audio_eld_512.at.f32");
const TONE_FRAMES: usize = 96_000;

/// The fixture's records: each message's stamp (seconds from the tone's first sample) and payload.
pub(crate) fn records(file: &[u8]) -> Vec<(f64, Vec<u8>)> {
    assert_eq!(&file[..8], b"SILL43\0\x01");
    let mut out = Vec::new();
    let mut i = 8;
    while i < file.len() {
        let n = u32::from_be_bytes(file[i..i + 4].try_into().unwrap()) as usize;
        let stamp = f64::from_be_bytes(file[i + 4..i + 12].try_into().unwrap());
        out.push((stamp, file[i + 12..i + 12 + n].to_vec()));
        i += 12 + n;
    }
    out
}

/// The tone the fixture was made from (audio_fixture.swift's).
fn tone() -> (Vec<f32>, Vec<f32>) {
    let mut l = Vec::with_capacity(TONE_FRAMES);
    let mut r = Vec::with_capacity(TONE_FRAMES);
    for i in 0..TONE_FRAMES {
        let mut g = 0.4;
        let edge = i.min(TONE_FRAMES - 1 - i);
        if edge < 480 {
            g *= 0.5 - 0.5 * (PI * (edge + 1) as f64 / 481.0).cos();
        }
        l.push((g * (2.0 * PI * 1000.0 * i as f64 / RATE).sin()) as f32);
        r.push((g * (2.0 * PI * 1500.0 * i as f64 / RATE).sin()) as f32);
    }
    (l, r)
}

/// The power of `x` at `hz` (a full-scale sine's is 0.25).
fn power(x: &[f32], hz: f64, rate: f64) -> f64 {
    let (mut re, mut im) = (0.0, 0.0);
    for (n, v) in x.iter().enumerate() {
        let a = 2.0 * PI * hz * n as f64 / rate;
        re += *v as f64 * a.cos();
        im += *v as f64 * a.sin();
    }
    (re * re + im * im) / (x.len() as f64 * x.len() as f64)
}

/// The same 10 ms at a time, averaged: a frame gained or lost here and there (rule 11) does not matter.
fn power_in_blocks(x: &[f32], hz: f64) -> f64 {
    let blocks: Vec<f64> = x.chunks_exact(480).map(|b| power(b, hz, RATE)).collect();
    blocks.iter().sum::<f64>() / blocks.len() as f64
}

/// 10·log10(signal ÷ difference) of `got` against `want`.
fn snr(got: &[f32], want: &[f32]) -> f64 {
    let signal: f64 = want.iter().map(|x| (*x as f64).powi(2)).sum();
    let noise: f64 = got.iter().zip(want).map(|(a, b)| (*a as f64 - *b as f64).powi(2)).sum();
    10.0 * (signal / noise.max(1e-30)).log10()
}

struct Fixture {
    frames: usize,
    priming: usize,
    asc: Vec<u8>,
    packets: Vec<Vec<u8>>,
}

fn fixture(file: &[u8]) -> Fixture {
    let records = records(file);
    let Some(AudioMessage::Format(format)) = AudioMessage::parse(&records[0].1) else { panic!("the format") };
    let mut packets = Vec::new();
    for (k, (stamp, payload)) in records[1..].iter().enumerate() {
        let Some(AudioMessage::Packets(p)) = AudioMessage::parse(payload) else { panic!("record {k}: packets") };
        assert_eq!((p.epoch, p.seq, p.segment_start, p.packets.len()), (1, k as u32, k == 0, 1));
        let f = format.frames_per_packet.unwrap() as f64;
        assert!((stamp - (k as f64 * f - format.priming_frames.unwrap() as f64) / RATE).abs() < 1e-12, "stamped with its first decoded sample");
        packets.extend(p.packets);
    }
    let asc = eld_config::audio_specific_config(format.cookie.as_deref().unwrap()).unwrap();
    Fixture { frames: format.frames_per_packet.unwrap() as usize, priming: format.priming_frames.unwrap() as usize, asc, packets }
}

fn decode_all(d: &mut EldDecoder, packets: &[Vec<u8>]) -> (Vec<f32>, Vec<f32>) {
    let (mut l, mut r) = (Vec::new(), Vec::new());
    for (k, p) in packets.iter().enumerate() {
        let (a, b) = d.decode(p).unwrap_or_else(|| panic!("packet {k} decodes"));
        assert_eq!((a.len(), b.len()), (d.frames_per_packet(), d.frames_per_packet()));
        l.extend(a);
        r.extend(b);
    }
    (l, r)
}

#[test]
fn ffmpeg_decodes_the_macs_aac_eld() {
    assert!(EldDecoder::available(), "this FFmpeg build decodes AAC-ELD");
    let (tone_l, tone_r) = tone();
    for (file, reference) in [(FIXTURE_480, APPLE_480), (FIXTURE_512, APPLE_512)] {
        let fx = fixture(file);
        let what = format!("{} frames a packet", fx.frames);
        assert_eq!(eld_config::parse(&fx.asc).map(|i| (i.object_type, i.sample_rate, i.channels, i.frame_length as usize, i.sbr)), Some((39, 48000, 2, fx.frames, false)));
        let mut d = EldDecoder::new(&fx.asc, fx.frames).unwrap();
        let (l, r) = decode_all(&mut d, &fx.packets);
        assert!(l.len() >= TONE_FRAMES + fx.priming, "{what}: {} frames decoded", l.len());

        // Frame i of the tone is frame i + priming of the packets, exactly: a segment's first
        // `primingFrames` are the codec's own, as the format says, and nothing else is delayed.
        let at = |shift: i64| {
            let from = (fx.priming as i64 + 4 + shift) as usize;
            let n = TONE_FRAMES - 8;
            (snr(&l[from..from + n], &tone_l[4..4 + n]), snr(&r[from..from + n], &tone_r[4..4 + n]))
        };
        let (sl, sr) = at(0);
        // (The codec's own noise on a pure tone is 26 to 32 dB under it, Apple's decoder's too.)
        assert!(sl > 22.0 && sr > 22.0, "{what}: the tone back, {sl:.1} and {sr:.1} dB over the difference");
        for shift in [-2, -1, 1, 2] {
            let (a, b) = at(shift);
            assert!(a < sl - 6.0 && b < sr - 6.0, "{what}: {shift} frames off is worse ({a:.1}, {b:.1} dB)");
        }

        // And what AudioToolbox's own decoder makes of the same packets (the first ten, the
        // priming included), sample for sample.
        let apple: Vec<f32> = reference.chunks_exact(4).map(|b| f32::from_le_bytes(b.try_into().unwrap())).collect();
        assert_eq!(apple.len(), 10 * fx.frames * 2);
        let (al, ar): (Vec<f32>, Vec<f32>) = apple.chunks(2).map(|f| (f[0], f[1])).unzip();
        let (same_l, same_r) = (snr(&l[..al.len()], &al), snr(&r[..ar.len()], &ar));
        eprintln!("{what}: FFmpeg against AudioToolbox's decoder: {same_l:.1} and {same_r:.1} dB; against the tone: {sl:.1} and {sr:.1} dB");
        assert!(same_l > 60.0 && same_r > 60.0, "{what}: as AudioToolbox decodes them ({same_l:.1}, {same_r:.1} dB)");
        assert!(l[..fx.priming / 2].iter().all(|x| x.abs() < 0.01), "{what}: the priming is the codec's own silence");

        // The tone's frequency and level, each channel its own.
        let (from, to) = (fx.priming + 4800, fx.priming + TONE_FRAMES - 4800);
        let full = 0.4 * 0.4 / 4.0;
        let (l1000, l1500, r1000, r1500) = (power(&l[from..to], 1000.0, RATE), power(&l[from..to], 1500.0, RATE), power(&r[from..to], 1000.0, RATE), power(&r[from..to], 1500.0, RATE));
        assert!((l1000 / full - 1.0).abs() < 0.02 && l1500 < full * 1e-4, "{what}: left is 1000 Hz at 0.4 ({l1000:.5}, {l1500:.7})");
        assert!((r1500 / full - 1.0).abs() < 0.02 && r1000 < full * 1e-4, "{what}: right is 1500 Hz at 0.4 ({r1500:.5}, {r1000:.7})");

        // A reset decoder is a fresh one: the same packets, the same frames, bit for bit.
        let t = Instant::now();
        d.reset();
        let reset_took = t.elapsed();
        let (l2, r2) = decode_all(&mut d, &fx.packets[..50]);
        assert!(l2[..] == l[..l2.len()] && r2[..] == r[..r2.len()], "{what}: after a reset, as a fresh decoder");
        assert!(reset_took < Duration::from_millis(20), "{what}: a reset takes {reset_took:?}");

        // One that starts in the middle of a segment (a join): its first packet is not the signal
        // (the playout drops it), and it is the signal again within a few packets.
        d.reset();
        let (joined, _) = decode_all(&mut d, &fx.packets[100..106]);
        let straight = &l[100 * fx.frames..106 * fx.frames];
        let first = snr(&joined[..fx.frames], &straight[..fx.frames]);
        let second = snr(&joined[fx.frames..2 * fx.frames], &straight[fx.frames..2 * fx.frames]);
        let fourth = snr(&joined[3 * fx.frames..4 * fx.frames], &straight[3 * fx.frames..4 * fx.frames]);
        assert!(first < 20.0, "{what}: a join's first packet is not the signal ({first:.1} dB)");
        assert!(second > 25.0 && fourth > 60.0, "{what}: …its second nearly ({second:.1} dB), its fourth exactly ({fourth:.1} dB)");
    }
}

#[test]
fn the_decoder_takes_anything() {
    let fx = fixture(FIXTURE_480);
    assert!(EldDecoder::new(&[], 480).is_err(), "no config");
    assert!(EldDecoder::new(&fx.asc, 0).is_err() && EldDecoder::new(&fx.asc, 8193).is_err(), "a packet size out of range");
    let mut r = Rng::new(11);
    // Configs that are not one: refused or opened, never a crash.
    for _ in 0..200 {
        let n = r.int(1, 12) as usize;
        if let Ok(mut d) = EldDecoder::new(&r.bytes(n), 480) {
            let _ = d.decode(&fx.packets[3]);
        }
    }
    // A config of another packet size than the format's: every packet fails, and it gives up.
    let asc512 = fixture(FIXTURE_512).asc;
    let mut wrong = EldDecoder::new(&asc512, 480).unwrap();
    for p in &fx.packets[..25] {
        assert!(wrong.decode(p).is_none());
    }
    assert!(wrong.broken(), "twenty failures in a row: no use");

    let mut d = EldDecoder::new(&fx.asc, 480).unwrap();
    assert!(d.decode(&[]).is_none(), "an empty packet");
    let mut failed = 0;
    for i in 0..600 {
        let mut p = fx.packets[20 + i % 100].clone();
        match i % 4 {
            0 => {
                let n = r.int(1, 300) as usize;
                p = r.bytes(n);
            }
            1 => p.truncate(r.int(1, p.len() as i64) as usize),
            2 => {
                for _ in 0..r.int(1, 8) {
                    let k = r.int(0, p.len() as i64 - 1) as usize;
                    p[k] = r.next() as u8;
                }
            }
            _ => {}
        }
        match d.decode(&p) {
            Some((l, rr)) => assert!(l.len() == 480 && rr.len() == 480),
            None => {
                failed += 1;
                d.reset();
            }
        }
        if i % 4 == 3 {
            assert!(!d.broken(), "a good packet in four keeps it going");
        }
    }
    assert!(failed > 0, "some of that was refused");
    // And it still decodes the stream.
    let mut fresh = EldDecoder::new(&fx.asc, 480).unwrap();
    d.reset();
    assert!(decode_all(&mut d, &fx.packets[..20]) == decode_all(&mut fresh, &fx.packets[..20]));
}

#[test]
fn the_resampler() {
    for to in [44_100.0, 96_000.0, 16_000.0, 88_200.0] {
        let mut rs = Resampler::new(RATE, to);
        let block = (to / 100.0) as usize;
        let mut out = Vec::new();
        let mut fed = 0usize;
        let mut buf = vec![0f32; block * 2];
        for _ in 0..100 {
            let need = rs.input_needed(block);
            let lag = rs.lag();
            assert!((0.0..=18.0).contains(&lag), "the lag is the taps' half ({lag})");
            // Left: 1 kHz. Right: a constant, and one frame of 1 at input frame 24,000.
            let input: Vec<f32> = (fed..fed + need).flat_map(|n| [(2.0 * PI * 1000.0 * n as f64 / RATE).sin() as f32, if n == 24_000 { 1.5 } else { 0.5 }]).collect();
            fed += need;
            rs.process(&input, &mut buf);
            out.extend_from_slice(&buf);
        }
        let frames = out.len() / 2;
        assert!((fed as f64 - frames as f64 * RATE / to).abs() < 40.0, "{to} Hz: it takes what it makes, at the ratio");
        let left: Vec<f32> = out.chunks(2).map(|f| f[0]).collect();
        let right: Vec<f32> = out.chunks(2).map(|f| f[1]).collect();
        // Output frame k is input time k × 48000 ÷ rate: the sine, in phase.
        let want: Vec<f32> = (0..frames).map(|k| (2.0 * PI * 1000.0 * k as f64 / to).sin() as f32).collect();
        let quality = snr(&left[1000..frames - 100], &want[1000..frames - 100]);
        assert!(quality > 55.0, "{to} Hz: a 1 kHz sine comes out one, in place ({quality:.1} dB)");
        assert!(right[2000..3000].iter().all(|x| (x - 0.5).abs() < 1e-3), "{to} Hz: a constant stays what it is");
        let peak = (0..frames).max_by(|a, b| right[*a].total_cmp(&right[*b])).unwrap();
        assert!((peak as f64 - 24_000.0 * to / RATE).abs() <= 1.0, "{to} Hz: an input frame comes out at its time ({peak})");
    }
    // Any block size, the smallest included.
    let mut rs = Resampler::new(RATE, 44_100.0);
    let mut r = Rng::new(3);
    for _ in 0..2000 {
        let n = r.int(0, 600) as usize;
        let input = vec![0.25f32; rs.input_needed(n) * 2];
        let mut out = vec![0f32; n * 2];
        rs.process(&input, &mut out);
    }
    assert_eq!(rs.input_needed(0), 0);
}

#[test]
fn the_outputs_steady_clock() {
    // Callbacks of 256 frames whose stamps jitter by ±1 ms: the clock's does not, and follows a drift.
    let mut steady = Steady::default();
    let mut r = Rng::new(5);
    let mut worst: f64 = 0.0;
    let mut last = f64::NEG_INFINITY;
    for k in 0..4000i64 {
        let sample = k * 256;
        let truth = 50.0 + sample as f64 / RATE * (1.0 + 100e-6);
        let got = steady.stamp(sample, truth + r.range(-0.001, 0.001));
        if k > 200 {
            worst = worst.max((got - truth).abs());
        }
        assert!(got > last, "time goes on");
        last = got;
    }
    assert!(worst < 0.0006, "±1 ms of jitter comes out under 0.6 ms ({:.3} ms)", worst * 1000.0);
    // A step (an underrun, a new route) is taken at once.
    let got = steady.stamp(4000 * 256, 50.0 + 4000.0 * 256.0 / RATE + 0.100);
    assert!((got - (50.0 + 4000.0 * 256.0 / RATE + 0.100)).abs() < 1e-9);
    steady.forget();
    assert!((steady.stamp(0, 7.0) - 7.0).abs() < 1e-12);
}

/// The whole path with no sound card: the fixture's messages into `MacSound` on the null sink at the
/// pace the Mac sends them (each packet once its last frame is captured, with a 60 fps picture
/// beside it), and what the sink takes is the tone. Then `stop()`, and the same epoch's packets
/// start it again without a new format.
#[test]
fn mac_sound_plays_the_tone_on_the_null_sink() {
    let fx = fixture(FIXTURE_480);
    let all = records(FIXTURE_480);
    let sound = MacSound::with_sink(Sink::Null);
    assert_eq!(sound.codecs(), ["aac-eld"]);
    assert_eq!(sound.take_second(), None, "nothing played yet");
    let heard: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = heard.clone();
    sound.set_tap(Some(Box::new(move |_, frames| sink.lock().unwrap().extend_from_slice(frames))));
    sound.set_stream_fps(60);
    sound.set_rtt(Some(3));
    sound.set_away(false);

    // Each packet is sent once its frames are captured and encoded; the picture's frames 8 ms after
    // they are captured.
    let play = |upto: usize| {
        let wall = 1_790_000_000.0;
        let begun = now();
        let (f, p) = (fx.frames as f64, fx.priming as f64);
        let mut frame = 0u32;
        for (k, (stamp, payload)) in all.iter().enumerate().take(upto + 1) {
            let send_at = if k == 0 { 0.0 } else { (k as f64 * f - p) / RATE + 0.004 };
            loop {
                let t = now() - begun;
                while (frame as f64 / 60.0 + 0.008) <= t.min(send_at) {
                    sound.on_video_frame(wall + frame as f64 / 60.0);
                    frame += 1;
                }
                if t >= send_at {
                    break;
                }
                std::thread::sleep(Duration::from_secs_f64((send_at - t).min(0.002)));
            }
            sound.on_message(payload, wall + stamp);
        }
    };
    play(all.len() - 1);
    std::thread::sleep(Duration::from_millis(400));

    let (behind, late) = sound.take_second().expect("a second closed with sound in it");
    assert!((-1..300).contains(&behind), "the sound trailed the picture by {behind} ms");
    assert!(late <= 20, "{late} packets too late to play");
    let second = sound.latest_second().unwrap();
    assert!(second.need > 0.045 && second.need < 0.130, "the need: 40 ms of cover (20 once two seconds are measured), a packet, the sink's 7 ms and 10 ms, 2 ms ({:.1} ms)", second.need * 1000.0);
    let stats = sound.stats();
    assert_eq!(stats.engines, 1);
    assert!(stats.sounding > 80_000 && stats.sounding <= 96_000 + 4800, "{} frames of sound of the tone's 96,000", stats.sounding);
    assert!((stats.peak - 0.4).abs() < 0.03 && (stats.rms - 0.4 / 2f32.sqrt()).abs() < 0.03, "the tone's level: peak {}, rms {}", stats.peak, stats.rms);

    let pcm = std::mem::take(&mut *heard.lock().unwrap());
    let first = pcm.chunks(2).position(|f| f[0] != 0.0 || f[1] != 0.0).expect("sound");
    let last = pcm.chunks(2).rposition(|f| f[0] != 0.0 || f[1] != 0.0).unwrap();
    assert!(last - first > 85_000, "about two seconds from first to last ({} frames)", last - first);
    // The middle second: the tone and nothing else, each channel its own.
    let (from, to) = (first + 24_000, first + 72_000);
    let left: Vec<f32> = pcm[from * 2..to * 2].chunks(2).map(|f| f[0]).collect();
    let right: Vec<f32> = pcm[from * 2..to * 2].chunks(2).map(|f| f[1]).collect();
    let full = 0.4 * 0.4 / 4.0;
    let (l1000, l1500, r1000, r1500) = (power_in_blocks(&left, 1000.0), power_in_blocks(&left, 1500.0), power_in_blocks(&right, 1000.0), power_in_blocks(&right, 1500.0));
    assert!(l1000 > full * 0.8 && l1500 < full * 0.01, "left is the 1000 Hz tone ({l1000:.5} of {full:.5}; {l1500:.6} at 1500 Hz)");
    assert!(r1500 > full * 0.8 && r1000 < full * 0.01, "right is the 1500 Hz tone ({r1500:.5} of {full:.5}; {r1000:.6} at 1000 Hz)");

    // Muted here: the Mac keeps sending, the output stops; unmuted, it plays again.
    sound.set_muted(true);
    assert!(sound.muted());
    sound.set_muted(false);

    // stop(): the stats go; the format stays, and its epoch's packets start the sound again.
    sound.stop();
    std::thread::sleep(Duration::from_millis(600));
    assert_eq!(sound.take_second(), None, "stopped: nothing played this session");
    let before = sound.stats();
    let heard_before = heard.lock().unwrap().len();
    // Its packets alone, no format: a hundred of them, a second.
    let packets_only = |upto: usize| {
        let wall = 1_790_000_100.0;
        let begun = now();
        for (k, (stamp, payload)) in all.iter().enumerate().skip(1).take(upto) {
            let send_at = (k as f64 * fx.frames as f64 - fx.priming as f64) / RATE + 0.004;
            while now() - begun < send_at {
                std::thread::sleep(Duration::from_millis(1));
            }
            sound.on_message(payload, wall + stamp);
        }
    };
    packets_only(100);
    std::thread::sleep(Duration::from_millis(300));
    let after = sound.stats();
    assert!(after.engines == before.engines + 1, "another output for it");
    assert!(after.sounding - before.sounding > 35_000, "its sound again ({} frames)", after.sounding - before.sounding);
    assert!(heard.lock().unwrap().len() > heard_before);
    sound.release();
    sound.on_message(&all[0].1, 0.0); // released: nothing, and no harm
    let _ = play;
}


/// A machine with no output device: everything is taken as ever, nothing is played, nothing grows.
#[test]
fn mac_sound_without_an_output() {
    let all = records(FIXTURE_480);
    let sound = MacSound::with_sink(Sink::Missing);
    for round in 0..3u32 {
        for (k, (stamp, payload)) in all.iter().enumerate() {
            sound.on_video_frame(1_790_000_000.0 + round as f64 * 10.0 + stamp);
            sound.on_message(payload, 1_790_000_000.0 + round as f64 * 10.0 + stamp);
            if k % 50 == 0 {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        sound.set_muted(round == 0);
        sound.stop();
    }
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(sound.take_second(), None);
    assert_eq!(sound.stats(), crate::audio::SoundStats::default());
    sound.release();
}

/// Whatever comes as a kind 43 payload, and whenever: nothing panics, on any thread.
#[test]
fn mac_sound_takes_anything() {
    let all = records(FIXTURE_480);
    let sound = Arc::new(MacSound::with_sink(Sink::Null));
    let threads: Vec<_> = (0..3u64)
        .map(|n| {
            let (sound, all) = (sound.clone(), all.clone());
            std::thread::spawn(move || {
                let mut r = Rng::new(40 + n);
                for i in 0..3000 {
                    let mut d = all[r.int(0, all.len() as i64 - 1) as usize].1.clone();
                    match r.int(0, 5) {
                        0 => d.truncate(r.int(0, d.len() as i64) as usize),
                        1 => {
                            let k = r.int(0, d.len() as i64 - 1) as usize;
                            d[k] = r.next() as u8;
                        }
                        2 => {
                            let n = r.int(0, 64) as usize;
                            d = r.bytes(n);
                        }
                        3 => d = format!("\u{1}{{\"codec\":\"aac-eld\",\"epoch\":{},\"framesPerPacket\":{},\"primingFrames\":{},\"sampleRate\":{},\"channels\":{},\"cookie\":\"+OZQAA==\"}}", r.int(-2, 70_000), r.int(-5, 9000), r.int(-5, 100_000), [48_000, 44_100, 0][r.int(0, 2) as usize], r.int(0, 3)).into_bytes(),
                        _ => {}
                    }
                    let stamp = [1.79e9 + i as f64 * 0.01, f64::NAN, f64::INFINITY, -1e300, 0.0][r.int(0, 4).min(r.int(0, 4)) as usize];
                    sound.on_message(&d, stamp);
                    sound.on_video_frame(stamp);
                    match r.int(0, 400) {
                        0 => sound.stop(),
                        1 => sound.set_muted(r.chance(0.5)),
                        2 => sound.set_foreground(r.chance(0.7)),
                        3 => sound.set_rtt([None, Some(-4), Some(30), Some(i32::MAX)][r.int(0, 3) as usize]),
                        4 => sound.set_stream_fps([0, -1, 60, 120, i64::MAX][r.int(0, 4) as usize]),
                        5 => sound.set_refresh_hz([0.0, f64::NAN, 60.0, 144.0, -3.0][r.int(0, 4) as usize]),
                        6 => sound.set_away(r.chance(0.5)),
                        7 => {
                            let _ = sound.take_second();
                        }
                        _ => {}
                    }
                    if i % 100 == 0 {
                        std::thread::sleep(Duration::from_millis(2));
                    }
                }
            })
        })
        .collect();
    for t in threads {
        t.join().expect("no panic on a feeding thread");
    }
    // The audio thread is alive and answers: a stop, then the tone's first second plays.
    sound.stop();
    sound.set_muted(false);
    sound.set_foreground(true);
    std::thread::sleep(Duration::from_millis(600));
    let before = sound.stats();
    let begun = now();
    for (k, (stamp, payload)) in all.iter().enumerate().take(101) {
        let send_at = if k == 0 { 0.0 } else { (k as f64 * 480.0 - 240.0) / RATE + 0.004 };
        while now() - begun < send_at {
            std::thread::sleep(Duration::from_millis(1));
        }
        sound.on_message(payload, 1_790_000_500.0 + stamp);
    }
    std::thread::sleep(Duration::from_millis(300));
    let after = sound.stats();
    assert!(after.sounding - before.sounding > 35_000, "after all that, the tone still plays ({} frames)", after.sounding - before.sounding);
}
