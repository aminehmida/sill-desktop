//! What the client shows: its state snapshot, its one-shot events and its own preferences.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::ledger::SettingsField;
use super::menu::Menu;
use super::modules::{ClipboardBridge, SoundPlayer, VideoSink};
use crate::crypto::KeyStore;
use crate::net::{Endpoint, HomeMac, LinkStats, SavedMac};
use crate::protocol::{AppInfo, CursorShape, HostSettingsState, LockShade, LockWhy, MacMenuItem, StreamSettings, StreamSource, WindowInfo};

/// Called whenever the state changed or an event was queued, from the client's own threads: wake
/// the UI thread (winit's `EventLoopProxy`, egui's `request_repaint`) and return at once.
pub type Waker = Arc<dyn Fn() + Send + Sync>;

/// What the client is made with.
#[derive(Clone)]
pub struct ClientConfig {
    /// Where the saved Macs, this device's preferences and (with the file key store) its key live.
    pub config_dir: PathBuf,
    /// This computer's name, as the Mac lists the device ("Sam’s ThinkPad").
    pub device_name: String,
    /// Its model or system ("Windows PC", "Linux").
    pub device_model: String,
    /// The hello's `appVersion`; a Mac's device floor compares it.
    pub app_version: String,
    /// The hello's `build`. Logged only.
    pub build: String,
    /// The display's refresh rate: the most frames a second worth asking for.
    pub display_fps: u32,
    /// Where the device key is kept; `None`: a [`crate::crypto::FileKeyStore`] in `config_dir`.
    pub key_store: Option<Arc<dyn KeyStore>>,
    pub video: Arc<dyn VideoSink>,
    pub sound: Option<Arc<dyn SoundPlayer>>,
    pub clipboard: Option<Arc<dyn ClipboardBridge>>,
    pub waker: Option<Waker>,
    /// Whether loopback addresses are kept for a saved Mac (a test host on the same computer).
    pub allow_loopback: bool,
    /// Browse the network for home doors from the start.
    pub discovery: bool,
    /// The Bonjour type browsed: `_sill._tcp`, or a test type.
    pub service_type: String,
    /// Tests only: what the browser lists is reached at this address instead.
    pub discovery_override: Option<Endpoint>,
}

impl ClientConfig {
    /// Defaults for everything but the directory and the video sink: no sound, no clipboard, no
    /// discovery until [`super::Client::start_discovery`].
    pub fn new(config_dir: impl Into<PathBuf>, video: Arc<dyn VideoSink>) -> ClientConfig {
        ClientConfig {
            config_dir: config_dir.into(),
            device_name: "Computer".to_string(),
            device_model: std::env::consts::OS.to_string(),
            app_version: env!("CARGO_PKG_VERSION").to_string(),
            build: "1".to_string(),
            display_fps: 60,
            key_store: None,
            video,
            sound: None,
            clipboard: None,
            waker: None,
            allow_loopback: false,
            discovery: false,
            service_type: crate::net::SERVICE_TYPE.to_string(),
            discovery_override: None,
        }
    }
}

/// Where the connection stands.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Phase {
    #[default]
    Idle,
    /// Dialing, or connected and waiting for the first window list. `reconnecting`: the stream
    /// screen stays up meanwhile (a blip).
    Connecting { mac_name: String, reconnecting: bool },
    /// A session with its first window list.
    Streaming,
}

/// How the session reaches the Mac.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Route {
    /// Through Tailscale.
    Tailscale,
    /// On this computer (a test host).
    Loopback,
    /// On this network (the home door, or a private address).
    Local,
}

/// A pairing ask waiting for its code: the Mac shows one now.
#[derive(Debug, Clone, PartialEq)]
pub struct HomeAsk {
    pub found: HomeMac,
    pub mac_fingerprint: [u8; 32],
}

/// The streamed app's menu bar as this device knows it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MenuSnapshot {
    pub version: Option<i64>,
    pub app: Option<String>,
    pub top: Vec<MacMenuItem>,
    pub top_note: Option<String>,
    pub top_stale: bool,
    /// Menus opened on this version, by id.
    pub menus: BTreeMap<String, Menu>,
}

/// A moment's line after a Paste.
#[derive(Debug, Clone, PartialEq)]
pub struct ClipboardNote {
    pub text: String,
    pub spoken: String,
    pub warning: bool,
}

/// This device's own settings, never sent; kept in `prefs.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Prefs {
    /// Three- and four-finger gestures go to the Mac (kind 28).
    pub trackpad_gestures: bool,
    pub hud: bool,
    pub sound_muted: bool,
    pub share_clipboard: bool,
    /// A Paste also presses ⌘V on the Mac once it has the text.
    pub paste_right_away: bool,
    pub reverse_scroll: bool,
    /// "list" or "tiles" ([`crate::net::ConnectionList`]).
    pub connection_style: Option<String>,
    /// Mac ID → group name.
    pub mac_groups: BTreeMap<String, String>,
}

impl Default for Prefs {
    fn default() -> Self {
        Prefs {
            trackpad_gestures: true,
            hud: false,
            sound_muted: false,
            share_clipboard: true,
            paste_right_away: true,
            reverse_scroll: false,
            connection_style: None,
            mac_groups: BTreeMap::new(),
        }
    }
}

/// Everything the screens show, as of one moment. Cheap to hold: pictures are shared.
#[derive(Debug, Clone, Default)]
pub struct ClientState {
    /// Bumped at every change.
    pub revision: u64,
    pub phase: Phase,
    /// The connect screen's line ("Connecting to…", why a dial failed, "Lost the connection…").
    pub status: Option<String>,
    /// A few seconds' line over the stream ("‹Mac› kept its setting.").
    pub notice: Option<String>,
    /// The last failure's words (a dial, a session's end, a pairing), kept until the next attempt.
    pub last_error: Option<String>,
    pub saved_macs: Vec<SavedMac>,
    /// The home doors Bonjour lists on this network.
    pub home_macs: Vec<HomeMac>,
    /// Those of `home_macs` that are no saved Mac's.
    pub unpaired_home_macs: Vec<HomeMac>,
    /// The saved Macs this network lists now, by `mac_id`.
    pub saved_macs_at_home: Vec<String>,
    pub current_mac: Option<SavedMac>,
    pub mac_name: String,
    pub windows: Vec<WindowInfo>,
    pub apps: Vec<AppInfo>,
    /// What streams now.
    pub active: StreamSource,
    /// JPEG bytes by window id (kind 3).
    pub thumbnails: HashMap<u32, Arc<Vec<u8>>>,
    /// PNG bytes by bundle ID (kind 4); kept across sessions.
    pub icons: HashMap<String, Arc<Vec<u8>>>,
    /// The coded picture's size, from the stream's SPS.
    pub video_size: Option<(u32, u32)>,
    /// The Mac's cursor (kind 14): a PNG with its hotspot and size in Mac points.
    pub cursor: Option<Arc<CursorShape>>,
    /// ≥ 1: the Mac takes kind 28's gestures.
    pub host_gestures: i64,
    pub host_version: Option<String>,
    /// ≥ 1: the Mac takes and sends the clipboard.
    pub host_clipboard: i64,
    /// ≥ 1: the Mac repeats a key held down ([`super::keys::KeyTyping`]).
    pub host_key_repeat: i64,
    /// Why the Mac shows nothing of itself; `None`: it shows itself.
    pub lock_why: Option<LockWhy>,
    /// This account's own desktop while another user has the screen, to look at.
    pub off_console_view: bool,
    /// What covers the stream.
    pub lock_shade: Option<LockShade>,
    /// The last second's link statistics.
    pub stats: Option<LinkStats>,
    pub route: Option<Route>,
    /// A pairing exchange is under way.
    pub pairing: bool,
    pub pair_error: Option<String>,
    /// The name of the home Mac being asked to show its code.
    pub asking: Option<String>,
    /// The Mac shows a code for this device: [`super::Client::pair_home_code`] takes it.
    pub home_ask: Option<HomeAsk>,
    /// The session's first kind 16 came.
    pub has_settings: bool,
    /// The last kind 16 as it came.
    pub host_settings: Option<HostSettingsState>,
    /// The Mac's settings with this device's unanswered picks over them: what the panel shows.
    pub shown_settings: Option<StreamSettings>,
    /// The fields with a pick the Mac has not answered yet.
    pub pending_settings: Vec<SettingsField>,
    pub menu: MenuSnapshot,
    pub has_menus: bool,
    /// Where the Mac's pointer is (frame fractions) while this device is not the one moving it.
    pub mac_pointer: Option<(f64, f64)>,
    /// Whether this device moved the pointer last.
    pub pointer_here: bool,
    /// Bumped when a report from the Mac moves the anchor a relative pointer carries on from.
    pub pointer_reseeds: u64,
    /// Aa, the text scale asked for; `None`: off.
    pub text_scale: Option<f64>,
    pub prefs: Prefs,
    /// Streaming, in front, used in the last 10 minutes: keep the screen awake.
    pub hold_awake: bool,
    /// The Mac's last list says it shares its clipboard (and it is not locked).
    pub clipboard_shared: bool,
    pub clipboard_note: Option<ClipboardNote>,
    /// Whether the app is in front, as the GUI last said.
    pub foreground: bool,
}

impl ClientState {
    /// This session's key is on the Mac's trust list (kind 16); `None` before the Mac says.
    pub fn session_paired(&self) -> Option<bool> {
        self.host_settings.as_ref().and_then(|h| h.session_paired)
    }

    /// The Mac has sound: its settings carry Send Audio, on or off.
    pub fn mac_has_sound(&self) -> bool {
        self.shown_settings.as_ref().map(|s| s.send_audio.is_some()).unwrap_or(false)
    }

    pub fn is_streaming(&self) -> bool {
        self.phase == Phase::Streaming
    }
}

/// One-shot things, in the order they happened ([`super::Client::poll_event`]).
#[derive(Debug, Clone, PartialEq)]
pub enum ClientEvent {
    /// A pairing finished and its Mac was saved; a session with it is being connected.
    Paired { mac_id: String },
    PairFailed { message: String },
    /// A home Mac shows its code for this device: ask the person for it.
    PairingCodeWanted { mac_name: String },
    /// A session got its first window list.
    Connected { mac_id: String, mac_name: String },
    /// A window's thumbnail arrived (`state.thumbnails`).
    Thumbnail { window_id: u32 },
    /// An app's icon arrived (`state.icons`).
    Icon { bundle_id: String },
    /// The Mac's cursor changed (`state.cursor`).
    CursorShape,
    /// The stream's picture size changed.
    VideoSize { width: u32, height: u32 },
    /// The Mac said why it is about to close the session (kind 22).
    Goodbye { reason: String, message: Option<String> },
    /// A session ended. `reconnecting`: the client is trying to get it back by itself.
    SessionEnded { message: Option<String>, reconnecting: bool },
    /// A few seconds' line ("‹Mac› kept its setting.").
    Notice(String),
    /// The Mac's text was written to this computer's clipboard.
    ClipboardFromMac { text: String },
    ClipboardNote(ClipboardNote),
    /// A menu item's press was answered.
    MenuPressed { ok: bool, note: Option<String> },
    /// The Mac kept its own value for these fields.
    SettingsRefused { fields: Vec<SettingsField> },
    /// A keyframe was asked for (kind 44).
    KeyframeRequested,
}
