//! The Mac's sound on a desktop (kind 43): the wire's messages, the playout policy, AAC-ELD through
//! FFmpeg and the default output device through cpal, behind [`MacSound`]. A port of iOSClient's
//! StreamClient+Audio, AudioPlayout, AudioOutput and AudioDecoder (Sill 0.6.1) by way of the Android
//! client's `audio/`.

pub mod decoder;
pub mod eld_config;
mod engine;
pub mod message;
pub mod playout;
pub mod resample;
mod sound;
pub mod timeline;

pub use decoder::{AudioError, EldDecoder};
pub use engine::SoundStats;
pub use message::{codec, AudioFormat, AudioMessage, AudioPackets};
pub use playout::AudioPlayout;
pub use sound::MacSound;
pub use timeline::{ClockFeed, OutputClock, PlayerTimeline};

/// Where the sound goes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Sink {
    /// The default output device (and nothing heard, nothing broken, while there is none).
    #[default]
    Device,
    /// No device: a thread takes what a device would, as the clock goes (5 ms at a time, heard
    /// 10 ms later), so the whole path runs where there is no sound card.
    Null,
    /// A machine with no output at all, for the checks.
    #[cfg(test)]
    Missing,
}

/// What an output took from the timeline, as it takes it: the timeline sample of the first frame,
/// and the frames (48 kHz, stereo, interleaved; silence where nothing is scheduled). Called on the
/// output's real-time thread: it must not block.
pub type Tap = Box<dyn FnMut(i64, &[f32]) + Send>;

/// Device time: seconds of the process's monotonic clock (every arrival, reading and deadline here).
pub fn now() -> f64 {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    // From 1000: a time is never near 0, where "never" (0) is kept.
    1000.0 + START.get_or_init(std::time::Instant::now).elapsed().as_secs_f64()
}

#[cfg(test)]
mod tests;
