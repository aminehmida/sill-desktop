//! What the client drives on this computer: the Mac's sound and the clipboard.

use sill_core::client::{ClipboardBridge, SoundPlayer};
use sill_media::audio::MacSound;
use std::sync::Mutex;

pub struct Sound(pub MacSound);

impl SoundPlayer for Sound {
    fn codecs(&self) -> Vec<String> {
        self.0.codecs()
    }
    fn set_muted(&self, muted: bool) {
        self.0.set_muted(muted)
    }
    fn on_message(&self, payload: &[u8], host_timestamp: f64) {
        self.0.on_message(payload, host_timestamp)
    }
    fn on_video_frame(&self, host_timestamp: f64) {
        self.0.on_video_frame(host_timestamp)
    }
    fn stop(&self) {
        self.0.stop()
    }
    fn take_second(&self) -> Option<(i32, i32)> {
        self.0.take_second()
    }
    fn set_away(&self, away: bool) {
        self.0.set_away(away)
    }
    fn set_rtt(&self, rtt_ms: Option<i32>) {
        self.0.set_rtt(rtt_ms)
    }
    fn set_stream_fps(&self, fps: i64) {
        self.0.set_stream_fps(fps)
    }
}

/// The system clipboard; on a computer without one (a headless session) nothing is shared.
pub struct Clipboard(Mutex<Option<arboard::Clipboard>>);

impl Clipboard {
    pub fn new() -> Self {
        Self(Mutex::new(arboard::Clipboard::new().map_err(|e| log::info!("no clipboard here: {e}")).ok()))
    }
}

impl ClipboardBridge for Clipboard {
    fn read_text(&self) -> Option<String> {
        self.0.lock().unwrap().as_mut()?.get_text().ok()
    }
    fn write_text(&self, text: &str) -> bool {
        self.0.lock().unwrap().as_mut().map(|c| c.set_text(text.to_string()).is_ok()).unwrap_or(false)
    }
}
