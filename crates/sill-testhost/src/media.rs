//! The synthetic host's pictures and sound: the HEVC clip, tiny PNGs made by hand, the
//! embedded JPEG thumbnails and the AAC-ELD tone.
//!
//! The frame format was read from `Sources/SillHost/HEVCEncoder.swift` (`handle`,
//! `parameterSets(from:)`) and `StreamCoordinator.startPipeline`: a frame's payload is the
//! bytes of VideoToolbox's sample buffer as they are (each NAL unit behind a 4-byte
//! big-endian length, hvcC style), and the VPS, SPS and PPS travel apart, from the
//! sample's format description, as kind 0 before every keyframe.
//!
//! The clip (`assets/clip.h265`) was made by VideoToolbox itself, through ffmpeg:
//! `ffmpeg -f lavfi -i testsrc=size=1280x720:rate=60 -t 2 -pix_fmt yuv420p
//! -c:v hevc_videotoolbox -profile:v main -g 30 -bf 0 -b:v 500k -realtime 0 -f hevc`:
//! Main profile, 8-bit 4:2:0, 120 frames, an IDR every 30, no B-frames. ffmpeg writes
//! it as Annex B with the parameter sets in band before each IDR; they are taken out
//! here, as VideoToolbox keeps them out of a sample's data. VideoToolbox also puts one
//! prefix SEI NAL in each keyframe's sample; it is kept in the frame, as the real host
//! sends whatever the sample holds.

/// One access unit, as a kind 1 payload.
pub struct AccessUnit {
    /// 4-byte length-prefixed NAL units: VCL NALs, and any SEI VideoToolbox put with them.
    pub data: Vec<u8>,
    /// An IRAP picture (NAL types 16 to 23): the header's keyframe flag.
    pub keyframe: bool,
}

/// The clip, parsed once at start.
pub struct Clip {
    /// `ParameterSets.encoded()` of the clip's VPS, SPS and PPS.
    pub parameter_sets: Vec<u8>,
    /// The frames in order; the first is a keyframe.
    pub units: Vec<AccessUnit>,
    /// Encoded pixels.
    pub width: u32,
    pub height: u32,
}

static CLIP: &[u8] = include_bytes!("../assets/clip.h265");

/// Splits an Annex B stream into its NAL units (start codes of 3 or 4 bytes).
pub fn annex_b_nals(data: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut nals = Vec::with_capacity(starts.len());
    for (n, &s) in starts.iter().enumerate() {
        let mut e = if n + 1 < starts.len() { starts[n + 1] - 3 } else { data.len() };
        // A 4-byte start code's leading zero, and any trailing_zero_8bits.
        while e > s && data[e - 1] == 0 {
            e -= 1;
        }
        if e > s {
            nals.push(&data[s..e]);
        }
    }
    nals
}

/// Parses an Annex B HEVC stream into parameter sets and access units.
pub fn parse_clip(data: &[u8], width: u32, height: u32) -> Clip {
    let (mut vps, mut sps, mut pps) = (None, None, None);
    let mut units: Vec<AccessUnit> = Vec::new();
    let mut pending: Vec<u8> = Vec::new(); // prefix SEI waiting for its picture
    let mut open = false;
    for nal in annex_b_nals(data) {
        if nal.len() < 3 {
            continue;
        }
        let kind = (nal[0] >> 1) & 0x3F;
        let push = |into: &mut Vec<u8>| {
            into.extend_from_slice(&(nal.len() as u32).to_be_bytes());
            into.extend_from_slice(nal);
        };
        match kind {
            32 => vps = vps.or(Some(nal.to_vec())),
            33 => sps = sps.or(Some(nal.to_vec())),
            34 => pps = pps.or(Some(nal.to_vec())),
            // AUD, end of sequence, end of bitstream, filler: never in a VideoToolbox sample.
            35..=38 => {}
            // Prefix SEI: with the picture that follows it.
            39 => {
                open = false;
                push(&mut pending);
            }
            0..=31 => {
                let first_slice = nal[2] & 0x80 != 0;
                if first_slice || !open {
                    let mut d = std::mem::take(&mut pending);
                    push(&mut d);
                    units.push(AccessUnit { data: d, keyframe: (16..=23).contains(&kind) });
                    open = true;
                } else if let Some(u) = units.last_mut() {
                    push(&mut u.data);
                }
            }
            // Suffix SEI and anything else: with the current picture.
            _ => {
                if let Some(u) = units.last_mut().filter(|_| open) {
                    push(&mut u.data);
                }
            }
        }
    }
    let sets: Vec<Vec<u8>> = [vps, sps, pps].into_iter().flatten().collect();
    Clip { parameter_sets: crate::wire::parameter_sets(&sets), units, width, height }
}

/// The embedded clip.
pub fn clip() -> Clip {
    parse_clip(CLIP, 1280, 720)
}

/// Small baseline JPEGs (320×200, made with ffmpeg's mjpeg encoder), one per fake window.
pub static THUMBNAILS: [&[u8]; 4] = [
    include_bytes!("../assets/thumb1.jpg"),
    include_bytes!("../assets/thumb2.jpg"),
    include_bytes!("../assets/thumb3.jpg"),
    include_bytes!("../assets/thumb4.jpg"),
];

/// The AAC-ELD tone: real packets from AudioToolbox's encoder (through ffmpeg's `aac_at`,
/// profile ELD, 48 kHz stereo, 128 kbps, 512 frames a packet), 75 packets from the
/// encoder's steady state: 0.8 s, 352 whole cycles of 440 Hz, so the loop has no click.
pub struct Tone {
    /// The codec's magic cookie as AudioToolbox's encoder gives it for this format: an
    /// MPEG-4 ES descriptor around the AudioSpecificConfig `f8 e6 40 00` (measured on
    /// macOS with `AudioConverterGetProperty(kAudioConverterCompressionMagicCookie)`).
    pub cookie: &'static [u8],
    pub packets: Vec<&'static [u8]>,
}

/// Frames each packet decodes to.
pub const TONE_FRAMES_PER_PACKET: u32 = 512;
/// The codec's own frames at a segment's start.
pub const TONE_PRIMING_FRAMES: u32 = 256;
/// The sample rate.
pub const TONE_SAMPLE_RATE: u32 = 48_000;

static TONE: &[u8] = include_bytes!("../assets/tone.eld");
static TONE_COOKIE: [u8; 41] = [
    0x03, 0x80, 0x80, 0x80, 0x24, 0x00, 0x00, 0x00, 0x04, 0x80, 0x80, 0x80, 0x16, 0x40, 0x14, 0x00, 0x18, 0x00, 0x00,
    0x01, 0xf4, 0x00, 0x00, 0x01, 0xf4, 0x00, 0x05, 0x80, 0x80, 0x80, 0x04, 0xf8, 0xe6, 0x40, 0x00, 0x06, 0x80, 0x80,
    0x80, 0x01, 0x02,
];

/// The embedded tone. The asset: AudioSpecificConfig length (u8), the config, a packet
/// count (u16 BE), then per packet a u16 BE length and its bytes.
pub fn tone() -> Tone {
    let asc = TONE[0] as usize;
    let mut at = 1 + asc;
    let count = u16::from_be_bytes([TONE[at], TONE[at + 1]]) as usize;
    at += 2;
    let mut packets = Vec::with_capacity(count);
    for _ in 0..count {
        let len = u16::from_be_bytes([TONE[at], TONE[at + 1]]) as usize;
        at += 2;
        packets.push(&TONE[at..at + len]);
        at += len;
    }
    Tone { cookie: &TONE_COOKIE, packets }
}

fn crc32(chunks: &[&[u8]]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for chunk in chunks {
        for &b in *chunk {
            crc ^= u32::from(b);
            for _ in 0..8 {
                crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
            }
        }
    }
    !crc
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &x in data {
        a = (a + u32::from(x)) % 65_521;
        b = (b + a) % 65_521;
    }
    (b << 16) | a
}

fn png_chunk(out: &mut Vec<u8>, name: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(name);
    out.extend_from_slice(data);
    out.extend_from_slice(&crc32(&[name, data]).to_be_bytes());
}

/// A valid PNG of 8-bit RGBA pixels, its data in stored (uncompressed) deflate blocks.
pub fn png_rgba(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    assert_eq!(rgba.len(), (width * height * 4) as usize);
    let mut raw = Vec::with_capacity(rgba.len() + height as usize);
    for row in rgba.chunks((width * 4) as usize) {
        raw.push(0); // filter: none
        raw.extend_from_slice(row);
    }
    let mut z = vec![0x78, 0x01];
    let mut blocks = raw.chunks(65_535).peekable();
    while let Some(block) = blocks.next() {
        z.push(u8::from(blocks.peek().is_none()));
        z.extend_from_slice(&(block.len() as u16).to_le_bytes());
        z.extend_from_slice(&(!(block.len() as u16)).to_le_bytes());
        z.extend_from_slice(block);
    }
    z.extend_from_slice(&adler32(&raw).to_be_bytes());

    let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    png_chunk(&mut out, b"IHDR", &ihdr);
    png_chunk(&mut out, b"IDAT", &z);
    png_chunk(&mut out, b"IEND", &[]);
    out
}

/// An app icon: a 32×32 rounded square of one colour with a lighter band.
pub fn icon_png(rgb: [u8; 3]) -> Vec<u8> {
    let n = 32u32;
    let mut px = Vec::with_capacity((n * n * 4) as usize);
    for y in 0..n {
        for x in 0..n {
            let corner = |a: u32, b: u32| a + b < 5;
            let outside =
                corner(x, y) || corner(n - 1 - x, y) || corner(x, n - 1 - y) || corner(n - 1 - x, n - 1 - y);
            if outside {
                px.extend_from_slice(&[0, 0, 0, 0]);
            } else if (10..14).contains(&y) && (6..26).contains(&x) {
                px.extend_from_slice(&[255, 255, 255, 255]);
            } else {
                px.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
            }
        }
    }
    png_rgba(n, n, &px)
}

/// A cursor: its hotspot and size in Mac points, and its PNG at 2× (as the Mac's are).
pub struct Cursor {
    pub hotspot: (f32, f32),
    pub size: (f32, f32),
    pub png: Vec<u8>,
}

/// Two shapes: 0 an arrow (hotspot at its tip), 1 an I-beam (hotspot at its middle).
pub fn cursor(shape: usize) -> Cursor {
    let n = 32u32;
    let mut px = vec![0u8; (n * n * 4) as usize];
    let mut set = |x: u32, y: u32, c: [u8; 4]| {
        let i = ((y * n + x) * 4) as usize;
        px[i..i + 4].copy_from_slice(&c);
    };
    if shape.is_multiple_of(2) {
        for y in 0..24u32 {
            for x in 0..=(y * 2 / 3) {
                let edge = x == 0 || x == y * 2 / 3 || y == 23;
                set(x + 2, y + 2, if edge { [255, 255, 255, 255] } else { [0, 0, 0, 255] });
            }
        }
        Cursor { hotspot: (1.0, 1.0), size: (16.0, 16.0), png: png_rgba(n, n, &px) }
    } else {
        for y in 4..28u32 {
            for x in 14..18u32 {
                set(x, y, [0, 0, 0, 255]);
            }
        }
        for x in 10..22u32 {
            for y in [4u32, 5, 26, 27] {
                set(x, y, [0, 0, 0, 255]);
            }
        }
        Cursor { hotspot: (8.0, 8.0), size: (16.0, 16.0), png: png_rgba(n, n, &px) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clip_parses() {
        let c = clip();
        assert_eq!(c.units.len(), 120);
        assert!(c.units[0].keyframe);
        assert_eq!(c.units.iter().filter(|u| u.keyframe).count(), 4);
        assert!(c.units.iter().enumerate().all(|(i, u)| u.keyframe == (i % 30 == 0)));
        // nalLen 4, three sets: VPS (type 32), SPS (33), PPS (34).
        assert_eq!(&c.parameter_sets[..2], &[4, 3]);
        let mut at = 2;
        for expected in [32u8, 33, 34] {
            let len = u16::from_be_bytes([c.parameter_sets[at], c.parameter_sets[at + 1]]) as usize;
            assert_eq!((c.parameter_sets[at + 2] >> 1) & 0x3F, expected);
            at += 2 + len;
        }
        assert_eq!(at, c.parameter_sets.len());
        // Every frame: whole length-prefixed NALs, no parameter set or AUD among them.
        for u in &c.units {
            let mut at = 0;
            let mut vcl = 0;
            while at < u.data.len() {
                let len = u32::from_be_bytes(u.data[at..at + 4].try_into().unwrap()) as usize;
                let kind = (u.data[at + 4] >> 1) & 0x3F;
                assert!(!(32..=38).contains(&kind));
                if kind < 32 {
                    vcl += 1;
                    assert_eq!((16..=23).contains(&kind), u.keyframe);
                }
                at += 4 + len;
            }
            assert_eq!(at, u.data.len());
            assert!(vcl >= 1);
        }
    }

    #[test]
    fn png_is_well_formed() {
        let p = icon_png([10, 20, 30]);
        assert_eq!(&p[..8], &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
        assert_eq!(&p[12..16], b"IHDR");
        assert_eq!(&p[p.len() - 8..p.len() - 4], b"IEND");
        // The standard check value of CRC-32 and Adler-32.
        assert_eq!(crc32(&[b"123456789"]), 0xCBF4_3926);
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
        assert!(cursor(0).png.len() > 100 && cursor(1).png.len() > 100);
    }

    #[test]
    fn tone_and_thumbnails() {
        let t = tone();
        assert_eq!(t.packets.len(), 75);
        assert!(t.packets.iter().all(|p| !p.is_empty() && p.len() < 1000));
        for j in THUMBNAILS {
            assert_eq!(&j[..3], &[0xFF, 0xD8, 0xFF]);
            assert_eq!(&j[j.len() - 2..], &[0xFF, 0xD9]);
        }
    }
}
