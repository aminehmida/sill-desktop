//! The client's own thread of thought (iOSClient/StreamClient.swift's job, as SillClient.kt has
//! it): one task that owns every decision, fed by commands from the handle, the session's tasks,
//! the dials and the network browser. Frames never pass through it.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::time::Duration;

use ring::rand::{SecureRandom, SystemRandom};
use tokio::sync::mpsc;
use tokio::time::Instant;

use super::clipboard::{copy, ClipboardPolicy, DeviceSend, Sent, Subscription};
use super::keyframe::{DeviceIdlePolicy, KeyframeDecision, KeyframeRecovery};
use super::keys::Keys;
use super::ledger::SettingsLedger;
use super::menu::{MenuOutcome, MenuState};
use super::state::*;
use crate::crypto::{base64url, constant_time_eq, mac_id, pairing_code, recognition_tag, safe_text, verify_mac_info, DeviceIdentity, PairLink};
use crate::net::pairer::{self, PairingDevice};
use crate::net::session::{monotonic, SessionEnd};
use crate::net::tls::{self, DialError, DialReason, TlsConnection, DIAL_OVERALL, SESSION_ALPN};
use crate::net::{
    AskOutcome, DialOrder, DiscoveryPolicy, Endpoint, HomeDiscovery, HomeDoor, HomeMac, LinkStats, PairOutcome, ReconnectClock,
    ReconnectStep, SavedMac, SavedMacs, Session, SessionListener,
};
use crate::protocol::{
    self, blobs, input, key_mods, kind, payloads, CursorShape, Goodbye, HostSettingsState, LockShade, LockedMac, MacInfo, MacMenu,
    MacMenuItem, MacPointer, Message, ParameterSets, SettingsChange, SignedMacInfo, StreamSource, WindowList,
};

/// The pointer sprite's state (`PointerPresence`): the Mac's while the Mac or another device
/// moved it last, this device's own while it did.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PointerState {
    pub mac_pointer: Option<(f64, f64)>,
    pub pointer_here: bool,
    pub own_pointer: (f64, f64),
    pub reseeds: u64,
}

/// What the handle, the session's tasks and the actor share.
pub(crate) struct Shared {
    pub tx: mpsc::UnboundedSender<Cmd>,
    pub runtime: tokio::runtime::Handle,
    pub config: ClientConfig,
    pub identity: DeviceIdentity,
    pub store: SavedMacs,
    pub state: Mutex<Arc<ClientState>>,
    pub changed: Condvar,
    pub events: Mutex<VecDeque<ClientEvent>>,
    pub waker: RwLock<Option<Waker>>,
    /// The live session, for input sent straight from the caller's thread.
    pub session: Mutex<Option<Arc<Session>>>,
    pub pointer: Mutex<PointerState>,
    pub host_gestures: AtomicI64,
    pub gestures_enabled: AtomicBool,
    pub active_is_window: AtomicBool,
    pub shade_unlocking: AtomicBool,
    pub hold_awake: AtomicBool,
    /// Seconds (monotonic, as bits) of the last input sent to the Mac.
    pub input_at: AtomicU64,
}

impl Shared {
    pub fn wake(&self) {
        let waker = self.waker.read().unwrap().clone();
        if let Some(w) = waker {
            w();
        }
    }

    pub fn emit(&self, event: ClientEvent) {
        let mut q = self.events.lock().unwrap();
        // Nobody listening for a long time: the oldest go.
        if q.len() >= 4096 {
            q.pop_front();
        }
        q.push_back(event);
    }

    pub fn current_session(&self) -> Option<Arc<Session>> {
        self.session.lock().unwrap().clone()
    }

    /// Input the person sent to the Mac: any thread.
    pub fn note_input(&self) {
        self.input_at.store(monotonic().to_bits(), Ordering::Relaxed);
        if !self.hold_awake.load(Ordering::Relaxed) {
            let _ = self.tx.send(Cmd::ReviewIdle);
        }
    }

    /// This device took the pointer (`trackpad`: a relative pointer drew it).
    pub fn took_pointer(&self, x: f64, y: f64, trackpad: bool) {
        let mut p = self.pointer.lock().unwrap();
        let flags_changed = !p.pointer_here || (!trackpad && p.mac_pointer.is_some());
        p.pointer_here = true;
        p.own_pointer = (x, y);
        if !trackpad {
            p.mac_pointer = None;
        }
        drop(p);
        if flags_changed {
            let _ = self.tx.send(Cmd::Dirty);
        }
    }
}

/// What the actor is told.
pub(crate) enum Cmd {
    // From the handle.
    PairWithCode { address: String, code: String },
    PairWithLink { text: String, extra_address: Option<String> },
    ClearPairError,
    TapHome { service_name: String },
    PairHomeCode { code: String },
    CancelHomeAsk,
    Connect { mac_id: String },
    Disconnect,
    Forget { mac_id: String },
    StartDiscovery,
    StopDiscovery,
    SetForeground(bool),
    Select { source: StreamSource },
    Launch { bundle_id: String },
    WindowCommand { id: u32, action: String },
    SetPanel { width: f64, height: f64 },
    SetDisplayFps(u32),
    ChooseTextScale(Option<f64>),
    ChangeSettings(SettingsChange),
    OpenMenu(MacMenuItem),
    ChooseMenuItem(MacMenuItem),
    PasteClipboard { text: Option<String> },
    SetPrefs(Box<dyn FnOnce(&mut Prefs) + Send>),
    NoteAction,
    Notice(String),
    PictureInterrupted,
    KeyframeShown,
    ReviewIdle,
    Dirty,
    // From the client's own tasks.
    PairDone(PairOutcome),
    AskDone { found: HomeMac, outcome: AskOutcome },
    Dialed { generation: u64, conn: Box<TlsConnection>, any_key: bool },
    DialFailed { generation: u64, mac: Box<SavedMac>, error: DialError },
    Discovery(Vec<HomeMac>),
    // From a session's tasks.
    Message { id: u64, msg: Message },
    Thumbnail { id: u64, window_id: u32, jpeg: Vec<u8> },
    Icon { bundle_id: String, png: Vec<u8> },
    Cursor { id: u64, shape: CursorShape },
    Pointer { id: u64, pointer: MacPointer },
    VideoSize { id: u64, size: (u32, u32) },
    KeyframeArrived { id: u64 },
    Stats { id: u64, stats: LinkStats },
    Ended { id: u64, end: SessionEnd },
}

#[derive(Debug, Clone, PartialEq)]
enum Timer {
    FirstList { generation: u64, id: u64 },
    ReconnectCheck,
    AutoDesktop { id: u64, picked_before: u64 },
    ShadeTimeout,
    Viewport,
    KeyframeCheck,
    IdleCheck,
    ClearNotice,
    Suspend,
    ReceiptLate,
    PasteWork,
    ClipboardNoteExpiry,
}

/// A session's listener: the reader task's side of the client.
struct Link {
    sh: Arc<Shared>,
    id: u64,
}

impl SessionListener for Link {
    fn on_message(&self, session: &Arc<Session>, msg: Message) {
        let (sh, id) = (&self.sh, self.id);
        match msg.kind {
            kind::FRAME => {
                session.note_frame(msg.timestamp);
                sh.config.video.frame(&msg.payload, msg.is_keyframe, msg.timestamp);
                if let Some(s) = &sh.config.sound {
                    s.on_video_frame(msg.timestamp);
                }
                // The first keyframe after a locked Mac's unlock: its picture is back, the cover goes.
                if msg.is_keyframe && sh.shade_unlocking.load(Ordering::Relaxed) {
                    let _ = sh.tx.send(Cmd::KeyframeArrived { id });
                }
            }
            kind::AUDIO => {
                if let Some(s) = &sh.config.sound {
                    s.on_message(&msg.payload, msg.timestamp);
                }
            }
            kind::PARAMETER_SETS => {
                if let Some(sets) = ParameterSets::parse(&msg.payload) {
                    sh.config.video.parameter_sets(&msg.payload);
                    if let Some(size) = sets.picture_size() {
                        let _ = sh.tx.send(Cmd::VideoSize { id, size });
                    }
                }
            }
            kind::THUMBNAIL => {
                if let Some(t) = blobs::thumbnail(&msg.payload) {
                    let _ = sh.tx.send(Cmd::Thumbnail { id, window_id: t.window_id, jpeg: t.jpeg });
                }
            }
            kind::APP_ICON => {
                if let Some(i) = blobs::icon(&msg.payload) {
                    let _ = sh.tx.send(Cmd::Icon { bundle_id: i.bundle_id, png: i.png });
                }
            }
            kind::CURSOR_SHAPE => {
                if let Some(shape) = blobs::cursor_shape(&msg.payload) {
                    let _ = sh.tx.send(Cmd::Cursor { id, shape });
                }
            }
            kind::MAC_POINTER => {
                if let Some(p) = MacPointer::parse(&msg.payload) {
                    // Stale when built before the Mac read this device's latest input, or a move still waits.
                    if p.seen < session.inputs_on_session() as i64 || session.move_waiting() {
                        return;
                    }
                    let _ = sh.tx.send(Cmd::Pointer { id, pointer: p });
                }
            }
            _ => {
                let _ = sh.tx.send(Cmd::Message { id, msg });
            }
        }
    }

    fn on_ended(&self, _session: &Arc<Session>, end: SessionEnd) {
        let _ = self.sh.tx.send(Cmd::Ended { id: self.id, end });
    }

    fn on_stats(&self, _session: &Arc<Session>, stats: LinkStats) {
        if let Some(s) = &self.sh.config.sound {
            s.set_rtt(if stats.rtt_ms >= 0 { Some(stats.rtt_ms) } else { None });
        }
        let _ = self.sh.tx.send(Cmd::Stats { id: self.id, stats });
    }

    fn sound_stats(&self) -> Option<(i32, i32)> {
        self.sh.config.sound.as_ref().and_then(|s| s.take_second())
    }
}

/// A Paste waiting for its receipt.
struct Receipt {
    nonce: [u8; ClipboardPolicy::RECEIPT_LENGTH],
    /// Its time ran out ("Didn’t reach…" shown); a receipt that comes later still counts.
    late: bool,
}

#[derive(Default)]
struct ClipState {
    host_clipboard: Option<i64>,
    subscribed: bool,
    streaming: bool,
    last_sent: Option<Sent>,
    receipt: Option<Receipt>,
}

pub(crate) struct Core {
    sh: Arc<Shared>,
    st: ClientState,
    dirty: bool,
    timers: Vec<(Instant, Timer)>,
    session: Option<(u64, Arc<Session>)>,
    next_session_id: u64,
    session_ready: bool,
    generation: u64,
    goodbye: Option<Goodbye>,
    auto_reconnect: Option<ReconnectClock>,
    reconnect_dialing: bool,
    /// An open door's session: the key its first connection saw, which its reconnect pins.
    open_key: Option<[u8; 32]>,
    /// A session with an open door's unsaved Mac.
    open_session: bool,
    user_closed: bool,
    last_auto_desktop_at: Option<u64>,
    last_user_pick_at: u64,
    panel: Option<(f64, f64)>,
    display_fps: u32,
    last_viewport_json: Option<String>,
    resume_mac: Option<SavedMac>,
    ledger: SettingsLedger,
    menus: MenuState,
    recovery: KeyframeRecovery,
    foreground: bool,
    control_at: f64,
    session_start_at: Option<f64>,
    front_at: f64,
    clip: ClipState,
    discovery: Option<HomeDiscovery>,
    home_ask: Option<HomeAsk>,
}

fn now_ms() -> u64 {
    (monotonic() * 1000.0) as u64
}

fn prefs_path(config: &ClientConfig) -> std::path::PathBuf {
    config.config_dir.join("prefs.json")
}

pub(crate) fn load_prefs(config: &ClientConfig) -> Prefs {
    std::fs::read(prefs_path(config)).ok().and_then(|d| serde_json::from_slice(&d).ok()).unwrap_or_default()
}

impl Core {
    pub(crate) fn new(sh: Arc<Shared>, initial: ClientState) -> Core {
        let display_fps = sh.config.display_fps;
        Core {
            sh,
            st: initial,
            dirty: false,
            timers: Vec::new(),
            session: None,
            next_session_id: 1,
            session_ready: false,
            generation: 0,
            goodbye: None,
            auto_reconnect: None,
            reconnect_dialing: false,
            open_key: None,
            open_session: false,
            user_closed: false,
            last_auto_desktop_at: None,
            last_user_pick_at: 0,
            panel: None,
            display_fps,
            last_viewport_json: None,
            resume_mac: None,
            ledger: SettingsLedger::new(),
            menus: MenuState::new(),
            recovery: KeyframeRecovery::new(),
            foreground: true,
            control_at: 0.0,
            session_start_at: None,
            front_at: monotonic(),
            clip: ClipState::default(),
            discovery: None,
            home_ask: None,
        }
    }

    /// The actor's loop: commands and timers, each followed by publishing what changed.
    pub(crate) async fn run(mut self, mut rx: mpsc::UnboundedReceiver<Cmd>) {
        if self.sh.config.discovery {
            self.start_discovery();
        }
        loop {
            let next = self.timers.iter().map(|t| t.0).min();
            tokio::select! {
                cmd = rx.recv() => match cmd {
                    Some(cmd) => self.handle(cmd),
                    None => break,
                },
                _ = async { tokio::time::sleep_until(next.unwrap()).await }, if next.is_some() => self.fire_timers(),
            }
            // A burst (a catalog's thumbnails, a drag's moves) is one publication.
            while let Ok(cmd) = rx.try_recv() {
                self.handle(cmd);
            }
            self.publish();
        }
    }

    // MARK: publishing

    fn touch(&mut self) -> &mut ClientState {
        self.dirty = true;
        &mut self.st
    }

    fn emit(&mut self, e: ClientEvent) {
        self.sh.emit(e);
        self.dirty = true;
    }

    fn publish(&mut self) {
        if !self.dirty {
            return;
        }
        self.dirty = false;
        {
            let p = self.sh.pointer.lock().unwrap();
            self.st.mac_pointer = p.mac_pointer;
            self.st.pointer_here = p.pointer_here;
            self.st.pointer_reseeds = p.reseeds;
        }
        self.st.revision += 1;
        self.st.foreground = self.foreground;
        self.sh.host_gestures.store(self.st.host_gestures, Ordering::Relaxed);
        self.sh.gestures_enabled.store(self.st.prefs.trackpad_gestures, Ordering::Relaxed);
        self.sh.active_is_window.store(self.st.active.is_window(), Ordering::Relaxed);
        self.sh.shade_unlocking.store(self.st.lock_shade == Some(LockShade::Unlocking), Ordering::Relaxed);
        self.sh.hold_awake.store(self.st.hold_awake, Ordering::Relaxed);
        *self.sh.state.lock().unwrap() = Arc::new(self.st.clone());
        self.sh.changed.notify_all();
        self.sh.wake();
    }

    fn set_status(&mut self, status: Option<String>) {
        self.touch().status = status;
    }

    /// A failure's words: the status line, and `last_error` until the next attempt.
    fn set_failure(&mut self, words: String) {
        let st = self.touch();
        st.last_error = Some(words.clone());
        st.status = Some(words);
    }

    fn notice(&mut self, text: String) {
        self.touch().notice = Some(text.clone());
        self.emit(ClientEvent::Notice(text));
        self.set_timer(Timer::ClearNotice, 5.0);
    }

    fn refresh_saved(&mut self) {
        let all = self.sh.store.all();
        self.touch().saved_macs = all;
        self.refresh_unpaired();
    }

    fn refresh_unpaired(&mut self) {
        let unpaired: Vec<HomeMac> =
            self.st.home_macs.iter().filter(|h| !self.st.saved_macs.iter().any(|m| self.home_of(m).as_ref() == Some(*h))).cloned().collect();
        if unpaired != self.st.unpaired_home_macs {
            self.touch().unpaired_home_macs = unpaired;
        }
        let at_home: Vec<String> = self.st.saved_macs.iter().filter(|m| self.home_of(m).is_some()).map(|m| m.mac_id.clone()).collect();
        if at_home != self.st.saved_macs_at_home {
            self.touch().saved_macs_at_home = at_home;
        }
    }

    // MARK: timers

    fn set_timer(&mut self, timer: Timer, after_s: f64) {
        self.cancel_timer(&timer);
        self.timers.push((Instant::now() + Duration::from_secs_f64(after_s.max(0.0)), timer));
    }

    /// Cancels every timer of `like`'s kind.
    fn cancel_timer(&mut self, like: &Timer) {
        let d = std::mem::discriminant(like);
        self.timers.retain(|t| std::mem::discriminant(&t.1) != d);
    }

    fn fire_timers(&mut self) {
        let now = Instant::now();
        let mut due = Vec::new();
        self.timers.retain(|t| {
            if t.0 <= now {
                due.push(t.1.clone());
                false
            } else {
                true
            }
        });
        for t in due {
            self.fire(t);
        }
    }

    fn fire(&mut self, timer: Timer) {
        match timer {
            Timer::FirstList { generation, id } => {
                // A TLS session counts as connected at its first window list (10 s at most).
                if generation == self.generation && self.session_id() == Some(id) && !self.session_ready {
                    if let Some((_, s)) = &self.session {
                        s.close(Some("no window list in 10 s"));
                    }
                }
            }
            Timer::ReconnectCheck => self.reconnect_if_due(),
            Timer::AutoDesktop { id, picked_before } => {
                if self.session_id() == Some(id) && self.st.active == StreamSource::None && self.last_user_pick_at == picked_before {
                    self.last_auto_desktop_at = Some(now_ms());
                    self.select(StreamSource::Desktop, false);
                }
            }
            Timer::ShadeTimeout => {
                if self.st.lock_shade == Some(LockShade::Unlocking) {
                    self.touch().lock_shade = None;
                }
            }
            Timer::Viewport => self.send_viewport_now(),
            Timer::KeyframeCheck => self.consider_keyframe(),
            Timer::IdleCheck => self.review_idle(),
            Timer::ClearNotice => self.touch().notice = None,
            Timer::Suspend => self.suspend(),
            Timer::ReceiptLate => self.receipt_late(),
            Timer::PasteWork => self.paste_work(),
            Timer::ClipboardNoteExpiry => self.touch().clipboard_note = None,
        }
    }

    // MARK: commands

    fn handle(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::PairWithCode { address, code } => self.pair_with_code(&address, &code),
            Cmd::PairWithLink { text, extra_address } => self.pair_with_link(&text, extra_address.as_deref()),
            Cmd::ClearPairError => self.touch().pair_error = None,
            Cmd::TapHome { service_name } => self.tap_home(&service_name),
            Cmd::PairHomeCode { code } => self.pair_home_code(&code),
            Cmd::CancelHomeAsk => self.cancel_home_ask(),
            Cmd::Connect { mac_id } => match self.sh.store.by_id(&mac_id) {
                Some(mac) => {
                    self.touch().last_error = None;
                    self.connect(mac, false, false);
                }
                None => self.set_failure("That Mac is no longer saved. Pair it again.".to_string()),
            },
            Cmd::Disconnect => self.disconnect(),
            Cmd::Forget { mac_id } => {
                if self.st.current_mac.as_ref().map(|m| m.mac_id == mac_id).unwrap_or(false) {
                    self.disconnect();
                }
                self.sh.store.remove(&mac_id);
                self.refresh_saved();
            }
            Cmd::StartDiscovery => self.start_discovery(),
            Cmd::StopDiscovery => self.stop_discovery(),
            Cmd::SetForeground(front) => {
                if front {
                    self.on_foreground()
                } else {
                    self.on_stopped()
                }
            }
            Cmd::Select { source } => self.select(source, true),
            Cmd::Launch { bundle_id } => {
                self.last_user_pick_at = now_ms();
                self.note_action();
                if let Some((_, s)) = &self.session {
                    s.send_json(kind::LAUNCH_APP, &payloads::launch_app(&bundle_id));
                }
            }
            Cmd::WindowCommand { id, action } => {
                self.note_action();
                if let Some((_, s)) = &self.session {
                    s.send_json(kind::WINDOW_COMMAND, &payloads::window_command(id, &action));
                }
            }
            Cmd::SetPanel { width, height } => {
                if self.panel != Some((width, height)) {
                    self.panel = Some((width, height));
                    self.send_viewport(false);
                }
            }
            Cmd::SetDisplayFps(fps) => {
                if self.display_fps != fps {
                    self.display_fps = fps;
                    self.send_viewport(false);
                }
            }
            Cmd::ChooseTextScale(scale) => {
                self.note_action();
                self.touch().text_scale = scale;
                self.send_viewport(false);
            }
            Cmd::ChangeSettings(change) => self.change_settings(&change),
            Cmd::OpenMenu(item) => {
                let Some((_, s)) = &self.session else { return };
                let Some(r) = self.menus.open(&item, now_ms()) else { return };
                s.send_json(kind::FETCH_MENU, &payloads::fetch_menu(r.token, r.version, r.id.as_deref(), r.title.as_deref()));
                self.refresh_menus();
            }
            Cmd::ChooseMenuItem(item) => {
                self.note_action();
                let Some((_, s)) = &self.session else { return };
                let Some(r) = self.menus.choose(&item, now_ms()) else { return };
                s.send_json(
                    kind::PRESS_MENU_ITEM,
                    &payloads::press_menu_item(r.token, r.version, r.id.as_deref().unwrap_or(""), r.title.as_deref()),
                );
            }
            Cmd::PasteClipboard { text } => self.paste_clipboard(text),
            Cmd::SetPrefs(change) => self.set_prefs(change),
            Cmd::NoteAction => self.note_action(),
            Cmd::Notice(text) => self.notice(text),
            Cmd::PictureInterrupted => {
                if self.session.is_some() {
                    self.recovery.interrupted(monotonic());
                    self.consider_keyframe();
                }
            }
            Cmd::KeyframeShown => self.recovery.picture_back(monotonic(), self.foreground),
            Cmd::ReviewIdle => self.review_idle(),
            Cmd::Dirty => self.dirty = true,
            Cmd::PairDone(outcome) => self.pair_done(outcome),
            Cmd::AskDone { found, outcome } => {
                self.touch().asking = None;
                match outcome {
                    AskOutcome::Shown { mac_fingerprint } => {
                        let ask = HomeAsk { found: found.clone(), mac_fingerprint };
                        self.home_ask = Some(ask.clone());
                        self.touch().home_ask = Some(ask);
                        self.emit(ClientEvent::PairingCodeWanted { mac_name: found.service_name });
                    }
                    AskOutcome::Refused(message) => self.set_failure(message),
                }
            }
            Cmd::Dialed { generation, conn, any_key } => self.dialed(generation, *conn, any_key),
            Cmd::DialFailed { generation, mac, error } => {
                if generation == self.generation {
                    self.dial_failed(&mac, error);
                }
            }
            Cmd::Discovery(list) => {
                self.touch().home_macs = list;
                self.refresh_unpaired();
                // Every change of what the network lists is a look for the automatic reconnect.
                self.reconnect_if_due();
            }
            Cmd::Message { id, msg } => {
                if self.session_id() == Some(id) {
                    self.message(msg);
                }
            }
            Cmd::Thumbnail { id, window_id, jpeg } => {
                if self.session_id() == Some(id) {
                    self.touch().thumbnails.insert(window_id, Arc::new(jpeg));
                    self.emit(ClientEvent::Thumbnail { window_id });
                }
            }
            Cmd::Icon { bundle_id, png } => {
                self.touch().icons.insert(bundle_id.clone(), Arc::new(png));
                self.emit(ClientEvent::Icon { bundle_id });
            }
            Cmd::Cursor { id, shape } => {
                if self.session_id() == Some(id) {
                    self.touch().cursor = Some(Arc::new(shape));
                    self.emit(ClientEvent::CursorShape);
                }
            }
            Cmd::Pointer { id, pointer } => {
                if self.session_id() == Some(id) {
                    let mut p = self.sh.pointer.lock().unwrap();
                    p.pointer_here = false;
                    p.mac_pointer = pointer.position();
                    if p.mac_pointer.is_some() {
                        p.reseeds += 1;
                    }
                    drop(p);
                    self.dirty = true;
                }
            }
            Cmd::VideoSize { id, size } => {
                if self.session_id() == Some(id) && self.st.video_size != Some(size) {
                    self.touch().video_size = Some(size);
                    self.emit(ClientEvent::VideoSize { width: size.0, height: size.1 });
                    self.send_viewport(false);
                }
            }
            Cmd::KeyframeArrived { id } => {
                if self.session_id() == Some(id) {
                    let next = LockedMac::shade_after_frame(self.st.lock_shade, true);
                    self.set_shade(next);
                }
            }
            Cmd::Stats { id, stats } => {
                if self.session_id() == Some(id) {
                    self.touch().stats = Some(stats);
                    let now = now_ms();
                    if self.ledger.expire(now, self.timeout_ms()) {
                        self.notice(format!("{} didn’t answer. Try again.", self.st.mac_name));
                        self.refresh_settings();
                    }
                    if self.menus.expire(now, self.timeout_ms()) {
                        self.refresh_menus();
                    }
                }
            }
            Cmd::Ended { id, end } => self.ended(id, end),
        }
    }

    fn session_id(&self) -> Option<u64> {
        self.session.as_ref().map(|s| s.0)
    }

    fn pairing_device(sh: &Shared) -> PairingDevice<'_> {
        PairingDevice { identity: &sh.identity, name: &sh.config.device_name, model: &sh.config.device_model, allow_loopback: sh.config.allow_loopback }
    }

    fn device_label(&self) -> String {
        format!("{} ({})", self.sh.config.device_name, self.sh.config.device_model)
    }

    // MARK: pairing

    fn checked_code(&mut self, code: &str) -> Option<String> {
        match pairing_code::check(code) {
            pairing_code::Check::Ok(digits) => Some(digits),
            pairing_code::Check::Length => {
                self.pair_error("A code has 12 digits.");
                None
            }
            pairing_code::Check::Typo => {
                self.pair_error("That code has a typo. Check it against your Mac.");
                None
            }
        }
    }

    fn pair_error(&mut self, words: &str) {
        let st = self.touch();
        st.pair_error = Some(words.to_string());
        st.last_error = Some(words.to_string());
        self.emit(ClientEvent::PairFailed { message: words.to_string() });
    }

    fn pair_with_code(&mut self, address: &str, code: &str) {
        let Some(endpoint) = Endpoint::parse(address, 7455) else {
            self.pair_error("Enter the Mac’s address, as its pairing window shows it.");
            return;
        };
        let Some(digits) = self.checked_code(code) else { return };
        if !self.begin_pairing() {
            return;
        }
        let sh = self.sh.clone();
        self.sh.runtime.spawn(async move {
            let outcome = pairer::by_code(&Core::pairing_device(&sh), &endpoint, &digits, &sh.store, None).await;
            let _ = sh.tx.send(Cmd::PairDone(outcome));
        });
    }

    fn pair_with_link(&mut self, text: &str, extra_address: Option<&str>) {
        let link = match PairLink::parse(text) {
            Ok(l) => l,
            Err(why) => {
                self.pair_error(&why);
                return;
            }
        };
        let also = extra_address.filter(|a| !a.trim().is_empty()).and_then(|a| Endpoint::parse(a, link.port));
        if !self.begin_pairing() {
            return;
        }
        let sh = self.sh.clone();
        self.sh.runtime.spawn(async move {
            let outcome = pairer::by_link(&Core::pairing_device(&sh), &link, &sh.store, also).await;
            let _ = sh.tx.send(Cmd::PairDone(outcome));
        });
    }

    fn begin_pairing(&mut self) -> bool {
        if self.st.pairing {
            return false;
        }
        let st = self.touch();
        st.pairing = true;
        st.pair_error = None;
        st.last_error = None;
        true
    }

    fn pair_done(&mut self, outcome: PairOutcome) {
        self.touch().pairing = false;
        match outcome {
            PairOutcome::Failed(message) => self.pair_error(&message),
            PairOutcome::Paired { mac, .. } => {
                self.refresh_saved();
                self.emit(ClientEvent::Paired { mac_id: mac.mac_id.clone() });
                self.connect(mac, false, false);
            }
        }
    }

    // MARK: home (Bonjour)

    /// The home door of a saved Mac on this network, by its recognition tag; never a plain door.
    fn home_of(&self, mac: &SavedMac) -> Option<HomeMac> {
        let key = mac.recognition_key.as_deref().and_then(base64url::decode)?;
        self.st
            .home_macs
            .iter()
            .find(|h| h.door != HomeDoor::Plain && h.tag.as_deref().map(|t| recognition_tag::matches(t, &key)).unwrap_or(false))
            .cloned()
    }

    /// A tap on a Mac at home: its saved record's session; else ask it to show its code (or
    /// connect, on an open door).
    fn tap_home(&mut self, service_name: &str) {
        let Some(found) = self.st.home_macs.iter().find(|h| h.service_name == service_name).cloned() else {
            self.set_failure(format!("{service_name} is no longer on this network."));
            return;
        };
        if let Some(mac) = self.st.saved_macs.clone().into_iter().find(|m| self.home_of(m).as_ref() == Some(&found)) {
            self.touch().last_error = None;
            self.connect(mac, false, false);
            return;
        }
        match found.door {
            HomeDoor::Plain => {
                self.set_failure(format!("{} runs an older Sill. Update Sill on the Mac to connect.", found.service_name));
                return;
            }
            HomeDoor::Open => {
                self.connect_open(&found);
                return;
            }
            HomeDoor::PairingRequired => {}
        }
        if self.st.asking.is_some() {
            return;
        }
        let st = self.touch();
        st.asking = Some(found.service_name.clone());
        st.pair_error = None;
        st.last_error = None;
        let sh = self.sh.clone();
        self.sh.runtime.spawn(async move {
            let outcome = pairer::ask(&Core::pairing_device(&sh), &found, false, None).await;
            let _ = sh.tx.send(Cmd::AskDone { found, outcome });
        });
    }

    /// The code the Mac showed for this device's ask, over the same door, pinned to the key the
    /// ask saw.
    fn pair_home_code(&mut self, code: &str) {
        let Some(ask) = self.home_ask.clone() else { return };
        let Some(digits) = self.checked_code(code) else { return };
        if !self.begin_pairing() {
            return;
        }
        self.home_ask = None;
        self.touch().home_ask = None;
        let sh = self.sh.clone();
        self.sh.runtime.spawn(async move {
            let outcome =
                pairer::by_code(&Core::pairing_device(&sh), &ask.found.endpoint(), &digits, &sh.store, Some(&ask.mac_fingerprint)).await;
            let _ = sh.tx.send(Cmd::PairDone(outcome));
        });
    }

    /// The device's Cancel: the Mac closes the window its ask opened.
    fn cancel_home_ask(&mut self) {
        let Some(ask) = self.home_ask.take() else { return };
        self.touch().home_ask = None;
        let sh = self.sh.clone();
        self.sh.runtime.spawn(async move {
            let _ = pairer::ask(&Core::pairing_device(&sh), &ask.found, true, Some(&ask.mac_fingerprint)).await;
        });
    }

    /// An open door (the Mac's Require pairing is off): a session with any key, nothing saved.
    fn connect_open(&mut self, found: &HomeMac) {
        let transient = SavedMac {
            mac_id: mac_id::make(&[0u8; 32]),
            fingerprint: vec![0u8; 32],
            name: found.service_name.clone(),
            recognition_key: None,
            remote_port: found.port,
            addresses: Vec::new(),
            typed_addresses: vec![found.endpoint().to_string()],
            info_issued_at: 0.0,
            last_worked: None,
            paired_at: 0.0,
            last_connected_at: None,
            revoked: false,
        };
        self.touch().last_error = None;
        self.connect(transient, false, true);
    }

    fn start_discovery(&mut self) {
        if self.discovery.is_none() {
            let tx = self.sh.tx.clone();
            self.discovery = Some(HomeDiscovery::new(&self.sh.config.service_type, self.sh.config.discovery_override.clone(), move |list| {
                let _ = tx.send(Cmd::Discovery(list));
            }));
        }
        if let Some(Err(e)) = self.discovery.as_ref().map(|d| d.start()) {
            self.touch().last_error = Some(format!("Couldn’t look for Macs on this network: {e}"));
        }
    }

    fn stop_discovery(&mut self) {
        if let Some(d) = &self.discovery {
            d.stop();
        }
    }

    // MARK: connecting

    /// A session with a saved Mac: its home door first when Bonjour lists it on this network,
    /// then its remote addresses (Tailscale's first), 1 s apart; the first TLS handshake pinned
    /// to its key wins. `any_key`: an open door's unsaved Mac.
    fn connect(&mut self, mac: SavedMac, reconnecting: bool, any_key: bool) {
        let same = self.st.current_mac.as_ref().map(|m| m.mac_id == mac.mac_id).unwrap_or(false);
        if !reconnecting && same && self.st.phase != Phase::Idle {
            return;
        }
        // A live session replaced (a pairing confirmed while streaming): nothing of it carries over.
        if let Some((_, s)) = self.session.take() {
            *self.sh.session.lock().unwrap() = None;
            s.close(Some("another connection"));
            self.end_session_state();
        }
        self.generation += 1;
        let generation = self.generation;
        self.user_closed = false;
        self.goodbye = None;
        self.session_ready = false;
        self.open_session = any_key;
        // A tap starts afresh: no automatic reconnect left, and an open door is dialed with any key.
        if !reconnecting {
            self.end_reconnect();
            self.open_key = None;
        }
        // The reconnect of an open door's session pins the key that session saw.
        let pin: Option<Vec<u8>> =
            if any_key { self.open_key.filter(|_| reconnecting).map(|k| k.to_vec()) } else { Some(mac.fingerprint.clone()) };
        self.session_start_at = DeviceIdlePolicy::session_start(self.session_start_at, reconnecting, monotonic());
        let home = if any_key { None } else { self.home_of(&mac).map(|h| h.endpoint()) };
        let mut endpoints: Vec<Endpoint> = Vec::new();
        for e in home.into_iter().chain(DialOrder::endpoints(&mac)) {
            if !endpoints.contains(&e) {
                endpoints.push(e);
            }
        }
        let st = self.touch();
        st.current_mac = Some(mac.clone());
        st.mac_name = mac.name.clone();
        st.phase = Phase::Connecting { mac_name: mac.name.clone(), reconnecting };
        st.status = Some(if reconnecting { format!("Reconnecting to {}…", mac.name) } else { format!("Connecting to {}…", mac.name) });
        if endpoints.is_empty() {
            self.touch().phase = Phase::Idle;
            self.set_failure(format!("No address is saved for {}. Pair it again.", mac.name));
            return;
        }
        // A pairing that finished after the app left the screen: the session still lets go after its grace.
        if !self.foreground {
            self.on_background();
        }
        let sh = self.sh.clone();
        self.sh.runtime.spawn(async move {
            match tls::dial_any(&sh.identity, &endpoints, SESSION_ALPN, pin.as_deref(), DIAL_OVERALL).await {
                Ok(conn) => {
                    let _ = sh.tx.send(Cmd::Dialed { generation, conn: Box::new(conn), any_key });
                }
                Err(error) => {
                    let _ = sh.tx.send(Cmd::DialFailed { generation, mac: Box::new(mac), error });
                }
            }
        });
    }

    fn dialed(&mut self, generation: u64, conn: TlsConnection, any_key: bool) {
        if generation != self.generation {
            return; // superseded: the connection closes as it is dropped
        }
        let id = self.next_session_id;
        self.next_session_id += 1;
        if any_key && self.open_key.is_none() {
            self.open_key = Some(conn.mac_fingerprint);
        }
        let route = self.route(&conn.host);
        self.touch().route = route;
        let codecs = self.sh.config.sound.as_ref().map(|s| s.codecs()).filter(|c| !c.is_empty());
        if let Some(s) = &self.sh.config.sound {
            // Away (a VPN or the internet) until the Mac says: the sound's jitter allowance.
            s.set_away(!matches!(route, Some(Route::Local) | Some(Route::Loopback)));
        }
        let hello = payloads::hello(
            &self.sh.config.app_version,
            &self.sh.config.build,
            &self.device_label(),
            codecs.as_deref(),
            Some(LockedMac::VIEW_GENERATION),
        );
        let listener = Arc::new(Link { sh: self.sh.clone(), id });
        let session = Session::start(conn, self.device_label(), hello, listener, &self.sh.runtime);
        *self.sh.session.lock().unwrap() = Some(session.clone());
        self.session = Some((id, session));
        self.set_timer(Timer::FirstList { generation, id }, 10.0);
    }

    fn dial_failed(&mut self, mac: &SavedMac, error: DialError) {
        let auto = self.auto_reconnect.as_ref().map(|r| r.mac_id == mac.mac_id).unwrap_or(false);
        if auto && self.reconnect_dialing {
            self.reconnect_dialing = false;
            if matches!(error.reason, DialReason::WrongMac | DialReason::RefusedKey) {
                // The Mac answered for good: another key, or this device's key refused. Not dialed
                // again by itself; a tap still does.
                self.end_reconnect();
                self.touch().phase = Phase::Idle;
                let words = if error.reason == DialReason::WrongMac && self.open_session {
                    DiscoveryPolicy::other_key_line(&mac.name, "computer")
                } else {
                    error.message
                };
                self.set_failure(words);
                return;
            }
            // One dial that did not connect: the reconnect looks again when its clock says.
            if let Some(r) = &mut self.auto_reconnect {
                r.dial_failed(monotonic());
            }
            self.reconnect_if_due();
            return;
        }
        self.touch().phase = Phase::Idle;
        let words = self.away_words(mac, error.reason).unwrap_or(error.message);
        self.set_failure(words);
    }

    /// Away from home, every address of the Mac a VPN's, and none answered: either end's VPN may
    /// be the one off, so the words name both.
    fn away_words(&self, mac: &SavedMac, reason: DialReason) -> Option<String> {
        if self.home_of(mac).is_some() || !matches!(reason, DialReason::Dns | DialReason::Timeout | DialReason::Network) {
            return None;
        }
        let endpoints = DialOrder::endpoints(mac);
        if endpoints.is_empty() || !endpoints.iter().all(|e| Endpoint::is_tailscale(&e.host)) {
            return None;
        }
        Some(format!("Tailscale looks off on this computer or on {0}. Turn it on for both, then connect to {0}.", mac.name))
    }

    fn disconnect(&mut self) {
        self.note_action();
        self.user_closed = true;
        self.generation += 1;
        self.resume_mac = None;
        self.end_reconnect();
        if let Some((_, s)) = self.session.take() {
            *self.sh.session.lock().unwrap() = None;
            s.close(Some("disconnected by the user"));
        }
        self.end_session_state();
        let st = self.touch();
        st.phase = Phase::Idle;
        st.status = None;
    }

    /// The app went to the background: let the session go after a grace, and come back to it.
    fn on_background(&mut self) {
        if self.st.phase == Phase::Idle || self.st.current_mac.is_none() {
            return;
        }
        self.set_timer(Timer::Suspend, 15.0);
    }

    fn suspend(&mut self) {
        let Some(mac) = self.st.current_mac.clone() else { return };
        self.resume_mac = Some(mac);
        self.user_closed = true;
        self.generation += 1;
        self.end_reconnect(); // the return starts one afresh
        if let Some((_, s)) = self.session.take() {
            *self.sh.session.lock().unwrap() = None;
            s.close(Some("in the background"));
        }
        self.end_session_state();
        let st = self.touch();
        st.phase = Phase::Idle;
        st.status = None;
    }

    fn on_foreground(&mut self) {
        self.foreground = true;
        self.dirty = true;
        self.front_at = monotonic();
        self.cancel_timer(&Timer::Suspend);
        if let Some((_, s)) = &self.session {
            s.reset_liveness();
        }
        // Back on the screen: a reconnect still running dials a listed row at once and counts again.
        if let Some(r) = &mut self.auto_reconnect {
            r.came_forward(monotonic());
        }
        self.review_idle();
        self.consider_keyframe();
        // A session let go in the background comes back through the automatic reconnect.
        if let Some(mac) = self.resume_mac.take() {
            let mac = self.sh.store.by_id(&mac.mac_id).unwrap_or(mac);
            self.start_reconnect(&mac, false);
        }
        self.reconnect_if_due();
    }

    /// The app left the screen (the session itself goes after its grace).
    fn on_stopped(&mut self) {
        self.foreground = false;
        self.dirty = true;
        self.recovery.left_foreground();
        if let Some(s) = &self.sh.config.sound {
            s.stop();
        }
        self.review_idle();
        self.on_background();
    }

    /// A session with `mac` ended by itself, or was let go in the background: the automatic
    /// reconnect. `on_stream`: a session that was streaming keeps the stream screen while the
    /// reconnect is a blip.
    fn start_reconnect(&mut self, mac: &SavedMac, on_stream: bool) {
        self.end_reconnect();
        self.auto_reconnect = Some(ReconnectClock::new(mac.mac_id.clone(), mac.name.clone(), monotonic(), on_stream));
        self.reconnect_if_due();
    }

    fn end_reconnect(&mut self) {
        self.auto_reconnect = None;
        self.reconnect_dialing = false;
        self.cancel_timer(&Timer::ReconnectCheck);
    }

    /// The automatic reconnect's look: at a loss, a dial that did not connect, every change of
    /// what the network lists, the return to the screen, and the time it said to look again.
    fn reconnect_if_due(&mut self) {
        self.cancel_timer(&Timer::ReconnectCheck);
        let Some(r) = &self.auto_reconnect else { return };
        if self.reconnect_dialing || self.session.is_some() {
            return;
        }
        let id = r.mac_id.clone();
        let Some(mac) = self.sh.store.by_id(&id).or_else(|| self.st.current_mac.clone().filter(|m| m.mac_id == id)) else {
            self.end_reconnect();
            return;
        };
        let now = monotonic();
        let listed = if self.open_session { None } else { self.home_of(&mac).map(|h| h.since) };
        let foreground = self.foreground;
        let step = self.auto_reconnect.as_mut().unwrap().next(listed, foreground, now);
        match step {
            ReconnectStep::Dial { line } => {
                self.reconnect_dialing = true;
                let any_key = self.open_session;
                self.connect(mac.clone(), true, any_key);
                if self.st.phase == Phase::Idle {
                    self.end_reconnect(); // nothing to dial: connect said why
                    return;
                }
                if !self.auto_reconnect.as_ref().map(|r| r.shows_stream()).unwrap_or(false) {
                    self.touch().phase = Phase::Connecting { mac_name: mac.name.clone(), reconnecting: false };
                }
                self.set_status(Some(line));
            }
            ReconnectStep::Wait { at, line } => {
                let shows = self.auto_reconnect.as_ref().map(|r| r.shows_stream()).unwrap_or(false);
                self.touch().phase = if shows { Phase::Connecting { mac_name: mac.name.clone(), reconnecting: true } } else { Phase::Idle };
                self.set_status(Some(line));
                if let Some(at) = at {
                    self.set_timer(Timer::ReconnectCheck, (at - now).max(0.01));
                }
            }
        }
    }

    fn end_session_state(&mut self) {
        self.sh.config.video.reset();
        self.session_ready = false;
        self.ledger.reset();
        self.menus.reset();
        {
            let mut p = self.sh.pointer.lock().unwrap();
            p.mac_pointer = None;
            p.pointer_here = true;
        }
        let st = self.touch();
        st.windows.clear();
        st.active = StreamSource::None;
        st.thumbnails.clear();
        st.apps.clear();
        st.video_size = None;
        st.cursor = None;
        st.stats = None;
        st.has_settings = false;
        st.host_settings = None;
        st.shown_settings = None;
        st.pending_settings.clear();
        st.menu = MenuSnapshot::default();
        st.has_menus = false;
        st.route = None;
        st.host_clipboard = 0;
        st.host_key_repeat = 0;
        st.host_gestures = 0;
        st.lock_why = None;
        st.off_console_view = false;
        st.clipboard_shared = false;
        st.clipboard_note = None;
        self.last_viewport_json = None;
        self.set_shade(None);
        if let Some(s) = &self.sh.config.sound {
            s.stop();
        }
        // The clipboard: nothing is subscribed any more, and a ⌘V for this session never goes to the next.
        self.clip = ClipState::default();
        for t in [Timer::ReceiptLate, Timer::PasteWork, Timer::ClipboardNoteExpiry, Timer::KeyframeCheck, Timer::Viewport] {
            self.cancel_timer(&t);
        }
        self.cancel_timer(&Timer::AutoDesktop { id: 0, picked_before: 0 });
        self.cancel_timer(&Timer::FirstList { generation: 0, id: 0 });
        self.recovery.reset();
        self.review_idle();
    }

    // MARK: the session's messages

    fn message(&mut self, msg: Message) {
        match msg.kind {
            kind::WINDOW_LIST => {
                if let Some(list) = WindowList::parse(&msg.payload) {
                    self.window_list(list);
                }
            }
            kind::APP_LIST => self.touch().apps = protocol::parse_app_list(&msg.payload),
            kind::HOST_SETTINGS => {
                if let Some(state) = HostSettingsState::parse(&msg.payload) {
                    self.settings_state(state);
                }
            }
            kind::MAC_INFO => {
                if let Some(signed) = SignedMacInfo::parse(&msg.payload) {
                    self.mac_info(&signed);
                }
            }
            // A goodbye without a readable reason is a notice that asks for nothing: no reconnect.
            kind::GOODBYE => {
                let g = Goodbye::parse(&msg.payload).unwrap_or_default();
                self.emit(ClientEvent::Goodbye { reason: g.reason.clone(), message: g.message.clone() });
                self.goodbye = Some(g);
            }
            kind::CLIPBOARD => self.clipboard_message(&msg.payload),
            kind::PONG => self.receipt(&msg.payload),
            kind::MAC_MENU => {
                if let Some(m) = MacMenu::parse(&msg.payload) {
                    if let MenuOutcome::Pressed { ok, note } = self.menus.receive(&m) {
                        if !ok {
                            self.notice(note.clone().unwrap_or_else(|| MenuState::CHANGED.to_string()));
                        }
                        self.emit(ClientEvent::MenuPressed { ok, note });
                    }
                    self.refresh_menus();
                }
            }
            _ => {}
        }
    }

    fn window_list(&mut self, list: WindowList) {
        let Some((id, session)) = self.session.clone() else { return };
        let first = !self.session_ready;
        let why = LockedMac::why(&list);
        let view = LockedMac::view(&list);
        let was = self.st.active;
        let fallback = self.st.current_mac.as_ref().map(|m| m.name.clone()).unwrap_or_else(|| "Mac".to_string());
        {
            let st = self.touch();
            st.windows = list.windows.clone();
            let name = safe_text::label(&list.mac_name, 64);
            st.mac_name = if name.is_empty() { fallback } else { name };
            st.host_gestures = list.gestures.unwrap_or(0);
            st.host_version = list.host_version.clone();
            st.host_clipboard = list.clipboard.unwrap_or(0);
            st.host_key_repeat = list.key_repeat.unwrap_or(0);
            st.lock_why = why;
            st.off_console_view = view;
            st.thumbnails.retain(|id, _| list.windows.iter().any(|w| w.id == *id));
            st.active = list.active;
        }
        let shade = LockedMac::shade(self.st.lock_shade, why, view);
        self.set_shade(shade);
        if why.is_some() {
            if let Some(s) = &self.sh.config.sound {
                s.stop();
            }
        }
        if first {
            self.session_ready = true;
            self.cancel_timer(&Timer::FirstList { generation: 0, id: 0 });
            {
                let st = self.touch();
                st.phase = Phase::Streaming;
                st.status = None;
                st.last_error = None;
            }
            self.end_reconnect();
            if let Some(mac) = self.st.current_mac.clone() {
                let worked = Endpoint::new(session.host.clone(), session.port).key();
                self.sh.store.update(&mac.mac_id, |m| {
                    m.last_worked = Some(worked);
                    m.last_connected_at = Some(protocol::now());
                    m.revoked = false;
                });
                self.refresh_saved();
                let name = self.st.mac_name.clone();
                self.emit(ClientEvent::Connected { mac_id: mac.mac_id, mac_name: name });
            }
            // The menus' subscription, then what this device wants: its viewport.
            let sub = self.menus.subscribe();
            session.send_json(kind::FETCH_MENU, &payloads::fetch_menu(sub.token, None, None, None));
            self.last_viewport_json = None;
            self.send_viewport(true);
        }
        // Nothing streams: ask for the Desktop (at most every 10 s), unless the watched window
        // just closed, when the device waits 2 s for the person's own pick first.
        if list.active == StreamSource::None {
            let now = now_ms();
            let watched_gone = matches!(was, StreamSource::Window(w) if !list.windows.iter().any(|x| x.id == w));
            if self.last_auto_desktop_at.map(|at| now.saturating_sub(at) > 10_000).unwrap_or(true) {
                if !watched_gone {
                    self.last_auto_desktop_at = Some(now);
                    self.select(StreamSource::Desktop, false);
                } else {
                    let wait = 2.0 + f64::from(self.st.stats.map(|s| s.rtt_max_ms.max(0)).unwrap_or(0)) / 1000.0;
                    self.set_timer(Timer::AutoDesktop { id, picked_before: self.last_user_pick_at }, wait);
                }
            }
        }
        self.clipboard_window_list(&list);
        self.review_idle();
    }

    /// The cover over the stream; "Unlocking…" lasts `UNLOCKING_TIMEOUT_S` at most.
    fn set_shade(&mut self, next: Option<LockShade>) {
        self.cancel_timer(&Timer::ShadeTimeout);
        if self.st.lock_shade != next {
            self.touch().lock_shade = next;
        }
        self.sh.shade_unlocking.store(next == Some(LockShade::Unlocking), Ordering::Relaxed);
        if next == Some(LockShade::Unlocking) {
            self.set_timer(Timer::ShadeTimeout, LockedMac::UNLOCKING_TIMEOUT_S);
        }
    }

    fn settings_state(&mut self, state: HostSettingsState) {
        if let Some(s) = &self.sh.config.sound {
            if state.settings.send_audio == Some(false) {
                s.stop();
            }
            if let Some(stream) = &state.stream {
                s.set_stream_fps(stream.fps);
            }
            if let Some(away) = &state.away {
                s.set_away(away.this_connection_away);
            }
        }
        let away_before = self.st.host_settings.as_ref().and_then(|h| h.away.as_ref()).map(|a| a.this_connection_away);
        let away_now = state.away.as_ref().map(|a| a.this_connection_away);
        let refused = self.ledger.receive(state.clone());
        let st = self.touch();
        st.has_settings = true;
        st.host_settings = Some(state);
        if !refused.is_empty() {
            self.notice(format!("{} kept its setting.", self.st.mac_name));
            self.emit(ClientEvent::SettingsRefused { fields: refused });
        }
        self.refresh_settings();
        // Home or away decides the rate this device asks for.
        if away_before != away_now {
            self.send_viewport(false);
        }
    }

    fn refresh_settings(&mut self) {
        let shown = self.ledger.shown();
        let pending = {
            use super::ledger::SettingsField::*;
            [MaxFps, Bitrate, CaptureScale, PrioritizeSpeed, VirtualDisplay, DirectWireless, SendAudio]
                .into_iter()
                .filter(|f| self.ledger.is_pending(*f))
                .collect()
        };
        let st = self.touch();
        st.shown_settings = shown;
        st.pending_settings = pending;
    }

    fn refresh_menus(&mut self) {
        let snapshot = MenuSnapshot {
            version: self.menus.version(),
            app: self.menus.app().map(str::to_string),
            top: self.menus.top().to_vec(),
            top_note: self.menus.top_note().map(str::to_string),
            top_stale: self.menus.top_stale(),
            menus: self.menus.menus().clone(),
        };
        let st = self.touch();
        st.has_menus = !snapshot.top.is_empty();
        st.menu = snapshot;
    }

    fn mac_info(&mut self, signed: &SignedMacInfo) {
        let Some((_, session)) = &self.session else { return };
        let Some(fp) = verify_mac_info(&signed.info, &signed.key, &signed.sig) else { return };
        if !constant_time_eq(&fp, &session.mac_fingerprint) {
            return;
        }
        let Some(info) = MacInfo::parse(&signed.info) else { return };
        if info.mac_id != mac_id::make(&fp) {
            return;
        }
        let Some(mac) = self.sh.store.by_id(&info.mac_id) else { return };
        if !constant_time_eq(&mac.fingerprint, &fp) || info.issued_at <= mac.info_issued_at {
            return;
        }
        // Never link-local (169.254/16, all of fe80::/10, a scope) or loopback.
        let usable: Vec<_> = info.addresses.iter().filter(|a| SavedMacs::keepable(&a.host, false)).cloned().collect();
        self.sh.store.update(&info.mac_id, |m| {
            let name = safe_text::label(&info.name, 64);
            if !name.is_empty() {
                m.name = name;
            }
            if (1..=65535).contains(&info.remote_port) {
                m.remote_port = info.remote_port as u16;
            }
            if info.remote_access && !usable.is_empty() {
                m.addresses = usable;
            }
            m.info_issued_at = info.issued_at;
        });
        self.refresh_saved();
        let current = self.sh.store.by_id(&info.mac_id);
        self.touch().current_mac = current;
    }

    fn ended(&mut self, id: u64, end: SessionEnd) {
        if self.session_id() != Some(id) {
            return;
        }
        self.session = None;
        *self.sh.session.lock().unwrap() = None;
        let mac = self.st.current_mac.clone();
        let was_streaming = self.session_ready;
        self.end_session_state();
        let Some(mac) = mac.filter(|_| !self.user_closed) else { return };
        let g = self.goodbye.clone();
        let name = mac.name.clone();
        let at_home = self.home_of(&mac).is_some();
        // The Mac refused this device's key right after the handshake: as its goodbye "removed"
        // (upstream's DiscoveryPolicy: the saved Mac is revoked, nothing reconnects).
        let key_refused = end.key_refused && g.is_none();
        let (words, reconnect) = if key_refused {
            (Some(format!("{name} removed this device. Pair it again to connect.")), false)
        } else {
            goodbye_policy(g.as_ref(), &name, at_home)
        };
        if key_refused || g.as_ref().map(|g| g.reason == "removed").unwrap_or(false) {
            self.sh.store.update(&mac.mac_id, |m| m.revoked = true);
            self.refresh_saved();
        }
        if reconnect {
            let own_dial = self.auto_reconnect.as_ref().map(|r| r.mac_id == mac.mac_id).unwrap_or(false) && !was_streaming;
            if own_dial {
                // The reconnect's own dial ended before its first window list, with no goodbye that
                // ends it: one dial that did not connect, not a new loss.
                self.reconnect_dialing = false;
                if let Some(r) = &mut self.auto_reconnect {
                    r.dial_failed(monotonic());
                }
                self.reconnect_if_due();
            } else {
                self.start_reconnect(&mac, was_streaming);
            }
            // The Mac's own words ("‹Mac› quit Sill.") stay until the reconnect's next line.
            if let (Some(w), true) = (&words, self.auto_reconnect.is_some()) {
                self.set_status(Some(w.clone()));
            }
            self.emit(ClientEvent::SessionEnded { message: words.or(end.why), reconnecting: self.auto_reconnect.is_some() });
            return;
        }
        self.end_reconnect();
        self.touch().phase = Phase::Idle;
        let words = words.unwrap_or_else(|| format!("Lost the connection to {name}."));
        self.set_failure(words.clone());
        self.emit(ClientEvent::SessionEnded { message: Some(words), reconnecting: false });
    }

    // MARK: keyframe recovery (kind 44)

    /// Ask the Mac for a keyframe when `KeyframeRecovery` says so, and look again when it is due.
    fn consider_keyframe(&mut self) {
        self.cancel_timer(&Timer::KeyframeCheck);
        let Some((_, s)) = self.session.clone() else { return };
        let streaming = self.session_ready && self.st.active != StreamSource::None;
        let now = monotonic();
        let wait = KeyframeRecovery::answer_wait(self.st.stats.map(|s| s.rtt_max_ms));
        let again = match self.recovery.consider(now, self.foreground, streaming, wait) {
            KeyframeDecision::Nothing => return,
            KeyframeDecision::AskNow => {
                s.send(kind::KEYFRAME_REQUEST, &[]);
                self.emit(ClientEvent::KeyframeRequested);
                now + wait
            }
            KeyframeDecision::AskAt(at) | KeyframeDecision::KeyframeDue(at) => at,
        };
        self.set_timer(Timer::KeyframeCheck, (again - now).max(0.05));
    }

    // MARK: holding the screen on (DeviceIdlePolicy)

    /// One of the stream screen's controls was used: counts as use.
    fn note_action(&mut self) {
        self.control_at = monotonic();
        if !self.st.hold_awake {
            self.review_idle();
        }
    }

    fn review_idle(&mut self) {
        self.cancel_timer(&Timer::IdleCheck);
        let now = monotonic();
        let input_at = f64::from_bits(self.sh.input_at.load(Ordering::Relaxed));
        let last = [input_at, self.control_at, self.front_at, self.session_start_at.unwrap_or(0.0)].into_iter().fold(0.0, f64::max);
        let streaming = self.st.phase == Phase::Streaming && self.st.active != StreamSource::None;
        let ends = DeviceIdlePolicy::hold_ends(self.foreground, streaming, Some(last).filter(|l| *l > 0.0), now, DeviceIdlePolicy::IDLE_LIMIT_S);
        if self.st.hold_awake != ends.is_some() {
            self.touch().hold_awake = ends.is_some();
        }
        if let Some(ends) = ends {
            self.set_timer(Timer::IdleCheck, ends - now + 0.05);
        }
    }

    // MARK: what the screens do

    fn select(&mut self, source: StreamSource, user: bool) {
        let Some((_, s)) = self.session.clone() else { return };
        if user {
            self.last_user_pick_at = now_ms();
            self.note_action();
        }
        s.send_json(kind::SELECT_SOURCE, &payloads::select(source));
    }

    fn wanted_fps(&self) -> u32 {
        let away = self.st.host_settings.as_ref().and_then(|h| h.away.as_ref()).map(|a| a.this_connection_away).unwrap_or(true);
        let max = self.display_fps.max(30);
        if away { max.min(60) } else { max }
    }

    fn send_viewport(&mut self, immediately: bool) {
        self.cancel_timer(&Timer::Viewport);
        if immediately {
            self.send_viewport_now();
        } else {
            self.set_timer(Timer::Viewport, 0.25);
        }
    }

    fn send_viewport_now(&mut self) {
        let Some((_, s)) = &self.session else { return };
        if !self.session_ready {
            return;
        }
        let Some((w, h)) = self.panel else { return };
        let Some(json) = payloads::viewport(w, h, self.st.text_scale, self.wanted_fps(), true) else { return };
        if self.last_viewport_json.as_deref() == Some(json.as_str()) {
            return;
        }
        s.send_json(kind::VIEWPORT, &json);
        self.last_viewport_json = Some(json);
    }

    fn change_settings(&mut self, change: &SettingsChange) {
        self.note_action();
        let Some((_, s)) = &self.session else { return };
        let Some((token, out)) = self.ledger.pick(change, now_ms()) else { return };
        s.send_json(kind::CHANGE_SETTINGS, &payloads::change_settings(token, &out));
        self.refresh_settings();
    }

    fn timeout_ms(&self) -> u64 {
        4_000u64.max(4 * self.st.stats.map(|s| s.rtt_max_ms.max(0)).unwrap_or(0) as u64)
    }

    fn set_prefs(&mut self, change: Box<dyn FnOnce(&mut Prefs) + Send>) {
        let before = self.st.prefs.clone();
        let mut prefs = before.clone();
        change(&mut prefs);
        if prefs == before {
            return;
        }
        self.note_action();
        if prefs.sound_muted != before.sound_muted {
            if let Some(s) = &self.sh.config.sound {
                s.set_muted(prefs.sound_muted);
            }
        }
        let clipboard_switched = prefs.share_clipboard != before.share_clipboard;
        self.touch().prefs = prefs.clone();
        if clipboard_switched {
            self.update_subscription();
        }
        let _ = std::fs::create_dir_all(&self.sh.config.config_dir);
        if let Ok(json) = serde_json::to_vec_pretty(&prefs) {
            let _ = std::fs::write(prefs_path(&self.sh.config), json);
        }
    }

    fn route(&self, host: &str) -> Option<Route> {
        if Endpoint::is_loopback(host) {
            Some(Route::Loopback)
        } else if self.st.home_macs.iter().any(|h| h.host == host) {
            // What the network's browser listed is the home door, whatever its address looks like.
            Some(Route::Local)
        } else if Endpoint::is_tailscale(host) {
            Some(Route::Tailscale)
        } else if is_private(host) {
            Some(Route::Local)
        } else {
            None
        }
    }

    // MARK: the clipboard (kind 32; StreamClient+Clipboard)

    fn send_clipboard(&self, payload: &[u8]) {
        if let Some((_, s)) = &self.session {
            s.send(kind::CLIPBOARD, payload);
        }
    }

    /// Every window list: subscribe, stop, or say nothing, as its `clipboard` and `locked` allow.
    fn clipboard_window_list(&mut self, list: &WindowList) {
        self.clip.streaming = list.active != StreamSource::None;
        let value = ClipboardPolicy::host_clipboard(list.clipboard, list.locked);
        self.clip.host_clipboard = value;
        let takes = ClipboardPolicy::host_takes(value);
        if self.st.clipboard_shared != takes {
            self.touch().clipboard_shared = takes;
        }
        self.update_subscription();
    }

    fn update_subscription(&mut self) {
        if self.session.is_none() {
            return;
        }
        let (step, after) = ClipboardPolicy::device_subscription(self.clip.host_clipboard, self.st.prefs.share_clipboard, self.clip.subscribed);
        self.clip.subscribed = after;
        match step {
            Subscription::Subscribe => self.send_clipboard(&ClipboardPolicy::subscription()),
            Subscription::Stop => self.send_clipboard(&ClipboardPolicy::stopping()),
            Subscription::None => {}
        }
    }

    /// A kind 32 from the Mac: its text onto this computer's clipboard.
    fn clipboard_message(&mut self, payload: &[u8]) {
        let Some(board) = self.sh.config.clipboard.clone() else { return };
        let count = board.change_count();
        let Some(text) =
            ClipboardPolicy::device_writes(payload, self.st.prefs.share_clipboard, self.clip.host_clipboard, self.clip.last_sent.as_ref(), count)
        else {
            return;
        };
        if board.write_text(&text) {
            self.emit(ClientEvent::ClipboardFromMac { text });
        }
    }

    /// Paste: this computer's text (or `text`) to the Mac, the receipt's ping after it on the same
    /// connection, and ⌘V once the Mac has it (Paste on ‹Mac› Right Away).
    fn paste_clipboard(&mut self, text: Option<String>) {
        self.note_action();
        let Some((_, s)) = self.session.clone() else { return };
        let board = self.sh.config.clipboard.clone();
        let strings: Vec<String> = match text {
            Some(t) => vec![t],
            None => board.as_ref().and_then(|b| b.read_text()).into_iter().collect(),
        };
        let mac = self.mac_word();
        match ClipboardPolicy::device_sends(&strings, true, self.clip.host_clipboard, self.st.prefs.share_clipboard) {
            DeviceSend::Send(payload) => {
                s.send(kind::CLIPBOARD, &payload);
                // Its receipt: until it comes nothing says it was sent.
                let mut nonce = [0u8; ClipboardPolicy::RECEIPT_LENGTH];
                let _ = SystemRandom::new().fill(&mut nonce);
                s.send(kind::PING, &nonce);
                let count = board.as_ref().map(|b| b.change_count()).unwrap_or(-1);
                if let Some(t) = strings.into_iter().find(|t| !t.is_empty()) {
                    self.clip.last_sent = Some(Sent { text: t, change_count: count });
                }
                self.clip.receipt = Some(Receipt { nonce, late: false });
                let wait = ClipboardPolicy::receipt_wait(self.st.stats.map(|s| s.rtt_max_ms));
                self.set_timer(Timer::ReceiptLate, wait);
            }
            DeviceSend::TooLong => self.clipboard_note(copy::too_long(&mac), copy::too_long(&mac), true),
            DeviceSend::Nothing => {}
        }
    }

    /// A pong of `RECEIPT_LENGTH` bytes (a ping's own are 8): when it is this Paste's, the Mac has
    /// read the text; ⌘V follows with Paste Right Away on.
    fn receipt(&mut self, payload: &[u8]) {
        let Some(r) = &self.clip.receipt else { return };
        if r.nonce != payload {
            return;
        }
        let late = r.late;
        self.clip.receipt = None;
        self.cancel_timer(&Timer::ReceiptLate);
        if !self.pastes_now(late) {
            self.receipt_noted(false);
            return;
        }
        // ⌘V on the Mac, PASTE_DELAY after the receipt; the session's end cancels it.
        self.set_timer(Timer::PasteWork, ClipboardPolicy::PASTE_DELAY);
    }

    fn pastes_now(&self, late: bool) -> bool {
        ClipboardPolicy::pastes_on_receipt(self.st.prefs.paste_right_away, late, self.clip.streaming, self.clip.host_clipboard, false)
    }

    fn paste_work(&mut self) {
        let Some((_, s)) = self.session.clone() else { return };
        if !self.pastes_now(false) {
            self.receipt_noted(false);
            return;
        }
        self.sh.note_input();
        press(&s, ClipboardPolicy::PASTE_KEY, key_mods::COMMAND);
        self.receipt_noted(true);
    }

    fn receipt_noted(&mut self, pasted: bool) {
        let (text, spoken) = ClipboardPolicy::receipt_note(pasted, &self.mac_word());
        self.clipboard_note(text, spoken, false);
    }

    /// No receipt in time: "Didn’t reach…".
    fn receipt_late(&mut self) {
        let Some(r) = &mut self.clip.receipt else { return };
        if r.late {
            return;
        }
        r.late = true;
        let mac = self.mac_word();
        self.clipboard_note(copy::not_sent(&mac), copy::not_sent(&mac), true);
    }

    fn clipboard_note(&mut self, text: String, spoken: String, warning: bool) {
        let note = ClipboardNote { text, spoken, warning };
        self.touch().clipboard_note = Some(note.clone());
        self.emit(ClientEvent::ClipboardNote(note));
        self.set_timer(Timer::ClipboardNoteExpiry, ClipboardPolicy::NOTE_LIFETIME);
    }

    fn mac_word(&self) -> String {
        if self.st.mac_name.is_empty() { "the Mac".to_string() } else { self.st.mac_name.clone() }
    }
}

/// A shortcut as a keyboard sends it: its modifiers' own keys around it (`KeyChord.press`).
pub(crate) fn press(s: &Session, usage: u16, modifiers: u64) {
    let mods = Keys::modifier_keys(modifiers);
    let mut held = 0u64;
    for (u, bit) in &mods {
        held |= bit;
        s.send_input(&input::key(*u, true, held), false);
    }
    s.send_input(&input::key(usage, true, held), false);
    s.send_input(&input::key(usage, false, held), false);
    for (u, bit) in mods.iter().rev() {
        held &= !bit;
        s.send_input(&input::key(*u, false, held), false);
    }
}

/// `GoodbyePolicy`: the words, and whether this device reconnects by itself.
pub fn goodbye_policy(g: Option<&Goodbye>, name: &str, at_home: bool) -> (Option<String>, bool) {
    let Some(g) = g else { return (None, true) };
    match g.reason.as_str() {
        "quit" => (Some(format!("{name} quit Sill.")), true),
        "busy" => (Some(format!("{name} is serving as many devices as it can.")), true),
        "removed" => (Some(format!("{name} removed this device. Pair it again to connect.")), false),
        "pairingRequired" => (Some(format!("{name} now asks devices to pair. Pair this device again.")), false),
        // Never back through the remote door; at home its home door still serves this device.
        "remoteOff" => (Some(format!("Remote Access is off on {name}.")), at_home),
        "internetOff" => (Some(format!("{name} no longer takes connections from the internet.")), at_home),
        _ => (
            Some(g.message.as_deref().map(|m| safe_text::label(m, 300)).filter(|m| !m.is_empty()).unwrap_or_else(|| format!("{name} ended the session."))),
            g.reconnect == Some(true),
        ),
    }
}

fn is_private(host: &str) -> bool {
    let p: Vec<u32> = host.split('.').filter_map(|x| x.parse().ok()).collect();
    if p.len() == 4 {
        return p[0] == 10 || (p[0] == 192 && p[1] == 168) || (p[0] == 172 && (16..=31).contains(&p[1]));
    }
    let lower = host.to_lowercase();
    lower.starts_with("fe80:") || (lower.starts_with("fd") && !Endpoint::is_tailscale(host))
}
