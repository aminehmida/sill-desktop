//! Kind 43 (message.rs) against Audio.swift's checks (Tests/checks/protocol and compatibility), and
//! the cookie (eld_config.rs) against what AudioToolbox's AAC-ELD encoder really hands the Mac (480
//! and 512 frames, 48 kHz stereo, 128 kbps). AudioMessageTest.kt, ported.

use super::{hex, unhex, Rng};
use crate::audio::eld_config::{self, Info};
use crate::audio::{AudioFormat, AudioMessage, AudioPackets};
use base64::Engine;

fn packets(epoch: u32, seq: u32, start: bool, list: Vec<Vec<u8>>) -> Vec<u8> {
    // The epoch modulo 65,536, as the Mac writes it.
    AudioPackets { epoch: epoch as u16, seq, segment_start: start, packets: list }.serialized()
}

fn parsed(epoch: u16, seq: u32, start: bool, list: Vec<Vec<u8>>) -> Option<AudioMessage> {
    Some(AudioMessage::Packets(AudioPackets { epoch, seq, segment_start: start, packets: list }))
}

fn format(json: &str) -> Vec<u8> {
    let mut d = vec![1u8];
    d.extend_from_slice(json.as_bytes());
    d
}

fn eld() -> Vec<u8> {
    (0..162).map(|i| (i * 7 + 1) as u8).collect()
}

fn example() -> Vec<u8> {
    packets(3, 0, true, vec![eld()])
}

#[test]
fn packets_as_the_mac_writes_them() {
    let (eld, example) = (eld(), example());
    assert_eq!(hex(&example[..11]), "020003000000000101" .to_string() + "00a2", "the plan's example: 02 0003 00000000 01 01 00A2");
    assert_eq!(example.len(), 173);
    assert_eq!(&example[11..173], &eld[..]);
    assert_eq!(AudioMessage::parse(&example), parsed(3, 0, true, vec![eld.clone()]));
    let many: Vec<Vec<u8>> = (0..255usize).map(|i| vec![i as u8; i % 40]).collect();
    assert_eq!(
        AudioMessage::parse(&packets(65_535, 4_000_000_000, false, many.clone())),
        parsed(65_535, 4_000_000_000, false, many),
        "255 packets (some empty), the top epoch and a large seq"
    );
    assert_eq!(AudioMessage::parse(&packets(70_000, 1, false, vec![eld.clone()])), parsed(4464, 1, false, vec![eld.clone()]), "the epoch modulo 65,536");
    assert_eq!(hex(&packets(0x0102, 0x0304_0506, false, vec![vec![9]])[..9]), "020102030405060001", "seq and epoch big endian");
    let mut flagged = example.clone();
    flagged[7] = 0x03;
    assert_eq!(AudioMessage::parse(&flagged), parsed(3, 0, true, vec![eld.clone()]), "bit 0 is the segment's start; bits 1–7 ignored");
    flagged[7] = 0xFE;
    assert_eq!(AudioMessage::parse(&flagged), parsed(3, 0, false, vec![eld.clone()]), "0xFE: no segment start");
    let big = vec![1u8; 16_384 - 9 - 2];
    assert!(AudioMessage::parse(&packets(1, 1, false, vec![big.clone()])).is_some(), "16,384 bytes are taken");
    let mut bigger = big;
    bigger.push(1);
    assert!(AudioMessage::parse(&packets(1, 1, false, vec![bigger])).is_none(), "over 16 KB");
    let Some(AudioMessage::Packets(capped)) = AudioMessage::parse(&packets(1, 1, false, vec![vec![1]; 300])) else { panic!("300 packets") };
    assert_eq!(capped.packets.len(), 255, "at most 255 packets are written");
    let long = AudioPackets { epoch: 1, seq: 1, segment_start: false, packets: vec![vec![7; 70_000]] }.serialized();
    assert_eq!(long.len(), 9 + 2 + 65_535, "at most 65,535 bytes of a packet are written");
}

#[test]
fn what_a_reader_refuses() {
    let (eld, example) = (eld(), example());
    let refused = |what: &str, d: &[u8]| assert!(AudioMessage::parse(d).is_none(), "{what}");
    let retyped = |t: u8| {
        let mut d = example.clone();
        d[0] = t;
        d
    };
    refused("an empty payload", &[]);
    refused("an unknown type (3)", &retyped(3));
    refused("type 0", &retyped(0));
    refused("type 1 with no JSON", &[1]);
    refused("type 1 that is not JSON", &format("aac"));
    refused("type 1 that is a JSON array", &format("[]"));
    refused("type 1 over 4 KB", &format(&format!("{{\"app\":\"{}\"}}", "a".repeat(4100))));
    assert!(AudioMessage::parse(&format(&format!("{{\"app\":\"{}\"}}", "a".repeat(4085)))).is_some(), "4,096 bytes are taken");
    refused("type 2 with only its type", &[2]);
    refused("type 2 cut short in its header", &example[..8]);
    refused("a count of 0", &[2, 0, 3, 0, 0, 0, 0, 1, 0]);
    refused("a count of 0 and bytes after it", &[2, 0, 3, 0, 0, 0, 0, 1, 0, 0, 1, 7]);
    refused("a length past the end", &example[..example.len() - 1]);
    let mut over = example.clone();
    over.push(0);
    refused("bytes left over", &over);
    refused("missing its second packet", &packets(3, 0, false, vec![eld.clone(), eld.clone()])[..9 + 2 + 162 + 1]);
    let mut count_too_high = packets(3, 0, false, vec![eld]);
    count_too_high[8] = 2;
    refused("counting a packet it does not carry", &count_too_high);
}

#[test]
fn never_panics() {
    let mut r = Rng::new(20260927);
    let mut parsed = 0;
    for i in 0..4000 {
        let mut d: Vec<u8>;
        if i < 2000 {
            let n = r.int(0, 40) as usize;
            d = r.bytes(n);
            if i % 3 == 0 && !d.is_empty() {
                d[0] = 2;
            }
            if i % 3 == 1 && !d.is_empty() {
                d[0] = 1;
            }
        } else {
            let list: Vec<Vec<u8>> = (0..r.int(1, 4)).map(|_| {
                let n = r.int(0, 20) as usize;
                r.bytes(n)
            }).collect();
            d = packets(r.int(0, 65_535) as u32, r.next() as u32, r.chance(0.5), list);
            match r.int(0, 3) {
                0 => d.truncate(r.int(0, d.len() as i64) as usize),
                1 => {
                    let k = r.int(0, d.len() as i64 - 1) as usize;
                    d[k] ^= r.int(1, 255) as u8;
                }
                2 => d.push(r.next() as u8),
                _ => {}
            }
        }
        let Some(m) = AudioMessage::parse(&d) else { continue };
        parsed += 1;
        if let AudioMessage::Packets(p) = &m {
            let mut again = p.serialized();
            again[7] = d[7]; // the flags' ignored bits are not kept
            assert_eq!(again, d, "type 2 parsed writes the same bytes back");
        }
        assert!(AudioMessage::parse(&m.serialized()).is_some());
    }
    assert!(parsed > 0);
    // Type 1 with every sort of JSON in it.
    for i in 0..3000 {
        let mut d = format(r#"{"app":"Safari","bitrate":128000,"channels":2,"codec":"aac-eld","cookie":"+PAhLAA=","epoch":3,"framesPerPacket":480}"#);
        for _ in 0..r.int(1, 3) {
            let k = r.int(1, d.len() as i64 - 1) as usize;
            d[k] = if i % 2 == 0 { r.next() as u8 } else { b"{}[]\",:0919.e-tfn\\"[r.int(0, 17) as usize] };
        }
        let _ = AudioMessage::parse(&d);
    }
    let deep = format(&"[".repeat(4000));
    assert!(AudioMessage::parse(&deep).is_none(), "nesting too deep to read is refused, not followed");
}

#[test]
fn the_format() {
    let f = AudioMessage::parse(&format(
        r#"{"app":"Safari","bitrate":128000,"channels":2,"codec":"aac-eld","cookie":"+PAhLAA=","epoch":3,"framesPerPacket":480,"primingFrames":240,"sampleRate":48000,"source":"app"}"#,
    ));
    let want = AudioFormat {
        codec: Some("aac-eld".into()),
        sample_rate: Some(48000),
        channels: Some(2),
        frames_per_packet: Some(480),
        priming_frames: Some(240),
        bitrate: Some(128_000),
        epoch: Some(3),
        cookie: Some(vec![0xF8, 0xF0, 0x21, 0x2C, 0x00]),
        source: Some(AudioFormat::SOURCE_APP.into()),
        app: Some("Safari".into()),
    };
    assert_eq!(f, Some(AudioMessage::Format(want.clone())), "every field, the cookie in base64");
    assert_eq!(AudioMessage::parse(&AudioMessage::Format(want.clone()).serialized()), Some(AudioMessage::Format(want)), "written and read back");
    let fmt = |f: AudioFormat| Some(AudioMessage::Format(f));
    assert_eq!(AudioMessage::parse(&format("{}")), fmt(AudioFormat::default()), "{{}} is a format with nothing in it");
    assert_eq!(
        AudioMessage::parse(&format(r#"{"codec":"opus","v":2,"channels":6}"#)),
        fmt(AudioFormat { codec: Some("opus".into()), channels: Some(6), ..Default::default() }),
        "a later host's: unknown keys skipped, an unknown codec kept"
    );
    assert_eq!(AudioMessage::parse(&format(r#"{"epoch":null,"cookie":null,"app":null}"#)), fmt(AudioFormat::default()), "null as absent");
    // As Swift's JSONDecoder: whole numbers only (1.0 and 1e3 are), the right types, strict base64.
    assert_eq!(AudioMessage::parse(&format(r#"{"epoch":1.0}"#)), fmt(AudioFormat { epoch: Some(1), ..Default::default() }));
    assert_eq!(AudioMessage::parse(&format(r#"{"epoch":1e3}"#)), fmt(AudioFormat { epoch: Some(1000), ..Default::default() }));
    assert_eq!(AudioMessage::parse(&format(r#"{"bitrate":9223372036854775807}"#)), fmt(AudioFormat { bitrate: Some(i64::MAX), ..Default::default() }));
    assert_eq!(AudioMessage::parse(&format(r#"{"bitrate":-9223372036854775808}"#)), fmt(AudioFormat { bitrate: Some(i64::MIN), ..Default::default() }));
    for bad in [
        r#"{"epoch":1.5}"#,
        r#"{"epoch":"3"}"#,
        r#"{"epoch":true}"#,
        r#"{"bitrate":9223372036854775808}"#,
        r#"{"bitrate":1e30}"#,
        r#"{"codec":1}"#,
        r#"{"cookie":"not base64!"}"#,
        r#"{"cookie":"YQ"}"#,
        r#"{"cookie":"Y Q=="}"#,
        r#"{"cookie":"-_=="}"#,
        r#"{"cookie":3}"#,
    ] {
        assert!(AudioMessage::parse(&format(bad)).is_none(), "refused: {bad}");
    }
    let Some(AudioMessage::Format(padded)) = AudioMessage::parse(&format(r#"{"cookie":"YQ=="}"#)) else { panic!("the cookie, padded") };
    assert_eq!(padded.cookie, Some(vec![97]), "the cookie, padded");
}

// MARK: - The cookie

/// AudioToolbox's kAudioConverterCompressionMagicCookie for AAC-ELD, 480 and 512 frames (an ES_Descriptor).
const COOKIE_480: &str = "0380808024000000048080801640140018000001f4000001f4000580808004f8e65000068080800102";
const COOKIE_512: &str = "0380808024000000048080801640140018000001f4000001f4000580808004f8e64000068080800102";

#[test]
fn the_macs_cookie_unwrapped() {
    assert_eq!(base64::engine::general_purpose::STANDARD.encode(unhex(COOKIE_480)), "A4CAgCQAAAAEgICAFkAUABgAAAH0AAAB9AAFgICABPjmUAAGgICAAQI=");
    let asc480 = eld_config::audio_specific_config(&unhex(COOKIE_480)).unwrap();
    assert_eq!(hex(&asc480), "f8e65000", "the DecoderSpecificInfo of the ES_Descriptor");
    let info = |object_type, sample_rate, channels, frame_length, sbr| Some(Info { object_type, sample_rate, channels, frame_length, sbr });
    assert_eq!(eld_config::parse(&asc480), info(39, 48000, 2, 480, false));
    let asc512 = eld_config::audio_specific_config(&unhex(COOKIE_512)).unwrap();
    assert_eq!(hex(&asc512), "f8e64000");
    assert_eq!(eld_config::parse(&asc512), info(39, 48000, 2, 512, false));
    assert_eq!(hex(&eld_config::audio_specific_config(&unhex(&format!("00000000{COOKIE_480}"))).unwrap()), "f8e65000", "an esds box's body with its version and flags");
    assert_eq!(hex(&eld_config::audio_specific_config(&unhex("f8e65000")).unwrap()), "f8e65000", "a bare AudioSpecificConfig is taken as it is");
    assert_eq!(eld_config::parse(&unhex("f8f0212c00")), info(39, 16000, 1, 512, true), "Audio.swift's test cookie: ELD at 16 kHz mono, 512, LD-SBR");
    assert_eq!(eld_config::parse(&unhex("1210")), info(2, 44100, 2, 1024, false), "AAC-LC 44.1 kHz stereo");
    assert_eq!(eld_config::audio_specific_config(&[]), None, "an empty cookie");
    assert_eq!(eld_config::audio_specific_config(&unhex(COOKIE_480)[..30]), None, "a descriptor cut short");
    assert_eq!(eld_config::audio_specific_config(&unhex("0380808005000000060102")), None, "a descriptor with no DecoderSpecificInfo");
    assert_eq!(eld_config::parse(&unhex("f8")), None, "a config cut short");
    let mut r = Rng::new(7);
    for i in 0..3000 {
        let d = if i % 2 == 0 {
            let n = r.int(0, 49) as usize;
            r.bytes(n)
        } else {
            let mut c = unhex(COOKIE_480);
            let k = r.int(0, c.len() as i64 - 1) as usize;
            c[k] = r.next() as u8;
            c.truncate(r.int(0, 41) as usize);
            c
        };
        if let Some(asc) = eld_config::audio_specific_config(&d) {
            let _ = eld_config::parse(&asc);
        }
    }
}

/// The fixtures carry what the Mac's encoder really made: their formats read, and their cookies are
/// the ones above.
#[test]
fn the_fixtures_formats() {
    for (bytes, frames, priming, cookie) in [(super::decode::FIXTURE_480, 480, 240, COOKIE_480), (super::decode::FIXTURE_512, 512, 256, COOKIE_512)] {
        let records = super::decode::records(bytes);
        let Some(AudioMessage::Format(f)) = AudioMessage::parse(&records[0].1) else { panic!("the fixture's first record is its format") };
        assert_eq!((f.codec.as_deref(), f.sample_rate, f.channels), (Some("aac-eld"), Some(48000), Some(2)));
        assert_eq!((f.frames_per_packet, f.priming_frames, f.epoch, f.bitrate), (Some(frames), Some(priming), Some(1), Some(128_000)));
        assert_eq!(hex(f.cookie.as_deref().unwrap()), cookie);
        assert_eq!(f.cookie.unwrap().len(), 41);
    }
}
