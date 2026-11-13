//! The format's cookie, as a decoder takes it. AudioToolbox's kAudioConverterCompressionMagicCookie
//! for AAC-ELD, which the Mac sends as is, is an MPEG-4 ES_Descriptor (an `esds` box's body, 41 bytes):
//!
//! ```text
//! 03 len  ES_ID(2) flags(1)  04 len  objectType(1) streamType(1) bufferSize(3) maxBitrate(4) avgBitrate(4)
//!                                    05 len  AudioSpecificConfig   06 len  SLConfig
//! ```
//!
//! FFmpeg's AAC decoder takes the bare AudioSpecificConfig as its extradata (4 bytes for ELD: AOT 39,
//! 48 kHz, stereo, its frame length, no SBR). A cookie that is already a bare AudioSpecificConfig is
//! taken as it is. Pure; never panics.

/// AAC-ELD's audio object type (ER AAC ELD).
pub const AOT_ELD: u32 = 39;
const RATES: [u32; 13] = [96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350];

/// What an AudioSpecificConfig says. `frame_length` is 480 or 512 for ELD (its frameLengthFlag),
/// else 1024.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Info {
    pub object_type: u32,
    pub sample_rate: u32,
    pub channels: u32,
    pub frame_length: u32,
    pub sbr: bool,
}

fn is_descriptor(b: u8) -> bool {
    matches!(b, 0x03..=0x05)
}

/// The AudioSpecificConfig in a cookie: the DecoderSpecificInfo of an ES_Descriptor, or the cookie
/// itself.
pub fn audio_specific_config(cookie: &[u8]) -> Option<Vec<u8>> {
    if cookie.is_empty() {
        return None;
    }
    // An `esds` box's body may keep its version and flags (4 zero bytes) before the descriptor.
    let from = if cookie.len() > 5 && cookie[..4] == [0, 0, 0, 0] && is_descriptor(cookie[4]) { 4 } else { 0 };
    if is_descriptor(cookie[from]) {
        find(cookie, from, cookie.len(), 0).map(|d| d.to_vec())
    } else {
        Some(cookie.to_vec())
    }
}

/// The AudioSpecificConfig's fields, or None for one cut short or of a form this does not read.
pub fn parse(asc: &[u8]) -> Option<Info> {
    let mut r = Bits { d: asc, bit: 0 };
    let mut aot = r.read(5)?;
    if aot == 31 {
        aot = 32 + r.read(6)?;
    }
    let index = r.read(4)?;
    let rate = if index == 0xF { r.read(24)? } else { *RATES.get(index as usize)? };
    let channels = r.read(4)?;
    if aot != AOT_ELD {
        return Some(Info { object_type: aot, sample_rate: rate, channels, frame_length: 1024, sbr: aot == 5 || aot == 29 });
    }
    // ELDSpecificConfig: frameLengthFlag, three resilience flags, ldSbrPresentFlag.
    let frame_length_flag = r.read(1)?;
    r.read(3)?;
    let sbr = r.read(1)?;
    Some(Info { object_type: aot, sample_rate: rate, channels, frame_length: if frame_length_flag == 1 { 480 } else { 512 }, sbr: sbr == 1 })
}

/// The DecoderSpecificInfo (tag 5) among the descriptors in [from, end).
fn find(d: &[u8], from: usize, end: usize, depth: u32) -> Option<&[u8]> {
    if depth > 4 {
        return None;
    }
    let mut i = from;
    while i < end {
        let tag = d[i];
        i += 1;
        // The size: up to four bytes of seven bits, the top bit saying another follows.
        let mut length = 0usize;
        let mut n = 0;
        loop {
            if i >= end || n == 4 {
                return None;
            }
            let b = d[i];
            i += 1;
            n += 1;
            length = length << 7 | (b & 0x7F) as usize;
            if b & 0x80 == 0 {
                break;
            }
        }
        if length > end - i {
            return None;
        }
        let body = i;
        let body_end = i + length;
        match tag {
            0x05 => return Some(&d[body..body_end]),
            0x03 => {
                // ES_Descriptor: ES_ID, then flags, and what they say follows, then its descriptors.
                if length < 3 {
                    return None;
                }
                let mut j = body + 2;
                let flags = d[j];
                j += 1;
                if flags & 0x80 != 0 {
                    j += 2;
                }
                if flags & 0x40 != 0 {
                    if j >= body_end {
                        return None;
                    }
                    j += 1 + d[j] as usize;
                }
                if flags & 0x20 != 0 {
                    j += 2;
                }
                if j > body_end {
                    return None;
                }
                if let Some(found) = find(d, j, body_end, depth + 1) {
                    return Some(found);
                }
            }
            // DecoderConfigDescriptor: 13 bytes of its own, then its descriptors.
            0x04 if length >= 13 => {
                if let Some(found) = find(d, body + 13, body_end, depth + 1) {
                    return Some(found);
                }
            }
            _ => {}
        }
        i = body_end;
    }
    None
}

struct Bits<'a> {
    d: &'a [u8],
    bit: usize,
}

impl Bits<'_> {
    fn read(&mut self, n: usize) -> Option<u32> {
        if self.bit + n > self.d.len() * 8 {
            return None;
        }
        let mut v = 0u32;
        for _ in 0..n {
            let b = (self.d[self.bit >> 3] >> (7 - (self.bit & 7))) & 1;
            v = v << 1 | b as u32;
            self.bit += 1;
        }
        Some(v)
    }
}
