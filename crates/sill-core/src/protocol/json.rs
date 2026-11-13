//! The JSON payloads of `Sources/StreamProtocol`, written and read so they come out exactly as
//! Swift's `JSONEncoder` and `JSONDecoder` have them: an enum with associated values is
//! `{"case":{"_0":first unlabeled value, "label":labeled value…}}`, a case without values is
//! `{"case":{}}`, and an optional left nil is a missing key. Fields added later are optional,
//! unknown keys are ignored, nothing is renamed. Reading is as lenient as the Android client's
//! (`org.json`'s `opt…` accessors): a missing or mistyped optional field takes its default.

use base64::Engine;
use serde_json::{json, Map, Value};

use super::policies::ViewportBounds;

// MARK: lenient JSON access (org.json's opt… semantics)

/// A JSON object read leniently.
pub(crate) struct J<'a>(pub &'a Map<String, Value>);

pub(crate) fn parse_object(bytes: &[u8]) -> Option<Map<String, Value>> {
    match serde_json::from_slice::<Value>(bytes).ok()? {
        Value::Object(m) => Some(m),
        _ => None,
    }
}

fn as_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    }
}

fn as_i64(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        Value::String(s) => s.trim().parse::<f64>().ok().map(|f| f as i64),
        _ => None,
    }
}

fn as_string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Null => None,
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        other => Some(other.to_string()),
    }
}

fn as_bool(v: &Value) -> Option<bool> {
    match v {
        Value::Bool(b) => Some(*b),
        Value::String(s) if s.eq_ignore_ascii_case("true") => Some(true),
        Value::String(s) if s.eq_ignore_ascii_case("false") => Some(false),
        _ => None,
    }
}

impl<'a> J<'a> {
    fn present(&self, key: &str) -> Option<&'a Value> {
        self.0.get(key).filter(|v| !v.is_null())
    }
    pub fn string_or(&self, key: &str, default: &str) -> String {
        self.present(key).and_then(as_string).unwrap_or_else(|| default.to_string())
    }
    pub fn string(&self, key: &str) -> String {
        self.string_or(key, "")
    }
    pub fn string_opt(&self, key: &str) -> Option<String> {
        self.present(key).and_then(as_string)
    }
    /// `getString`: `None` when missing or null.
    pub fn string_req(&self, key: &str) -> Option<String> {
        self.string_opt(key)
    }
    pub fn int_or(&self, key: &str, default: i64) -> i64 {
        self.present(key).and_then(as_i64).unwrap_or(default)
    }
    pub fn int(&self, key: &str) -> i64 {
        self.int_or(key, 0)
    }
    pub fn int_opt(&self, key: &str) -> Option<i64> {
        self.present(key).map(|v| as_i64(v).unwrap_or(0))
    }
    pub fn int_req(&self, key: &str) -> Option<i64> {
        self.present(key).and_then(as_i64)
    }
    pub fn double_or(&self, key: &str, default: f64) -> f64 {
        self.present(key).and_then(as_f64).unwrap_or(default)
    }
    pub fn double_opt(&self, key: &str) -> Option<f64> {
        self.present(key).and_then(as_f64).filter(|f| f.is_finite())
    }
    pub fn double_req(&self, key: &str) -> Option<f64> {
        self.present(key).and_then(as_f64)
    }
    pub fn bool(&self, key: &str) -> bool {
        self.present(key).and_then(as_bool).unwrap_or(false)
    }
    /// Only a real JSON boolean.
    pub fn bool_opt(&self, key: &str) -> Option<bool> {
        self.present(key).and_then(Value::as_bool)
    }
    pub fn bool_req(&self, key: &str) -> Option<bool> {
        self.present(key).and_then(as_bool)
    }
    pub fn has(&self, key: &str) -> bool {
        self.0.contains_key(key)
    }
    pub fn object(&self, key: &str) -> Option<J<'a>> {
        self.0.get(key).and_then(Value::as_object).map(J)
    }
    pub fn array(&self, key: &str) -> Option<&'a Vec<Value>> {
        self.0.get(key).and_then(Value::as_array)
    }
}

fn fin(v: f64) -> f64 {
    if v.is_finite() { v } else { 0.0 }
}

// MARK: device → host

/// `StreamSource` (Switcher.swift): what streams.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum StreamSource {
    #[default]
    None,
    Desktop,
    /// A window by its `CGWindowID`.
    Window(u32),
}

impl StreamSource {
    pub fn to_json(self) -> Value {
        match self {
            StreamSource::None => json!({"none": {}}),
            StreamSource::Desktop => json!({"desktop": {}}),
            StreamSource::Window(id) => json!({"window": {"_0": id}}),
        }
    }

    pub(crate) fn parse(o: Option<J<'_>>) -> StreamSource {
        let Some(o) = o else { return StreamSource::None };
        if o.has("desktop") {
            return StreamSource::Desktop;
        }
        if let Some(w) = o.object("window") {
            return StreamSource::Window(w.int_or("_0", -1).clamp(0, u32::MAX as i64) as u32);
        }
        StreamSource::None
    }

    pub fn is_window(self) -> bool {
        matches!(self, StreamSource::Window(_))
    }
}

/// `PointerAction` (Input.swift).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PointerAction {
    /// Cursor only.
    Move,
    LeftDown,
    LeftUp,
    RightDown,
    RightUp,
}

impl PointerAction {
    pub fn wire(self) -> &'static str {
        match self {
            PointerAction::Move => "move",
            PointerAction::LeftDown => "leftDown",
            PointerAction::LeftUp => "leftUp",
            PointerAction::RightDown => "rightDown",
            PointerAction::RightUp => "rightUp",
        }
    }
}

/// `ScrollPhase` (Input.swift): the boundaries of a trackpad-like scroll gesture, sent around
/// scroll deltas: `Began` → deltas… → `Ended`, then optionally `MomentumBegan` → deltas… →
/// `MomentumEnded`, the device generating the decaying momentum deltas itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScrollPhase {
    Began,
    Ended,
    MomentumBegan,
    MomentumEnded,
}

impl ScrollPhase {
    pub fn wire(self) -> &'static str {
        match self {
            ScrollPhase::Began => "began",
            ScrollPhase::Ended => "ended",
            ScrollPhase::MomentumBegan => "momentumBegan",
            ScrollPhase::MomentumEnded => "momentumEnded",
        }
    }
}

/// `InputEvent` (Input.swift), kind 8. Positions are fractions of the video frame (0…1 across its
/// width and height).
pub mod input {
    use super::*;

    pub fn pointer(action: PointerAction, x: f64, y: f64) -> String {
        json!({"pointer": {"_0": action.wire(), "x": fin(x), "y": fin(y)}}).to_string()
    }

    /// Scroll with the cursor at (x, y). `dx`/`dy` are fractions of the frame the content should
    /// move, natural-scrolling sign: positive `dy` moves the content down.
    pub fn scroll(x: f64, y: f64, dx: f64, dy: f64) -> String {
        json!({"scroll": {"x": fin(x), "y": fin(y), "dx": fin(dx), "dy": fin(dy)}}).to_string()
    }

    pub fn scroll_gesture(phase: ScrollPhase, x: f64, y: f64) -> String {
        json!({"scrollGesture": {"_0": phase.wire(), "x": fin(x), "y": fin(y)}}).to_string()
    }

    /// `"\n"`, `"\t"` and `"\u{8}"` (backspace) reach the Mac as their keys; everything else is typed.
    pub fn text(s: &str) -> String {
        json!({"text": {"_0": s}}).to_string()
    }

    /// A USB HID keyboard usage with `UIKeyModifierFlags` bits ([`key_mods`]).
    pub fn key(hid_usage: u16, down: bool, modifiers: u64) -> String {
        json!({"key": {"hidUsage": hid_usage, "down": down, "modifiers": modifiers}}).to_string()
    }
}

/// `UIKeyModifierFlags`, which the Mac reads as `CGEventFlags`' shift/control/option/command.
pub mod key_mods {
    pub const SHIFT: u64 = 1 << 17;
    pub const CONTROL: u64 = 1 << 18;
    pub const OPTION: u64 = 1 << 19;
    pub const COMMAND: u64 = 1 << 20;
}

/// The six trackpad gestures of kind 28 (Gesture.swift).
pub mod gestures {
    /// The fingers move up: Mission Control.
    pub const SWIPE_UP: &str = "swipeUp";
    /// Down: App Exposé.
    pub const SWIPE_DOWN: &str = "swipeDown";
    /// Left: the next Space.
    pub const SWIPE_LEFT: &str = "swipeLeft";
    /// Right: the previous Space.
    pub const SWIPE_RIGHT: &str = "swipeRight";
    /// In: Apps (Launchpad).
    pub const PINCH: &str = "pinch";
    /// Out: Show Desktop.
    pub const SPREAD: &str = "spread";
    pub const NAMES: [&str; 6] = [SWIPE_UP, SWIPE_DOWN, SWIPE_LEFT, SWIPE_RIGHT, PINCH, SPREAD];
    /// `WindowList.gestures` from a host that takes kind 28's six gestures.
    pub const GENERATION: i64 = 1;
}

/// The window commands of kind 15 (`WindowCommand.action`).
pub mod window_actions {
    pub const CLOSE: &str = "close";
    pub const MINIMIZE: &str = "minimize";
    pub const FULL_SCREEN: &str = "fullScreen";
}

/// Builders of the device → host JSON payloads.
pub mod payloads {
    use super::*;

    /// Kind 6.
    pub fn select(source: StreamSource) -> String {
        source.to_json().to_string()
    }

    /// Kind 7.
    pub fn launch_app(bundle_id: &str) -> String {
        json!({"bundleID": bundle_id}).to_string()
    }

    /// Kind 15.
    pub fn window_command(id: u32, action: &str) -> String {
        json!({"id": id, "action": action}).to_string()
    }

    /// Kind 9 (Viewport.swift): the stream panel in device points, the Aa scale or none, the rate.
    /// Held to [`ViewportBounds`] first, so no host refuses it; `None` while the panel is not
    /// measured yet.
    pub fn viewport(width: f64, height: f64, scale: Option<f64>, fps: u32, local_cursor: bool) -> Option<String> {
        let held = ViewportBounds::held(width, height, scale)?;
        let mut o = json!({"width": held.width, "height": held.height, "fps": fps, "localCursor": local_cursor});
        if let Some(s) = held.scale {
            o["scale"] = json!(s);
        }
        Some(o.to_string())
    }

    /// Kind 10: this device's monotonic clock, echoed back as kind 11.
    pub fn ping(clock: f64) -> [u8; 8] {
        clock.to_bits().to_be_bytes()
    }

    /// Kind 12. `audio_behind_ms` and `audio_late`: `None` until this session played sound.
    #[allow(clippy::too_many_arguments)]
    pub fn client_stats(
        fps: i32, frame_age_ms: i32, rtt_ms: i32, device: &str, frame_age_max_ms: i32, rtt_max_ms: i32,
        audio_behind_ms: Option<i32>, audio_late: Option<i32>,
    ) -> String {
        let mut o = json!({
            "fps": fps, "frameAgeMs": frame_age_ms, "rttMs": rtt_ms, "device": device,
            "frameAgeMaxMs": frame_age_max_ms, "rttMaxMs": rtt_max_ms,
        });
        if let Some(v) = audio_behind_ms {
            o["audioBehindMs"] = json!(v);
        }
        if let Some(v) = audio_late {
            o["audioLate"] = json!(v);
        }
        o.to_string()
    }

    /// Kind 23, the first message of every session connection. `audio`: the codecs of the Mac's
    /// sound this device plays, best first (a host sends kind 43 only then); `off_console_view`:
    /// this device shows a Mac's off-console view.
    pub fn hello(app_version: &str, build: &str, device: &str, audio: Option<&[String]>, off_console_view: Option<i64>) -> String {
        let mut o = json!({"appVersion": app_version, "build": build, "protocol": 1, "device": device});
        if let Some(a) = audio {
            o["audio"] = json!(a);
        }
        if let Some(v) = off_console_view {
            o["offConsoleView"] = json!(v);
        }
        o.to_string()
    }

    /// Kind 17: only the fields one control changed, and a token.
    pub fn change_settings(token: i64, change: &SettingsChange) -> String {
        let mut o = json!({"token": token});
        if let Some(v) = change.max_fps {
            o["maxFPS"] = json!(v);
        }
        if let Some(v) = change.bitrate {
            o["bitrate"] = json!(v);
        }
        if let Some(v) = change.capture_scale {
            o["captureScale"] = json!(v);
        }
        if let Some(v) = change.prioritize_speed {
            o["prioritizeSpeed"] = json!(v);
        }
        if let Some(v) = change.virtual_display {
            o["virtualDisplay"] = json!(v);
        }
        if let Some(v) = change.direct_wireless {
            o["directWireless"] = json!(v);
        }
        if let Some(v) = change.send_audio {
            o["sendAudio"] = json!(v);
        }
        o.to_string()
    }

    /// Kind 27: without an id, the subscription to the top level.
    pub fn fetch_menu(token: i64, version: Option<i64>, id: Option<&str>, title: Option<&str>) -> String {
        let mut o = json!({"token": token});
        if let Some(v) = version {
            o["version"] = json!(v);
        }
        if let Some(v) = id {
            o["id"] = json!(v);
        }
        if let Some(v) = title {
            o["title"] = json!(v);
        }
        o.to_string()
    }

    /// Kind 25.
    pub fn press_menu_item(token: i64, version: Option<i64>, id: &str, title: Option<&str>) -> String {
        let mut o = json!({"token": token, "id": id});
        if let Some(v) = version {
            o["version"] = json!(v);
        }
        if let Some(v) = title {
            o["title"] = json!(v);
        }
        o.to_string()
    }

    /// Kind 28.
    pub fn gesture(name: &str, fingers: u32) -> String {
        json!({"gesture": name, "fingers": fingers}).to_string()
    }

    /// Kind 19.
    pub fn pair_request(method: &str, proof: &str, name: &str, model: &str) -> String {
        json!({"v": 1, "method": method, "proof": proof, "name": name, "model": model}).to_string()
    }
}

// MARK: host → device

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowInfo {
    pub id: u32,
    pub title: String,
    pub app_name: String,
    pub bundle_id: String,
    pub width: i32,
    pub height: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppInfo {
    pub name: String,
    pub bundle_id: String,
}

/// Kind 2.
#[derive(Debug, Clone, PartialEq)]
pub struct WindowList {
    pub mac_name: String,
    pub windows: Vec<WindowInfo>,
    pub active: StreamSource,
    pub launch_id: Option<String>,
    pub host_version: Option<String>,
    pub protocol: Option<i64>,
    pub gestures: Option<i64>,
    /// The clipboard generation this host takes and sends (1: text); `None`: send it no kind 32.
    pub clipboard: Option<i64>,
    /// 1: the host repeats a key held down, so a keyboard's characters may go as keys.
    pub key_repeat: Option<i64>,
    /// The Mac shows nothing of itself (locked, or another user on its console).
    pub locked: Option<bool>,
    /// With `locked`: the console is not this session's.
    pub off_console: Option<bool>,
    /// With `locked` and `off_console`: the Mac shows this account's own desktop anyway.
    pub off_console_view: Option<bool>,
}

impl WindowList {
    pub fn parse(bytes: &[u8]) -> Option<WindowList> {
        let m = parse_object(bytes)?;
        let o = J(&m);
        let windows = o
            .array("windows")
            .map(|arr| {
                arr.iter()
                    .filter_map(Value::as_object)
                    .map(|w| {
                        let w = J(w);
                        WindowInfo {
                            id: w.int("id").clamp(0, u32::MAX as i64) as u32,
                            title: w.string("title"),
                            app_name: w.string("appName"),
                            bundle_id: w.string("bundleID"),
                            width: w.int("width") as i32,
                            height: w.int("height") as i32,
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        Some(WindowList {
            mac_name: o.string_or("macName", "Mac"),
            windows,
            active: StreamSource::parse(o.object("active")),
            launch_id: o.string_opt("launchID"),
            host_version: o.string_opt("hostVersion"),
            protocol: o.int_opt("protocol"),
            gestures: o.int_opt("gestures"),
            clipboard: o.int_opt("clipboard"),
            key_repeat: o.int_opt("keyRepeat"),
            locked: o.bool_opt("locked"),
            off_console: o.bool_opt("offConsole"),
            off_console_view: o.bool_opt("offConsoleView"),
        })
    }
}

/// Kind 5.
pub fn parse_app_list(bytes: &[u8]) -> Vec<AppInfo> {
    match serde_json::from_slice::<Value>(bytes) {
        Ok(Value::Array(arr)) => arr
            .iter()
            .filter_map(Value::as_object)
            .map(|a| AppInfo { name: J(a).string("name"), bundle_id: J(a).string("bundleID") })
            .collect(),
        _ => Vec::new(),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct StreamSettings {
    pub max_fps: i64,
    pub bitrate: i64,
    pub capture_scale: f64,
    pub prioritize_speed: bool,
    pub virtual_display: bool,
    pub direct_wireless: Option<bool>,
    /// Send Audio, for every device; `None`: a Mac without sound.
    pub send_audio: Option<bool>,
}

/// Kind 17's fields: only what one control changed.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SettingsChange {
    pub max_fps: Option<i64>,
    pub bitrate: Option<i64>,
    pub capture_scale: Option<f64>,
    pub prioritize_speed: Option<bool>,
    pub virtual_display: Option<bool>,
    pub direct_wireless: Option<bool>,
    pub send_audio: Option<bool>,
}

impl SettingsChange {
    pub fn is_empty(&self) -> bool {
        *self == SettingsChange::default()
    }

    pub fn applied(&self, s: &StreamSettings) -> StreamSettings {
        StreamSettings {
            max_fps: self.max_fps.unwrap_or(s.max_fps),
            bitrate: self.bitrate.unwrap_or(s.bitrate),
            capture_scale: self.capture_scale.unwrap_or(s.capture_scale),
            prioritize_speed: self.prioritize_speed.unwrap_or(s.prioritize_speed),
            virtual_display: self.virtual_display.unwrap_or(s.virtual_display),
            direct_wireless: self.direct_wireless.or(s.direct_wireless),
            send_audio: self.send_audio.or(s.send_audio),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunningStream {
    pub width: i64,
    pub height: i64,
    pub fps: i64,
    pub mbps: i64,
    pub on_virtual_display: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AwayQuality {
    pub home_bitrate: i64,
    pub home_capture_scale: f64,
    pub away_bitrate: i64,
    pub away_capture_scale: f64,
    pub this_connection_away: bool,
    pub away_running: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LinkReport {
    pub state: String,
    pub withheld_per_second: i64,
    pub bitrate: i64,
    pub carried_kbps: Option<i64>,
    pub suggested_bitrate: Option<i64>,
    pub suggested_capture_scale: Option<f64>,
}

impl LinkReport {
    pub fn is_behind(&self) -> bool {
        self.state == "behind"
    }
}

/// Kind 16.
#[derive(Debug, Clone, PartialEq)]
pub struct HostSettingsState {
    pub settings: StreamSettings,
    pub persistent: bool,
    pub virtual_display_available: bool,
    pub virtual_display_note: Option<String>,
    pub software_encoder: bool,
    pub stream: Option<RunningStream>,
    /// The token of the kind 17 this state answers.
    pub answering: Option<i64>,
    pub away: Option<AwayQuality>,
    pub link: Option<LinkReport>,
    /// Why no sound comes although Send Audio is on.
    pub audio_note: Option<String>,
    /// This connection's key is on the Mac's trust list; `None` from older Macs.
    pub session_paired: Option<bool>,
}

impl HostSettingsState {
    pub fn parse(bytes: &[u8]) -> Option<HostSettingsState> {
        let m = parse_object(bytes)?;
        let o = J(&m);
        let s = o.object("settings")?;
        let settings = StreamSettings {
            max_fps: s.int_req("maxFPS")?,
            bitrate: s.int_req("bitrate")?,
            capture_scale: s.double_req("captureScale")?,
            prioritize_speed: s.bool_req("prioritizeSpeed")?,
            virtual_display: s.bool_req("virtualDisplay")?,
            direct_wireless: s.bool_opt("directWireless"),
            send_audio: s.bool_opt("sendAudio"),
        };
        let stream = o.object("stream").map(|it| RunningStream {
            width: it.int("width"),
            height: it.int("height"),
            fps: it.int("fps"),
            mbps: it.int("mbps"),
            on_virtual_display: it.bool("onVirtualDisplay"),
        });
        let away = o.object("away").map(|it| AwayQuality {
            home_bitrate: it.int("homeBitrate"),
            home_capture_scale: it.double_or("homeCaptureScale", 2.0),
            away_bitrate: it.int("awayBitrate"),
            away_capture_scale: it.double_or("awayCaptureScale", 1.0),
            this_connection_away: it.bool("thisConnectionAway"),
            away_running: it.bool("awayRunning"),
        });
        let link = o.object("link").map(|it| LinkReport {
            state: it.string("state"),
            withheld_per_second: it.int("withheldPerSecond"),
            bitrate: it.int("bitrate"),
            carried_kbps: it.int_opt("carriedKbps"),
            suggested_bitrate: it.int_opt("suggestedBitrate"),
            suggested_capture_scale: it.double_opt("suggestedCaptureScale"),
        });
        Some(HostSettingsState {
            settings,
            persistent: o.bool("persistent"),
            virtual_display_available: o.bool("virtualDisplayAvailable"),
            virtual_display_note: o.string_opt("virtualDisplayNote"),
            software_encoder: o.bool("softwareEncoder"),
            stream,
            answering: o.int_opt("answering"),
            away,
            link,
            audio_note: o.string_opt("audioNote"),
            session_paired: o.bool_opt("sessionPaired"),
        })
    }
}

/// `QualityPreset` (HostSettings.swift): bits per second per 60 fps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QualityPreset {
    Low,
    Efficient,
    Balanced,
    High,
    Pro,
    Ultra,
    Extreme,
}

impl QualityPreset {
    pub const ALL: [QualityPreset; 7] = [
        QualityPreset::Low, QualityPreset::Efficient, QualityPreset::Balanced, QualityPreset::High,
        QualityPreset::Pro, QualityPreset::Ultra, QualityPreset::Extreme,
    ];

    pub fn bitrate(self) -> i64 {
        match self {
            QualityPreset::Low => 4_000_000,
            QualityPreset::Efficient => 8_000_000,
            QualityPreset::Balanced => 15_000_000,
            QualityPreset::High => 25_000_000,
            QualityPreset::Pro => 40_000_000,
            QualityPreset::Ultra => 80_000_000,
            QualityPreset::Extreme => 150_000_000,
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            QualityPreset::Low => "Low",
            QualityPreset::Efficient => "Efficient",
            QualityPreset::Balanced => "Balanced",
            QualityPreset::High => "High",
            QualityPreset::Pro => "Pro",
            QualityPreset::Ultra => "Ultra",
            QualityPreset::Extreme => "Extreme",
        }
    }

    /// The preset's title, or "‹n› Mbps" for a bitrate that is none.
    pub fn name(bitrate: i64) -> String {
        Self::ALL
            .iter()
            .find(|p| p.bitrate() == bitrate)
            .map(|p| p.title().to_string())
            .unwrap_or_else(|| format!("{} Mbps", Self::mbps(bitrate)))
    }

    pub fn short_title(bitrate: i64, capture_scale: f64) -> String {
        format!("{} · {}", Self::name(bitrate), if capture_scale >= 1.5 { "Retina" } else { "Standard" })
    }

    pub fn mbps(bps: i64) -> String {
        if bps % 1_000_000 == 0 { format!("{}", bps / 1_000_000) } else { format!("{:.1}", bps as f64 / 1_000_000.0) }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacMenuItem {
    pub id: Option<String>,
    pub title: Option<String>,
    pub separator: bool,
    pub enabled: bool,
    pub mark: Option<String>,
    pub key: Option<String>,
    pub submenu: bool,
}

/// Kind 24.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacMenu {
    pub version: Option<i64>,
    pub app: Option<String>,
    pub bundle_id: Option<String>,
    pub menus: Option<Vec<MacMenuItem>>,
    pub answering: Option<i64>,
    pub menu: Option<String>,
    pub items: Option<Vec<MacMenuItem>>,
    pub more: Option<i64>,
    pub pressed: Option<bool>,
    pub stale: bool,
    pub note: Option<String>,
}

impl MacMenu {
    pub fn parse(bytes: &[u8]) -> Option<MacMenu> {
        let m = parse_object(bytes)?;
        let o = J(&m);
        let items = |key: &str| -> Option<Vec<MacMenuItem>> {
            o.array(key).map(|arr| {
                arr.iter()
                    .filter_map(Value::as_object)
                    .map(|it| {
                        let it = J(it);
                        MacMenuItem {
                            id: it.string_opt("id"),
                            title: it.string_opt("title"),
                            separator: it.bool("separator"),
                            enabled: if it.has("enabled") { it.bool("enabled") } else { true },
                            mark: it.string_opt("mark"),
                            key: it.string_opt("key"),
                            submenu: it.bool("submenu"),
                        }
                    })
                    .collect()
            })
        };
        Some(MacMenu {
            version: o.int_opt("version"),
            app: o.string_opt("app"),
            bundle_id: o.string_opt("bundleID"),
            menus: items("menus"),
            answering: o.int_opt("answering"),
            menu: o.string_opt("menu"),
            items: items("items"),
            more: o.int_opt("more"),
            pressed: if o.has("pressed") { Some(o.bool("pressed")) } else { None },
            stale: o.bool("stale"),
            note: o.string_opt("note"),
        })
    }
}

/// Kind 26: where the Mac's pointer is, as fractions of the frame.
#[derive(Debug, Clone, PartialEq)]
pub struct MacPointer {
    pub x: Option<f64>,
    pub y: Option<f64>,
    pub inside: bool,
    /// How many of this connection's kind 8 messages the Mac had read when it built this.
    pub seen: i64,
}

impl MacPointer {
    pub fn parse(bytes: &[u8]) -> Option<MacPointer> {
        let m = parse_object(bytes)?;
        let o = J(&m);
        Some(MacPointer { x: o.double_opt("x"), y: o.double_opt("y"), inside: o.bool("inside"), seen: o.int_or("seen", 0) })
    }

    pub fn position(&self) -> Option<(f64, f64)> {
        match (self.inside, self.x, self.y) {
            (true, Some(x), Some(y)) if x.is_finite() && y.is_finite() => Some((x, y)),
            _ => None,
        }
    }
}

/// Kind 22.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Goodbye {
    pub reason: String,
    pub message: Option<String>,
    pub minimum_version: Option<String>,
    pub reconnect: Option<bool>,
}

impl Goodbye {
    pub fn parse(bytes: &[u8]) -> Option<Goodbye> {
        let m = parse_object(bytes)?;
        let o = J(&m);
        Some(Goodbye {
            reason: o.string_req("reason")?,
            message: o.string_opt("message"),
            minimum_version: o.string_opt("minimumVersion"),
            reconnect: if o.has("reconnect") { Some(o.bool("reconnect")) } else { None },
        })
    }
}

/// Kind 20.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PairResult {
    pub ok: bool,
    pub proof: Option<String>,
    pub mac_id: Option<String>,
    pub name: Option<String>,
    pub recognition_key: Option<String>,
    pub reason: Option<String>,
    pub tries_left: Option<i64>,
    pub retry_after: Option<f64>,
    pub method: Option<String>,
    pub message: Option<String>,
}

impl PairResult {
    pub fn parse(bytes: &[u8]) -> Option<PairResult> {
        let m = parse_object(bytes)?;
        let o = J(&m);
        Some(PairResult {
            ok: o.bool("ok"),
            proof: o.string_opt("proof"),
            mac_id: o.string_opt("macID"),
            name: o.string_opt("name"),
            recognition_key: o.string_opt("recognitionKey"),
            reason: o.string_opt("reason"),
            tries_left: o.int_opt("triesLeft"),
            retry_after: o.double_opt("retryAfter"),
            method: o.string_opt("method"),
            message: o.string_opt("message"),
        })
    }
}

/// One address of a Mac, from its signed kind 18.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacAddress {
    pub host: String,
    pub port: Option<u16>,
    pub kind: String,
    pub via: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MacInfo {
    pub mac_id: String,
    pub name: String,
    pub issued_at: f64,
    pub remote_access: bool,
    pub remote_port: i64,
    pub internet: bool,
    pub addresses: Vec<MacAddress>,
}

impl MacInfo {
    pub fn parse(bytes: &[u8]) -> Option<MacInfo> {
        let m = parse_object(bytes)?;
        let o = J(&m);
        let mut addresses = Vec::new();
        for a in o.array("addresses").map(|a| a.as_slice()).unwrap_or(&[]) {
            let Some(a) = a.as_object() else { continue };
            let a = J(a);
            addresses.push(MacAddress {
                host: a.string_req("host")?,
                port: a.int_opt("port").and_then(|p| u16::try_from(p).ok()),
                kind: a.string("kind"),
                via: a.string("via"),
            });
        }
        Some(MacInfo {
            mac_id: o.string_req("macID")?,
            name: o.string_or("name", "Mac"),
            issued_at: o.double_or("issuedAt", 0.0),
            remote_access: o.bool("remoteAccess"),
            remote_port: o.int_or("remotePort", 7455),
            internet: o.bool("internet"),
            addresses,
        })
    }
}

/// Kind 18's envelope: `info` is the exact JSON bytes the Mac signed, so they are verified as sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedMacInfo {
    pub info: Vec<u8>,
    /// The signing key's uncompressed P-256 point.
    pub key: Vec<u8>,
    /// An ASN.1 DER ECDSA signature over `info` (SHA-256).
    pub sig: Vec<u8>,
}

impl SignedMacInfo {
    pub fn parse(bytes: &[u8]) -> Option<SignedMacInfo> {
        let m = parse_object(bytes)?;
        let o = J(&m);
        let dec = |k: &str| base64::engine::general_purpose::STANDARD.decode(o.string_req(k)?).ok();
        Some(SignedMacInfo { info: dec("info")?, key: dec("key")?, sig: dec("sig")? })
    }
}
