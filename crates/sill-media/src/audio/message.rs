//! The Mac's sound on the wire (kind 43; StreamProtocol's Audio.swift). A kind 43 payload's first
//! byte says what follows: type 1, how to decode what follows (JSON, [`AudioFormat`]); type 2, codec
//! packets of one epoch (binary, [`AudioPackets`]). A reader skips a type it does not know.
//!
//! Pure and total: nothing here panics, whatever the bytes.

use base64::Engine;
use serde_json::Value;

/// The codecs by name, as the hello and the format carry them (Audio.swift's AudioCodec).
pub mod codec {
    /// AAC-ELD: 48 kHz stereo, 480 or 512 frames a packet (10 or 10.7 ms), 128 kbps.
    pub const AAC_ELD: &str = "aac-eld";
}

/// Host → device (kind 43, type 1): how to decode what follows. Sent before a device's first packet
/// of an epoch, and again for every new epoch. Every field optional, so `{}` decodes. The numbers
/// are Swift's `Int` (64 bits).
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct AudioFormat {
    /// "aac-eld". A device that does not play it ignores the sound.
    pub codec: Option<String>,
    /// 48000.
    pub sample_rate: Option<i64>,
    /// 2.
    pub channels: Option<i64>,
    /// 480 or 512: the frames each packet decodes to.
    pub frames_per_packet: Option<i64>,
    /// 240 or 256: the decoded frames a new segment starts with that are the codec's own (dropped).
    pub priming_frames: Option<i64>,
    /// The encoder's target in bits per second (display only).
    pub bitrate: Option<i64>,
    /// The epoch of the packets this format belongs to (`AudioPackets::epoch`, 0 to 65535).
    pub epoch: Option<i64>,
    /// The codec's magic cookie, base64 in the JSON. Audio.swift calls it AAC-ELD's
    /// AudioSpecificConfig; what AudioToolbox hands the Mac is an MPEG-4 ES_Descriptor wrapping it
    /// (`eld_config::audio_specific_config` unwraps it).
    pub cookie: Option<Vec<u8>>,
    /// What the sound is of, display only: "app" (then `app` names it), "desktop" or "test".
    pub source: Option<String>,
    /// "Safari": the app whose sound this is.
    pub app: Option<String>,
}

struct Mismatch;

/// 2^63: the first number past Swift's Int.
const TWO_TO_63: f64 = 9_223_372_036_854_775_808.0;

impl AudioFormat {
    pub const SOURCE_APP: &'static str = "app";
    pub const SOURCE_DESKTOP: &'static str = "desktop";
    pub const SOURCE_TEST: &'static str = "test";

    /// The JSON as Swift's JSONDecoder reads it: unknown keys skipped, null as absent, a number that
    /// is not a whole 64-bit integer or a value of the wrong type fails the whole format (None), and
    /// the cookie strict base64 (padded, nothing outside the standard alphabet).
    pub fn parse_json(bytes: &[u8]) -> Option<AudioFormat> {
        let v: Value = serde_json::from_slice(bytes).ok()?;
        let o = v.as_object()?;
        let get = |k: &str| o.get(k).filter(|v| !v.is_null());
        let int = |k: &str| -> Result<Option<i64>, Mismatch> {
            let Some(v) = get(k) else { return Ok(None) };
            let Value::Number(n) = v else { return Err(Mismatch) };
            if let Some(i) = n.as_i64() {
                return Ok(Some(i));
            }
            if n.is_u64() {
                return Err(Mismatch); // past Int.max
            }
            // 1.0 and 1e3 are whole; 1.5 is not. Past ±2^63 Swift refuses it too.
            let d = n.as_f64().ok_or(Mismatch)?;
            if !d.is_finite() || d != d.floor() || d >= TWO_TO_63 || d < -TWO_TO_63 {
                return Err(Mismatch);
            }
            Ok(Some(d as i64))
        };
        let string = |k: &str| -> Result<Option<String>, Mismatch> {
            match get(k) {
                None => Ok(None),
                Some(Value::String(s)) => Ok(Some(s.clone())),
                Some(_) => Err(Mismatch),
            }
        };
        let parse = || -> Result<AudioFormat, Mismatch> {
            Ok(AudioFormat {
                codec: string("codec")?,
                sample_rate: int("sampleRate")?,
                channels: int("channels")?,
                frames_per_packet: int("framesPerPacket")?,
                priming_frames: int("primingFrames")?,
                bitrate: int("bitrate")?,
                epoch: int("epoch")?,
                cookie: match string("cookie")? {
                    None => None,
                    // Foundation's Data(base64Encoded:): padding required, no whitespace, the
                    // standard alphabet.
                    Some(s) => Some(strict_base64().decode(s.as_bytes()).map_err(|_| Mismatch)?),
                },
                source: string("source")?,
                app: string("app")?,
            })
        };
        parse().ok()
    }

    /// The JSON a host writes (the fields that are set, the cookie in base64).
    pub fn to_json(&self) -> Vec<u8> {
        let mut o = serde_json::Map::new();
        let mut put = |k: &str, v: Option<Value>| {
            if let Some(v) = v {
                o.insert(k.to_string(), v);
            }
        };
        put("codec", self.codec.clone().map(Value::from));
        put("sampleRate", self.sample_rate.map(Value::from));
        put("channels", self.channels.map(Value::from));
        put("framesPerPacket", self.frames_per_packet.map(Value::from));
        put("primingFrames", self.priming_frames.map(Value::from));
        put("bitrate", self.bitrate.map(Value::from));
        put("epoch", self.epoch.map(Value::from));
        put("cookie", self.cookie.as_ref().map(|c| Value::from(base64::engine::general_purpose::STANDARD.encode(c))));
        put("source", self.source.clone().map(Value::from));
        put("app", self.app.clone().map(Value::from));
        serde_json::to_vec(&Value::Object(o)).unwrap_or_default()
    }
}

/// Padded, the standard alphabet, nothing else; trailing bits are not checked (Foundation's are not).
fn strict_base64() -> base64::engine::GeneralPurpose {
    use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
    GeneralPurpose::new(
        &base64::alphabet::STANDARD,
        GeneralPurposeConfig::new()
            .with_decode_padding_mode(DecodePaddingMode::RequireCanonical)
            .with_decode_allow_trailing_bits(true),
    )
}

/// Host → device (kind 43, type 2): one or more codec packets of one epoch, back to back. The
/// message header's time stamp is the host's wall clock of the first decoded sample of its first
/// packet; the packets after it follow `framesPerPacket / sampleRate` apart. Big endian:
///
/// ```text
/// type 2 · epoch UInt16 · seq UInt32 · flags UInt8 · count UInt8 · count × (length UInt16 · bytes)
/// ```
///
/// flags bit 0: the first packet is the first of a segment (reset the decoder, drop the format's
/// `primingFrames`); bits 1 to 7 are ignored.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct AudioPackets {
    /// 0 to 65535, wrapping.
    pub epoch: u16,
    /// The first packet's number in its epoch, from 0.
    pub seq: u32,
    /// Flags bit 0.
    pub segment_start: bool,
    /// 1 to 255 packets of at most 65,535 bytes each.
    pub packets: Vec<Vec<u8>>,
}

impl AudioPackets {
    pub const MAX_COUNT: usize = 255;
    pub const MAX_PACKET_BYTES: usize = 65_535;
    /// Before the packets: the type, epoch, seq, flags and count.
    pub const HEADER_BYTES: usize = 9;

    /// The payload, its type byte included; at most 255 packets, of at most 65,535 bytes each.
    pub fn serialized(&self) -> Vec<u8> {
        let kept = &self.packets[..self.packets.len().min(Self::MAX_COUNT)];
        let mut out = Vec::with_capacity(Self::HEADER_BYTES + kept.iter().map(|p| 2 + p.len().min(Self::MAX_PACKET_BYTES)).sum::<usize>());
        out.push(AudioMessage::TYPE_PACKETS);
        out.extend_from_slice(&self.epoch.to_be_bytes());
        out.extend_from_slice(&self.seq.to_be_bytes());
        out.push(self.segment_start as u8);
        out.push(kept.len() as u8);
        for p in kept {
            let n = p.len().min(Self::MAX_PACKET_BYTES);
            out.extend_from_slice(&(n as u16).to_be_bytes());
            out.extend_from_slice(&p[..n]);
        }
        out
    }
}

/// A kind 43 payload, either type.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum AudioMessage {
    Format(AudioFormat),
    Packets(AudioPackets),
}

impl AudioMessage {
    pub const TYPE_FORMAT: u8 = 1;
    pub const TYPE_PACKETS: u8 = 2;
    /// The largest payload of each type a reader takes; a device drops a message past them whole.
    pub const MAX_FORMAT_PAYLOAD: usize = 4096;
    pub const MAX_PACKETS_PAYLOAD: usize = 16384;

    /// The payload, its type byte first.
    pub fn serialized(&self) -> Vec<u8> {
        match self {
            AudioMessage::Format(f) => {
                let mut d = vec![Self::TYPE_FORMAT];
                d.extend_from_slice(&f.to_json());
                d
            }
            AudioMessage::Packets(p) => p.serialized(),
        }
    }

    /// None for an empty payload, a type this build does not know, a format over 4 KB or one that
    /// does not decode, and for packets over 16 KB, a header cut short, a count of 0, a length past
    /// the end or bytes left over. Never panics, whatever the input.
    pub fn parse(payload: &[u8]) -> Option<AudioMessage> {
        match *payload.first()? {
            Self::TYPE_FORMAT => {
                if payload.len() > Self::MAX_FORMAT_PAYLOAD {
                    return None;
                }
                AudioFormat::parse_json(&payload[1..]).map(AudioMessage::Format)
            }
            Self::TYPE_PACKETS => {
                if payload.len() > Self::MAX_PACKETS_PAYLOAD || payload.len() < AudioPackets::HEADER_BYTES {
                    return None;
                }
                let epoch = u16::from_be_bytes([payload[1], payload[2]]);
                let seq = u32::from_be_bytes([payload[3], payload[4], payload[5], payload[6]]);
                let flags = payload[7];
                let count = payload[8] as usize;
                if count < 1 {
                    return None;
                }
                let mut packets = Vec::with_capacity(count);
                let mut offset = AudioPackets::HEADER_BYTES;
                for _ in 0..count {
                    let head = payload.get(offset..offset + 2)?;
                    let length = u16::from_be_bytes([head[0], head[1]]) as usize;
                    offset += 2;
                    packets.push(payload.get(offset..offset + length)?.to_vec());
                    offset += length;
                }
                if offset != payload.len() {
                    return None;
                }
                Some(AudioMessage::Packets(AudioPackets { epoch, seq, segment_start: flags & 1 == 1, packets }))
            }
            _ => None,
        }
    }
}
