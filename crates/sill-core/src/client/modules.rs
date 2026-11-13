//! What the client drives but does not implement: the picture, the Mac's sound, and this
//! computer's clipboard. Implementations live with the GUI.

/// Where the video goes. Called on the session's reader task, in arrival order, straight off the
/// socket: never block for long (hand the bytes to a decoder thread).
///
/// # Byte format
///
/// * [`parameter_sets`](VideoSink::parameter_sets) gets the kind 0 payload as it came:
///   `nalLength(1) count(1)`, then per set `length(2, big-endian)` and the raw NAL unit (VPS, SPS,
///   PPS; no start codes). [`crate::protocol::ParameterSets::parse`] reads it,
///   [`annex_b`](crate::protocol::ParameterSets::annex_b) gives the three behind `00 00 00 01`
///   start codes, and `vps()`/`sps()`/`pps()` each alone.
/// * [`frame`](VideoSink::frame) gets the kind 1 payload as it came: one HEVC access unit as
///   **length-prefixed NAL units** (hvcC/"AVCC" style, a 4-byte big-endian length before each NAL
///   unit, as `nalLength` says), **not Annex B**, and without parameter sets (a keyframe is
///   preceded by a kind 0 only when they changed, and once at a stream's start).
///   [`crate::protocol::ParameterSets::to_annex_b`] converts one frame to Annex B.
pub trait VideoSink: Send + Sync {
    /// New parameter sets: the decoder is to be (re)configured before the next keyframe.
    fn parameter_sets(&self, payload: &[u8]);
    /// One access unit. `host_timestamp`: the Mac's wall clock when it was captured, seconds
    /// since 1970.
    fn frame(&self, data: &[u8], keyframe: bool, host_timestamp: f64);
    /// The session ended (or was replaced): forget the stream; the next one starts with
    /// parameter sets and a keyframe.
    fn reset(&self) {}
}

/// A sink that drops everything.
pub struct NoVideo;

impl VideoSink for NoVideo {
    fn parameter_sets(&self, _payload: &[u8]) {}
    fn frame(&self, _data: &[u8], _keyframe: bool, _host_timestamp: f64) {}
}

/// The Mac's sound on this device (kind 43; iOSClient's StreamClient+Audio, AudioPlayout,
/// AudioOutput, AudioDecoder). The client owns one for its life and drives it.
pub trait SoundPlayer: Send + Sync {
    /// The codecs of the Mac's sound this device plays, best first (the hello's `audio`, e.g.
    /// `["aac-eld"]`); empty: none, and the Mac sends no kind 43.
    fn codecs(&self) -> Vec<String>;
    /// Mute on this device alone: the Mac keeps sending, nothing is heard.
    fn set_muted(&self, muted: bool);
    /// One kind 43 payload as it came, with its header's time stamp (host wall clock, s). Reader task.
    fn on_message(&self, payload: &[u8], host_timestamp: f64);
    /// One video frame as it came, with its header's time stamp. Reader task.
    fn on_video_frame(&self, _host_timestamp: f64) {}
    /// Stop playing and forget the stream: the session ended, the app left the screen, the Mac
    /// locked, Send Audio went off. The next format starts it again. Any thread.
    fn stop(&self);
    /// `ClientStats`' sound fields for the second just ended, then a new second: `audioBehindMs`
    /// (the median ms the sound trailed the picture, -1 for a second without sound) and
    /// `audioLate` (packets too late to play); `None` while no sound has played this session.
    fn take_second(&self) -> Option<(i32, i32)> {
        None
    }
    /// Away (a VPN or the internet) or at home: the jitter allowance.
    fn set_away(&self, _away: bool) {}
    /// The last second's median round trip, when it had one.
    fn set_rtt(&self, _rtt_ms: Option<i32>) {}
    /// The running stream's frame rate.
    fn set_stream_fps(&self, _fps: i64) {}
}

/// This computer's clipboard, as the client needs it for kind 32 (Clipboard.swift,
/// StreamClient+Clipboard): text only. Called on the client's own thread.
pub trait ClipboardBridge: Send + Sync {
    /// The clipboard's text now, when it has any.
    fn read_text(&self) -> Option<String>;
    /// Puts the Mac's text on the clipboard; whether it was written.
    fn write_text(&self, text: &str) -> bool;
    /// A number that changes with every change of the clipboard (macOS's `changeCount`, Windows'
    /// sequence number); a constant where the system has none. It keeps the Mac's echo of a text
    /// this device just sent from being written back.
    fn change_count(&self) -> i64 {
        -1
    }
}
