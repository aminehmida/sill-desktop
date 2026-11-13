//! Pure rules shared with the Mac and the other clients, ported from `Sources/StreamProtocol`
//! (Viewport.swift's `ViewportBounds`, TextScale.swift, Switcher.swift's `LockedMac`,
//! ScrollDirection.swift) and iOSClient's AwayCopy.swift.

use super::json::{AwayQuality, LinkReport, QualityPreset, SettingsChange, StreamSource, WindowList};

/// What a host takes from a device's viewport (kind 9): sides 1 to 16384 points, a scale 0.25 to 4.
pub struct ViewportBounds;

/// A viewport held to the host's bounds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HeldViewport {
    pub width: f64,
    pub height: f64,
    pub scale: Option<f64>,
}

impl ViewportBounds {
    pub const MIN_SIDE: f64 = 1.0;
    pub const MAX_SIDE: f64 = 16384.0;
    pub const MIN_SCALE: f64 = 0.25;
    pub const MAX_SCALE: f64 = 4.0;

    /// What a device sends: each side held to its range and a scale to its range, so no host
    /// refuses it; a scale that is not a positive number becomes none. `None` while a side is not
    /// a positive number (a panel not measured yet).
    pub fn held(width: f64, height: f64, scale: Option<f64>) -> Option<HeldViewport> {
        if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
            return None;
        }
        Some(HeldViewport {
            width: width.clamp(Self::MIN_SIDE, Self::MAX_SIDE),
            height: height.clamp(Self::MIN_SIDE, Self::MAX_SIDE),
            scale: scale.filter(|s| s.is_finite() && *s > 0.0).map(|s| s.clamp(Self::MIN_SCALE, Self::MAX_SCALE)),
        })
    }
}

/// Aa, the text size control (TextScale.swift): 0.5× to 1.5× in 0.25 steps, and Off (no scale:
/// the Mac leaves the window alone) one step below, offered only where no scale leaves the window
/// alone. Usable only while a window streams: the Mac ignores the scale for the Desktop.
pub struct TextScale;

impl TextScale {
    pub const OFF: f64 = 0.25;
    pub const SCALES: [f64; 5] = [0.5, 0.75, 1.0, 1.25, 1.5];
    pub const UNAVAILABLE_HINT: &'static str = "Available for a window.";

    pub fn detents(offered: bool) -> Vec<f64> {
        let mut d = Vec::with_capacity(6);
        if offered {
            d.push(Self::OFF);
        }
        d.extend_from_slice(&Self::SCALES);
        d
    }

    /// Where `scale` sits on the ruler: its own value, Off's place for none when offered, else 1×.
    pub fn position(scale: Option<f64>, offered: bool) -> f64 {
        scale.unwrap_or(if offered { Self::OFF } else { 1.0 })
    }

    /// The scale a detent stands for: `None` for Off.
    pub fn scale_at(position: f64) -> Option<f64> {
        if position == Self::OFF { None } else { Some(position) }
    }

    /// The nearest detent to `value`, clamped to the ends.
    pub fn snap(value: f64, offered: bool) -> f64 {
        let d = Self::detents(offered);
        let c = value.clamp(d[0], d[d.len() - 1]);
        let mut best = d[0];
        for &x in &d {
            if (x - c).abs() < (best - c).abs() {
                best = x;
            }
        }
        best
    }

    /// "Off", or the scale with trailing zeros dropped and "×": 0.5×, 1×, 1.25×.
    pub fn title(position: f64) -> String {
        if position == Self::OFF { "Off".to_string() } else { format!("{}×", Self::number(position)) }
    }

    pub fn number(scale: f64) -> String {
        let mut t = format!("{scale:.2}");
        while t.ends_with('0') {
            t.pop();
        }
        if t.ends_with('.') {
            t.pop();
        }
        t
    }

    /// Whether the ruler has Off: not for a window on the Mac's virtual display (no scale is 1×
    /// there). While a window streams, where it streams decides (the running stream's
    /// `onVirtualDisplay`); otherwise the Virtual Display setting as the device knows it.
    pub fn offers_off(virtual_display: Option<bool>, streaming: StreamSource, on_virtual_display: Option<bool>) -> bool {
        if let (true, Some(on)) = (streaming.is_window(), on_virtual_display) {
            return !on;
        }
        virtual_display != Some(true)
    }

    /// Aa sizes a window, never the Desktop.
    pub fn selectable(streaming: StreamSource) -> bool {
        streaming.is_window()
    }
}

/// Why a Mac shows nothing of itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LockWhy {
    ScreenLocked,
    OtherUser,
}

/// What covers the stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LockShade {
    Locked(LockWhy),
    Unlocking,
}

/// A locked Mac on the device (Switcher.swift's `LockedMac`): why, the cover over the stream and
/// its words.
pub struct LockedMac;

impl LockedMac {
    /// What this device says in its hello: it shows a Mac's off-console view.
    pub const VIEW_GENERATION: i64 = 1;
    /// How long "Unlocking…" waits for the picture at most, in seconds.
    pub const UNLOCKING_TIMEOUT_S: f64 = 4.0;

    pub const VIEW_LINE: &'static str =
        "Your account’s own desktop, not this Mac’s screen. Taps and keys reach it once your account is back on the screen.";
    pub const VIEW_PANEL_LINE: &'static str = "Another user is on this Mac’s screen. You’re seeing your account’s own desktop and windows, not the screen. Taps, keys and opening apps wait until your account is back on it.";
    pub const VIEW_TEXT_SCALE_HINT: &'static str = "Available once your account is back on this Mac’s screen.";

    pub fn why(list: &WindowList) -> Option<LockWhy> {
        if list.locked != Some(true) {
            return None;
        }
        Some(if list.off_console == Some(true) { LockWhy::OtherUser } else { LockWhy::ScreenLocked })
    }

    /// The off-console view: this session's own desktop and windows, to look at.
    pub fn view(list: &WindowList) -> bool {
        list.locked == Some(true) && list.off_console == Some(true) && list.off_console_view == Some(true)
    }

    /// Whether the stream is covered: only for a Mac off console that shows nothing of itself.
    pub fn covers(why: Option<LockWhy>, view: bool) -> bool {
        why == Some(LockWhy::OtherUser) && !view
    }

    /// The cover after a window list: another user's while it says so (none in the off-console
    /// view, whose picture is live); a locked screen shows no cover (the Mac streams its lock
    /// screen); after a cover, "Unlocking…" until the next keyframe.
    pub fn shade(was: Option<LockShade>, why: Option<LockWhy>, view: bool) -> Option<LockShade> {
        if view && why == Some(LockWhy::OtherUser) {
            return None;
        }
        if why == Some(LockWhy::OtherUser) {
            return Some(LockShade::Locked(LockWhy::OtherUser));
        }
        was.map(|_| LockShade::Unlocking)
    }

    /// A keyframe ends "Unlocking…"; nothing else does.
    pub fn shade_after_frame(was: Option<LockShade>, keyframe: bool) -> Option<LockShade> {
        if was == Some(LockShade::Unlocking) && keyframe { None } else { was }
    }

    /// The Apps drawer's and the Settings panel's line, where the Mac's windows would be.
    pub fn line(mac: &str, why: LockWhy) -> String {
        match why {
            LockWhy::ScreenLocked => format!("{} is locked. Unlock it to see its windows.", Self::name(mac)),
            LockWhy::OtherUser => {
                "Another user started controlling this Mac. Log in to your account to reactivate screen sharing with Sill.".to_string()
            }
        }
    }

    pub fn title(mac: &str, shade: LockShade) -> String {
        match shade {
            LockShade::Locked(LockWhy::ScreenLocked) => format!("{} is locked", Self::name(mac)),
            LockShade::Locked(LockWhy::OtherUser) => "Another user started controlling this Mac.".to_string(),
            LockShade::Unlocking => "Unlocking…".to_string(),
        }
    }

    /// `paired`: this session's key is the Mac's (kind 16's `sessionPaired`): only then do keys
    /// reach a locked screen.
    pub fn detail(shade: LockShade, paired: Option<bool>) -> Option<&'static str> {
        match shade {
            LockShade::Locked(LockWhy::ScreenLocked) => Some(if paired == Some(true) {
                "To unlock it from here, type its password and press Return. Its windows come back once it is unlocked."
            } else {
                "Its windows come back once it is unlocked."
            }),
            LockShade::Locked(LockWhy::OtherUser) => Some("Log in to your account to reactivate screen sharing with Sill."),
            LockShade::Unlocking => None,
        }
    }

    fn name(mac: &str) -> &str {
        if mac.is_empty() { "The Mac" } else { mac }
    }
}

/// Where a scroll came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScrollSource {
    Finger,
    Pad,
    Trackpad,
    Wheel,
}

/// Reverse Scrolling (ScrollDirection.swift): the device's own switch, never sent. It turns around
/// a scroll made on the glass and its coast; a connected trackpad or mouse already follows the
/// system's own scrolling setting.
pub struct ScrollDirection;

impl ScrollDirection {
    pub fn reversible(source: ScrollSource) -> bool {
        matches!(source, ScrollSource::Finger | ScrollSource::Pad)
    }

    /// What a scroll's deltas, and its coast's velocity, are multiplied by.
    pub fn factor(source: ScrollSource, reversed: bool) -> f64 {
        if reversed && Self::reversible(source) { -1.0 } else { 1.0 }
    }
}

/// The Settings panel's callout while the link cannot keep up.
#[derive(Debug, Clone, PartialEq)]
pub struct AwayCallout {
    pub text: String,
    pub button: Option<String>,
    pub change: Option<SettingsChange>,
}

/// The link while it cannot keep up (iOSClient's AwayCopy.swift).
pub struct AwayCopy;

impl AwayCopy {
    /// How long the stream's line stays after the report clears (ms).
    pub const LINGER_MS: u64 = 2_000;

    /// First match wins: away while a device at home is connected (the Mac keeps the home quality);
    /// a suggestion; nothing lower. `None` while the link keeps up.
    pub fn callout(link: Option<&LinkReport>, away: Option<&AwayQuality>, mac: &str) -> Option<AwayCallout> {
        let link = link.filter(|l| l.is_behind())?;
        let quality = QualityPreset::name(link.bitrate);
        if let Some(a) = away {
            if a.this_connection_away && !a.away_running {
                return Some(AwayCallout {
                    text: format!("The link can’t carry {quality}, which {mac} keeps while a device at home is connected."),
                    button: None,
                    change: None,
                });
            }
        }
        if let Some(bitrate) = link.suggested_bitrate {
            let scale = link.suggested_capture_scale;
            let title = match scale {
                Some(s) => QualityPreset::short_title(bitrate, s),
                None => QualityPreset::name(bitrate),
            };
            return Some(AwayCallout {
                text: format!("The link can’t carry {quality}. {title} is recommended."),
                button: Some(format!("Use {title}")),
                change: Some(SettingsChange { bitrate: Some(bitrate), capture_scale: scale, ..Default::default() }),
            });
        }
        Some(AwayCallout { text: format!("The link to {mac} is too slow for a steady picture, even at Low."), button: None, change: None })
    }

    /// The stream screen's line while the link is behind.
    pub fn stream_line(link: Option<&LinkReport>, away: Option<&AwayQuality>, mac: &str) -> Option<String> {
        let c = Self::callout(link, away, mac)?;
        Some(if c.button.is_some() {
            format!("The link can’t keep up with {}. Lower it in Settings.", QualityPreset::name(link?.bitrate))
        } else {
            format!("The link to {mac} can’t keep up.")
        })
    }
}
