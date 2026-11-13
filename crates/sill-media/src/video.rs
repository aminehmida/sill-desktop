//! The Mac's picture: HEVC (Main, 8-bit 4:2:0) decoded through FFmpeg, in hardware where the OS
//! has it (VideoToolbox, Direct3D 11, VAAPI) and in software otherwise, handed on as NV12 in
//! memory. One decoder per stream; the caller feeds whole access units in Annex B, in order.

use ffmpeg as ff;
use std::ffi::CStr;
use std::os::raw::c_int;
use std::ptr;

/// One decoded picture, NV12: `y` is `width × height` bytes, `uv` is `chroma_width × 2 ×
/// chroma_height` bytes (U and V interleaved), both without row padding.
pub struct Picture {
    pub width: u32,
    pub height: u32,
    pub y: Vec<u8>,
    pub uv: Vec<u8>,
    /// Full range (0–255) instead of video range (16–235).
    pub full_range: bool,
    /// BT.709 matrix; otherwise BT.601.
    pub bt709: bool,
    /// The time stamp given with the access unit this picture came from.
    pub pts: f64,
}

impl Picture {
    pub fn chroma_width(&self) -> u32 {
        self.width.div_ceil(2)
    }
    pub fn chroma_height(&self) -> u32 {
        self.height.div_ceil(2)
    }
}

/// Which decoder to open.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum Prefer {
    /// The OS's hardware decoder when one opens, software otherwise.
    #[default]
    Auto,
    Software,
    /// An FFmpeg hardware device type by name ("videotoolbox", "d3d11va", "vaapi", "cuda", "vulkan").
    Named(String),
}

#[derive(Debug)]
pub struct DecodeError {
    pub code: i32,
    pub what: String,
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.what, self.code)
    }
}
impl std::error::Error for DecodeError {}

fn err(code: c_int, doing: &str) -> DecodeError {
    let mut buf = [0 as std::os::raw::c_char; 128];
    let text = unsafe {
        ff::av_strerror(code, buf.as_mut_ptr(), buf.len());
        CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned()
    };
    DecodeError { code, what: format!("{doing}: {text}") }
}

pub struct VideoDecoder {
    ctx: *mut ff::AVCodecContext,
    frame: *mut ff::AVFrame,
    sw_frame: *mut ff::AVFrame,
    pkt: *mut ff::AVPacket,
    hw: Option<String>,
    /// Frames the hardware path gave; while 0, a failing hardware decoder is replaced by software.
    hw_pictures: u64,
    spare: Vec<(Vec<u8>, Vec<u8>)>,
}

// The context is used from one thread at a time: the type is moved, never shared.
unsafe impl Send for VideoDecoder {}

#[cfg(target_os = "macos")]
const HW_ORDER: &[&str] = &["videotoolbox"];
#[cfg(target_os = "windows")]
const HW_ORDER: &[&str] = &["d3d11va"];
#[cfg(all(unix, not(target_os = "macos")))]
const HW_ORDER: &[&str] = &["vaapi", "cuda", "vulkan"];

unsafe extern "C" fn pick_format(ctx: *mut ff::AVCodecContext, list: *const ff::AVPixelFormat) -> ff::AVPixelFormat {
    let wanted = ff::AVPixelFormat((*ctx).opaque as usize as c_int);
    let mut p = list;
    while (*p).0 != ff::AVPixelFormat::NONE.0 {
        if (*p).0 == wanted.0 {
            return wanted;
        }
        p = p.add(1);
    }
    // The hardware does not take this stream: the first software format, which FFmpeg lists last.
    let mut p = list;
    let mut last = *p;
    while (*p).0 != ff::AVPixelFormat::NONE.0 {
        last = *p;
        p = p.add(1);
    }
    log::warn!("the hardware decoder does not take this stream: decoding in software");
    (*ctx).opaque = ff::AVPixelFormat::NONE.0 as isize as *mut _;
    last
}

impl VideoDecoder {
    pub fn new(prefer: &Prefer) -> Result<Self, DecodeError> {
        unsafe { ff::av_log_set_level(16) }; // AV_LOG_ERROR
        let names: Vec<String> = match prefer {
            Prefer::Auto => HW_ORDER.iter().map(|s| s.to_string()).collect(),
            Prefer::Software => vec![],
            Prefer::Named(n) => vec![n.clone()],
        };
        for name in &names {
            match Self::open(Some(name)) {
                Ok(d) => return Ok(d),
                Err(e) => log::info!("no {name} decoder here: {e}"),
            }
        }
        Self::open(None)
    }

    fn open(hw: Option<&str>) -> Result<Self, DecodeError> {
        unsafe {
            let codec = ff::avcodec_find_decoder(ff::AVCodecID::HEVC);
            if codec.is_null() {
                return Err(DecodeError { code: -1, what: "this FFmpeg has no HEVC decoder".into() });
            }
            let mut ctx = ff::avcodec_alloc_context3(codec);
            if ctx.is_null() {
                return Err(DecodeError { code: -1, what: "out of memory".into() });
            }
            (*ctx).flags |= ff::AV_CODEC_FLAG_LOW_DELAY as c_int;
            if let Some(name) = hw {
                let cname = std::ffi::CString::new(name).unwrap();
                let kind = ff::av_hwdevice_find_type_by_name(cname.as_ptr());
                let mut format = ff::AVPixelFormat::NONE;
                let mut i = 0;
                loop {
                    let cfg = ff::avcodec_get_hw_config(codec, i);
                    if cfg.is_null() {
                        break;
                    }
                    if (*cfg).device_type.0 == kind.0 && ((*cfg).methods & ff::AV_CODEC_HW_CONFIG_METHOD_HW_DEVICE_CTX.0 as c_int) != 0 {
                        format = (*cfg).pix_fmt;
                        break;
                    }
                    i += 1;
                }
                if kind.0 == ff::AVHWDeviceType::NONE.0 || format.0 == ff::AVPixelFormat::NONE.0 {
                    ff::avcodec_free_context(&mut ctx);
                    return Err(DecodeError { code: -1, what: format!("this FFmpeg has no {name} HEVC decoding") });
                }
                let mut device = ptr::null_mut();
                let r = ff::av_hwdevice_ctx_create(&mut device, kind, ptr::null(), ptr::null_mut(), 0);
                if r < 0 {
                    ff::avcodec_free_context(&mut ctx);
                    return Err(err(r, "opening the device"));
                }
                (*ctx).hw_device_ctx = device;
                (*ctx).opaque = format.0 as usize as *mut _;
                (*ctx).get_format = Some(pick_format);
            } else {
                // Software: slices in parallel, never frames (a frame thread is a frame of delay).
                (*ctx).thread_count = 0;
                (*ctx).thread_type = ff::FF_THREAD_SLICE as c_int;
                (*ctx).opaque = ff::AVPixelFormat::NONE.0 as isize as *mut _;
            }
            let r = ff::avcodec_open2(ctx, codec, ptr::null_mut());
            if r < 0 {
                ff::avcodec_free_context(&mut ctx);
                return Err(err(r, "opening the decoder"));
            }
            Ok(Self {
                ctx,
                frame: ff::av_frame_alloc(),
                sw_frame: ff::av_frame_alloc(),
                pkt: ff::av_packet_alloc(),
                hw: hw.map(|s| s.to_string()),
                hw_pictures: 0,
                spare: Vec::new(),
            })
        }
    }

    /// "videotoolbox", "d3d11va", "vaapi", … or "software".
    pub fn backend(&self) -> &str {
        // The hardware turned the stream down inside FFmpeg (pick_format): it decodes in software.
        if unsafe { (*self.ctx).opaque as isize } == ff::AVPixelFormat::NONE.0 as isize {
            return "software";
        }
        self.hw.as_deref().unwrap_or("software")
    }

    /// Give a shown picture's memory back for the next one.
    pub fn recycle(&mut self, p: Picture) {
        if self.spare.len() < 4 {
            self.spare.push((p.y, p.uv));
        }
    }

    /// Decode one access unit (Annex B; parameter sets may come alone or in front of a keyframe).
    /// `out` gets each picture that is ready, usually one. After an error the stream needs a
    /// keyframe; when a hardware decoder fails before its first picture, software takes its place
    /// here and the error is still returned.
    pub fn decode(&mut self, annexb: &[u8], pts: f64, out: &mut dyn FnMut(Picture)) -> Result<(), DecodeError> {
        let r = self.decode_inner(annexb, pts, out);
        if r.is_err() && self.hw.is_some() && self.hw_pictures == 0 {
            log::warn!("{} failed before its first picture: decoding in software", self.backend());
            if let Ok(mut soft) = Self::open(None) {
                std::mem::swap(self, &mut soft);
            }
        }
        r
    }

    fn decode_inner(&mut self, annexb: &[u8], pts: f64, out: &mut dyn FnMut(Picture)) -> Result<(), DecodeError> {
        unsafe {
            let r = ff::av_new_packet(self.pkt, annexb.len() as c_int);
            if r < 0 {
                return Err(err(r, "allocating a packet"));
            }
            ptr::copy_nonoverlapping(annexb.as_ptr(), (*self.pkt).data, annexb.len());
            (*self.pkt).pts = pts.to_bits() as i64;
            let r = ff::avcodec_send_packet(self.ctx, self.pkt);
            ff::av_packet_unref(self.pkt);
            if r < 0 && r != ff::AVERROR(libc::EAGAIN) {
                return Err(err(r, "decoding"));
            }
            loop {
                let r = ff::avcodec_receive_frame(self.ctx, self.frame);
                if r == ff::AVERROR(libc::EAGAIN) || r == ff::AVERROR_EOF {
                    return Ok(());
                }
                if r < 0 {
                    return Err(err(r, "decoding"));
                }
                let mut src = self.frame;
                let hw_frame = !(*self.frame).hw_frames_ctx.is_null();
                if hw_frame {
                    ff::av_frame_unref(self.sw_frame);
                    let r = ff::av_hwframe_transfer_data(self.sw_frame, self.frame, 0);
                    if r < 0 {
                        ff::av_frame_unref(self.frame);
                        return Err(err(r, "reading the picture back"));
                    }
                    src = self.sw_frame;
                }
                let stamp = f64::from_bits((*self.frame).pts as u64);
                let picture = self.copy_out(src, self.frame, stamp);
                ff::av_frame_unref(self.frame);
                ff::av_frame_unref(self.sw_frame);
                let picture = picture?;
                if hw_frame {
                    self.hw_pictures += 1;
                }
                out(picture);
            }
        }
    }

    /// `pixels` holds the planes, `props` the color description (the hardware frame keeps it).
    unsafe fn copy_out(&mut self, pixels: *const ff::AVFrame, props: *const ff::AVFrame, pts: f64) -> Result<Picture, DecodeError> {
        let f = &*pixels;
        let (w, h) = (f.width as usize, f.height as usize);
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        let (mut y, mut uv) = self.spare.pop().unwrap_or_default();
        y.resize(w * h, 0);
        uv.resize(cw * 2 * ch, 0);
        let copy_plane = |dst: &mut [u8], data: *const u8, stride: c_int, row: usize, rows: usize| {
            let stride = stride as isize;
            if stride as usize == row {
                ptr::copy_nonoverlapping(data, dst.as_mut_ptr(), row * rows);
            } else {
                for r in 0..rows {
                    ptr::copy_nonoverlapping(data.offset(stride * r as isize), dst.as_mut_ptr().add(r * row), row);
                }
            }
        };
        let format = ff::AVPixelFormat(f.format);
        let mut full = (*props).color_range.0 == ff::AVColorRange::JPEG.0;
        if format.0 == ff::AVPixelFormat::NV12.0 {
            copy_plane(&mut y, f.data[0], f.linesize[0], w, h);
            copy_plane(&mut uv, f.data[1], f.linesize[1], cw * 2, ch);
        } else if format.0 == ff::AVPixelFormat::YUV420P.0 || format.0 == ff::AVPixelFormat::YUVJ420P.0 {
            full |= format.0 == ff::AVPixelFormat::YUVJ420P.0;
            copy_plane(&mut y, f.data[0], f.linesize[0], w, h);
            for r in 0..ch {
                let u = std::slice::from_raw_parts(f.data[1].offset(f.linesize[1] as isize * r as isize), cw);
                let v = std::slice::from_raw_parts(f.data[2].offset(f.linesize[2] as isize * r as isize), cw);
                let row = &mut uv[r * cw * 2..(r + 1) * cw * 2];
                for ((pair, u), v) in row.chunks_exact_mut(2).zip(u).zip(v) {
                    pair[0] = *u;
                    pair[1] = *v;
                }
            }
        } else {
            self.spare.push((y, uv));
            return Err(DecodeError { code: -1, what: format!("a picture format this app does not show ({})", f.format) });
        }
        let space = (*props).colorspace.0;
        // Untagged: BT.709 for anything HD, as players assume.
        let bt709 = if space == ff::AVColorSpace::SMPTE170M.0 || space == ff::AVColorSpace::BT470BG.0 {
            false
        } else {
            space == ff::AVColorSpace::BT709.0 || h >= 720 || w >= 1280
        };
        Ok(Picture { width: w as u32, height: h as u32, y, uv, full_range: full, bt709, pts })
    }

    /// Forget what is in flight (after a gap in the stream): the next access unit must be a keyframe.
    pub fn flush(&mut self) {
        unsafe { ff::avcodec_flush_buffers(self.ctx) }
    }
}

impl Drop for VideoDecoder {
    fn drop(&mut self) {
        unsafe {
            ff::av_frame_free(&mut self.frame);
            ff::av_frame_free(&mut self.sw_frame);
            ff::av_packet_free(&mut self.pkt);
            ff::avcodec_free_context(&mut self.ctx);
        }
    }
}

/// Splits an Annex B elementary stream (a file, a test clip) into access units: each ends before
/// the next VCL NAL that starts a picture, with the parameter sets and SEI in front of it kept
/// with it.
pub fn split_access_units(stream: &[u8]) -> Vec<&[u8]> {
    // Start code offsets (of the 00 00 01) and the NAL header after each.
    let mut nals = Vec::new();
    let mut i = 0;
    while i + 3 < stream.len() {
        if stream[i] == 0 && stream[i + 1] == 0 && stream[i + 2] == 1 {
            let start = if i > 0 && stream[i - 1] == 0 { i - 1 } else { i };
            nals.push((start, i + 3));
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut units = Vec::new();
    let mut unit_start: Option<usize> = None;
    let mut has_picture = false;
    for &(start, header) in &nals {
        if header + 2 >= stream.len() {
            break;
        }
        let kind = (stream[header] >> 1) & 0x3f;
        let vcl = kind < 32;
        let first_slice = vcl && (stream[header + 2] & 0x80) != 0;
        let begins = if vcl { first_slice } else { has_picture && (kind <= 39 || kind == 35) };
        if unit_start.is_none() {
            unit_start = Some(start);
        } else if has_picture && begins {
            units.push(&stream[unit_start.unwrap()..start]);
            unit_start = Some(start);
            has_picture = false;
        }
        if vcl {
            has_picture = true;
        }
    }
    if let Some(s) = unit_start {
        units.push(&stream[s..]);
    }
    units
}

/// Whether an Annex B access unit holds an IRAP picture (a keyframe).
pub fn is_keyframe(unit: &[u8]) -> bool {
    let mut i = 0;
    while i + 3 < unit.len() {
        if unit[i] == 0 && unit[i + 1] == 0 && unit[i + 2] == 1 {
            let kind = (unit[i + 3] >> 1) & 0x3f;
            if (16..=23).contains(&kind) {
                return true;
            }
            i += 3;
        } else {
            i += 1;
        }
    }
    false
}
