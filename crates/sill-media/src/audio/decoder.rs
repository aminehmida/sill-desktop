//! The Mac's sound, decoded (iOSClient's AudioDecoder): AAC-ELD packets in, 48 kHz stereo out as a
//! float buffer per channel, one packet at a time, through FFmpeg's AAC decoder, which decodes AOT 39
//! from the AudioSpecificConfig given as its extradata (`eld_config` unwraps it from the Mac's cookie);
//! the frame length (480 or 512) comes from that config.
//!
//! A fresh or reset decoder decodes packet n of a segment to its frames n·F to (n+1)·F − 1, the first
//! `primingFrames` of a segment being the codec's own, as AudioToolbox's does on the Mac (checked
//! against its encoder's packets in this module's tests).
//!
//! Reset. A decoder that must start clean (a new segment, the packet after a gap) is swapped for a
//! fresh context: opening one costs well under a millisecond, and nothing of the old one's filter
//! bank can stay.
//!
//! Threads: every call on one thread (`MacSound`'s audio thread).

use ffmpeg as ff;
use std::ffi::CStr;
use std::os::raw::c_int;
use std::ptr;
use std::sync::OnceLock;

/// This many failures in a row: the decoder does not work here, and the epoch plays nothing.
const FAILURES_TO_GIVE_UP: u32 = 20;

#[derive(Debug, Clone)]
pub struct AudioError(pub String);

impl std::fmt::Display for AudioError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for AudioError {}

fn err(code: c_int, doing: &str) -> AudioError {
    let mut buf = [0 as std::os::raw::c_char; 128];
    let text = unsafe {
        ff::av_strerror(code, buf.as_mut_ptr(), buf.len());
        CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned()
    };
    AudioError(format!("{doing}: {text} ({code})"))
}

/// One packet's frames: the left channel's, the right's.
pub type Stereo = (Vec<f32>, Vec<f32>);

pub struct EldDecoder {
    asc: Vec<u8>,
    frames_per_packet: usize,
    ctx: *mut ff::AVCodecContext,
    frame: *mut ff::AVFrame,
    pkt: *mut ff::AVPacket,
    /// The context has been fed nothing since it was opened: a reset is free.
    fresh: bool,
    failures: u32,
    told_size: bool,
}

// The context is used from one thread at a time: the type is moved, never shared.
unsafe impl Send for EldDecoder {}

impl EldDecoder {
    /// Whether this FFmpeg build decodes AAC-ELD (its AAC decoder opens with an ELD config).
    pub fn available() -> bool {
        static AVAILABLE: OnceLock<bool> = OnceLock::new();
        // ER AAC ELD, 48 kHz, stereo, 480 frames: what the Mac's encoder says of itself.
        *AVAILABLE.get_or_init(|| match Self::open(&[0xF8, 0xE6, 0x50, 0x00]) {
            Ok(mut ctx) => {
                unsafe { ff::avcodec_free_context(&mut ctx) };
                true
            }
            Err(e) => {
                log::info!("audio: no AAC-ELD decoder in this FFmpeg: {e}");
                false
            }
        })
    }

    /// A decoder for this AudioSpecificConfig, `frames_per_packet` frames a packet.
    pub fn new(asc: &[u8], frames_per_packet: usize) -> Result<Self, AudioError> {
        if !(1..=8192).contains(&frames_per_packet) {
            return Err(AudioError("the packet size".into()));
        }
        let ctx = Self::open(asc)?;
        let (frame, pkt) = unsafe { (ff::av_frame_alloc(), ff::av_packet_alloc()) };
        let d = EldDecoder { asc: asc.to_vec(), frames_per_packet, ctx, frame, pkt, fresh: true, failures: 0, told_size: false };
        if frame.is_null() || pkt.is_null() {
            return Err(AudioError("out of memory".into())); // `d` frees what was made
        }
        Ok(d)
    }

    fn open(asc: &[u8]) -> Result<*mut ff::AVCodecContext, AudioError> {
        if asc.is_empty() || asc.len() > 1024 {
            return Err(AudioError("the decoder's config".into()));
        }
        unsafe {
            ff::av_log_set_level(16); // AV_LOG_ERROR
            let codec = ff::avcodec_find_decoder(ff::AVCodecID::AAC);
            if codec.is_null() {
                return Err(AudioError("this FFmpeg has no AAC decoder".into()));
            }
            let mut ctx = ff::avcodec_alloc_context3(codec);
            if ctx.is_null() {
                return Err(AudioError("out of memory".into()));
            }
            let extra = ff::av_mallocz(asc.len() + ff::AV_INPUT_BUFFER_PADDING_SIZE as usize) as *mut u8;
            if extra.is_null() {
                ff::avcodec_free_context(&mut ctx);
                return Err(AudioError("out of memory".into()));
            }
            ptr::copy_nonoverlapping(asc.as_ptr(), extra, asc.len());
            (*ctx).extradata = extra; // freed with the context
            (*ctx).extradata_size = asc.len() as c_int;
            // What a demuxer would say of the stream, from the config (the decoder's frames are not
            // valid to FFmpeg without): the Mac's is 48 kHz stereo.
            let info = super::eld_config::parse(asc).filter(|i| (1..=8).contains(&i.channels) && (8000..=96_000).contains(&i.sample_rate));
            (*ctx).sample_rate = info.map_or(48_000, |i| i.sample_rate) as c_int;
            ff::av_channel_layout_default(&mut (*ctx).ch_layout, info.map_or(2, |i| i.channels) as c_int);
            // One packet in, its frames out: no frame threads (each is a packet of delay).
            (*ctx).thread_count = 1;
            (*ctx).request_sample_fmt = ff::AVSampleFormat::FLTP;
            let r = ff::avcodec_open2(ctx, codec, ptr::null_mut());
            if r < 0 {
                ff::avcodec_free_context(&mut ctx);
                return Err(err(r, "opening the AAC decoder"));
            }
            Ok(ctx)
        }
    }

    pub fn frames_per_packet(&self) -> usize {
        self.frames_per_packet
    }

    /// It failed too often to be any use (the epoch plays nothing).
    pub fn broken(&self) -> bool {
        self.failures >= FAILURES_TO_GIVE_UP || self.ctx.is_null()
    }

    /// One packet's `frames_per_packet` frames, a buffer per channel. None when the decoder made
    /// nothing, failed, or made another number of frames (the caller plays silence in its place).
    pub fn decode(&mut self, packet: &[u8]) -> Option<Stereo> {
        if packet.is_empty() || self.broken() {
            return None;
        }
        let got = match self.decode_on(packet) {
            Ok(g) => g,
            Err(e) => {
                log::debug!("audio: decode failed: {e}");
                None
            }
        };
        if let Some((l, _)) = &got {
            if l.len() != self.frames_per_packet && !self.told_size {
                self.told_size = true;
                log::warn!("audio: the decoder made {} frames of a packet, not {}", l.len(), self.frames_per_packet);
            }
        }
        let got = got.filter(|(l, r)| l.len() == self.frames_per_packet && r.len() == self.frames_per_packet);
        if got.is_some() {
            self.failures = 0
        } else {
            self.failures += 1
        }
        got
    }

    fn decode_on(&mut self, packet: &[u8]) -> Result<Option<Stereo>, AudioError> {
        unsafe {
            let r = ff::av_new_packet(self.pkt, packet.len() as c_int);
            if r < 0 {
                return Err(err(r, "allocating a packet"));
            }
            ptr::copy_nonoverlapping(packet.as_ptr(), (*self.pkt).data, packet.len());
            self.fresh = false;
            let r = ff::avcodec_send_packet(self.ctx, self.pkt);
            ff::av_packet_unref(self.pkt);
            if r < 0 && r != ff::AVERROR(libc::EAGAIN) {
                return Err(err(r, "decoding"));
            }
            let mut out = None;
            // One frame a packet; anything more (there never is) is taken so nothing stays inside.
            for _ in 0..8 {
                let r = ff::avcodec_receive_frame(self.ctx, self.frame);
                if r == ff::AVERROR(libc::EAGAIN) || r == ff::AVERROR_EOF {
                    break;
                }
                if r < 0 {
                    return Err(err(r, "decoding"));
                }
                if out.is_none() {
                    out = self.pcm();
                }
                ff::av_frame_unref(self.frame);
            }
            Ok(out)
        }
    }

    /// The frame's samples as two float buffers (a mono frame on both).
    unsafe fn pcm(&mut self) -> Option<Stereo> {
        let f = &*self.frame;
        let n = usize::try_from(f.nb_samples).ok().filter(|n| (1..=1 << 16).contains(n))?;
        let ch = usize::try_from(f.ch_layout.nb_channels).ok().filter(|c| (1..=64).contains(c))?;
        if f.sample_rate != super::playout::RATE as c_int {
            log::warn!("audio: the decoder gives {} Hz", f.sample_rate);
            return None;
        }
        if f.extended_data.is_null() {
            return None;
        }
        let plane = |i: usize| -> *const u8 { *f.extended_data.add(i) as *const u8 };
        let second = if ch > 1 { 1 } else { 0 };
        let mut left = vec![0f32; n];
        let mut right = vec![0f32; n];
        // Planar: a plane per channel. Packed: one plane, the channels interleaved.
        macro_rules! planar {
            ($t:ty, $conv:expr) => {{
                let (a, b) = (plane(0) as *const $t, plane(second) as *const $t);
                if a.is_null() || b.is_null() {
                    return None;
                }
                for i in 0..n {
                    left[i] = $conv(a.add(i).read_unaligned());
                    right[i] = $conv(b.add(i).read_unaligned());
                }
            }};
        }
        macro_rules! packed {
            ($t:ty, $conv:expr) => {{
                let a = plane(0) as *const $t;
                if a.is_null() {
                    return None;
                }
                for i in 0..n {
                    left[i] = $conv(a.add(i * ch).read_unaligned());
                    right[i] = $conv(a.add(i * ch + second).read_unaligned());
                }
            }};
        }
        let fmt = f.format;
        if fmt == ff::AVSampleFormat::FLTP.0 {
            planar!(f32, |x: f32| x)
        } else if fmt == ff::AVSampleFormat::FLT.0 {
            packed!(f32, |x: f32| x)
        } else if fmt == ff::AVSampleFormat::S16P.0 {
            planar!(i16, |x: i16| x as f32 / 32768.0)
        } else if fmt == ff::AVSampleFormat::S16.0 {
            packed!(i16, |x: i16| x as f32 / 32768.0)
        } else if fmt == ff::AVSampleFormat::S32P.0 {
            planar!(i32, |x: i32| x as f32 / 2147483648.0)
        } else if fmt == ff::AVSampleFormat::S32.0 {
            packed!(i32, |x: i32| x as f32 / 2147483648.0)
        } else {
            log::warn!("audio: the decoder gives sample format {fmt}");
            return None;
        }
        Some((left, right))
    }

    /// The next packet decodes as on a fresh decoder (a new segment, the packet after a gap).
    pub fn reset(&mut self) {
        if self.fresh {
            return;
        }
        match Self::open(&self.asc) {
            Ok(ctx) => {
                unsafe { ff::avcodec_free_context(&mut self.ctx) };
                self.ctx = ctx;
            }
            Err(e) => {
                // The old one, flushed, is the best there is; and it counts as a failure.
                log::warn!("audio: no fresh decoder: {e}");
                unsafe { ff::avcodec_flush_buffers(self.ctx) };
                self.failures += 1;
            }
        }
        self.fresh = true;
    }
}

impl Drop for EldDecoder {
    fn drop(&mut self) {
        unsafe {
            ff::av_frame_free(&mut self.frame);
            ff::av_packet_free(&mut self.pkt);
            ff::avcodec_free_context(&mut self.ctx);
        }
    }
}
