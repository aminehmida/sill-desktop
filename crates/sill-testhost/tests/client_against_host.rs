//! The real `sill-core` client against the synthetic host, in one process on loopback: pairing,
//! the saved Mac's reconnect, the catalog, video, input, settings, the clipboard, keyframe
//! recovery and the refusals. Ephemeral ports; nothing waits longer than it must.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sill_core::client::{usage, ClipboardBridge, MouseButton};
use sill_core::crypto::{mac_id, pairing_code};
use sill_core::net::{SavedMac, SavedMacs};
use sill_core::protocol::{gestures, key_mods, ParameterSets, ScrollPhase, SettingsChange, StreamSource};
use sill_core::{Client, ClientConfig, ClientEvent, ClientState, Phase, VideoSink};
use sill_testhost::{DeviceEvent, HostConfig, InputEvent, Source, TestHost};

const WAIT: Duration = Duration::from_secs(10);

#[derive(Default)]
struct Recorder {
    parameter_sets: Mutex<Vec<Vec<u8>>>,
    frames: AtomicU64,
    keyframes: AtomicU64,
    bad_frames: AtomicU64,
    first_is_keyframe: Mutex<Option<bool>>,
    resets: AtomicU64,
}

impl VideoSink for Recorder {
    fn parameter_sets(&self, payload: &[u8]) {
        self.parameter_sets.lock().unwrap().push(payload.to_vec());
    }
    fn frame(&self, data: &[u8], keyframe: bool, host_timestamp: f64) {
        self.first_is_keyframe.lock().unwrap().get_or_insert(keyframe);
        let nal_length = self.parameter_sets.lock().unwrap().last().and_then(|p| ParameterSets::parse(p)).map(|p| p.nal_length);
        let annex_b = nal_length.and_then(|n| ParameterSets::to_annex_b(data, n));
        if annex_b.map(|a| a.len() != data.len()).unwrap_or(true) || host_timestamp <= 0.0 {
            self.bad_frames.fetch_add(1, Ordering::SeqCst);
        }
        self.frames.fetch_add(1, Ordering::SeqCst);
        if keyframe {
            self.keyframes.fetch_add(1, Ordering::SeqCst);
        }
    }
    fn reset(&self) {
        self.resets.fetch_add(1, Ordering::SeqCst);
    }
}

#[derive(Default)]
struct Board {
    text: Mutex<Option<String>>,
    changes: AtomicU64,
}

impl ClipboardBridge for Board {
    fn read_text(&self) -> Option<String> {
        self.text.lock().unwrap().clone()
    }
    fn write_text(&self, text: &str) -> bool {
        *self.text.lock().unwrap() = Some(text.to_string());
        self.changes.fetch_add(1, Ordering::SeqCst);
        true
    }
    fn change_count(&self) -> i64 {
        self.changes.load(Ordering::SeqCst) as i64
    }
}

struct Device {
    client: Client,
    video: Arc<Recorder>,
    board: Arc<Board>,
    dir: PathBuf,
    wakes: Arc<AtomicU64>,
}

fn temp_dir(tag: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!("sill-it-{}-{}-{tag}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn device_in(dir: PathBuf) -> Device {
    let video = Arc::new(Recorder::default());
    let board = Arc::new(Board::default());
    let wakes = Arc::new(AtomicU64::new(0));
    let mut config = ClientConfig::new(dir.clone(), video.clone());
    config.device_name = "Test PC".into();
    config.device_model = "Rust".into();
    config.clipboard = Some(board.clone());
    config.allow_loopback = true;
    let w = wakes.clone();
    config.waker = Some(Arc::new(move || {
        w.fetch_add(1, Ordering::SeqCst);
    }));
    Device { client: Client::new(config).unwrap(), video, board, dir, wakes }
}

fn device(tag: &str) -> Device {
    device_in(temp_dir(tag))
}

impl Device {
    fn wait(&self, what: &str, ready: impl Fn(&ClientState) -> bool) -> Arc<ClientState> {
        match self.client.wait_for(WAIT, ready) {
            Ok(s) => s,
            Err(s) => panic!("timed out waiting for {what}: phase {:?}, status {:?}, pair error {:?}, active {:?}", s.phase, s.status, s.pair_error, s.active),
        }
    }

    /// Pairs by the host's code and waits for the session's first window list.
    fn pair(&self, host: &TestHost) -> Arc<ClientState> {
        self.client.pair_with_code(&host.address().to_string(), &host.pairing_code());
        self.wait("the paired session", |s| s.phase == Phase::Streaming && s.has_settings)
    }
}

fn host() -> TestHost {
    TestHost::start(HostConfig::default()).unwrap()
}

fn wait_event(host: &TestHost, what: &str, predicate: impl Fn(&DeviceEvent) -> bool) -> DeviceEvent {
    host.wait_for(predicate, WAIT).unwrap_or_else(|| panic!("the host never saw {what}"))
}

fn wait_input(host: &TestHost, what: &str, predicate: impl Fn(&InputEvent) -> bool) {
    wait_event(host, what, |e| matches!(e, DeviceEvent::Input { input, .. } if predicate(input)));
}

fn until(what: &str, ready: impl Fn() -> bool) {
    let deadline = std::time::Instant::now() + WAIT;
    while !ready() {
        assert!(std::time::Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// A well-formed code that is not the host's.
fn wrong_code(right: &str) -> String {
    let mut digits: Vec<u8> = right.bytes().take(11).map(|b| b - b'0').collect();
    digits[0] = (digits[0] + 1) % 10;
    digits.push(pairing_code::check_digit(&digits));
    digits.iter().map(|d| (b'0' + d) as char).collect()
}

#[test]
fn pairs_gets_the_catalog_and_streams_a_window() {
    let host = host();
    let d = device("stream");
    let state = d.pair(&host);

    // Pairing: saved, pinned to the host's key, known to the host.
    assert_eq!(1, state.saved_macs.len());
    let mac = &state.saved_macs[0];
    assert_eq!(host.mac_id(), mac.mac_id);
    assert_eq!(host.fingerprint().to_vec(), mac.fingerprint);
    assert!(mac.recognition_key.is_some() && !mac.revoked);
    assert_eq!(vec![host.address().to_string()], mac.typed_addresses);
    assert_eq!(1, host.paired_devices().len());
    let events = d.client.drain_events();
    assert!(events.contains(&ClientEvent::Paired { mac_id: host.mac_id() }));
    assert!(events.iter().any(|e| matches!(e, ClientEvent::Connected { mac_id, .. } if *mac_id == host.mac_id())));

    // The hello came first, and says what this device is.
    let DeviceEvent::Hello { hello, .. } = wait_event(&host, "the hello", |e| matches!(e, DeviceEvent::Hello { .. })) else { unreachable!() };
    assert_eq!(Some("Test PC (Rust)"), hello.device.as_deref());

    // The catalog.
    assert_eq!(host.name(), state.mac_name);
    assert_eq!(vec![101, 102, 103], state.windows.iter().map(|w| w.id).collect::<Vec<_>>());
    assert_eq!(Some("0.6.1"), state.host_version.as_deref());
    assert_eq!((1, 1, 1), (state.host_gestures, state.host_clipboard, state.host_key_repeat));
    assert_eq!(Some(true), state.session_paired());
    let state = d.wait("apps, icons, thumbnails, the cursor and the menus", |s| {
        !s.apps.is_empty() && !s.icons.is_empty() && s.thumbnails.len() == 3 && s.cursor.is_some() && s.has_menus
    });
    assert!(state.thumbnails[&101].starts_with(&[0xFF, 0xD8]), "a JPEG");
    assert!(state.icons.values().all(|png| png.starts_with(&[0x89, b'P', b'N', b'G'])));
    assert!(state.cursor.as_ref().unwrap().png.starts_with(&[0x89, b'P', b'N', b'G']));

    // Nothing streamed, so the client asked for the Desktop by itself; then the person picks a window.
    d.wait("the Desktop", |s| s.active == StreamSource::Desktop);
    d.client.select_source(StreamSource::Window(101));
    wait_event(&host, "the pick", |e| matches!(e, DeviceEvent::SelectSource { source: Source::Window(101), .. }));
    let state = d.wait("the window streaming", |s| s.active == StreamSource::Window(101) && s.video_size.is_some());
    assert_eq!(Some((1280, 720)), state.video_size);
    assert!(state.host_settings.as_ref().unwrap().stream.is_some());

    // Parameter sets, then at least 30 frames of length-prefixed NAL units, a keyframe first.
    until("30 frames", || d.video.frames.load(Ordering::SeqCst) >= 30);
    let sets = ParameterSets::parse(d.video.parameter_sets.lock().unwrap().last().unwrap()).unwrap();
    assert_eq!((4, 3), (sets.nal_length, sets.sets.len()));
    assert!(sets.vps().is_some() && sets.sps().is_some() && sets.pps().is_some());
    assert_eq!(Some((1280, 720)), sets.picture_size());
    assert_eq!(Some(true), *d.video.first_is_keyframe.lock().unwrap());
    assert_eq!(0, d.video.bad_frames.load(Ordering::SeqCst));
    assert!(d.video.keyframes.load(Ordering::SeqCst) >= 1 && host.frames_sent() >= 30);

    // The viewport, held to the host's bounds, with the rate this device wants.
    d.client.set_viewport(1280.0, 720.0);
    let DeviceEvent::Viewport { viewport, refused, .. } = wait_event(&host, "the viewport", |e| matches!(e, DeviceEvent::Viewport { .. })) else {
        unreachable!()
    };
    assert_eq!((1280.0, 720.0, None), (viewport.width, viewport.height, refused));

    // The link's statistics reach both ends once a second.
    wait_event(&host, "client stats", |e| matches!(e, DeviceEvent::ClientStats { stats, .. } if stats.fps > 0));
    let state = d.wait("stats", |s| s.stats.map(|x| x.fps > 0 && x.rtt_ms >= 0).unwrap_or(false));
    assert!(state.hold_awake);
    assert!(d.wakes.load(Ordering::SeqCst) > 0);

    d.client.disconnect();
    let state = d.wait("idle", |s| s.phase == Phase::Idle);
    assert!(state.windows.is_empty() && state.status.is_none());
    wait_event(&host, "the session's end", |e| matches!(e, DeviceEvent::SessionEnded { .. }));
    assert!(d.video.resets.load(Ordering::SeqCst) >= 1);
}

#[test]
fn input_reaches_the_host() {
    let host = host();
    let d = device("input");
    d.pair(&host);
    d.wait("something streaming", |s| s.active != StreamSource::None);

    // A physical key with its modifier, down and up.
    d.client.key(usage::LEFT_SHIFT, true, key_mods::SHIFT);
    d.client.key(0x04, true, key_mods::SHIFT);
    d.client.key(0x04, false, key_mods::SHIFT);
    d.client.key(usage::LEFT_SHIFT, false, 0);
    wait_input(&host, "A down", |i| *i == InputEvent::Key { hid_usage: 0x04, down: true, modifiers: key_mods::SHIFT });
    wait_input(&host, "shift up", |i| *i == InputEvent::Key { hid_usage: 0xE1, down: false, modifiers: 0 });

    // A shortcut: its modifier's own key around it.
    d.client.press(0x16, key_mods::COMMAND);
    wait_input(&host, "⌘ up", |i| *i == InputEvent::Key { hid_usage: 0xE3, down: false, modifiers: 0 });
    let keys: Vec<(u16, bool, u64)> = host
        .events()
        .into_iter()
        .filter_map(|e| match e {
            DeviceEvent::Input { input: InputEvent::Key { hid_usage, down, modifiers }, .. } => Some((hid_usage, down, modifiers)),
            _ => None,
        })
        .collect();
    assert_eq!(
        vec![(0xE3, true, key_mods::COMMAND), (0x16, true, key_mods::COMMAND), (0x16, false, key_mods::COMMAND), (0xE3, false, 0)],
        keys[keys.len() - 4..]
    );

    d.client.text("héllo\n");
    wait_input(&host, "text", |i| *i == InputEvent::Text { text: "héllo\n".into() });

    // The mouse: moves are coalesced, the newest always arrives, and a click flushes it first.
    for i in 0..50 {
        d.client.mouse_move(0.1 + f64::from(i) * 0.001, 0.2);
    }
    d.client.mouse_move(0.25, 0.75);
    d.client.mouse_button(MouseButton::Left, true, 0.25, 0.75);
    d.client.mouse_button(MouseButton::Left, false, 0.25, 0.75);
    d.client.mouse_button(MouseButton::Right, true, 0.5, 0.5);
    d.client.mouse_button(MouseButton::Right, false, 0.5, 0.5);
    wait_input(&host, "right up", |i| *i == InputEvent::Pointer { action: "rightUp".into(), x: 0.5, y: 0.5 });
    let pointers: Vec<(String, f64, f64)> = host
        .events()
        .into_iter()
        .filter_map(|e| match e {
            DeviceEvent::Input { input: InputEvent::Pointer { action, x, y }, .. } => Some((action, x, y)),
            _ => None,
        })
        .collect();
    let down = pointers.iter().position(|p| p.0 == "leftDown").unwrap();
    assert_eq!(("move".to_string(), 0.25, 0.75), pointers[down - 1], "the newest move goes before the click");
    assert!(down < 51, "moves were coalesced: {down} went out");
    assert_eq!(("leftUp".to_string(), 0.25, 0.75), pointers[down + 1]);

    // A relative mouse carries on from where the pointer is.
    assert_eq!((0.5, 0.5), d.client.pointer_anchor());
    let (x, y) = d.client.mouse_move_relative(0.125, -0.25);
    assert_eq!((0.625, 0.25), (x, y));
    wait_input(&host, "the relative move", |i| *i == InputEvent::Pointer { action: "move".into(), x: 0.625, y: 0.25 });

    // A scroll as a trackpad makes it, and a wheel's bare one.
    d.client.scroll_phase(ScrollPhase::Began, 0.5, 0.5);
    d.client.scroll(0.5, 0.5, 0.0, -0.05);
    d.client.scroll_phase(ScrollPhase::Ended, 0.5, 0.5);
    d.client.scroll_phase(ScrollPhase::MomentumBegan, 0.5, 0.5);
    d.client.scroll(0.5, 0.5, 0.0, -0.01);
    d.client.scroll_phase(ScrollPhase::MomentumEnded, 0.5, 0.5);
    wait_input(&host, "the scroll", |i| *i == InputEvent::Scroll { x: 0.5, y: 0.5, dx: 0.0, dy: -0.05 });
    wait_input(&host, "momentum's end", |i| *i == InputEvent::ScrollGesture { phase: "momentumEnded".into(), x: 0.5, y: 0.5 });

    // A trackpad gesture, a window command, an app.
    assert!(d.client.gesture(gestures::SWIPE_UP, 3));
    wait_event(&host, "the gesture", |e| matches!(e, DeviceEvent::Gesture { gesture, fingers: Some(3), .. } if gesture == "swipeUp"));
    d.client.update_prefs(|p| p.trackpad_gestures = false);
    d.wait("the preference", |s| !s.prefs.trackpad_gestures);
    assert!(!d.client.gesture(gestures::PINCH, 4), "off on this device: nothing is sent");
    d.client.window_command(103, "minimize");
    wait_event(&host, "the window command", |e| matches!(e, DeviceEvent::WindowCommand { id: 103, action, .. } if action == "minimize"));
    d.wait("the window gone from the list", |s| !s.windows.iter().any(|w| w.id == 103));
    let app = d.wait("apps", |s| !s.apps.is_empty()).apps[0].clone();
    d.client.launch_app(&app.bundle_id);
    wait_event(&host, "the launch", |e| matches!(e, DeviceEvent::LaunchApp { bundle_id, .. } if *bundle_id == app.bundle_id));
    assert!(!host.events().iter().any(|e| matches!(e, DeviceEvent::Undecodable { .. } | DeviceEvent::Unknown { .. })));
}

#[test]
fn settings_change_is_acknowledged_and_a_refusal_goes_back() {
    let host = host();
    let d = device("settings");
    let state = d.pair(&host);
    let before = state.shown_settings.clone().unwrap();
    let wanted = if before.bitrate == 8_000_000 { 25_000_000 } else { 8_000_000 };

    d.client.change_settings(SettingsChange { bitrate: Some(wanted), max_fps: Some(before.max_fps), ..Default::default() });
    let DeviceEvent::ChangeSettings { change, refused, .. } = wait_event(&host, "kind 17", |e| matches!(e, DeviceEvent::ChangeSettings { .. })) else {
        unreachable!()
    };
    assert!(change.token.is_some() && refused.is_empty());
    assert_eq!((Some(wanted), None), (change.bitrate, change.max_fps), "only what changes is sent");
    let state = d.wait("the answer", |s| {
        s.pending_settings.is_empty() && s.host_settings.as_ref().map(|h| h.settings.bitrate == wanted).unwrap_or(false)
    });
    assert_eq!(wanted, state.shown_settings.as_ref().unwrap().bitrate);

    // A value the Mac does not take: shown at once, then put back with a notice.
    host.clear_events();
    d.client.drain_events();
    d.client.change_settings(SettingsChange { bitrate: Some(1_234_567), ..Default::default() });
    wait_event(&host, "the refused kind 17", |e| matches!(e, DeviceEvent::ChangeSettings { refused, .. } if !refused.is_empty()));
    let state = d.wait("the refusal", |s| s.pending_settings.is_empty() && s.notice.is_some());
    assert_eq!(wanted, state.shown_settings.as_ref().unwrap().bitrate);
    assert!(d.client.drain_events().iter().any(|e| matches!(e, ClientEvent::SettingsRefused { .. })));
}

#[test]
fn menus_open_and_press() {
    let host = host();
    let d = device("menus");
    d.pair(&host);
    d.wait("something streaming", |s| s.active != StreamSource::None);
    let state = d.wait("the menu bar", |s| s.has_menus && s.menu.top.iter().any(|m| m.submenu));
    let top = state.menu.top.iter().find(|m| m.submenu && m.id.is_some()).unwrap().clone();
    d.client.open_menu(&top);
    let id = top.id.clone().unwrap();
    let state = d.wait("the menu's items", |s| matches!(s.menu.menus.get(&id), Some(sill_core::client::Menu::Items { .. })));
    let Some(sill_core::client::Menu::Items { items, .. }) = state.menu.menus.get(&id) else { unreachable!() };
    let item = items.iter().find(|i| i.enabled && !i.separator && !i.submenu && i.id.is_some()).expect("an item to choose").clone();
    d.client.drain_events();
    d.client.choose_menu_item(&item);
    wait_event(&host, "the press", |e| matches!(e, DeviceEvent::PressMenuItem { id, pressed: true, .. } if *id == item.id));
    until("the press's answer", || d.client.drain_events().iter().any(|e| matches!(e, ClientEvent::MenuPressed { ok: true, .. })));
}

#[test]
fn clipboard_goes_both_ways() {
    let host = host();
    let d = device("clipboard");
    let state = d.pair(&host);
    assert!(state.clipboard_shared);
    d.wait("something streaming", |s| s.active != StreamSource::None);
    wait_event(&host, "the subscription", |e| matches!(e, DeviceEvent::Clipboard { text: None, stop: false, .. }));

    // Copied on the Mac: lands on this computer's clipboard.
    host.set_clipboard_text("copied on the Mac");
    until("the Mac's text", || d.board.read_text().as_deref() == Some("copied on the Mac"));
    until("its event", || d.client.drain_events().contains(&ClientEvent::ClipboardFromMac { text: "copied on the Mac".into() }));

    // Paste on the Mac: this computer's text, its receipt, then ⌘V there.
    *d.board.text.lock().unwrap() = Some("copied here".into());
    host.clear_events();
    d.client.paste_clipboard();
    wait_event(&host, "the text", |e| matches!(e, DeviceEvent::Clipboard { text: Some(t), .. } if t == "copied here"));
    until("the Mac's clipboard", || host.clipboard_text().as_deref() == Some("copied here"));
    wait_event(&host, "the receipt's ping", |e| matches!(e, DeviceEvent::Ping { payload, .. } if payload.len() == 16));
    wait_input(&host, "⌘V", |i| *i == InputEvent::Key { hid_usage: usage::V, down: true, modifiers: key_mods::COMMAND });
    let state = d.wait("the note", |s| s.clipboard_note.is_some());
    assert_eq!("Pasted on Test Mac.", state.clipboard_note.as_ref().unwrap().text);
    assert_eq!(Some("copied here".to_string()), d.board.read_text(), "the Mac never echoes it back");

    // The switch off: the subscription stops, and nothing more is written here.
    d.client.update_prefs(|p| p.share_clipboard = false);
    wait_event(&host, "the stop", |e| matches!(e, DeviceEvent::Clipboard { stop: true, .. }));
    host.set_clipboard_text("not for this device");
    host.clear_events();
    d.client.paste_text("nor this for the Mac");
    wait_event(&host, "a ping after it", |e| matches!(e, DeviceEvent::Ping { .. }));
    assert_eq!(Some("not for this device".to_string()), host.clipboard_text());
    assert_eq!(Some("copied here".to_string()), d.board.read_text());
}

#[test]
fn an_interrupted_picture_asks_for_a_keyframe() {
    let host = host();
    let d = device("keyframe");
    d.pair(&host);
    d.wait("something streaming", |s| s.active != StreamSource::None);
    until("the first frames", || d.video.frames.load(Ordering::SeqCst) >= 5);
    let (sets_before, host_before) = (d.video.parameter_sets.lock().unwrap().len(), host.keyframes_sent());

    d.client.video_feedback().picture_interrupted();
    wait_event(&host, "kind 44", |e| matches!(e, DeviceEvent::KeyframeRequest { honoured: true, .. }));
    until("the keyframe", || host.keyframes_sent() > host_before && d.video.parameter_sets.lock().unwrap().len() > sets_before);
    until("the request's event", || d.client.drain_events().contains(&ClientEvent::KeyframeRequested));
    d.client.video_feedback().keyframe_shown();

    // Asked once: the keyframe that came is the answer.
    assert_eq!(1, host.events().iter().filter(|e| matches!(e, DeviceEvent::KeyframeRequest { .. })).count());
}

#[test]
fn a_saved_mac_is_reconnected_to_after_a_restart() {
    let host = host();
    let d = device("saved");
    d.pair(&host);
    let fingerprint = d.client.device_fingerprint();
    d.client.disconnect();
    d.wait("idle", |s| s.phase == Phase::Idle);
    let dir = d.dir.clone();
    drop(d);

    // A new launch: the same key, the saved Mac, no code.
    let d = device_in(dir);
    assert_eq!(fingerprint, d.client.device_fingerprint());
    let saved = d.client.state().saved_macs.clone();
    assert_eq!(1, saved.len());
    assert!(saved[0].last_connected_at.is_some());
    assert_eq!(Some(format!("127.0.0.1|{}", host.port())), saved[0].last_worked);
    host.clear_events();
    d.client.connect(&saved[0].mac_id);
    let state = d.wait("the session", |s| s.phase == Phase::Streaming && !s.windows.is_empty());
    assert_eq!(Some(sill_core::client::Route::Loopback), state.route);
    wait_event(&host, "a paired session", |e| matches!(e, DeviceEvent::SessionStarted { paired: true, .. }));
    assert!(!host.events().iter().any(|e| matches!(e, DeviceEvent::PairRequest { .. })));
    assert_eq!(1, host.paired_devices().len());
}

#[test]
fn the_mac_says_goodbye_and_locks() {
    let host = host();
    let d = device("goodbye");
    d.pair(&host);
    d.wait("something streaming", |s| s.active != StreamSource::None);

    // Another user on the Mac's screen: covered; back: "Unlocking…" until a keyframe.
    host.set_locked(true, true);
    let state = d.wait("the cover", |s| s.lock_shade.is_some());
    assert_eq!(Some(sill_core::protocol::LockWhy::OtherUser), state.lock_why);
    assert!(state.windows.is_empty() && !state.clipboard_shared);
    host.set_locked(false, false);
    d.wait("the windows back", |s| s.lock_why.is_none() && !s.windows.is_empty());
    d.client.select_source(StreamSource::Desktop);
    d.wait("the cover gone", |s| s.lock_shade.is_none() && s.clipboard_shared);

    // A goodbye that asks for no reconnect ends the session with the Mac's own words.
    d.client.drain_events();
    host.send_goodbye("maintenance", Some("Back in a minute."));
    let state = d.wait("the end", |s| s.phase == Phase::Idle);
    assert_eq!(Some("Back in a minute."), state.status.as_deref());
    assert_eq!(state.status, state.last_error);
    let events = d.client.drain_events();
    assert!(events.contains(&ClientEvent::Goodbye { reason: "maintenance".into(), message: Some("Back in a minute.".into()) }));
    assert!(events.contains(&ClientEvent::SessionEnded { message: Some("Back in a minute.".into()), reconnecting: false }));
}

#[test]
fn a_wrong_code_is_refused_and_nothing_is_saved() {
    let host = host();
    let d = device("wrongcode");
    let address = host.address().to_string();

    // Typos never leave the device.
    d.client.pair_with_code(&address, "1234");
    assert_eq!(Some("A code has 12 digits."), d.wait("the length", |s| s.pair_error.is_some()).pair_error.as_deref());
    let mut typo = host.pairing_code();
    let last = typo.pop().unwrap();
    typo.push(if last == '0' { '1' } else { '0' });
    d.client.pair_with_code(&address, &typo);
    d.wait("the typo", |s| s.pair_error.as_deref() == Some("That code has a typo. Check it against your Mac."));
    assert!(host.events().is_empty());

    // A well-formed code that is not the Mac's: the Mac counts it.
    d.client.pair_with_code(&address, &wrong_code(&host.pairing_code()));
    let state = d.wait("the refusal", |s| !s.pairing && s.pair_error.as_deref().map(|e| e.starts_with("That code didn’t match.")).unwrap_or(false));
    assert_eq!(Some("That code didn’t match. 4 tries left."), state.pair_error.as_deref());
    assert!(state.saved_macs.is_empty() && state.phase == Phase::Idle);
    assert!(host.paired_devices().is_empty());
    wait_event(&host, "the wrong proof", |e| matches!(e, DeviceEvent::PairRequest { answer, tries_left: Some(4), .. } if answer == "code"));

    // The right one still pairs.
    d.pair(&host);
}

#[test]
fn an_unpaired_device_is_refused() {
    let host = host();

    // A device the Mac removed: told so, revoked, and refused inside TLS from then on.
    let d = device("removed");
    d.pair(&host);
    host.forget_devices();
    let state = d.wait("the goodbye", |s| s.phase == Phase::Idle && s.saved_macs.iter().all(|m| m.revoked));
    assert_eq!(Some("Test Mac removed this device. Pair it again to connect."), state.status.as_deref());
    host.clear_events();
    d.client.connect(&host.mac_id());
    wait_event(&host, "the handshake's refusal", |e| matches!(e, DeviceEvent::HandshakeRefused { .. }));
    let state = d.wait("the refusal", |s| s.phase == Phase::Idle && s.last_error.is_some());
    assert_eq!(Some("Test Mac removed this device. Pair it again to connect."), state.status.as_deref());
    assert!(state.windows.is_empty());
    assert!(!host.events().iter().any(|e| matches!(e, DeviceEvent::SessionStarted { .. })));

    // A device that never paired but knows the Mac's key and address: the same, and no reconnect loop.
    let dir = temp_dir("stranger");
    SavedMacs::open(&dir).upsert(saved_record(&host, &host.fingerprint()));
    let stranger = device_in(dir);
    host.clear_events();
    stranger.client.connect(&host.mac_id());
    let state = stranger.wait("the stranger's refusal", |s| s.phase == Phase::Idle && s.last_error.is_some());
    assert!(state.saved_macs[0].revoked);
    assert!(state.status.as_deref().unwrap().contains("removed this device"));
    assert_eq!(0, stranger.video.frames.load(Ordering::SeqCst));
    assert!(!host.events().iter().any(|e| matches!(e, DeviceEvent::SessionStarted { .. })));
}

fn saved_record(at: &TestHost, fingerprint: &[u8; 32]) -> SavedMac {
    SavedMac {
        mac_id: mac_id::make(fingerprint),
        fingerprint: fingerprint.to_vec(),
        name: "Pinned Mac".into(),
        recognition_key: None,
        remote_port: at.port(),
        addresses: Vec::new(),
        typed_addresses: vec![at.address().to_string()],
        info_issued_at: 0.0,
        last_worked: None,
        paired_at: 1.0,
        last_connected_at: None,
        revoked: false,
    }
}

#[test]
fn another_key_at_a_saved_macs_address_is_refused() {
    // The device paired with one Mac; another answers at its address.
    let (paired_with, impostor) = (host(), host());
    assert_ne!(paired_with.fingerprint(), impostor.fingerprint());
    let d = device("pin");
    d.pair(&paired_with);
    d.client.disconnect();
    d.wait("idle", |s| s.phase == Phase::Idle);
    let dir = d.dir.clone();
    drop(d);
    let store = SavedMacs::open(&dir);
    store.update(&paired_with.mac_id(), |m| {
        m.typed_addresses = vec![impostor.address().to_string()];
        m.last_worked = None;
    });
    drop(store);

    let d = device_in(dir);
    d.client.connect(&paired_with.mac_id());
    let state = d.wait("the mismatch", |s| s.phase == Phase::Idle && s.last_error.is_some());
    assert_eq!(Some("The Mac at 127.0.0.1 isn’t the one this device paired with."), state.status.as_deref());
    assert!(!state.saved_macs[0].revoked, "the saved Mac itself did nothing");
    // The handshake ended before this device's certificate went: the impostor learned nothing and served nothing.
    assert!(!impostor.events().iter().any(|e| matches!(e, DeviceEvent::SessionStarted { .. } | DeviceEvent::Hello { .. })));
    assert_eq!(0, d.video.frames.load(Ordering::SeqCst));

    // Pairing by a link pins too: the impostor never sees a pairing request for the other Mac's link.
    let link = paired_with.pair_link().expect("a pairing link");
    let fresh = device("pin-link");
    fresh.client.pair_with_link(&link, Some(&impostor.address().to_string()));
    fresh.wait("the link's pairing", |s| !s.pairing && (s.pair_error.is_some() || s.phase == Phase::Streaming));
    assert!(!impostor.events().iter().any(|e| matches!(e, DeviceEvent::PairRequest { .. })));
}

/// Needs multicast on a real interface, so not part of the default run:
/// `cargo test -p sill-testhost -- --ignored discovery`.
#[test]
#[ignore = "needs multicast DNS on the local network"]
fn discovery_finds_the_home_door_and_pairs_through_an_ask() {
    let name = format!("Sill Test {}", std::process::id());
    let host = TestHost::start(HostConfig { mdns: true, name: name.clone(), bind: "0.0.0.0".parse().unwrap(), ..Default::default() }).unwrap();
    let d = device("mdns");
    d.client.start_discovery();
    let state = d.wait("the home door", |s| s.unpaired_home_macs.iter().any(|h| h.service_name == name));
    let found = state.home_macs.iter().find(|h| h.service_name == name).unwrap();
    assert_eq!((host.port(), sill_core::net::HomeDoor::PairingRequired), (found.port, found.door));
    assert!(sill_core::crypto::recognition_tag::matches(found.tag.as_deref().unwrap(), &host.recognition_key()));

    // A click asks the Mac to show its code; the code then pairs, pinned to the key the ask saw.
    d.client.connect_home(&name);
    let state = d.wait("the ask's answer", |s| s.home_ask.is_some() || s.last_error.is_some());
    assert!(state.home_ask.is_some(), "{:?}", state.last_error);
    d.client.pair_home_code(&host.pairing_code());
    let state = d.wait("the session", |s| s.phase == Phase::Streaming);
    assert!(state.unpaired_home_macs.iter().all(|h| h.service_name != name), "its row is the saved Mac's now");
    assert_eq!(Some(sill_core::client::Route::Local), state.route);
}
