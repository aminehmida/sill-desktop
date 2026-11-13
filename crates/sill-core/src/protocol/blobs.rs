//! Binary payloads: `ImageBlob` and `CursorShapeBlob` (a key, then the image bytes), and HEVC
//! parameter sets and frames.
//!
//! # Video byte format
//!
//! * Kind 0 (parameter sets): `nalLength(1) count(1)`, then per set `length(2, BE)` and its bytes
//!   (VPS, SPS, PPS as VideoToolbox gives them, each a raw NAL unit with its 2-byte NAL header and
//!   no start code).
//! * Kind 1 (frame): one access unit as **length-prefixed NAL units** (hvcC/"AVCC" style): each
//!   NAL unit is preceded by a big-endian length of `nalLength` bytes (4 in practice). It is
//!   **not** Annex B; parameter sets are not repeated inside frames. [`ParameterSets::to_annex_b`]
//!   converts a frame to Annex B (`00 00 00 01` start codes), and [`ParameterSets::annex_b`] gives
//!   VPS/SPS/PPS behind start codes to prepend before a keyframe.

/// A window's thumbnail (kind 3): `windowID(4, BE)` then a JPEG.
#[derive(Debug, Clone, PartialEq)]
pub struct Thumbnail {
    pub window_id: u32,
    pub jpeg: Vec<u8>,
}

/// An app's icon (kind 4): `length(2, BE)`, the bundle ID in UTF-8, then a PNG.
#[derive(Debug, Clone, PartialEq)]
pub struct Icon {
    pub bundle_id: String,
    pub png: Vec<u8>,
}

/// The Mac's cursor (kind 14): four big-endian `f32` (hotspot x, y, width, height in Mac points),
/// then a PNG, usually 2×.
#[derive(Debug, Clone, PartialEq)]
pub struct CursorShape {
    pub hot_x: f32,
    pub hot_y: f32,
    pub width: f32,
    pub height: f32,
    pub png: Vec<u8>,
}

pub fn thumbnail(d: &[u8]) -> Option<Thumbnail> {
    if d.len() <= 4 {
        return None;
    }
    Some(Thumbnail { window_id: u32::from_be_bytes(d[0..4].try_into().unwrap()), jpeg: d[4..].to_vec() })
}

pub fn icon(d: &[u8]) -> Option<Icon> {
    if d.len() <= 2 {
        return None;
    }
    let len = u16::from_be_bytes([d[0], d[1]]) as usize;
    if d.len() <= 2 + len {
        return None;
    }
    Some(Icon { bundle_id: String::from_utf8_lossy(&d[2..2 + len]).into_owned(), png: d[2 + len..].to_vec() })
}

pub fn cursor_shape(d: &[u8]) -> Option<CursorShape> {
    if d.len() <= 16 {
        return None;
    }
    let f = |i: usize| f32::from_bits(u32::from_be_bytes(d[i * 4..i * 4 + 4].try_into().unwrap()));
    Some(CursorShape { hot_x: f(0), hot_y: f(1), width: f(2), height: f(3), png: d[16..].to_vec() })
}

/// The Annex B start code.
pub const START_CODE: [u8; 4] = [0, 0, 0, 1];

/// HEVC VPS/SPS/PPS as kind 0 carries them (`ParameterSets` in `StreamMessage.swift`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParameterSets {
    /// Bytes of each NAL unit's length prefix inside frames (4 in practice).
    pub nal_length: usize,
    pub sets: Vec<Vec<u8>>,
}

impl ParameterSets {
    /// Parses a kind 0 payload.
    pub fn parse(d: &[u8]) -> Option<ParameterSets> {
        if d.len() < 2 {
            return None;
        }
        let nal = d[0] as usize;
        let count = d[1] as usize;
        let mut off = 2;
        let mut sets = Vec::with_capacity(count);
        for _ in 0..count {
            if d.len() < off + 2 {
                return None;
            }
            let len = u16::from_be_bytes([d[off], d[off + 1]]) as usize;
            off += 2;
            if d.len() < off + len {
                return None;
            }
            sets.push(d[off..off + len].to_vec());
            off += len;
        }
        Some(ParameterSets { nal_length: nal, sets })
    }

    /// VPS, SPS and PPS behind start codes.
    pub fn annex_b(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for s in &self.sets {
            out.extend_from_slice(&START_CODE);
            out.extend_from_slice(s);
        }
        out
    }

    fn of_type(&self, nal_type: u8) -> Option<&[u8]> {
        self.sets.iter().find(|s| s.len() > 2 && (s[0] >> 1) & 0x3F == nal_type).map(|s| s.as_slice())
    }

    /// The VPS (NAL type 32), without a start code.
    pub fn vps(&self) -> Option<&[u8]> {
        self.of_type(32)
    }

    /// The SPS (NAL type 33), without a start code.
    pub fn sps(&self) -> Option<&[u8]> {
        self.of_type(33)
    }

    /// The PPS (NAL type 34), without a start code.
    pub fn pps(&self) -> Option<&[u8]> {
        self.of_type(34)
    }

    /// The coded picture's size from the SPS, cropped by its conformance window.
    pub fn picture_size(&self) -> Option<(u32, u32)> {
        self.sps().and_then(hevc_sps_size)
    }

    /// One access unit from length-prefixed NAL units to Annex B. `None` for a frame whose
    /// lengths do not add up.
    pub fn to_annex_b(frame: &[u8], nal_length: usize) -> Option<Vec<u8>> {
        if nal_length == 0 || nal_length > 4 {
            return None;
        }
        let mut out = Vec::with_capacity(frame.len() + 64);
        let mut off = 0;
        while off + nal_length <= frame.len() {
            let mut len = 0usize;
            for i in 0..nal_length {
                len = (len << 8) | frame[off + i] as usize;
            }
            off += nal_length;
            if off + len > frame.len() {
                return None;
            }
            out.extend_from_slice(&START_CODE);
            out.extend_from_slice(&frame[off..off + len]);
            off += len;
        }
        if off == frame.len() { Some(out) } else { None }
    }

    /// The same in place, for the usual 4-byte prefix: each length becomes a start code.
    /// Returns whether the frame was well formed (it is left half-converted otherwise).
    pub fn to_annex_b_in_place(frame: &mut [u8]) -> bool {
        let mut off = 0;
        while off + 4 <= frame.len() {
            let len = u32::from_be_bytes(frame[off..off + 4].try_into().unwrap()) as usize;
            if off + 4 + len > frame.len() {
                return false;
            }
            frame[off..off + 4].copy_from_slice(&START_CODE);
            off += 4 + len;
        }
        off == frame.len()
    }
}

/// Just enough of an HEVC SPS (ITU-T H.265 §7.3.2.2) to read the picture's size.
pub fn hevc_sps_size(nal: &[u8]) -> Option<(u32, u32)> {
    // RBSP: drop the 2-byte NAL header and every emulation prevention byte (00 00 03).
    let mut rbsp = Vec::with_capacity(nal.len());
    let mut zeros = 0;
    for &b in nal.iter().skip(2) {
        if zeros >= 2 && b == 3 {
            zeros = 0;
            continue;
        }
        rbsp.push(b);
        zeros = if b == 0 { zeros + 1 } else { 0 };
    }
    let mut r = Bits { d: &rbsp, pos: 0 };
    r.skip(4)?; // sps_video_parameter_set_id
    let max_sub_layers_minus1 = r.u(3)? as usize;
    r.skip(1)?; // sps_temporal_id_nesting_flag
    // profile_tier_level(1, maxSubLayersMinus1)
    r.skip(2 + 1 + 5 + 32 + 4 + 43 + 1 + 8)?;
    let mut profile_present = vec![false; max_sub_layers_minus1];
    let mut level_present = vec![false; max_sub_layers_minus1];
    for i in 0..max_sub_layers_minus1 {
        profile_present[i] = r.u(1)? == 1;
        level_present[i] = r.u(1)? == 1;
    }
    if max_sub_layers_minus1 > 0 {
        for _ in max_sub_layers_minus1..8 {
            r.skip(2)?;
        }
    }
    for i in 0..max_sub_layers_minus1 {
        if profile_present[i] {
            r.skip(88)?;
        }
        if level_present[i] {
            r.skip(8)?;
        }
    }
    r.ue()?; // sps_seq_parameter_set_id
    let chroma = r.ue()?;
    if chroma == 3 {
        r.skip(1)?;
    }
    let mut w = r.ue()? as i64;
    let mut h = r.ue()? as i64;
    if r.u(1)? == 1 {
        let sub_w = if chroma == 1 || chroma == 2 { 2 } else { 1 };
        let sub_h = if chroma == 1 { 2 } else { 1 };
        let (left, right, top, bottom) = (r.ue()? as i64, r.ue()? as i64, r.ue()? as i64, r.ue()? as i64);
        w -= sub_w * (left + right);
        h -= sub_h * (top + bottom);
    }
    if w <= 0 || h <= 0 { None } else { Some((w as u32, h as u32)) }
}

struct Bits<'a> {
    d: &'a [u8],
    pos: usize,
}

impl Bits<'_> {
    fn u(&mut self, n: u32) -> Option<u32> {
        let mut v = 0u32;
        for _ in 0..n {
            let byte = *self.d.get(self.pos >> 3)?;
            v = (v << 1) | u32::from((byte >> (7 - (self.pos & 7))) & 1);
            self.pos += 1;
        }
        Some(v)
    }
    fn skip(&mut self, n: usize) -> Option<()> {
        self.pos += n;
        if self.pos > self.d.len() * 8 { None } else { Some(()) }
    }
    fn ue(&mut self) -> Option<u32> {
        let mut zeros = 0;
        while self.u(1)? == 0 {
            zeros += 1;
            if zeros > 31 {
                return None;
            }
        }
        Some(((1u64 << zeros) - 1) as u32 + self.u(zeros)?)
    }
}
