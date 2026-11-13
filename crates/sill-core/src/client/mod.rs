//! The client (iOSClient/StreamClient.swift's job): the saved Macs, pairing, one session at a
//! time, the catalog, the settings, the menus, the pointer, the clipboard, and everything the
//! screens show.
//!
//! [`Client`] is a handle: `Send + Sync`, cheap to clone, with plain methods that never block on
//! the network. The crate owns its tokio runtime (two worker threads). What the screens show is
//! [`Client::state`], a snapshot; one-shot things come through [`Client::poll_event`]; the waker
//! given in [`ClientConfig`] is called whenever either has something new. Video goes straight
//! from the session's reader to the [`VideoSink`], never through the state.

pub mod clipboard;
mod core;
pub mod keyframe;
pub mod keys;
pub mod ledger;
pub mod menu;
pub mod modules;
pub mod state;

use std::collections::VecDeque;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::time::{Duration, Instant};

use tokio::sync::mpsc;

pub use self::core::goodbye_policy;
use self::core::{Cmd, Core, PointerState, Shared};
pub use clipboard::{Clipboard, ClipboardPolicy};
pub use keyframe::{DeviceIdlePolicy, KeyframeDecision, KeyframeRecovery};
pub use keys::{usage, ForwardedKey, ForwardedKeys, KeyTyping, Keys};
pub use ledger::{SettingsField, SettingsLedger};
pub use menu::{Menu, MenuOutcome, MenuRequest, MenuState};
pub use modules::{ClipboardBridge, NoVideo, SoundPlayer, VideoSink};
pub use state::*;

use crate::crypto::{DeviceIdentity, FileKeyStore, KeyStore};
use crate::net::SavedMacs;
use crate::protocol::{input, kind, payloads, MacMenuItem, PointerAction, ScrollPhase, SettingsChange, StreamSource};

/// A mouse button the Mac takes (the protocol has no others).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MouseButton {
    Left,
    Right,
}

/// Shuts the runtime down when the last handle goes.
struct RuntimeHolder(Mutex<Option<tokio::runtime::Runtime>>);

impl Drop for RuntimeHolder {
    fn drop(&mut self) {
        if let Some(rt) = self.0.lock().unwrap().take() {
            rt.shutdown_background();
        }
    }
}

/// The client's handle. Clones share one client; when the last is dropped its sessions close.
#[derive(Clone)]
pub struct Client {
    sh: Arc<Shared>,
    _runtime: Arc<RuntimeHolder>,
}

/// What a video sink reports back, so keyframe recovery works (`VideoDecoder.Events`). Cheap to
/// clone; callable from the decoder's thread.
#[derive(Clone)]
pub struct VideoFeedback {
    tx: mpsc::UnboundedSender<Cmd>,
}

impl VideoFeedback {
    /// The picture was interrupted (the decoder was rebuilt, dropped frames or met an error) and
    /// waits for a keyframe: the client asks the Mac for one (kind 44) as `KeyframeRecovery` paces.
    pub fn picture_interrupted(&self) {
        let _ = self.tx.send(Cmd::PictureInterrupted);
    }

    /// A keyframe was decoded and shown: the picture is back.
    pub fn keyframe_shown(&self) {
        let _ = self.tx.send(Cmd::KeyframeShown);
    }
}

impl Client {
    /// Makes the client: loads (or creates) this device's key, the saved Macs and the
    /// preferences, and starts its runtime. Nothing is dialed until asked.
    pub fn new(config: ClientConfig) -> io::Result<Client> {
        let key_store: Arc<dyn KeyStore> = match &config.key_store {
            Some(k) => k.clone(),
            None => Arc::new(FileKeyStore::new(config.config_dir.clone())),
        };
        let identity = DeviceIdentity::load_or_create(key_store.as_ref())?;
        let store = SavedMacs::open(config.config_dir.clone());
        let prefs = core::load_prefs(&config);
        let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(2).thread_name("sill-core").enable_all().build()?;
        let (tx, rx) = mpsc::unbounded_channel();
        if let Some(s) = &config.sound {
            s.set_muted(prefs.sound_muted);
        }
        let initial = ClientState {
            saved_macs: store.all(),
            mac_name: "Mac".to_string(),
            pointer_here: true,
            foreground: true,
            prefs,
            ..Default::default()
        };
        let sh = Arc::new(Shared {
            tx,
            runtime: runtime.handle().clone(),
            waker: RwLock::new(config.waker.clone()),
            gestures_enabled: AtomicBool::new(initial.prefs.trackpad_gestures),
            config,
            identity,
            store,
            state: Mutex::new(Arc::new(initial.clone())),
            changed: Condvar::new(),
            events: Mutex::new(VecDeque::new()),
            session: Mutex::new(None),
            pointer: Mutex::new(PointerState { mac_pointer: None, pointer_here: true, own_pointer: (0.5, 0.5), reseeds: 0 }),
            host_gestures: AtomicI64::new(0),
            active_is_window: AtomicBool::new(false),
            shade_unlocking: AtomicBool::new(false),
            hold_awake: AtomicBool::new(false),
            input_at: AtomicU64::new(0f64.to_bits()),
        });
        runtime.spawn(Core::new(sh.clone(), initial).run(rx));
        Ok(Client { sh, _runtime: Arc::new(RuntimeHolder(Mutex::new(Some(runtime)))) })
    }

    fn cmd(&self, cmd: Cmd) {
        let _ = self.sh.tx.send(cmd);
    }

    // MARK: observing

    /// What the screens show, as of now. Cheap: the snapshot is shared.
    pub fn state(&self) -> Arc<ClientState> {
        self.sh.state.lock().unwrap().clone()
    }

    /// The next one-shot event, oldest first.
    pub fn poll_event(&self) -> Option<ClientEvent> {
        self.sh.events.lock().unwrap().pop_front()
    }

    /// Every event waiting.
    pub fn drain_events(&self) -> Vec<ClientEvent> {
        self.sh.events.lock().unwrap().drain(..).collect()
    }

    /// Replaces the waker (called at every state change and event, from the client's threads).
    pub fn set_waker(&self, waker: Option<Waker>) {
        *self.sh.waker.write().unwrap() = waker;
    }

    /// Blocks the calling thread until `ready` holds for the state, or `timeout` passes; the
    /// state then, and whether it held. For tests and scripts, never the UI thread.
    pub fn wait_for(&self, timeout: Duration, ready: impl Fn(&ClientState) -> bool) -> Result<Arc<ClientState>, Arc<ClientState>> {
        let deadline = Instant::now() + timeout;
        let mut guard = self.sh.state.lock().unwrap();
        loop {
            if ready(&guard) {
                return Ok(guard.clone());
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(guard.clone());
            }
            guard = self.sh.changed.wait_timeout(guard, deadline - now).unwrap().0;
        }
    }

    /// SHA-256 of this device's SubjectPublicKeyInfo: what a Mac lists for it.
    pub fn device_fingerprint(&self) -> [u8; 32] {
        *self.sh.identity.fingerprint()
    }

    // MARK: pairing and the saved Macs

    /// Pairs by the address and the 12-digit code of the Mac's pairing window, then connects.
    /// `state.pairing` while it runs; `state.pair_error` or [`ClientEvent::Paired`] after.
    pub fn pair_with_code(&self, address: &str, code: &str) {
        self.cmd(Cmd::PairWithCode { address: address.to_string(), code: code.to_string() });
    }

    /// Pairs by a `sill://pair` link (the QR code's content), then connects. `extra_address`: an
    /// address to dial before the link's own.
    pub fn pair_with_link(&self, link: &str, extra_address: Option<&str>) {
        self.cmd(Cmd::PairWithLink { text: link.to_string(), extra_address: extra_address.map(str::to_string) });
    }

    pub fn clear_pair_error(&self) {
        self.cmd(Cmd::ClearPairError);
    }

    /// A session with a saved Mac: its home door first when the network lists it, then its
    /// remote addresses.
    pub fn connect(&self, mac_id: &str) {
        self.cmd(Cmd::Connect { mac_id: mac_id.to_string() });
    }

    /// A click on a Mac the network lists (`state.home_macs`, by its `service_name`): a saved
    /// Mac's session; an unpaired one is asked to show its code
    /// ([`ClientEvent::PairingCodeWanted`], then [`pair_home_code`](Self::pair_home_code)); an
    /// open door is connected to with nothing saved.
    pub fn connect_home(&self, service_name: &str) {
        self.cmd(Cmd::TapHome { service_name: service_name.to_string() });
    }

    /// The code the Mac showed for this device's ask.
    pub fn pair_home_code(&self, code: &str) {
        self.cmd(Cmd::PairHomeCode { code: code.to_string() });
    }

    /// Cancel instead: the Mac closes the window its ask opened.
    pub fn cancel_home_ask(&self) {
        self.cmd(Cmd::CancelHomeAsk);
    }

    /// Ends the session, or the attempt to get one.
    pub fn disconnect(&self) {
        self.cmd(Cmd::Disconnect);
    }

    /// Forgets a saved Mac (and ends its session).
    pub fn forget(&self, mac_id: &str) {
        self.cmd(Cmd::Forget { mac_id: mac_id.to_string() });
    }

    /// Browses the network for home doors (`state.home_macs`).
    pub fn start_discovery(&self) {
        self.cmd(Cmd::StartDiscovery);
    }

    pub fn stop_discovery(&self) {
        self.cmd(Cmd::StopDiscovery);
    }

    /// Whether the app is in front. Leaving (`false`) lets the session go after 15 s, as a phone
    /// does in the background, and coming back reconnects: a desktop app that should keep
    /// streaming while minimized simply never says `false`.
    pub fn set_foreground(&self, foreground: bool) {
        self.cmd(Cmd::SetForeground(foreground));
    }

    // MARK: the session

    /// What to stream: a window, the Desktop, or nothing.
    pub fn select_source(&self, source: StreamSource) {
        self.cmd(Cmd::Select { source });
    }

    /// Opens an installed app on the Mac; its first window gets selected.
    pub fn launch_app(&self, bundle_id: &str) {
        self.cmd(Cmd::Launch { bundle_id: bundle_id.to_string() });
    }

    /// Close, minimize or full-screen a window ([`crate::protocol::window_actions`]).
    pub fn window_command(&self, window_id: u32, action: &str) {
        self.cmd(Cmd::WindowCommand { id: window_id, action: action.to_string() });
    }

    /// The stream panel's size in points (logical pixels): the Mac sizes the window to it.
    pub fn set_viewport(&self, width: f64, height: f64) {
        self.cmd(Cmd::SetPanel { width, height });
    }

    /// The display's refresh rate changed (the window moved to another monitor).
    pub fn set_display_fps(&self, fps: u32) {
        self.cmd(Cmd::SetDisplayFps(fps));
    }

    /// Aa: the text scale ([`crate::protocol::TextScale`]); `None`: off.
    pub fn set_text_scale(&self, scale: Option<f64>) {
        self.cmd(Cmd::ChooseTextScale(scale));
    }

    /// Changes the Mac's settings: only what differs from what is shown is sent (kind 17), shown
    /// at once, and put back if the Mac refuses or does not answer.
    pub fn change_settings(&self, change: SettingsChange) {
        self.cmd(Cmd::ChangeSettings(change));
    }

    /// Opens one of the menu bar's menus (or a submenu): its items come into `state.menu.menus`.
    pub fn open_menu(&self, item: &MacMenuItem) {
        self.cmd(Cmd::OpenMenu(item.clone()));
    }

    /// Chooses a menu item ([`ClientEvent::MenuPressed`] answers).
    pub fn choose_menu_item(&self, item: &MacMenuItem) {
        self.cmd(Cmd::ChooseMenuItem(item.clone()));
    }

    /// Paste on the Mac: this computer's clipboard text (read through the [`ClipboardBridge`]) to
    /// the Mac's clipboard, and ⌘V there once it has it while `prefs.paste_right_away` is on.
    pub fn paste_clipboard(&self) {
        self.cmd(Cmd::PasteClipboard { text: None });
    }

    /// The same with a text of the caller's.
    pub fn paste_text(&self, text: &str) {
        self.cmd(Cmd::PasteClipboard { text: Some(text.to_string()) });
    }

    /// Changes this device's own preferences (saved to `prefs.json`; Share Clipboard and Mute
    /// take effect at once).
    pub fn update_prefs(&self, change: impl FnOnce(&mut Prefs) + Send + 'static) {
        self.cmd(Cmd::SetPrefs(Box::new(change)));
    }

    /// A control of the stream screen was used: counts as use for `state.hold_awake`.
    pub fn note_action(&self) {
        self.cmd(Cmd::NoteAction);
    }

    /// Shows a line for a few seconds (`state.notice`).
    pub fn show_notice(&self, text: &str) {
        self.cmd(Cmd::Notice(text.to_string()));
    }

    // MARK: video feedback

    /// The handle a [`VideoSink`] reports back through.
    pub fn video_feedback(&self) -> VideoFeedback {
        VideoFeedback { tx: self.sh.tx.clone() }
    }

    /// See [`VideoFeedback::picture_interrupted`].
    pub fn picture_interrupted(&self) {
        self.cmd(Cmd::PictureInterrupted);
    }

    /// See [`VideoFeedback::keyframe_shown`].
    pub fn keyframe_shown(&self) {
        self.cmd(Cmd::KeyframeShown);
    }

    // MARK: input (kind 8), any thread, straight to the session

    /// A pointer event at (x, y), fractions of the video frame (0…1 across its width and height).
    /// Moves are coalesced: at most one every 8 ms, the newest.
    pub fn pointer(&self, action: PointerAction, x: f64, y: f64) {
        self.pointer_from(action, x, y, false);
    }

    fn pointer_from(&self, action: PointerAction, x: f64, y: f64, relative: bool) {
        let Some(s) = self.sh.current_session() else { return };
        self.sh.note_input();
        s.send_input(&input::pointer(action, x, y), action == PointerAction::Move);
        self.sh.took_pointer(x, y, relative);
    }

    /// The mouse moved to (x, y) in stream coordinates (frame fractions).
    pub fn mouse_move(&self, x: f64, y: f64) {
        self.pointer(PointerAction::Move, x, y);
    }

    /// A mouse button went down or up at (x, y).
    pub fn mouse_button(&self, button: MouseButton, down: bool, x: f64, y: f64) {
        let action = match (button, down) {
            (MouseButton::Left, true) => PointerAction::LeftDown,
            (MouseButton::Left, false) => PointerAction::LeftUp,
            (MouseButton::Right, true) => PointerAction::RightDown,
            (MouseButton::Right, false) => PointerAction::RightUp,
        };
        self.pointer(action, x, y);
    }

    /// Where a relative pointer carries on from: the Mac's last reported position while it had
    /// the pointer, else where this device left it.
    pub fn pointer_anchor(&self) -> (f64, f64) {
        let p = self.sh.pointer.lock().unwrap();
        if p.pointer_here { p.own_pointer } else { p.mac_pointer.unwrap_or(p.own_pointer) }
    }

    /// The mouse moved by (dx, dy) frame fractions from the anchor (a captured or relative
    /// mouse): the protocol's positions are absolute, so the new one is sent. Returns it.
    pub fn mouse_move_relative(&self, dx: f64, dy: f64) -> (f64, f64) {
        let (ax, ay) = self.pointer_anchor();
        let (x, y) = ((ax + dx).clamp(0.0, 1.0), (ay + dy).clamp(0.0, 1.0));
        self.pointer_from(PointerAction::Move, x, y, true);
        (x, y)
    }

    /// A button of a relative mouse, at the anchor.
    pub fn mouse_button_relative(&self, button: MouseButton, down: bool) {
        let (x, y) = self.pointer_anchor();
        let action = match (button, down) {
            (MouseButton::Left, true) => PointerAction::LeftDown,
            (MouseButton::Left, false) => PointerAction::LeftUp,
            (MouseButton::Right, true) => PointerAction::RightDown,
            (MouseButton::Right, false) => PointerAction::RightUp,
        };
        self.pointer_from(action, x, y, true);
    }

    /// Scroll with the cursor at (x, y). `dx`/`dy` are fractions of the frame the content should
    /// move, natural-scrolling sign: positive `dy` moves the content down.
    pub fn scroll(&self, x: f64, y: f64, dx: f64, dy: f64) {
        let Some(s) = self.sh.current_session() else { return };
        self.sh.note_input();
        s.send_input(&input::scroll(x, y, dx, dy), false);
        self.sh.took_pointer(x, y, false);
    }

    /// A scroll gesture's boundary, so the Mac sees a trackpad-like gesture (rubber-banding, and
    /// momentum): `Began` → [`scroll`](Self::scroll)… → `Ended`, then optionally `MomentumBegan` →
    /// scrolls… → `MomentumEnded`. A wheel's clicks need none.
    pub fn scroll_phase(&self, phase: ScrollPhase, x: f64, y: f64) {
        if let Some(s) = self.sh.current_session() {
            s.send_input(&input::scroll_gesture(phase, x, y), false);
        }
    }

    /// Typed text. `"\n"`, `"\t"` and `"\u{8}"` reach the Mac as their keys; the rest is typed as
    /// Unicode.
    pub fn text(&self, text: &str) {
        let Some(s) = self.sh.current_session() else { return };
        self.sh.note_input();
        s.send_input(&input::text(text), false);
    }

    /// A physical key down or up by its USB HID keyboard usage ([`usage`]), with the modifiers
    /// held ([`crate::protocol::key_mods`]). The Mac's own layout makes the character and repeats
    /// a held key (`state.host_key_repeat` ≥ 1). [`ForwardedKeys`] keeps downs and ups paired.
    pub fn key(&self, hid_usage: u16, down: bool, modifiers: u64) {
        let Some(s) = self.sh.current_session() else { return };
        self.sh.note_input();
        s.send_input(&input::key(hid_usage, down, modifiers), false);
    }

    /// A shortcut as a keyboard sends it: its modifiers' own keys around the key (⌘V is
    /// `press(usage::V, key_mods::COMMAND)`).
    pub fn press(&self, hid_usage: u16, modifiers: u64) {
        let Some(s) = self.sh.current_session() else { return };
        self.sh.note_input();
        core::press(&s, hid_usage, modifiers);
    }

    /// The modifier keys of `modifiers` go down, in a keyboard's order (⌃ ⇧ ⌥ ⌘).
    pub fn modifiers_down(&self, modifiers: u64) {
        let Some(s) = self.sh.current_session() else { return };
        let mut held = 0u64;
        for (u, bit) in Keys::modifier_keys(modifiers) {
            held |= bit;
            s.send_input(&input::key(u, true, held), false);
        }
    }

    /// And up again, in reverse.
    pub fn modifiers_up(&self, modifiers: u64) {
        let Some(s) = self.sh.current_session() else { return };
        let mut held = modifiers;
        for (u, bit) in Keys::modifier_keys(modifiers).into_iter().rev() {
            held &= !bit;
            s.send_input(&input::key(u, false, held), false);
        }
    }

    /// A three- or four-finger trackpad gesture for the Mac's own shortcut (kind 28,
    /// [`crate::protocol::gestures`]); the Desktop is selected first. Only to a Mac whose list
    /// says `gestures` ≥ 1 and while `prefs.trackpad_gestures` is on; whether it was sent.
    pub fn gesture(&self, name: &str, fingers: u32) -> bool {
        let Some(s) = self.sh.current_session() else { return false };
        if self.sh.host_gestures.load(Ordering::Relaxed) < 1 || !self.sh.gestures_enabled.load(Ordering::Relaxed) {
            return false;
        }
        self.sh.note_input();
        if self.sh.active_is_window.load(Ordering::Relaxed) {
            s.send_json(kind::SELECT_SOURCE, &payloads::select(StreamSource::Desktop));
        }
        s.send_json(kind::GESTURE, &payloads::gesture(name, fingers));
        true
    }
}
