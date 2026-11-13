//! The host's state and what it does with it: the trust list and the pairing window, the
//! sessions, the catalog, the stream's pacing and every device message.
//!
//! One `std::sync::Mutex<State>` guards everything; nothing awaits while it is held
//! (sending to a session is a push onto its unbounded queue, written by its own task).

use crate::events::*;
use crate::identity::{self, Identity};
use crate::media::{self, Clip, Tone};
use crate::pairing::{self, b64url, CloseReason, Verdict, Window};
use crate::tls::{PAIRING_ALPN, SESSION_ALPN};
use crate::wire::{self, kind, Header};
use crate::{Door, HostConfig};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, watch, Notify};

/// The version every window list carries (`WindowList.hostVersion`).
pub(crate) const HOST_VERSION: &str = "0.6.1";
/// `StreamServer.tickInterval`.
const TICK: Duration = Duration::from_millis(30);
/// A session is live for this long after its last input (`StreamServer`'s tick rule).
const LIVE_AFTER_INPUT: Duration = Duration::from_secs(3);
/// `DoorPolicy.maxRemoteSessions`.
const MAX_REMOTE_SESSIONS: usize = 8;
/// `MenuRefusal.changedNote`.
const MENUS_CHANGED: &str = "The menus changed. Open the menu again.";
/// `MenuRefusal.tooManyNote`.
const MENUS_TOO_MANY: &str = "Too many requests. Open the menu again.";
/// `MenuRefusal.unavailableNote`.
const MENU_UNAVAILABLE: &str = "It isn’t available right now.";
/// `LockPolicy.menuNote`.
const MENUS_LOCKED: &str = "The Mac is locked.";
/// `Clipboard.maxTextBytes`.
const MAX_CLIPBOARD_TEXT: usize = 256 * 1024;
/// `QualityPreset`'s raw values (`SettingsChoices.bitrate`).
const BITRATES: [i64; 7] = [4_000_000, 8_000_000, 15_000_000, 25_000_000, 40_000_000, 80_000_000, 150_000_000];

/// What the lock's witnesses read (`LockPolicy.Reading`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Reading {
    Unlocked,
    ScreenLocked,
    OffConsole,
}

impl Reading {
    fn locked(self) -> bool {
        self != Reading::Unlocked
    }
}

/// One fake window (`WindowInfo`).
#[derive(Debug, Clone)]
pub(crate) struct WindowInfo {
    pub id: u32,
    pub title: String,
    pub app: usize,
    pub width: u32,
    pub height: u32,
}

/// An installed app (`AppInfo`) and its icon's colour.
pub(crate) struct App {
    pub name: &'static str,
    pub bundle_id: &'static str,
    pub rgb: [u8; 3],
}

pub(crate) const APPS: [App; 5] = [
    App { name: "TextEdit", bundle_id: "com.apple.TextEdit", rgb: [0x4A, 0x90, 0xD9] },
    App { name: "Safari", bundle_id: "com.apple.Safari", rgb: [0x1E, 0xA7, 0xE1] },
    App { name: "Terminal", bundle_id: "com.apple.Terminal", rgb: [0x2B, 0x2B, 0x2B] },
    App { name: "Finder", bundle_id: "com.apple.finder", rgb: [0x3B, 0x82, 0xF6] },
    App { name: "Notes", bundle_id: "com.apple.Notes", rgb: [0xF5, 0xC5, 0x18] },
];
const FINDER: usize = 3;

/// What goes to a session's writer.
pub(crate) enum Out {
    /// One whole message; `counted`: it counts in the session's `inflight`.
    Data(Arc<Vec<u8>>, bool),
    /// Close the connection once everything before this is written.
    Close,
}

pub(crate) struct Client {
    tx: mpsc::UnboundedSender<Out>,
    inflight: Arc<AtomicUsize>,
    /// Ends the session's reader at once (a peer that stopped reading).
    stop: Arc<Notify>,
    connected_at: Instant,
    /// Since when more than 2 messages have been untaken without a break.
    stalled_since: Option<Instant>,
    last_sent: Instant,
    fingerprint: [u8; 32],
    /// Its key was on the trust list when it was admitted.
    was_paired: bool,
    needs_keyframe: bool,
    hello_seen: bool,
    plays_audio: bool,
    audio_epoch_sent: Option<u16>,
    inputs_read: u64,
    fps: Option<i64>,
    last_published: Option<Value>,
    apps_sent: bool,
    clipboard: bool,
    menus: bool,
    closing: bool,
    settings_arrivals: Vec<Instant>,
    gesture_arrivals: Vec<Instant>,
    clipboard_arrivals: Vec<Instant>,
    fetch_arrivals: Vec<Instant>,
    press_arrivals: Vec<Instant>,
    last_keyframe_request: Option<Instant>,
}

struct AudioRun {
    epoch: u16,
    seq: u32,
    next: Instant,
    start_wall: f64,
}

pub(crate) struct Settings {
    max_fps: i64,
    bitrate: i64,
    capture_scale: f64,
    prioritize_speed: bool,
    direct_wireless: bool,
    send_audio: bool,
}

pub(crate) struct State {
    next_conn: u64,
    pub clients: BTreeMap<u64, Client>,
    pub paired: Vec<PairedDevice>,
    pub window: Window,
    key_cache: HashMap<String, [u8; 32]>,
    pub reading: Reading,
    restore_after_unlock: Option<Source>,
    windows: Vec<WindowInfo>,
    next_window_id: u32,
    pub active: Source,
    settings: Settings,
    fps: i64,
    clip_index: usize,
    next_frame: Instant,
    restart_pending: bool,
    last_restart: Instant,
    audio: Option<AudioRun>,
    audio_epoch: u16,
    next_tick: Instant,
    last_input: Option<Instant>,
    pub clipboard_text: Option<String>,
    cursor_shape: usize,
    menu_version: i64,
    menu_app: Option<usize>,
    pub last_viewport: Option<Viewport>,
    pub stopped: bool,
}

pub(crate) struct Inner {
    pub cfg: HostConfig,
    pub identity: Identity,
    pub recognition_key: [u8; 32],
    pub addr: SocketAddr,
    launch_id: String,
    /// The device gate's floor (`DeviceGate`): `None` is "0", which admits every device.
    floor: Option<Vec<u64>>,
    pub state: Mutex<State>,
    /// Signalled when a code's key is derived and when a session ends.
    pub changed: Condvar,
    pub log: EventLog,
    pub pacer: Notify,
    pub shutdown: watch::Sender<bool>,
    pub frames_sent: AtomicU64,
    pub keyframes_sent: AtomicU64,
    clip: Clip,
    tone: Tone,
    icons: Vec<Vec<u8>>,
    pub handle: tokio::runtime::Handle,
}

/// `SillVersion`: one leading "v" dropped, the longest prefix of digits and dots, at most
/// 8 parts of 1 to 9 digits; trailing zeros dropped.
pub(crate) fn parse_version(text: &str) -> Option<Vec<u64>> {
    let t = text.trim();
    let t = t.strip_prefix(['v', 'V']).unwrap_or(t);
    let prefix: String = t.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
    let prefix = prefix.trim_end_matches('.');
    if !prefix.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    let parts: Vec<&str> = prefix.split('.').collect();
    if parts.len() > 8 {
        return None;
    }
    let mut numbers = Vec::new();
    for p in parts {
        if p.is_empty() || p.len() > 9 {
            return None;
        }
        numbers.push(p.parse::<u64>().ok()?);
    }
    while numbers.last() == Some(&0) {
        numbers.pop();
    }
    Some(numbers)
}

fn version_at_least(v: &[u64], floor: &[u64]) -> bool {
    for i in 0..v.len().max(floor.len()) {
        let (a, b) = (v.get(i).copied().unwrap_or(0), floor.get(i).copied().unwrap_or(0));
        if a != b {
            return a > b;
        }
    }
    true
}

/// `SillVersion.description`: at least two parts.
fn version_text(v: &[u64]) -> String {
    let mut c = v.to_vec();
    while c.len() < 2 {
        c.push(0);
    }
    c.iter().map(u64::to_string).collect::<Vec<_>>().join(".")
}

/// At most `per_second` arrivals in any second (`ClipboardPolicy.admits` and the
/// coordinator's settings, gesture and menu rates).
fn admit(arrivals: &mut Vec<Instant>, per_second: usize, now: Instant) -> bool {
    arrivals.retain(|t| now.duration_since(*t) < Duration::from_secs(1));
    if arrivals.len() >= per_second {
        return false;
    }
    arrivals.push(now);
    true
}

/// The server's gate while the Mac reads locked (`LockPolicy.withheld`): thumbnails,
/// icons, the installed apps, the sound, the clipboard, the cursor shape and the pointer.
fn withheld(kind: u8, reading: Reading) -> bool {
    reading.locked()
        && matches!(
            kind,
            kind::THUMBNAIL
                | kind::APP_ICON
                | kind::APP_LIST
                | kind::AUDIO
                | kind::CLIPBOARD
                | kind::CURSOR_SHAPE
                | kind::MAC_POINTER
        )
}

struct MenuItem {
    title: &'static str,
    key: Option<&'static str>,
    enabled: bool,
    mark: Option<&'static str>,
    separator: bool,
    children: Vec<MenuItem>,
}

fn item(title: &'static str, key: Option<&'static str>) -> MenuItem {
    MenuItem { title, key, enabled: true, mark: None, separator: false, children: Vec::new() }
}

fn separator() -> MenuItem {
    MenuItem { title: "", key: None, enabled: true, mark: None, separator: true, children: Vec::new() }
}

/// The fake menu bar every app shows, the Apple menu (index 0) left out as on the real
/// host: ids are "1" (the app's own menu), "2" File, "3" Edit, "4" View, and their items
/// "2.0", "2.1", … with separators counted.
fn menu_bar(app: &'static str) -> Vec<MenuItem> {
    let menu = |title, children| MenuItem { children, ..item(title, None) };
    vec![
        menu(app, vec![item("About This App", None), separator(), item("Settings…", Some("⌘,")), item("Hide", Some("⌘H"))]),
        menu(
            "File",
            vec![
                item("New", Some("⌘N")),
                item("Open…", Some("⌘O")),
                separator(),
                item("Close", Some("⌘W")),
                MenuItem { enabled: false, ..item("Save…", Some("⌘S")) },
                menu("Export As", vec![item("PDF…", None), item("Rich Text", None)]),
            ],
        ),
        menu(
            "Edit",
            vec![
                MenuItem { enabled: false, ..item("Undo", Some("⌘Z")) },
                item("Redo", Some("⇧⌘Z")),
                separator(),
                item("Cut", Some("⌘X")),
                item("Copy", Some("⌘C")),
                item("Paste", Some("⌘V")),
            ],
        ),
        menu(
            "View",
            vec![MenuItem { mark: Some("✓"), ..item("Show Toolbar", Some("⌥⌘T")) }, item("Enter Full Screen", Some("fn F"))],
        ),
    ]
}

/// The item at an id like "2.5.1": the first index counts from the Apple menu's 0.
fn menu_lookup<'a>(bar: &'a [MenuItem], id: &str) -> Option<&'a MenuItem> {
    let mut parts = id.split('.').map(|p| p.parse::<usize>().ok());
    let first = parts.next()??;
    let mut at = bar.get(first.checked_sub(1)?)?;
    for p in parts {
        at = at.children.get(p?)?;
    }
    Some(at)
}

fn menu_item_json(id: String, m: &MenuItem) -> Value {
    if m.separator {
        return json!({"separator": true});
    }
    let mut o = json!({"id": id, "title": m.title});
    if !m.enabled {
        o["enabled"] = json!(false);
    }
    if let Some(mark) = m.mark {
        o["mark"] = json!(mark);
    }
    if let Some(key) = m.key {
        o["key"] = json!(key);
    }
    if !m.children.is_empty() {
        o["submenu"] = json!(true);
    }
    o
}

#[derive(Deserialize)]
struct PairRequestWire {
    v: i64,
    method: String,
    proof: String,
    name: String,
    model: Option<String>,
    cable: Option<bool>,
}

#[derive(Deserialize)]
struct LaunchAppWire {
    #[serde(rename = "bundleID")]
    bundle_id: String,
}

#[derive(Deserialize)]
struct WindowCommandWire {
    id: u32,
    action: String,
}

#[derive(Deserialize)]
struct MenuRequestWire {
    version: Option<i64>,
    id: Option<String>,
    title: Option<String>,
    token: Option<i64>,
}

#[derive(Deserialize)]
struct GestureWire {
    gesture: String,
    fingers: Option<i64>,
}

#[derive(Deserialize)]
struct ClipboardWire {
    text: Option<String>,
    #[allow(dead_code)]
    image: Option<String>,
    stop: Option<bool>,
}

/// A kind 20 payload (`PairResult`), nil fields left out.
fn pair_refusal(reason: &str) -> Value {
    json!({"ok": false, "reason": reason})
}

impl Inner {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        cfg: HostConfig,
        identity: Identity,
        recognition_key: [u8; 32],
        paired: Vec<PairedDevice>,
        addr: SocketAddr,
        shutdown: watch::Sender<bool>,
        handle: tokio::runtime::Handle,
    ) -> Inner {
        let now = Instant::now();
        let windows = vec![
            WindowInfo { id: 101, title: "Untitled.txt".into(), app: 0, width: 800, height: 600 },
            WindowInfo { id: 102, title: "Sill — Stream your Mac".into(), app: 1, width: 1200, height: 800 },
            WindowInfo { id: 103, title: "zsh — 80×24".into(), app: 2, width: 720, height: 480 },
        ];
        let floor = cfg.minimum_device_version.as_deref().and_then(parse_version).filter(|v| !v.is_empty());
        let settings = Settings {
            max_fps: 120,
            bitrate: 15_000_000,
            capture_scale: 2.0,
            prioritize_speed: false,
            direct_wireless: false,
            send_audio: cfg.send_audio,
        };
        Inner {
            identity,
            recognition_key,
            addr,
            launch_id: identity::uuid(),
            floor,
            state: Mutex::new(State {
                next_conn: 0,
                clients: BTreeMap::new(),
                paired,
                window: Window::Closed(CloseReason::None),
                key_cache: HashMap::new(),
                reading: Reading::Unlocked,
                restore_after_unlock: None,
                windows,
                next_window_id: 104,
                active: Source::None,
                settings,
                fps: 60,
                clip_index: 0,
                next_frame: now,
                restart_pending: false,
                last_restart: now,
                audio: None,
                audio_epoch: 0,
                next_tick: now,
                last_input: None,
                clipboard_text: None,
                cursor_shape: 0,
                menu_version: 1,
                menu_app: None,
                last_viewport: None,
                stopped: false,
            }),
            changed: Condvar::new(),
            log: EventLog::default(),
            pacer: Notify::new(),
            shutdown,
            frames_sent: AtomicU64::new(0),
            keyframes_sent: AtomicU64::new(0),
            clip: media::clip(),
            tone: media::tone(),
            icons: APPS.iter().map(|a| media::icon_png(a.rgb)).collect(),
            handle,
            cfg,
        }
    }

    pub fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn next_conn(&self) -> u64 {
        let mut st = self.lock();
        st.next_conn += 1;
        st.next_conn
    }

    // MARK: Sending

    fn push(c: &mut Client, bytes: Arc<Vec<u8>>, counted: bool) {
        if c.closing {
            return;
        }
        if counted {
            c.inflight.fetch_add(1, Ordering::SeqCst);
        }
        c.last_sent = Instant::now();
        let _ = c.tx.send(Out::Data(bytes, counted));
    }

    fn send_to(&self, st: &mut State, id: u64, kind: u8, payload: &[u8]) {
        if withheld(kind, st.reading) {
            return;
        }
        if let Some(c) = st.clients.get_mut(&id) {
            Self::push(c, Arc::new(wire::message(kind, wire::now(), false, payload)), true);
        }
    }

    fn broadcast(&self, st: &mut State, kind: u8, payload: &[u8]) {
        if withheld(kind, st.reading) {
            return;
        }
        let m = Arc::new(wire::message(kind, wire::now(), false, payload));
        for c in st.clients.values_mut() {
            Self::push(c, m.clone(), true);
        }
    }

    /// Kind 22 to one session, then its close (`StreamServer.goodbye`).
    fn goodbye(&self, st: &mut State, id: u64, goodbye: &Goodbye) {
        if let Some(c) = st.clients.get_mut(&id) {
            let payload = serde_json::to_vec(goodbye).unwrap_or_default();
            Self::push(c, Arc::new(wire::message(kind::GOODBYE, wire::now(), false, &payload)), true);
            c.closing = true;
            let _ = c.tx.send(Out::Close);
        }
    }

    pub fn goodbye_all(&self, st: &mut State, goodbye: &Goodbye) {
        let ids: Vec<u64> = st.clients.keys().copied().collect();
        for id in ids {
            self.goodbye(st, id, goodbye);
        }
    }

    // MARK: The catalog

    fn is_paired(st: &State, fp: &[u8; 32]) -> bool {
        let text = b64url(fp);
        st.paired.iter().any(|d| d.fingerprint == text)
    }

    /// The window list (`StreamCoordinator.listMessage` through `LockPolicy.list`).
    fn list_json(&self, st: &State) -> Vec<u8> {
        let locked = st.reading.locked();
        let windows: Vec<Value> = if locked {
            Vec::new()
        } else {
            st.windows
                .iter()
                .map(|w| {
                    json!({"id": w.id, "title": w.title, "appName": APPS[w.app].name, "bundleID": APPS[w.app].bundle_id,
                           "width": w.width, "height": w.height})
                })
                .collect()
        };
        let mut o = json!({
            "macName": self.cfg.name,
            "windows": windows,
            "active": st.active.to_json(),
            "launchID": self.launch_id,
            "hostVersion": HOST_VERSION,
            "protocol": 1,
            "gestures": 1,
            "keyRepeat": 1,
        });
        if !locked && self.cfg.share_clipboard {
            o["clipboard"] = json!(1);
        }
        if locked {
            o["locked"] = json!(true);
            if st.reading == Reading::OffConsole {
                o["offConsole"] = json!(true);
            }
        }
        serde_json::to_vec(&o).unwrap_or_default()
    }

    fn broadcast_list(&self, st: &mut State) {
        let list = self.list_json(st);
        self.broadcast(st, kind::WINDOW_LIST, &list);
    }

    /// Kind 16 for one session (`StreamCoordinator.settingsState`).
    fn settings_state(&self, st: &State, id: u64, answering: Option<i64>) -> Value {
        let s = &st.settings;
        let mut o = json!({
            "settings": {
                "maxFPS": s.max_fps, "bitrate": s.bitrate, "captureScale": s.capture_scale,
                "prioritizeSpeed": s.prioritize_speed, "virtualDisplay": false,
                "directWireless": s.direct_wireless, "sendAudio": s.send_audio,
            },
            "persistent": false,
            "virtualDisplayAvailable": false,
            "virtualDisplayNote": "Start SillHost with --virtual-display to use it.",
            "softwareEncoder": false,
            "away": {
                "homeBitrate": s.bitrate, "homeCaptureScale": s.capture_scale,
                "awayBitrate": 4_000_000, "awayCaptureScale": 1.0,
                "thisConnectionAway": false, "awayRunning": false,
            },
            "sessionPaired": st.clients.get(&id).is_some_and(|c| Self::is_paired(st, &c.fingerprint)),
        });
        if st.active != Source::None {
            o["stream"] = json!({
                "width": self.clip.width, "height": self.clip.height, "fps": st.fps,
                "mbps": s.bitrate * st.fps / 60 / 1_000_000, "onVirtualDisplay": false,
            });
        }
        if let Some(t) = answering {
            o["answering"] = json!(t);
        }
        o
    }

    /// To every session its own state, when that differs from the last one it was sent
    /// (`StreamCoordinator.publishSettings`).
    fn publish_settings(&self, st: &mut State) {
        let ids: Vec<u64> = st.clients.keys().copied().collect();
        for id in ids {
            let state = self.settings_state(st, id, None);
            let Some(c) = st.clients.get(&id) else { continue };
            if c.last_published.is_none() || c.last_published.as_ref() == Some(&state) {
                continue;
            }
            self.send_to(st, id, kind::HOST_SETTINGS, state.to_string().as_bytes());
            if let Some(c) = st.clients.get_mut(&id) {
                c.last_published = Some(state);
            }
        }
    }

    /// Kind 18 (`SignedMacInfo.signing`): `info` is base64 of the exact JSON bytes of a
    /// `MacInfo`, `key` base64 of the 65-byte point, `sig` base64 of a DER ECDSA P-256 /
    /// SHA-256 signature over those bytes. Plain base64, padded.
    fn mac_info(&self) -> Vec<u8> {
        let remote = self.cfg.door == Door::Remote;
        let addresses: Vec<Value> = if remote && self.cfg.remote_access {
            let host = if self.addr.ip().is_unspecified() { "127.0.0.1".to_string() } else { self.addr.ip().to_string() };
            vec![json!({"host": host, "kind": "lan", "via": "This Mac"})]
        } else {
            Vec::new()
        };
        let info = json!({
            "v": 1,
            "macID": pairing::mac_id(&self.identity.fingerprint),
            "name": self.cfg.name,
            "issuedAt": wire::now(),
            "remoteAccess": remote && self.cfg.remote_access,
            "remotePort": if remote { self.addr.port() } else { 7455 },
            "internet": false,
            "addresses": addresses,
        });
        let bytes = serde_json::to_vec(&info).unwrap_or_default();
        let signed = json!({
            "info": STANDARD.encode(&bytes),
            "key": STANDARD.encode(self.identity.point),
            "sig": STANDARD.encode(self.identity.sign(&bytes)),
        });
        signed.to_string().into_bytes()
    }

    fn cursor_blob(&self, st: &State) -> Vec<u8> {
        let c = media::cursor(st.cursor_shape);
        wire::cursor_shape(c.hotspot, c.size, &c.png)
    }

    fn send_apps_and_icons(&self, st: &mut State, id: u64) {
        for (app, png) in APPS.iter().zip(&self.icons) {
            self.send_to(st, id, kind::APP_ICON, &wire::icon(app.bundle_id, png));
        }
        let apps: Vec<Value> = APPS.iter().map(|a| json!({"name": a.name, "bundleID": a.bundle_id})).collect();
        self.send_to(st, id, kind::APP_LIST, Value::from(apps).to_string().as_bytes());
        if let Some(c) = st.clients.get_mut(&id) {
            c.apps_sent = true;
        }
    }

    fn send_thumbnails(&self, st: &mut State, id: Option<u64>) {
        let blobs: Vec<Vec<u8>> = st
            .windows
            .iter()
            .map(|w| wire::thumbnail(w.id, media::THUMBNAILS[w.id as usize % media::THUMBNAILS.len()]))
            .collect();
        for b in blobs {
            match id {
                Some(id) => self.send_to(st, id, kind::THUMBNAIL, &b),
                None => self.broadcast(st, kind::THUMBNAIL, &b),
            }
        }
    }

    /// The catalog on connect (`StreamCoordinator.sendCatalog`): kinds 2, 16, 18, then the
    /// icons (4) and the installed apps (5), the cursor's shape (14), then every thumbnail
    /// (3). No icons, apps, shape or thumbnails while the Mac reads locked.
    fn send_catalog(&self, st: &mut State, id: u64) {
        let list = self.list_json(st);
        self.send_to(st, id, kind::WINDOW_LIST, &list);
        let state = self.settings_state(st, id, None);
        self.send_to(st, id, kind::HOST_SETTINGS, state.to_string().as_bytes());
        if let Some(c) = st.clients.get_mut(&id) {
            c.last_published = Some(state);
        }
        let info = self.mac_info();
        self.send_to(st, id, kind::MAC_INFO, &info);
        if !st.reading.locked() {
            self.send_apps_and_icons(st, id);
            let shape = self.cursor_blob(st);
            self.send_to(st, id, kind::CURSOR_SHAPE, &shape);
            self.send_thumbnails(st, Some(id));
        }
    }

    // MARK: The stream

    /// `StreamCoordinator.wantedFPS`: the highest rate a device asked for (60 before any
    /// has said), from 24 to the frame rate limit.
    fn wanted_fps(st: &State) -> i64 {
        let asked = st.clients.values().filter_map(|c| c.fps).max().unwrap_or(60);
        asked.max(24).min(st.settings.max_fps)
    }

    /// Streams `source` in place of whatever streams now (`StreamCoordinator.select`): the
    /// clip from its first keyframe, every session waiting for it, then the window list
    /// with the new `active` and the settings' `stream`.
    fn select(&self, st: &mut State, source: Source) {
        let mut source = source;
        if let Source::Window(id) = source {
            if !st.windows.iter().any(|w| w.id == id) {
                source = Source::None;
            } else if st.reading.locked() {
                // No window's picture while locked: the Desktop, and the window after the unlock.
                st.restore_after_unlock = Some(source);
                source = Source::Desktop;
            }
        }
        st.active = source;
        for c in st.clients.values_mut() {
            c.needs_keyframe = true;
        }
        st.clip_index = 0;
        st.restart_pending = false;
        st.next_frame = Instant::now();
        st.fps = Self::wanted_fps(st);
        st.audio = None;
        self.refresh_menus(st);
        self.broadcast_list(st);
        self.publish_settings(st);
        self.pacer.notify_one();
    }

    /// One frame of the clip to every session, as `StreamCoordinator.startPipeline`'s
    /// `onEncoded` and `StreamServer.broadcast` send it: before a keyframe its parameter
    /// sets (kind 0, the keyframe flag set, the frame's time stamp) to every session; a
    /// session that waits for a keyframe gets nothing until one comes, then the parameter
    /// sets once more and the keyframe; a session with more than 2 messages untaken loses
    /// a delta, waits for a keyframe, and has the host make one; one that takes nothing for
    /// 4 s is dropped.
    fn emit_frame(&self, st: &mut State, now: Instant) {
        if st.restart_pending {
            st.restart_pending = false;
            st.clip_index = 0;
            st.last_restart = now;
        }
        let unit = &self.clip.units[st.clip_index % self.clip.units.len()];
        st.clip_index = (st.clip_index + 1) % self.clip.units.len();
        let ts = wire::now();
        let sets = Arc::new(wire::message(kind::PARAMETER_SETS, ts, true, &self.clip.parameter_sets));
        let frame = Arc::new(wire::message(kind::FRAME, ts, unit.keyframe, &unit.data));
        let mut want_keyframe = false;
        for c in st.clients.values_mut() {
            // A peer that has taken nothing for 4 s stopped reading (8 s grace after it
            // connected): dropped, as `StreamServer.send` drops one ("Client not draining").
            if c.inflight.load(Ordering::SeqCst) <= 2 {
                c.stalled_since = None;
            } else if now.duration_since(*c.stalled_since.get_or_insert(now)) > Duration::from_secs(4)
                && now.duration_since(c.connected_at) > Duration::from_secs(8)
            {
                c.closing = true;
                c.stop.notify_one();
                continue;
            }
            if unit.keyframe {
                Self::push(c, sets.clone(), true);
            }
            if c.needs_keyframe {
                if !unit.keyframe {
                    continue;
                }
                Self::push(c, sets.clone(), true);
                c.needs_keyframe = false;
            } else if c.inflight.load(Ordering::SeqCst) > 2 && !unit.keyframe {
                c.needs_keyframe = true;
                want_keyframe = true;
                continue;
            }
            if c.closing {
                continue;
            }
            Self::push(c, frame.clone(), true);
            self.frames_sent.fetch_add(1, Ordering::Relaxed);
            if unit.keyframe {
                self.keyframes_sent.fetch_add(1, Ordering::Relaxed);
            }
        }
        if want_keyframe && now.duration_since(st.last_restart) >= Duration::from_millis(250) {
            st.restart_pending = true;
        }
    }

    /// One packet of the tone (kind 43), its epoch's format first to a session that has
    /// not had it (`StreamServer.broadcastAudio`).
    fn emit_audio(&self, st: &mut State) {
        let Some(run) = st.audio.as_mut() else { return };
        let (epoch, seq, start_wall) = (run.epoch, run.seq, run.start_wall);
        run.seq = run.seq.wrapping_add(1);
        let app = match st.active {
            Source::Window(id) => st.windows.iter().find(|w| w.id == id).map(|w| APPS[w.app].name),
            _ => None,
        };
        let mut format = json!({
            "codec": "aac-eld", "sampleRate": media::TONE_SAMPLE_RATE, "channels": 2,
            "framesPerPacket": media::TONE_FRAMES_PER_PACKET, "primingFrames": media::TONE_PRIMING_FRAMES,
            "bitrate": 128_000, "epoch": epoch, "cookie": STANDARD.encode(self.tone.cookie),
            "source": if app.is_some() { "app" } else { "test" },
        });
        if let Some(app) = app {
            format["app"] = json!(app);
        }
        // The first decoded sample of packet `seq` is segment frame seq × F − priming.
        let frames = f64::from(seq) * f64::from(media::TONE_FRAMES_PER_PACKET) - f64::from(media::TONE_PRIMING_FRAMES);
        let ts = start_wall + frames / f64::from(media::TONE_SAMPLE_RATE);
        let packet = self.tone.packets[seq as usize % self.tone.packets.len()];
        let packets = Arc::new(wire::message(kind::AUDIO, ts, false, &wire::audio_packets(epoch, seq, seq == 0, &[packet])));
        let format =
            Arc::new(wire::message(kind::AUDIO, wire::now(), false, &wire::audio_format(format.to_string().as_bytes())));
        for c in st.clients.values_mut().filter(|c| c.plays_audio) {
            if c.audio_epoch_sent != Some(epoch) {
                Self::push(c, format.clone(), false);
                c.audio_epoch_sent = Some(epoch);
            }
            Self::push(c, packets.clone(), false);
        }
    }

    /// Does what is due (a frame, a sound packet, a tick) and says when to wake next.
    pub fn pace(&self, now: Instant) -> Option<Instant> {
        let mut st = self.lock();
        if st.clients.is_empty() || st.stopped {
            return None;
        }
        let mut next: Option<Instant> = None;
        let mut sooner = |t: Instant| next = Some(next.map_or(t, |n: Instant| n.min(t)));
        if st.active != Source::None {
            if now >= st.next_frame {
                self.emit_frame(&mut st, now);
                let interval = Duration::from_secs_f64(1.0 / st.fps.max(1) as f64);
                st.next_frame += interval;
                if st.next_frame < now {
                    st.next_frame = now + interval;
                }
            }
            sooner(st.next_frame);
            // The sound: only while Send Audio is on, the Mac is not locked, and a device
            // whose hello listed the codec is connected.
            let listeners = st.settings.send_audio && !st.reading.locked() && st.clients.values().any(|c| c.plays_audio);
            if listeners {
                if st.audio.is_none() {
                    st.audio_epoch = st.audio_epoch.wrapping_add(1);
                    st.audio = Some(AudioRun { epoch: st.audio_epoch, seq: 0, next: now, start_wall: wire::now() });
                }
                let step = Duration::from_secs_f64(f64::from(media::TONE_FRAMES_PER_PACKET) / f64::from(media::TONE_SAMPLE_RATE));
                let mut sent = 0;
                while st.audio.as_ref().is_some_and(|r| now >= r.next) && sent < 8 {
                    self.emit_audio(&mut st);
                    if let Some(r) = st.audio.as_mut() {
                        r.next += step;
                    }
                    sent += 1;
                }
                if let Some(r) = st.audio.as_mut() {
                    if r.next < now {
                        r.next = now + step;
                    }
                    sooner(r.next);
                }
            } else {
                st.audio = None;
            }
        }
        // Ticks (kind 13) while a session is live: something streams, or a device sent
        // input in the last 3 s. A TLS session skips a tick right after any other message.
        let live = st.active != Source::None || st.last_input.is_some_and(|t| now.duration_since(t) < LIVE_AFTER_INPUT);
        if live {
            if now >= st.next_tick {
                let tick = Arc::new(wire::message(kind::TICK, wire::now(), false, &[]));
                for c in st.clients.values_mut() {
                    if now.duration_since(c.last_sent) >= TICK {
                        Self::push(c, tick.clone(), false);
                    }
                }
                st.next_tick = now + TICK;
            }
            sooner(st.next_tick);
        }
        next
    }

    // MARK: The menus (kinds 24, 25, 27)

    fn menu_target(st: &State) -> Option<usize> {
        if st.reading.locked() {
            return None;
        }
        match st.active {
            Source::None => None,
            Source::Desktop => Some(FINDER),
            Source::Window(id) => st.windows.iter().find(|w| w.id == id).map(|w| w.app),
        }
    }

    fn top_level(st: &State, answering: Option<i64>) -> Value {
        let mut o = match st.menu_app {
            None => json!({"version": st.menu_version, "menus": []}),
            Some(app) => {
                let menus: Vec<Value> = menu_bar(APPS[app].name)
                    .iter()
                    .enumerate()
                    .map(|(i, m)| json!({"id": (i + 1).to_string(), "title": m.title, "submenu": true}))
                    .collect();
                json!({"version": st.menu_version, "app": APPS[app].name, "bundleID": APPS[app].bundle_id, "menus": menus})
            }
        };
        if let Some(t) = answering {
            o["answering"] = json!(t);
        }
        o
    }

    /// The app whose menus these are changed (to none included): the version moves and
    /// every subscriber gets the new top level (`MenuMirror`).
    fn refresh_menus(&self, st: &mut State) {
        let target = Self::menu_target(st);
        if target == st.menu_app {
            return;
        }
        st.menu_app = target;
        st.menu_version += 1;
        let top = Self::top_level(st, None).to_string();
        let ids: Vec<u64> = st.clients.iter().filter(|(_, c)| c.menus).map(|(id, _)| *id).collect();
        for id in ids {
            self.send_to(st, id, kind::MAC_MENU, top.as_bytes());
        }
    }

    fn fetch_menu(&self, st: &mut State, id: u64, r: MenuRequestWire) {
        let now = Instant::now();
        let v = st.menu_version;
        let answer = |o: Value| o.to_string().into_bytes();
        let Some(c) = st.clients.get_mut(&id) else { return };
        if !admit(&mut c.fetch_arrivals, 20, now) {
            let a = answer(json!({"version": v, "answering": r.token, "menu": r.id, "items": [], "note": MENUS_TOO_MANY}));
            self.log.push(DeviceEvent::Refused { conn: id, kind: kind::FETCH_MENU, reason: "rate".into() });
            return self.send_to(st, id, kind::MAC_MENU, &a);
        }
        let Some(menu_id) = r.id.clone() else {
            // The subscription: the top level now, and every later one.
            c.menus = true;
            let top = Self::top_level(st, r.token).to_string();
            return self.send_to(st, id, kind::MAC_MENU, top.as_bytes());
        };
        if st.reading.locked() {
            let a = answer(json!({"version": v, "answering": r.token, "menu": menu_id, "items": [], "note": MENUS_LOCKED}));
            self.log.push(DeviceEvent::Refused { conn: id, kind: kind::FETCH_MENU, reason: "menus".into() });
            return self.send_to(st, id, kind::MAC_MENU, &a);
        }
        let changed = json!({"version": v, "answering": r.token, "menu": menu_id, "items": [], "note": MENUS_CHANGED});
        if !c.menus || r.version != Some(v) {
            return self.send_to(st, id, kind::MAC_MENU, &answer(changed));
        }
        let Some(app) = st.menu_app else { return self.send_to(st, id, kind::MAC_MENU, &answer(changed)) };
        let bar = menu_bar(APPS[app].name);
        let found = menu_lookup(&bar, &menu_id).filter(|m| !m.children.is_empty() && Some(m.title) == r.title.as_deref());
        let a = match found {
            None => answer(changed),
            Some(m) => {
                let items: Vec<Value> =
                    m.children.iter().enumerate().map(|(i, x)| menu_item_json(format!("{menu_id}.{i}"), x)).collect();
                answer(json!({"version": v, "answering": r.token, "menu": menu_id, "items": items}))
            }
        };
        self.send_to(st, id, kind::MAC_MENU, &a);
    }

    /// Kind 25. Returns whether the item was "pressed".
    fn press_menu(&self, st: &mut State, id: u64, r: &MenuRequestWire) -> bool {
        let now = Instant::now();
        let v = st.menu_version;
        let Some(c) = st.clients.get_mut(&id) else { return false };
        let refuse = |note: &str| json!({"version": v, "answering": r.token, "pressed": false, "note": note}).to_string();
        let (pressed, a) = if !admit(&mut c.press_arrivals, 4, now) {
            self.log.push(DeviceEvent::Refused { conn: id, kind: kind::PRESS_MENU_ITEM, reason: "rate".into() });
            (false, refuse(MENUS_TOO_MANY))
        } else if st.reading.locked() {
            self.log.push(DeviceEvent::Refused { conn: id, kind: kind::PRESS_MENU_ITEM, reason: "menus".into() });
            (false, refuse(MENUS_LOCKED))
        } else if !c.menus || r.version != Some(v) {
            (false, refuse(MENUS_CHANGED))
        } else {
            let bar = st.menu_app.map(|app| menu_bar(APPS[app].name)).unwrap_or_default();
            match r.id.as_deref().and_then(|i| menu_lookup(&bar, i)) {
                Some(m) if !m.separator && r.title.as_deref() == Some(m.title) => {
                    if m.enabled && m.children.is_empty() && r.id.as_deref().is_some_and(|i| i.contains('.')) {
                        (true, json!({"version": v, "answering": r.token, "pressed": true}).to_string())
                    } else {
                        (false, refuse(MENU_UNAVAILABLE))
                    }
                }
                _ => (false, refuse(MENUS_CHANGED)),
            }
        };
        self.send_to(st, id, kind::MAC_MENU, a.as_bytes());
        pressed
    }

    // MARK: Sessions

    /// A session a door admitted (`StreamServer.serve`, then the coordinator's catalog).
    pub fn register(
        &self,
        id: u64,
        fingerprint: [u8; 32],
        tx: mpsc::UnboundedSender<Out>,
        inflight: Arc<AtomicUsize>,
        stop: Arc<Notify>,
    ) {
        let mut st = self.lock();
        let was_paired = Self::is_paired(&st, &fingerprint);
        st.clients.insert(
            id,
            Client {
                tx,
                inflight,
                stop,
                connected_at: Instant::now(),
                stalled_since: None,
                last_sent: Instant::now(),
                fingerprint,
                was_paired,
                needs_keyframe: true,
                hello_seen: false,
                plays_audio: false,
                audio_epoch_sent: None,
                inputs_read: 0,
                fps: None,
                last_published: None,
                apps_sent: false,
                clipboard: false,
                menus: false,
                closing: false,
                settings_arrivals: Vec::new(),
                gesture_arrivals: Vec::new(),
                clipboard_arrivals: Vec::new(),
                fetch_arrivals: Vec::new(),
                press_arrivals: Vec::new(),
                last_keyframe_request: None,
            },
        );
        self.log.push(DeviceEvent::SessionStarted { conn: id, fingerprint: b64url(&fingerprint), paired: was_paired });
        self.send_catalog(&mut st, id);
        // A device joined a running stream: a keyframe for it (every device gets it).
        if st.active != Source::None {
            st.restart_pending = true;
        }
        self.pacer.notify_one();
    }

    /// A session ended. With nobody left, nothing streams: the next device picks afresh.
    pub fn unregister(&self, id: u64) {
        let mut st = self.lock();
        if st.clients.remove(&id).is_none() {
            return;
        }
        self.log.push(DeviceEvent::SessionEnded { conn: id });
        if st.clients.is_empty() {
            st.last_viewport = None;
            st.restore_after_unlock = None;
            if st.active != Source::None {
                self.select(&mut st, Source::None);
            }
        } else if st.active != Source::None && Self::wanted_fps(&st) != st.fps {
            let active = st.active;
            self.select(&mut st, active);
        }
        drop(st);
        self.changed.notify_all();
    }

    /// The device's hello, the first of its connection (`StreamServer.took`).
    pub fn took_hello(&self, id: u64, hello: Hello) {
        let mut st = self.lock();
        if let Some(c) = st.clients.get_mut(&id) {
            c.hello_seen = true;
            // `AudioCodec.choose`: the first of the device's list that this host makes.
            c.plays_audio = hello.audio.as_ref().is_some_and(|a| a.iter().any(|x| x == "aac-eld"));
        }
        self.log.push(DeviceEvent::Hello { conn: id, hello });
        self.pacer.notify_one();
    }

    /// The device gate (`DeviceGate`): `None` when the floor admits this hello (or is
    /// "0"), else the update goodbye. `hello`: `None` for no hello or one that does not decode.
    pub fn gate_refusal(&self, hello: Option<&Hello>) -> Option<Goodbye> {
        let floor = self.floor.as_ref()?;
        let version = hello.and_then(|h| h.app_version.as_deref()).and_then(parse_version).unwrap_or_default();
        if hello.is_some() && version_at_least(&version, floor) {
            return None;
        }
        let name = hello.and_then(|h| h.device.as_deref()).unwrap_or("");
        let word = if name.starts_with("iPhone") {
            "iPhone"
        } else if name.starts_with("iPad") {
            "iPad"
        } else {
            "device"
        };
        let floor = version_text(floor);
        Some(Goodbye {
            reason: "update".into(),
            message: Some(format!(
                "Update Sill on your {word} to keep using {}. It needs version {floor} or later.",
                self.cfg.name
            )),
            minimum_version: Some(floor),
            reconnect: Some(false),
        })
    }

    /// Whether the device gate reads a session's first message before serving it.
    pub fn gate_is_on(&self) -> bool {
        self.floor.is_some()
    }

    // MARK: Admission (DoorPolicy)

    /// The verify block (`DoorPolicy.trusts`): `sill/1` for a paired key, and at home for
    /// any key while Require pairing is off; `sill-pair/1` always at home, and at the
    /// remote door only while a pairing window is open; anything else never.
    pub fn trusts(&self, alpn: Option<&str>, fp: Option<[u8; 32]>) -> bool {
        let Some(fp) = fp else { return false };
        let st = self.lock();
        let home = self.cfg.door == Door::Home;
        match alpn {
            Some(SESSION_ALPN) => Self::is_paired(&st, &fp) || (home && !self.cfg.require_pairing),
            Some(PAIRING_ALPN) => home || st.window.is_open(),
            _ => false,
        }
    }

    /// `DoorPolicy.atReady` for a `sill/1` connection the verify block let through: `Ok`
    /// to serve it, `Err(Some(reason))` for a goodbye and a close, `Err(None)` for a close.
    pub fn session_at_ready(&self, fp: &[u8; 32]) -> Result<(), Option<&'static str>> {
        let st = self.lock();
        let paired = Self::is_paired(&st, fp);
        match self.cfg.door {
            Door::Home if paired || !self.cfg.require_pairing => Ok(()),
            Door::Home => Err(None),
            Door::Remote if !paired => Err(None),
            Door::Remote if !self.cfg.remote_access => Err(Some("remoteOff")),
            Door::Remote if st.clients.len() >= MAX_REMOTE_SESSIONS => Err(Some("busy")),
            Door::Remote => Ok(()),
        }
    }

    // MARK: Pairing (RemoteAccess.judge, DoorPolicy.pairing, DoorPolicy.ask)

    /// Opens a fresh window (a new secret; the configured code, `code`, or a random one),
    /// replacing any other. K for the typed path is derived off this thread; a code proof
    /// before it is ready is answered "busy", as on the real host.
    pub fn open_window(self: &Arc<Self>, st: &mut State, by_device: Option<[u8; 32]>, code: Option<String>) {
        let code = code.or_else(|| self.cfg.code.clone()).unwrap_or_else(pairing::generate_code);
        let mut w = Window::open(identity::random16(), code.clone(), wire::now(), self.cfg.pairing_lifetime.as_secs_f64(), by_device);
        if let Some(k) = st.key_cache.get(&code) {
            w.set_code_key(*k, &code);
        } else {
            let inner = self.clone();
            self.handle.spawn_blocking(move || {
                let key = pairing::code_key(&code, &inner.identity.fingerprint);
                let mut st = inner.lock();
                st.key_cache.insert(code.clone(), key);
                st.window.set_code_key(key, &code);
                drop(st);
                inner.changed.notify_all();
            });
        }
        st.window = w;
    }

    fn window_closed(self: &Arc<Self>, st: &mut State, reason: CloseReason) {
        // The CLI opens a fresh one after a use, an expiry or a withdrawal (`windowClosed`).
        if self.cfg.reopen_pairing && matches!(reason, CloseReason::Used | CloseReason::Expired | CloseReason::Withdrawn) {
            self.open_window(st, None, None);
        }
    }

    /// Judges one kind 19 and returns kind 20's payload.
    pub fn judge_pairing(self: &Arc<Self>, conn: u64, payload: &[u8], fp_device: [u8; 32], source: &str) -> Option<Vec<u8>> {
        let Ok(r) = serde_json::from_slice::<PairRequestWire>(payload) else {
            self.log.push(DeviceEvent::PairingAbandoned { conn, reason: "kind 19 does not decode".into() });
            return None;
        };
        let mut st = self.lock();
        let home = self.cfg.door == Door::Home;
        let mut tries_left = None;
        let result: Value = if r.v != 1 {
            // Another generation: `closed`, no try counted.
            pair_refusal("closed")
        } else if r.method == "cancel" {
            // The device no longer needs the code its ask put up: that window closes,
            // withdrawn; the answer is `closed` whatever happened.
            if home && st.window.current().is_some_and(|w| w.by_device == Some(fp_device)) {
                st.window.close(CloseReason::Withdrawn);
                self.window_closed(&mut st, CloseReason::Withdrawn);
            }
            pair_refusal("closed")
        } else if r.method == "ask" {
            if !home {
                pair_refusal("closed")
            } else if st.reading.locked() {
                pair_refusal("locked")
            } else {
                if st.window.current().is_some_and(|w| wire::now() >= w.expires_at) {
                    st.window.close(CloseReason::Expired);
                    self.window_closed(&mut st, CloseReason::Expired);
                }
                if st.window.is_open() {
                    pair_refusal("shown")
                } else if self.cfg.ask_opens_window {
                    self.open_window(&mut st, Some(fp_device), None);
                    pair_refusal("shown")
                } else {
                    pair_refusal("openOnMac")
                }
            }
        } else {
            let was_open = st.window.is_open();
            let proof = pairing::b64url_decode(&r.proof).unwrap_or_default();
            let verdict = st.window.try_proof(
                &r.method,
                &proof,
                &fp_device,
                &self.identity.fingerprint,
                source,
                wire::now(),
                self.cfg.pairing_spacing,
            );
            match verdict {
                Verdict::Accept(proof_m) => {
                    let device = PairedDevice {
                        fingerprint: b64url(&fp_device),
                        name: r.name.clone(),
                        model: r.model.clone().filter(|m| !m.is_empty()),
                        paired_at: wire::now(),
                        method: r.method.clone(),
                    };
                    // Re-pairing the same key replaces its record. The trust list is
                    // updated before kind 20 goes out, so the session dial that follows
                    // is admitted.
                    st.paired.retain(|d| d.fingerprint != device.fingerprint);
                    st.paired.push(device);
                    crate::store::save_paired(&self.cfg, &st.paired);
                    self.window_closed(&mut st, CloseReason::Used);
                    self.publish_settings(&mut st); // `sessionPaired` may have changed
                    json!({
                        "ok": true,
                        "proof": b64url(&proof_m),
                        "macID": pairing::mac_id(&self.identity.fingerprint),
                        "name": self.cfg.name,
                        "recognitionKey": b64url(&self.recognition_key),
                    })
                }
                Verdict::Reject(left) => {
                    tries_left = Some(left);
                    json!({"ok": false, "reason": "code", "triesLeft": left})
                }
                Verdict::Busy(after) => json!({"ok": false, "reason": "busy", "retryAfter": (after * 10.0).ceil() / 10.0}),
                Verdict::Closed(reason) => {
                    if was_open && reason == CloseReason::Expired {
                        self.window_closed(&mut st, CloseReason::Expired);
                    }
                    pair_refusal(pairing::closed_reason(reason, was_open))
                }
            }
        };
        let answer = if result["ok"] == json!(true) { "ok".to_string() } else { result["reason"].as_str().unwrap_or("").to_string() };
        self.log.push(DeviceEvent::PairRequest {
            conn,
            fingerprint: b64url(&fp_device),
            v: r.v,
            method: r.method,
            name: r.name,
            model: r.model,
            cable: r.cable,
            answer,
            tries_left,
        });
        Some(result.to_string().into_bytes())
    }

    // MARK: The lock (LockPolicy)

    pub fn set_reading(&self, reading: Reading) {
        let mut st = self.lock();
        if st.reading == reading {
            return;
        }
        let was_locked = st.reading.locked();
        st.reading = reading;
        if reading.locked() {
            st.audio = None;
            if let Source::Window(_) = st.active {
                // A window's picture never streams while locked: the Desktop, and the
                // window comes back after the unlock.
                let active = st.active;
                self.select(&mut st, active);
            } else {
                self.refresh_menus(&mut st);
                self.broadcast_list(&mut st);
            }
        } else if was_locked {
            // Unlocked: what a device that connected meanwhile was not sent, the shape and
            // every thumbnail afresh, then the window that streamed before the lock.
            let late: Vec<u64> = st.clients.iter().filter(|(_, c)| !c.apps_sent).map(|(id, _)| *id).collect();
            for id in late {
                self.send_apps_and_icons(&mut st, id);
            }
            let shape = self.cursor_blob(&st);
            self.broadcast(&mut st, kind::CURSOR_SHAPE, &shape);
            match st.restore_after_unlock.take() {
                Some(source) if !st.clients.is_empty() => self.select(&mut st, source),
                _ => {
                    self.refresh_menus(&mut st);
                    self.broadcast_list(&mut st);
                    // A keyframe ends a device's "Unlocking…".
                    if st.active != Source::None {
                        st.restart_pending = true;
                    }
                }
            }
            self.send_thumbnails(&mut st, None);
        }
        self.pacer.notify_one();
    }

    // MARK: What the test drives

    pub fn set_clipboard(&self, text: &str) {
        let mut st = self.lock();
        st.clipboard_text = Some(text.to_string());
        if text.is_empty() || text.len() > MAX_CLIPBOARD_TEXT || !self.cfg.share_clipboard {
            return;
        }
        let payload = json!({"text": text}).to_string();
        let ids: Vec<u64> = st.clients.iter().filter(|(_, c)| c.clipboard).map(|(id, _)| *id).collect();
        for id in ids {
            self.send_to(&mut st, id, kind::CLIPBOARD, payload.as_bytes());
        }
    }

    pub fn push_cursor_shape(&self) {
        let mut st = self.lock();
        st.cursor_shape += 1;
        let shape = self.cursor_blob(&st);
        self.broadcast(&mut st, kind::CURSOR_SHAPE, &shape);
    }

    /// Kind 26 (`MacPointer`) to every session: over the stream at (x, y), or off it.
    pub fn push_mac_pointer(&self, position: Option<(f64, f64)>) {
        let mut st = self.lock();
        if withheld(kind::MAC_POINTER, st.reading) {
            return;
        }
        let round = |v: f64| (v * 10_000.0).round() / 10_000.0;
        for c in st.clients.values_mut() {
            let o = match position {
                Some((x, y)) => json!({"inside": true, "seen": c.inputs_read, "x": round(x), "y": round(y)}),
                None => json!({"inside": false, "seen": c.inputs_read}),
            };
            // Sent as the tick is: never counted against the picture.
            Self::push(c, Arc::new(wire::message(kind::MAC_POINTER, wire::now(), false, o.to_string().as_bytes())), false);
        }
    }

    pub fn push_thumbnails(&self) {
        let mut st = self.lock();
        self.send_thumbnails(&mut st, None);
    }

    /// Remove on the Mac: the trust list emptied, and every session whose key was on it
    /// told "removed" and closed (`DoorPolicy.afterGate`'s home rule).
    pub fn forget_devices(&self) {
        let mut st = self.lock();
        let ids: Vec<u64> = st
            .clients
            .iter()
            .filter(|(_, c)| c.was_paired || Self::is_paired(&st, &c.fingerprint))
            .map(|(id, _)| *id)
            .collect();
        st.paired.clear();
        crate::store::save_paired(&self.cfg, &st.paired);
        for id in ids {
            self.goodbye(&mut st, id, &Goodbye { reason: "removed".into(), ..Goodbye::default() });
        }
        self.publish_settings(&mut st);
    }

    // MARK: A device's messages (StreamServer.receiveLoop, StreamCoordinator.handle)

    pub fn on_message(self: &Arc<Self>, id: u64, h: Header, payload: Vec<u8>) {
        let mut st = self.lock();
        let st = &mut *st;
        if !st.clients.contains_key(&id) {
            return;
        }
        let now = Instant::now();
        let locked = st.reading.locked();
        let len = payload.len();
        let undecodable = || self.log.push(DeviceEvent::Undecodable { conn: id, kind: h.kind, len });
        let refused = |reason: &str| self.log.push(DeviceEvent::Refused { conn: id, kind: h.kind, reason: reason.into() });
        match h.kind {
            kind::PING => {
                // Echoed straight back, payload and time stamp unchanged, whatever its
                // length: 8 bytes for an ordinary ping, 16 for a Paste's receipt.
                if let Some(c) = st.clients.get_mut(&id) {
                    Self::push(c, Arc::new(wire::message(kind::PONG, h.timestamp, false, &payload)), true);
                }
                self.log.push(DeviceEvent::Ping { conn: id, payload, timestamp: h.timestamp });
            }
            kind::HELLO => {
                // The first of a connection counts; a second is skipped without a word.
                let first = st.clients.get_mut(&id).is_some_and(|c| !std::mem::replace(&mut c.hello_seen, true));
                if first {
                    match serde_json::from_slice::<Hello>(&payload) {
                        Ok(hello) => {
                            if let Some(c) = st.clients.get_mut(&id) {
                                c.plays_audio = hello.audio.as_ref().is_some_and(|a| a.iter().any(|x| x == "aac-eld"));
                            }
                            self.log.push(DeviceEvent::Hello { conn: id, hello });
                            self.pacer.notify_one();
                        }
                        Err(_) => undecodable(),
                    }
                }
            }
            kind::CLIENT_STATS => match serde_json::from_slice::<ClientStats>(&payload) {
                Ok(stats) => self.log.push(DeviceEvent::ClientStats { conn: id, stats }),
                Err(_) => undecodable(),
            },
            kind::SELECT_SOURCE => {
                let Some(source) = serde_json::from_slice::<Value>(&payload).ok().as_ref().and_then(Source::from_json) else {
                    return undecodable();
                };
                self.log.push(DeviceEvent::SelectSource { conn: id, source });
                if locked && matches!(source, Source::Window(_)) {
                    return refused("pick");
                }
                if !locked {
                    st.restore_after_unlock = None;
                }
                self.select(st, source);
            }
            kind::WINDOW_COMMAND => {
                let Ok(c) = serde_json::from_slice::<WindowCommandWire>(&payload) else { return undecodable() };
                if !["close", "minimize", "fullScreen"].contains(&c.action.as_str()) {
                    return undecodable();
                }
                self.log.push(DeviceEvent::WindowCommand { conn: id, id: c.id, action: c.action.clone() });
                if locked {
                    return refused("windowCommand");
                }
                // Close and minimize take the window off the list; streaming, it stops.
                if c.action != "fullScreen" && st.windows.iter().any(|w| w.id == c.id) {
                    st.windows.retain(|w| w.id != c.id);
                    if st.active == Source::Window(c.id) {
                        self.select(st, Source::None);
                    } else {
                        self.broadcast_list(st);
                    }
                }
            }
            kind::LAUNCH_APP => {
                let Ok(l) = serde_json::from_slice::<LaunchAppWire>(&payload) else { return undecodable() };
                self.log.push(DeviceEvent::LaunchApp { conn: id, bundle_id: l.bundle_id.clone() });
                if locked {
                    return refused("launch");
                }
                // An installed app opens, and its first window gets selected.
                if let Some(app) = APPS.iter().position(|a| a.bundle_id == l.bundle_id) {
                    let window = match st.windows.iter().find(|w| w.app == app) {
                        Some(w) => w.id,
                        None => {
                            let wid = st.next_window_id;
                            st.next_window_id += 1;
                            st.windows.push(WindowInfo { id: wid, title: APPS[app].name.into(), app, width: 900, height: 600 });
                            let blob = wire::thumbnail(wid, media::THUMBNAILS[wid as usize % media::THUMBNAILS.len()]);
                            self.broadcast(st, kind::THUMBNAIL, &blob);
                            wid
                        }
                    };
                    self.select(st, Source::Window(window));
                }
            }
            kind::INPUT => {
                let Some(input) = serde_json::from_slice::<Value>(&payload).ok().as_ref().and_then(InputEvent::from_json) else {
                    // Still counted as read (`inputsRead`), as the server counts every kind 8.
                    if let Some(c) = st.clients.get_mut(&id) {
                        c.inputs_read += 1;
                    }
                    return undecodable();
                };
                st.last_input = Some(now);
                let paired = st.clients.get(&id).is_some_and(|c| Self::is_paired(st, &c.fingerprint));
                if let Some(c) = st.clients.get_mut(&id) {
                    c.inputs_read += 1;
                }
                self.log.push(DeviceEvent::Input { conn: id, input });
                self.pacer.notify_one();
                // A locked screen takes a paired session's input; off console nobody's.
                match st.reading {
                    Reading::ScreenLocked if !paired => refused("input"),
                    Reading::OffConsole => refused("input"),
                    _ => {}
                }
            }
            kind::GESTURE => {
                let Ok(g) = serde_json::from_slice::<GestureWire>(&payload) else { return undecodable() };
                self.log.push(DeviceEvent::Gesture { conn: id, gesture: g.gesture, fingers: g.fingers });
                if locked {
                    return refused("gesture");
                }
                if !st.clients.get_mut(&id).is_some_and(|c| admit(&mut c.gesture_arrivals, 4, now)) {
                    refused("rate");
                }
            }
            kind::KEYFRAME_REQUEST => {
                // At most one a second per connection; nothing streams: nothing happens.
                let Some(c) = st.clients.get_mut(&id) else { return };
                let spaced = c.last_keyframe_request.is_none_or(|t| now.duration_since(t) >= Duration::from_secs(1));
                if spaced {
                    c.last_keyframe_request = Some(now);
                }
                let honoured = spaced && st.active != Source::None;
                if honoured {
                    // The encoder makes one for every device: the clip from its first IDR.
                    st.restart_pending = true;
                    self.pacer.notify_one();
                }
                self.log.push(DeviceEvent::KeyframeRequest { conn: id, honoured });
            }
            kind::VIEWPORT => {
                let Ok(v) = serde_json::from_slice::<Viewport>(&payload) else { return undecodable() };
                // `ViewportBounds.refusal`: each side 1 to 16384 points, a scale 0.25 to 4.
                let mut why = Vec::new();
                if !(1.0..=16384.0).contains(&v.width) {
                    why.push(format!("width {} (1 to 16384 points)", v.width));
                }
                if !(1.0..=16384.0).contains(&v.height) {
                    why.push(format!("height {} (1 to 16384 points)", v.height));
                }
                if let Some(s) = v.scale.filter(|s| !(0.25..=4.0).contains(s)) {
                    why.push(format!("scale {s} (0.25 to 4)"));
                }
                let refusal = (!why.is_empty()).then(|| why.join(", "));
                self.log.push(DeviceEvent::Viewport { conn: id, viewport: v.clone(), refused: refusal.clone() });
                if refusal.is_some() {
                    return;
                }
                if let Some(c) = st.clients.get_mut(&id) {
                    c.fps = Some(v.fps.unwrap_or(60));
                }
                st.last_viewport = Some(v);
                // Another rate: the stream restarts at it.
                if st.active != Source::None && Self::wanted_fps(st) != st.fps {
                    let active = st.active;
                    self.select(st, active);
                }
            }
            kind::PAIRING_WANTED => {
                // "Show your pairing code": at a TLS home door only an unpaired session's
                // counts (Require pairing off), and goes through the ask rule: a window
                // opens for it when none is open. A paired session's is ignored, and the
                // remote door ignores every one. (The real host also takes at most one per
                // 30 s per connection.)
                self.log.push(DeviceEvent::PairingWanted { conn: id });
                let fp = st.clients.get(&id).map(|c| c.fingerprint);
                if let Some(fp) = fp.filter(|fp| self.cfg.door == Door::Home && !Self::is_paired(st, fp)) {
                    if !locked && !st.window.is_open() && self.cfg.ask_opens_window {
                        self.open_window(st, Some(fp), None);
                    }
                }
            }
            kind::CHANGE_SETTINGS => {
                let Ok(change) = serde_json::from_slice::<SettingsChange>(&payload) else { return undecodable() };
                let paired = st.clients.get(&id).is_some_and(|c| Self::is_paired(st, &c.fingerprint));
                let within_rate = st.clients.get_mut(&id).is_some_and(|c| admit(&mut c.settings_arrivals, 4, now));
                let mut refusals: Vec<String> = Vec::new();
                if within_rate {
                    // `DeviceSettings.accepted`.
                    let s = &mut st.settings;
                    if let Some(v) = change.prioritize_speed {
                        s.prioritize_speed = v;
                    }
                    if let Some(v) = change.send_audio {
                        s.send_audio = v;
                    }
                    if let Some(v) = change.direct_wireless {
                        if self.cfg.door == Door::Remote {
                            refusals.push("direct wireless (not from a remote connection)".into());
                        } else if !paired {
                            refusals.push("direct wireless (only a paired device)".into());
                        } else {
                            s.direct_wireless = v;
                        }
                    }
                    if let Some(v) = change.max_fps {
                        if [60, 120].contains(&v) {
                            s.max_fps = v;
                        } else {
                            refusals.push(format!("frame rate limit {v}"));
                        }
                    }
                    if let Some(v) = change.bitrate {
                        if BITRATES.contains(&v) {
                            s.bitrate = v;
                        } else {
                            refusals.push(format!("bitrate {v}"));
                        }
                    }
                    if let Some(v) = change.capture_scale {
                        if v == 1.0 || v == 2.0 {
                            s.capture_scale = v;
                        } else {
                            refusals.push(format!("capture scale {v}"));
                        }
                    }
                    if change.virtual_display == Some(true) {
                        refusals.push("virtual display (needs SillHost --virtual-display)".into());
                    }
                }
                let token = change.token;
                self.log.push(DeviceEvent::ChangeSettings { conn: id, change, refused: refusals });
                if !within_rate {
                    refused("rate");
                }
                // The change's publish first (to every device whose state changed, this one
                // included, without `answering`), then exactly one answer to this device
                // alone, carrying the token: the settings as they now stand.
                self.publish_settings(st);
                let answer = self.settings_state(st, id, token);
                self.send_to(st, id, kind::HOST_SETTINGS, answer.to_string().as_bytes());
                if st.active != Source::None && Self::wanted_fps(st) != st.fps {
                    let active = st.active;
                    self.select(st, active);
                }
                self.pacer.notify_one();
            }
            kind::FETCH_MENU => {
                let Ok(r) = serde_json::from_slice::<MenuRequestWire>(&payload) else { return undecodable() };
                self.log.push(DeviceEvent::FetchMenu {
                    conn: id,
                    version: r.version,
                    id: r.id.clone(),
                    title: r.title.clone(),
                    token: r.token,
                });
                self.fetch_menu(st, id, r);
            }
            kind::PRESS_MENU_ITEM => {
                let Ok(r) = serde_json::from_slice::<MenuRequestWire>(&payload) else { return undecodable() };
                let pressed = self.press_menu(st, id, &r);
                self.log.push(DeviceEvent::PressMenuItem {
                    conn: id,
                    version: r.version,
                    id: r.id,
                    title: r.title,
                    token: r.token,
                    pressed,
                });
            }
            kind::CLIPBOARD => {
                let Ok(clip) = serde_json::from_slice::<ClipboardWire>(&payload) else { return undecodable() };
                let stop = clip.stop == Some(true);
                let text = clip.text.filter(|t| !t.is_empty());
                self.log.push(DeviceEvent::Clipboard { conn: id, text: text.clone(), stop });
                // Share clipboard off: ignored without a word or an answer.
                if !self.cfg.share_clipboard {
                    return;
                }
                let Some(c) = st.clients.get_mut(&id) else { return };
                if stop {
                    // `stop` wins over anything with it, and is never rate limited.
                    c.clipboard = false;
                    return;
                }
                if text.as_ref().is_some_and(|t| t.len() > MAX_CLIPBOARD_TEXT) {
                    return refused("tooLong");
                }
                if text.is_some() && locked {
                    return refused("clipboard");
                }
                if !admit(&mut c.clipboard_arrivals, 4, now) {
                    return refused("rate");
                }
                // Any kind 32 subscribes the connection to the Mac's copies. What is on the
                // clipboard then is never sent: only what is copied from now on.
                c.clipboard = true;
                if let Some(text) = text {
                    // The device's text becomes the Mac's clipboard and goes to every other
                    // subscriber, never back to its sender.
                    let message = json!({"text": text}).to_string();
                    st.clipboard_text = Some(text);
                    let others: Vec<u64> = st.clients.iter().filter(|(k, c)| **k != id && c.clipboard).map(|(k, _)| *k).collect();
                    for other in others {
                        self.send_to(st, other, kind::CLIPBOARD, message.as_bytes());
                    }
                }
            }
            _ => self.log.push(DeviceEvent::Unknown { conn: id, kind: h.kind, len }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert_eq!(parse_version("v0.4.0"), Some(vec![0, 4]));
        assert_eq!(parse_version("1.2.3-beta.1"), Some(vec![1, 2, 3]));
        assert_eq!(parse_version("0.3.0 (85)"), Some(vec![0, 3]));
        assert_eq!(parse_version("beta"), None);
        assert!(version_at_least(&[0, 10], &[0, 9]));
        assert!(!version_at_least(&[1], &[1, 0, 1]));
        assert!(version_at_least(&[0, 4], &[0, 4]));
        assert_eq!(version_text(&[1]), "1.0");
        assert_eq!(version_text(&[1, 0, 1]), "1.0.1");
    }

    #[test]
    fn menus() {
        let bar = menu_bar("TextEdit");
        assert_eq!(menu_lookup(&bar, "2").map(|m| m.title), Some("File"));
        assert_eq!(menu_lookup(&bar, "2.5.1").map(|m| m.title), Some("Rich Text"));
        assert!(menu_lookup(&bar, "2.2").is_some_and(|m| m.separator));
        assert!(menu_lookup(&bar, "0").is_none());
        assert!(menu_lookup(&bar, "9.1").is_none());
        assert_eq!(menu_item_json("2.2".into(), menu_lookup(&bar, "2.2").unwrap()), json!({"separator": true}));
    }

    #[test]
    fn rate() {
        let mut a = Vec::new();
        let now = Instant::now();
        assert!((0..4).all(|_| admit(&mut a, 4, now)));
        assert!(!admit(&mut a, 4, now));
        assert!(admit(&mut a, 4, now + Duration::from_millis(1001)));
    }
}
