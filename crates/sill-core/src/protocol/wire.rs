//! The wire format of `StreamMessage.swift`, which the Mac and every device share: a 14-byte
//! big-endian header, `kind(1) timestamp(8, f64) isKeyframe(1) payloadLength(4)`, then the payload.

use std::io;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncRead, AsyncReadExt};

/// Message kinds (the header's first byte).
pub mod kind {
    /// Host → device: HEVC parameter sets ([`crate::protocol::ParameterSets`]).
    pub const PARAMETER_SETS: u8 = 0;
    /// Host → device: one HEVC access unit, length-prefixed NAL units.
    pub const FRAME: u8 = 1;
    pub const WINDOW_LIST: u8 = 2;
    pub const THUMBNAIL: u8 = 3;
    pub const APP_ICON: u8 = 4;
    pub const APP_LIST: u8 = 5;
    /// Device → host.
    pub const SELECT_SOURCE: u8 = 6;
    pub const LAUNCH_APP: u8 = 7;
    pub const INPUT: u8 = 8;
    pub const VIEWPORT: u8 = 9;
    pub const PING: u8 = 10;
    pub const PONG: u8 = 11;
    pub const CLIENT_STATS: u8 = 12;
    pub const TICK: u8 = 13;
    pub const CURSOR_SHAPE: u8 = 14;
    pub const WINDOW_COMMAND: u8 = 15;
    pub const HOST_SETTINGS: u8 = 16;
    pub const CHANGE_SETTINGS: u8 = 17;
    /// Remote access and pairing.
    pub const MAC_INFO: u8 = 18;
    pub const PAIR_REQUEST: u8 = 19;
    pub const PAIR_RESULT: u8 = 20;
    pub const PAIRING_WANTED: u8 = 21;
    pub const GOODBYE: u8 = 22;
    /// Device → host: the first message of every session connection.
    pub const HELLO: u8 = 23;
    pub const MAC_MENU: u8 = 24;
    pub const PRESS_MENU_ITEM: u8 = 25;
    pub const MAC_POINTER: u8 = 26;
    pub const FETCH_MENU: u8 = 27;
    /// Trackpad gestures.
    pub const GESTURE: u8 = 28;
    /// The clipboard, both ways: only to a host whose window list says `clipboard` ≥ 1.
    pub const CLIPBOARD: u8 = 32;
    /// Host → device: the Mac's sound, only to a device whose hello lists a codec.
    pub const AUDIO: u8 = 43;
    /// Device → host, empty payload: this device's picture waits for a keyframe.
    pub const KEYFRAME_REQUEST: u8 = 44;
}

/// The header's length in bytes.
pub const HEADER_LENGTH: usize = 14;
/// Any device → host message (`StreamMessage.maxClientPayload`).
pub const MAX_CLIENT_PAYLOAD: usize = 1 << 20;
/// A video frame, as a device accepts it.
pub const MAX_FRAME_PAYLOAD: usize = 32 << 20;
/// Any other host → device message.
pub const MAX_OTHER_HOST_PAYLOAD: usize = 4 << 20;
/// Kind 20, the one message of a pairing connection's answer.
pub const MAX_PAIRING_PAYLOAD: usize = 4096;

/// One message off the wire.
#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    pub kind: u8,
    /// Seconds since 1970 on the sender's clock.
    pub timestamp: f64,
    pub is_keyframe: bool,
    pub payload: Vec<u8>,
}

/// Seconds since 1970, as the Mac's timestamps are.
pub fn now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

/// A whole message: header and payload.
pub fn encode_at(kind: u8, payload: &[u8], keyframe: bool, timestamp: f64) -> Vec<u8> {
    let mut b = Vec::with_capacity(HEADER_LENGTH + payload.len());
    b.push(kind);
    b.extend_from_slice(&timestamp.to_bits().to_be_bytes());
    b.push(u8::from(keyframe));
    b.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    b.extend_from_slice(payload);
    b
}

/// A message stamped with this device's wall clock.
pub fn encode(kind: u8, payload: &[u8]) -> Vec<u8> {
    encode_at(kind, payload, false, now())
}

/// A message whose payload is UTF-8 JSON.
pub fn encode_json(kind: u8, json: &str) -> Vec<u8> {
    encode(kind, json.as_bytes())
}

/// A parsed header.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Header {
    pub kind: u8,
    pub timestamp: f64,
    pub is_keyframe: bool,
    pub length: u32,
}

/// Parses the 14 header bytes.
pub fn parse_header(h: &[u8; HEADER_LENGTH]) -> Header {
    Header {
        kind: h[0],
        timestamp: f64::from_bits(u64::from_be_bytes(h[1..9].try_into().unwrap())),
        is_keyframe: h[9] == 1,
        length: u32::from_be_bytes(h[10..14].try_into().unwrap()),
    }
}

/// The most a host may announce for `kind`, as a device accepts it.
pub fn host_payload_cap(kind: u8) -> usize {
    if kind == kind::FRAME { MAX_FRAME_PAYLOAD } else { MAX_OTHER_HOST_PAYLOAD }
}

/// Why [`MessageReader::next`] ended.
#[derive(Debug)]
pub enum ReadError {
    /// A clean end: the stream closed between messages.
    Eof,
    /// The stream failed, ended mid-message, or announced more than the caps allow.
    Io(io::Error),
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReadError::Eof => write!(f, "end of stream"),
            ReadError::Io(e) => write!(f, "{e}"),
        }
    }
}

const PIECE: usize = 256 * 1024;

/// Reads one message at a time from a session's stream: the header, then the payload in pieces of
/// at most 256 KB, each piece a sign of life (a 1.6 MB keyframe on a 2 Mbit/s link takes seconds,
/// and liveness counts bytes, not whole messages). A header announcing more than the caps allow
/// ends the session: the Mac never sends that, so it is not the Mac.
pub struct MessageReader<R, F: FnMut()> {
    input: R,
    on_bytes: F,
}

impl<R: AsyncRead + Unpin, F: FnMut()> MessageReader<R, F> {
    pub fn new(input: R, on_bytes: F) -> Self {
        Self { input, on_bytes }
    }

    /// The next message.
    pub async fn next(&mut self) -> Result<Message, ReadError> {
        let mut header = [0u8; HEADER_LENGTH];
        self.read_fully(&mut header, true).await?;
        let h = parse_header(&header);
        let length = h.length as usize;
        if length > host_payload_cap(h.kind) {
            return Err(ReadError::Io(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("closing: the host announced {length} bytes for kind {}", h.kind),
            )));
        }
        let mut payload = vec![0u8; length];
        let mut off = 0;
        while off < length {
            let n = PIECE.min(length - off);
            self.read_fully_range(&mut payload, off, n).await?;
            off += n;
        }
        Ok(Message { kind: h.kind, timestamp: h.timestamp, is_keyframe: h.is_keyframe, payload })
    }

    async fn read_fully(&mut self, buf: &mut [u8], at_start: bool) -> Result<(), ReadError> {
        let mut off = 0;
        while off < buf.len() {
            let n = self.input.read(&mut buf[off..]).await.map_err(ReadError::Io)?;
            if n == 0 {
                if at_start && off == 0 {
                    return Err(ReadError::Eof);
                }
                return Err(ReadError::Io(io::Error::new(io::ErrorKind::UnexpectedEof, "the connection ended mid-message")));
            }
            off += n;
            (self.on_bytes)();
        }
        Ok(())
    }

    async fn read_fully_range(&mut self, buf: &mut [u8], offset: usize, len: usize) -> Result<(), ReadError> {
        let mut off = offset;
        let end = offset + len;
        while off < end {
            let n = self.input.read(&mut buf[off..end]).await.map_err(ReadError::Io)?;
            if n == 0 {
                return Err(ReadError::Io(io::Error::new(io::ErrorKind::UnexpectedEof, "the connection ended mid-message")));
            }
            off += n;
            (self.on_bytes)();
        }
        Ok(())
    }
}
