//! The Sill wire format, host side: the 14-byte header and the binary blobs.
//!
//! Everything here was read from `Sources/StreamProtocol/StreamMessage.swift`,
//! `Switcher.swift` (`ImageBlob`), `Viewport.swift` (`CursorShapeBlob`) and
//! `Audio.swift` (`AudioPackets`) of the Swift host, v0.6.1.

use std::time::{SystemTime, UNIX_EPOCH};

/// The header's length: kind u8, timestamp f64 BE, keyframe u8, payload length u32 BE.
pub const HEADER_LEN: usize = 14;
/// Any client → host message (`StreamMessage.maxClientPayload`): a larger one closes the session.
pub const MAX_CLIENT_PAYLOAD: usize = 1 << 20;
/// The one message of a pairing connection (`StreamMessage.maxPairingPayload`).
pub const MAX_PAIRING_PAYLOAD: usize = 4096;

/// The message kinds (`StreamMessageKind`'s raw values).
pub mod kind {
    /// Host → device: `ParameterSets.encoded()`.
    pub const PARAMETER_SETS: u8 = 0;
    /// Host → device: one HEVC access unit, 4-byte length-prefixed NAL units.
    pub const FRAME: u8 = 1;
    /// Host → device: JSON `WindowList`.
    pub const WINDOW_LIST: u8 = 2;
    /// Host → device: window id (u32 BE) + JPEG.
    pub const THUMBNAIL: u8 = 3;
    /// Host → device: bundle id length (u16 BE) + bundle id + PNG.
    pub const APP_ICON: u8 = 4;
    /// Host → device: JSON `[AppInfo]`.
    pub const APP_LIST: u8 = 5;
    /// Device → host: JSON `StreamSource`.
    pub const SELECT_SOURCE: u8 = 6;
    /// Device → host: JSON `LaunchApp`.
    pub const LAUNCH_APP: u8 = 7;
    /// Device → host: JSON `InputEvent`.
    pub const INPUT: u8 = 8;
    /// Device → host: JSON `Viewport`.
    pub const VIEWPORT: u8 = 9;
    /// Device → host: any payload, echoed back unchanged as a pong.
    pub const PING: u8 = 10;
    /// Host → device: the ping's payload and time stamp.
    pub const PONG: u8 = 11;
    /// Device → host: JSON `ClientStats`.
    pub const CLIENT_STATS: u8 = 12;
    /// Host → device: empty, every 30 ms while a session is live and nothing else went.
    pub const TICK: u8 = 13;
    /// Host → device: `CursorShapeBlob`.
    pub const CURSOR_SHAPE: u8 = 14;
    /// Device → host: JSON `WindowCommand`.
    pub const WINDOW_COMMAND: u8 = 15;
    /// Host → device: JSON `HostSettingsState`.
    pub const HOST_SETTINGS: u8 = 16;
    /// Device → host: JSON `HostSettingsChange`.
    pub const CHANGE_SETTINGS: u8 = 17;
    /// Host → device: JSON `SignedMacInfo`.
    pub const MAC_INFO: u8 = 18;
    /// Device → host: JSON `PairRequest`, the one message of a pairing connection.
    pub const PAIR_REQUEST: u8 = 19;
    /// Host → device: JSON `PairResult`.
    pub const PAIR_RESULT: u8 = 20;
    /// Device → host: empty, "show your pairing code".
    pub const PAIRING_WANTED: u8 = 21;
    /// Host → device: JSON `Goodbye`.
    pub const GOODBYE: u8 = 22;
    /// Device → host: JSON `Hello`.
    pub const HELLO: u8 = 23;
    /// Host → device: JSON `MacMenu`.
    pub const MAC_MENU: u8 = 24;
    /// Device → host: JSON `PressMenuItem`.
    pub const PRESS_MENU_ITEM: u8 = 25;
    /// Host → device: JSON `MacPointer`.
    pub const MAC_POINTER: u8 = 26;
    /// Device → host: JSON `FetchMenu`.
    pub const FETCH_MENU: u8 = 27;
    /// Device → host: JSON `TrackpadGesture`.
    pub const GESTURE: u8 = 28;
    /// Both ways: JSON `Clipboard`.
    pub const CLIPBOARD: u8 = 32;
    /// Host → device: `AudioMessage` (type byte 1: JSON format; 2: packets).
    pub const AUDIO: u8 = 43;
    /// Device → host: empty, "my picture waits for a keyframe".
    pub const KEYFRAME_REQUEST: u8 = 44;
}

/// A parsed 14-byte header.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Header {
    /// The raw kind byte.
    pub kind: u8,
    /// Seconds since 1970 on the sender's clock.
    pub timestamp: f64,
    /// True only when the byte is exactly 1, as `StreamMessage.parseHeader` reads it.
    pub keyframe: bool,
    /// The payload's length.
    pub len: usize,
}

/// Parses a header.
pub fn parse_header(b: &[u8; HEADER_LEN]) -> Header {
    Header {
        kind: b[0],
        timestamp: f64::from_bits(u64::from_be_bytes(b[1..9].try_into().unwrap())),
        keyframe: b[9] == 1,
        len: u32::from_be_bytes(b[10..14].try_into().unwrap()) as usize,
    }
}

/// One whole message: header and payload (`StreamMessage.serialized()`).
pub fn message(kind: u8, timestamp: f64, keyframe: bool, payload: &[u8]) -> Vec<u8> {
    let mut d = Vec::with_capacity(HEADER_LEN + payload.len());
    d.push(kind);
    d.extend_from_slice(&timestamp.to_bits().to_be_bytes());
    d.push(u8::from(keyframe));
    d.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    d.extend_from_slice(payload);
    d
}

/// The wall clock, seconds since 1970, as every header's time stamp.
pub fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// `ParameterSets.encoded()`: NAL length size (1 byte, 4), count (1 byte), then per set a
/// u16 BE length and the bytes (VPS, SPS, PPS in that order, as VideoToolbox lists them).
pub fn parameter_sets(sets: &[Vec<u8>]) -> Vec<u8> {
    let mut d = vec![4u8, sets.len() as u8];
    for s in sets {
        d.extend_from_slice(&(s.len() as u16).to_be_bytes());
        d.extend_from_slice(s);
    }
    d
}

/// `ImageBlob.encodeThumbnail`: window id (u32 BE) + JPEG.
pub fn thumbnail(window_id: u32, jpeg: &[u8]) -> Vec<u8> {
    let mut d = Vec::with_capacity(4 + jpeg.len());
    d.extend_from_slice(&window_id.to_be_bytes());
    d.extend_from_slice(jpeg);
    d
}

/// `ImageBlob.encodeIcon`: bundle id length (u16 BE) + bundle id (UTF-8) + PNG.
pub fn icon(bundle_id: &str, png: &[u8]) -> Vec<u8> {
    let mut d = Vec::with_capacity(2 + bundle_id.len() + png.len());
    d.extend_from_slice(&(bundle_id.len() as u16).to_be_bytes());
    d.extend_from_slice(bundle_id.as_bytes());
    d.extend_from_slice(png);
    d
}

/// `CursorShapeBlob.encode`: hotspot x, y, width, height as f32 BE (Mac points), then PNG.
pub fn cursor_shape(hot: (f32, f32), size: (f32, f32), png: &[u8]) -> Vec<u8> {
    let mut d = Vec::with_capacity(16 + png.len());
    for v in [hot.0, hot.1, size.0, size.1] {
        d.extend_from_slice(&v.to_bits().to_be_bytes());
    }
    d.extend_from_slice(png);
    d
}

/// A kind 43 payload of type 1: the type byte, then the JSON `AudioFormat`.
pub fn audio_format(json: &[u8]) -> Vec<u8> {
    let mut d = Vec::with_capacity(1 + json.len());
    d.push(1);
    d.extend_from_slice(json);
    d
}

/// A kind 43 payload of type 2 (`AudioPackets.serialized()`): type 2, epoch u16 BE, seq
/// u32 BE, flags u8 (bit 0: the first packet starts a segment), count u8, then per packet
/// a u16 BE length and the bytes.
pub fn audio_packets(epoch: u16, seq: u32, segment_start: bool, packets: &[&[u8]]) -> Vec<u8> {
    let mut d = vec![2u8];
    d.extend_from_slice(&epoch.to_be_bytes());
    d.extend_from_slice(&seq.to_be_bytes());
    d.push(u8::from(segment_start));
    d.push(packets.len() as u8);
    for p in packets {
        d.extend_from_slice(&(p.len() as u16).to_be_bytes());
        d.extend_from_slice(p);
    }
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_round_trip() {
        let m = message(kind::FRAME, 1234.5, true, b"abc");
        assert_eq!(m.len(), HEADER_LEN + 3);
        let h = parse_header(m[..HEADER_LEN].try_into().unwrap());
        assert_eq!(h, Header { kind: 1, timestamp: 1234.5, keyframe: true, len: 3 });
    }

    #[test]
    fn blobs() {
        assert_eq!(parameter_sets(&[vec![1, 2], vec![3]]), [4, 2, 0, 2, 1, 2, 0, 1, 3]);
        assert_eq!(thumbnail(0x0102_0304, b"J"), [1, 2, 3, 4, b'J']);
        assert_eq!(icon("ab", b"P"), [0, 2, b'a', b'b', b'P']);
        assert_eq!(audio_packets(1, 2, true, &[b"xy"]), [2, 0, 1, 0, 0, 0, 2, 1, 1, 0, 2, b'x', b'y']);
    }
}
