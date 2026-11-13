//! A synthetic Sill host for tests and CI.
//!
//! Sill streams a Mac's windows to other devices; this crate is the Mac's side with
//! nothing of a Mac in it: a TLS door that pairs devices and admits their sessions, a
//! catalog of fake windows, a looping HEVC clip, a tone, menus, a clipboard, and a log
//! of everything a device sent. It was written by reading the Swift host (Sill for Mac
//! 0.6.1: `Sources/StreamProtocol` and `Sources/SillHost`), independently of this
//! workspace's client, so that the two disagree where either misread the protocol.
//!
//! # What is as the real host does it (and where it was read)
//!
//! - **Framing** (`StreamMessage.swift`): 14-byte header, kind u8, time stamp f64 BE,
//!   keyframe u8, payload length u32 BE; a device message over 1 MiB closes the session.
//! - **TLS** (`RemoteTLS.swift`, `Door.swift`, `DoorPolicy.swift`): TLS 1.3 only, a
//!   self-signed P-256 certificate, a client certificate required, ALPN `sill/1`
//!   (session) and `sill-pair/1` (pairing) both offered, no tickets. Trust is the
//!   SHA-256 of the peer key's SubjectPublicKeyInfo. An unpaired key on `sill/1` is
//!   refused inside the handshake (`bad_certificate`): a TLS 1.3 client finishes its own
//!   handshake and then fails its first read. No goodbye is sent to it.
//! - **Pairing** (`Pairing.swift`, `PairingWindow.swift`, `RemoteAccess.judge`): one kind
//!   19, one kind 20, then closed. The 12-digit Damm code, PBKDF2-HMAC-SHA256 with
//!   600,000 rounds, both proofs, five wrong proofs then "stopped", "ask" and "cancel"
//!   at the home door.
//! - **Sessions** (`StreamServer.swift`, `StreamCoordinator.swift`): the catalog on
//!   connect in the order 2, 16, 18, icons (4), apps (5), cursor shape (14), thumbnails
//!   (3); nothing is sent in answer to a hello; video only after a kind 6; parameter sets
//!   (kind 0) before every keyframe; pings echoed; settings answered with `answering`.
//!
//! # What is this crate's own
//!
//! The fake windows, apps and menus; the clip and the tone standing in for a capture; the
//! state directory's layout; and the shortcuts listed on [`HostConfig`]'s fields. The
//! per-source connection limits and backoff of the real door are left out, as are the
//! origin rules (VPN, internet), the USB cable's pairing, the off-console view, the
//! away quality's switching and remote pacing.
//!
//! # Use
//!
//! ```no_run
//! use sill_testhost::{HostConfig, TestHost, DeviceEvent};
//! use std::time::Duration;
//!
//! let host = TestHost::start(HostConfig::default()).unwrap();
//! println!("pair with {} at {}", host.pairing_code(), host.address());
//! // … drive a client at it …
//! let hello = host.wait_for(|e| matches!(e, DeviceEvent::Hello { .. }), Duration::from_secs(5));
//! assert!(hello.is_some());
//! host.stop();
//! ```

mod events;
mod host;
mod identity;
mod media;
mod pairing;
mod server;
mod store;
mod tls;
pub mod wire;

#[cfg(test)]
mod selftest;

pub use events::{
    ClientStats, DeviceEvent, Goodbye, Hello, InputEvent, PairedDevice, SettingsChange, Source, Viewport,
};

use host::{Inner, Reading};
use std::collections::HashMap;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Which of the real host's two doors this host is (`DoorPolicy.Door`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Door {
    /// The home door (Bonjour `_sill._tcp`): `sill-pair/1` is always admitted and takes
    /// "ask" and "cancel"; `sill/1` is for paired keys, or any key with
    /// [`HostConfig::require_pairing`] off.
    #[default]
    Home,
    /// The remote door (port 7455 on a real Mac): `sill-pair/1` is admitted only while a
    /// pairing window is open (else the handshake is refused), "ask" and "cancel" are
    /// answered "closed"; `sill/1` is for paired keys only, and with
    /// [`HostConfig::remote_access`] off a paired key gets goodbye "remoteOff".
    Remote,
}

/// How to start a [`TestHost`]. `HostConfig::default()` is a home door on an ephemeral
/// loopback port with a fresh identity and an open pairing window.
#[derive(Debug, Clone)]
pub struct HostConfig {
    /// The address to listen on. Default 127.0.0.1.
    pub bind: IpAddr,
    /// The port; 0 picks an ephemeral one.
    pub port: u16,
    /// Where the identity and the paired devices are kept (`identity.json`,
    /// `paired.json`); `None` keeps both in memory. The layout is this crate's own.
    pub dir: Option<PathBuf>,
    /// A fixed pairing code: 12 digits with a valid Damm check digit (spaces and hyphens
    /// allowed); `start` refuses any other. `None`: a random code per window, as the
    /// real host makes them.
    pub code: Option<String>,
    /// The Mac's name: the window list's `macName`, kind 18's and kind 20's `name`, the
    /// Bonjour instance name.
    pub name: String,
    /// Advertise the home door over mDNS: `_sill._tcp`, TXT `r` (a fresh recognition
    /// tag) and `p` ("1": pairing required, "0": any device), as `HostIdentity.txtRecord`.
    pub mdns: bool,
    /// Which door this is. Default [`Door::Home`].
    pub door: Door,
    /// The home door's Require pairing (default on). Off, any P-256 key gets a session.
    pub require_pairing: bool,
    /// The remote door's Remote Access switch (default on; only read with [`Door::Remote`]).
    pub remote_access: bool,
    /// Whether a pairing window is open from the start (default yes).
    pub pairing_open: bool,
    /// After a pairing, an expiry or a withdrawal a fresh window opens by itself, as the
    /// real command-line host does (default yes). With [`HostConfig::code`] it has the same code.
    pub reopen_pairing: bool,
    /// The real window's spacing: after the 1st to 4th wrong proof the next is taken 1, 2,
    /// 4, 8 s later, and one source may try once per 5 s; sooner is answered "busy" with
    /// `retryAfter`. Default **off** (a shortcut, so wrong-code tests need not wait).
    pub pairing_spacing: bool,
    /// A window's lifetime (default 300 s, `PairingWindow.lifetime`).
    pub pairing_lifetime: Duration,
    /// An "ask" at the home door with no window open opens one and is answered "shown"
    /// (default yes). The real host answers an ask from the Mac's own addresses
    /// "openOnMac" instead (set this false for that); from another device it opens one.
    pub ask_opens_window: bool,
    /// The device gate's floor (`DeviceGate.minimumDeviceVersion`). `None` is the real
    /// host's "0": every device is served and nothing waits for a hello. With a version,
    /// a session's first message must be a hello of at least that version within 2 s;
    /// any other device gets goodbye "update" with `reconnect: false`.
    pub minimum_device_version: Option<String>,
    /// Send Audio (default off, as the real command-line host; a device turns it on with kind 17).
    pub send_audio: bool,
    /// Share clipboard with devices (default on): the window list says `clipboard: 1`.
    pub share_clipboard: bool,
}

impl Default for HostConfig {
    fn default() -> Self {
        HostConfig {
            bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
            port: 0,
            dir: None,
            code: None,
            name: "Test Mac".into(),
            mdns: false,
            door: Door::Home,
            require_pairing: true,
            remote_access: true,
            pairing_open: true,
            reopen_pairing: true,
            pairing_spacing: false,
            pairing_lifetime: Duration::from_secs(300),
            ask_opens_window: true,
            minimum_device_version: None,
            send_audio: false,
            share_clipboard: true,
        }
    }
}

struct Mdns {
    daemon: mdns_sd::ServiceDaemon,
    fullname: String,
}

/// A running synthetic host. It owns its tokio runtime (two worker threads): every
/// method is synchronous and may be called from any thread that is not one of the
/// runtime's own, another runtime's included (they block for moments only, but for
/// [`TestHost::wait_for`], [`TestHost::next_events`] and [`TestHost::stop`], which wait).
/// Dropping it stops it.
pub struct TestHost {
    inner: Arc<Inner>,
    runtime: Mutex<Option<tokio::runtime::Runtime>>,
    mdns: Mutex<Option<Mdns>>,
}

impl TestHost {
    /// Starts a host: loads or makes its identity, binds the port, opens the pairing
    /// window and derives its code's key (about 0.2 s of PBKDF2), then accepts connections.
    /// A second `start` is a second Mac with another identity (unless both share a `dir`).
    pub fn start(mut config: HostConfig) -> io::Result<TestHost> {
        if let Some(code) = &config.code {
            let checked = pairing::check_code(code).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "the pairing code must be 12 digits with a valid Damm check digit")
            })?;
            config.code = Some(checked);
        }
        if config.minimum_device_version.as_deref().is_some_and(|v| host::parse_version(v).is_none()) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "the minimum device version does not parse"));
        }
        let stored = store::load_or_create(config.dir.as_deref())?;
        let listener = std::net::TcpListener::bind(SocketAddr::new(config.bind, config.port))?;
        listener.set_nonblocking(true)?;
        let addr = listener.local_addr()?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("sill-testhost")
            .enable_all()
            .build()?;
        let (shutdown, _) = tokio::sync::watch::channel(false);
        let inner = Arc::new(Inner::new(
            config.clone(),
            stored.identity,
            stored.recognition_key,
            stored.paired,
            addr,
            shutdown,
            runtime.handle().clone(),
        ));
        if config.pairing_open {
            let mut st = inner.lock();
            inner.open_window(&mut st, None, None);
        }
        let listener = {
            let _guard = runtime.enter();
            tokio::net::TcpListener::from_std(listener)?
        };
        runtime.spawn(server::accept_loop(inner.clone(), listener));
        runtime.spawn(server::pacer(inner.clone()));
        let host = TestHost { inner, runtime: Mutex::new(Some(runtime)), mdns: Mutex::new(None) };
        if config.mdns {
            *host.mdns.lock().unwrap() = Some(host.advertise()?);
        }
        host.pairing_code(); // returns once the code's key is ready
        Ok(host)
    }

    /// Registers the home door as the real host does (`StreamServer`, `HostIdentity.txtRecord`).
    fn advertise(&self) -> io::Result<Mdns> {
        let other = |e: mdns_sd::Error| io::Error::other(e.to_string());
        let daemon = mdns_sd::ServiceDaemon::new().map_err(other)?;
        let mut txt = HashMap::new();
        txt.insert("r".to_string(), pairing::recognition_tag(&self.inner.recognition_key, None));
        txt.insert("p".to_string(), if self.inner.cfg.require_pairing { "1" } else { "0" }.to_string());
        // A host name of its own, so it never answers for the machine's real one.
        let host_name = format!("sill-testhost-{}.local.", self.mac_id().to_lowercase());
        let ip = self.inner.addr.ip();
        let info = if ip.is_unspecified() {
            mdns_sd::ServiceInfo::new("_sill._tcp.local.", &self.inner.cfg.name, &host_name, (), self.port(), txt)
                .map_err(other)?
                .enable_addr_auto()
        } else {
            mdns_sd::ServiceInfo::new("_sill._tcp.local.", &self.inner.cfg.name, &host_name, ip, self.port(), txt)
                .map_err(other)?
        };
        let fullname = info.get_fullname().to_string();
        daemon.register(info).map_err(other)?;
        Ok(Mdns { daemon, fullname })
    }

    /// The port the door listens on.
    pub fn port(&self) -> u16 {
        self.inner.addr.port()
    }

    /// Where to dial: the bound address and port (127.0.0.1 when bound to every address).
    pub fn address(&self) -> SocketAddr {
        let mut a = self.inner.addr;
        if a.ip().is_unspecified() {
            a.set_ip(IpAddr::V4(Ipv4Addr::LOCALHOST));
        }
        a
    }

    /// The Mac's name.
    pub fn name(&self) -> &str {
        &self.inner.cfg.name
    }

    /// The open pairing window's 12 digits, once its key is derived (so a proof made with
    /// it is never answered "busy" for that); empty when no window is open.
    pub fn pairing_code(&self) -> String {
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut st = self.inner.lock();
        loop {
            match st.window.current() {
                None => return String::new(),
                Some(w) if w.code_key.is_some() => return w.code.clone(),
                Some(w) => {
                    let Some(left) = deadline.checked_duration_since(Instant::now()) else { return w.code.clone() };
                    st = self.inner.changed.wait_timeout(st, left).unwrap_or_else(|e| e.into_inner()).0;
                }
            }
        }
    }

    /// The open window's QR secret (16 bytes: the key of method "qr"); `None` when closed.
    pub fn pairing_secret(&self) -> Option<[u8; 16]> {
        self.inner.lock().window.current().map(|w| w.secret)
    }

    /// The open window's `sill://pair?v=1&m=…&k=…&s=…&n=…&p=…&a=…` link (`PairLink.url`),
    /// with this host's address; `None` when closed.
    pub fn pair_link(&self) -> Option<String> {
        let secret = self.pairing_secret()?;
        let escape = |s: &str| -> String {
            s.bytes()
                .map(|b| {
                    if b.is_ascii_alphanumeric() || b"-._~:".contains(&b) {
                        char::from(b).to_string()
                    } else {
                        format!("%{b:02X}")
                    }
                })
                .collect()
        };
        let a = self.address();
        Some(format!(
            "sill://pair?v=1&m={}&k={}&s={}&n={}&p={}&a={}",
            self.mac_id(),
            pairing::b64url(&self.fingerprint()),
            pairing::b64url(&secret),
            escape(self.name()),
            a.port(),
            escape(&a.ip().to_string()),
        ))
    }

    /// The host's pin: SHA-256 of its key's SubjectPublicKeyInfo.
    pub fn fingerprint(&self) -> [u8; 32] {
        self.inner.identity.fingerprint
    }

    /// The Mac ID: Crockford base32 of the fingerprint's first 10 bytes (16 characters).
    pub fn mac_id(&self) -> String {
        pairing::mac_id(&self.inner.identity.fingerprint)
    }

    /// The 32 bytes kind 20 hands a paired device, which resolve the Bonjour TXT tag `r`.
    pub fn recognition_key(&self) -> [u8; 32] {
        self.inner.recognition_key
    }

    /// The host's certificate as it presents it in TLS (DER).
    pub fn certificate_der(&self) -> Vec<u8> {
        self.inner.identity.cert_der.clone()
    }

    /// Everything devices did so far, oldest first.
    pub fn events(&self) -> Vec<DeviceEvent> {
        self.inner.log.snapshot()
    }

    /// The first event in the log that `predicate` accepts, waiting up to `timeout` for
    /// one to arrive. Blocks on a condition variable, never spins.
    pub fn wait_for(&self, predicate: impl Fn(&DeviceEvent) -> bool, timeout: Duration) -> Option<DeviceEvent> {
        self.inner.log.wait_for(predicate, timeout)
    }

    /// The events after the first `seen` (counted since the host started; `clear_events`
    /// does not reset the count), waiting up to `timeout` for at least one, with the new
    /// count: a tail of the log, for a printer.
    pub fn next_events(&self, seen: u64, timeout: Duration) -> (Vec<DeviceEvent>, u64) {
        self.inner.log.wait_since(seen, timeout)
    }

    /// Empties the log.
    pub fn clear_events(&self) {
        self.inner.log.clear();
    }

    /// The trust list.
    pub fn paired_devices(&self) -> Vec<PairedDevice> {
        self.inner.lock().paired.clone()
    }

    /// Removes every paired device, as Remove on the Mac: each session of a removed key
    /// gets goodbye "removed" and is closed, and its next `sill/1` dial fails its handshake.
    pub fn forget_devices(&self) {
        self.inner.forget_devices();
    }

    /// Opens a pairing window (when none is open) or closes the open one, as the Mac's
    /// user would. A closed window answers every proof "closed".
    pub fn set_pairing_open(&self, open: bool) {
        let mut st = self.inner.lock();
        if open && !st.window.is_open() {
            self.inner.open_window(&mut st, None, None);
        } else if !open {
            st.window.close(pairing::CloseReason::Cancelled);
        }
    }

    /// Replaces the window with a fresh one and returns its code: a random one, also
    /// when [`HostConfig::code`] fixed the first.
    pub fn new_pairing_code(&self) -> String {
        {
            let mut st = self.inner.lock();
            self.inner.open_window(&mut st, None, Some(pairing::generate_code()));
        }
        self.pairing_code()
    }

    /// The lock's reading (`LockPolicy`): unlocked; this session's screen locked
    /// (`locked`); or the console another user's (`locked` and `off_console`). Locked,
    /// the window list has no windows and no `clipboard`, and says `locked` (and
    /// `offConsole`); thumbnails, icons, apps, sound, clipboard, pointer and cursor shape
    /// stop; a streamed window gives way to the Desktop and comes back at the unlock. A
    /// guess: off console the real host streams a black still, this one keeps the clip.
    pub fn set_locked(&self, locked: bool, off_console: bool) {
        self.inner.set_reading(match (locked, off_console) {
            (false, _) => Reading::Unlocked,
            (true, false) => Reading::ScreenLocked,
            (true, true) => Reading::OffConsole,
        });
    }

    /// As if `text` was copied on the Mac: it becomes the host's clipboard and goes (kind
    /// 32) to every session that subscribed, unless the Mac is locked.
    pub fn set_clipboard_text(&self, text: &str) {
        self.inner.set_clipboard(text);
    }

    /// The host's clipboard: what was last copied "on the Mac" or sent by a device.
    pub fn clipboard_text(&self) -> Option<String> {
        self.inner.lock().clipboard_text.clone()
    }

    /// Kind 22 `{"reason":…,"message":…}` to every session, then their close.
    pub fn send_goodbye(&self, reason: &str, message: Option<&str>) {
        self.send_goodbye_with(&Goodbye { reason: reason.into(), message: message.map(str::to_string), ..Goodbye::default() });
    }

    /// The same with every field of the goodbye (`minimumVersion`, `reconnect`).
    pub fn send_goodbye_with(&self, goodbye: &Goodbye) {
        let mut st = self.inner.lock();
        self.inner.goodbye_all(&mut st, goodbye);
    }

    /// The Mac's cursor changed shape: the next of two shapes (an arrow, an I-beam) goes
    /// to every session as kind 14.
    pub fn push_cursor_shape(&self) {
        self.inner.push_cursor_shape();
    }

    /// The Mac's own pointer moved (kind 26): over the stream at a fraction of the frame,
    /// or off it (`None`). Sent to every session, `seen` being that session's count of
    /// kind 8 messages read. (The real host leaves out the device that moves the pointer.)
    pub fn push_mac_pointer(&self, position: Option<(f64, f64)>) {
        self.inner.push_mac_pointer(position);
    }

    /// Every window's thumbnail again (kind 3), as the real host's periodic pass.
    pub fn push_thumbnails(&self) {
        self.inner.push_thumbnails();
    }

    /// Kind 1 messages handed to sessions so far (one per frame per session).
    pub fn frames_sent(&self) -> u64 {
        self.inner.frames_sent.load(Ordering::Relaxed)
    }

    /// The keyframes among them.
    pub fn keyframes_sent(&self) -> u64 {
        self.inner.keyframes_sent.load(Ordering::Relaxed)
    }

    /// Sessions being served now.
    pub fn session_count(&self) -> usize {
        self.inner.lock().clients.len()
    }

    /// What streams now.
    pub fn active_source(&self) -> Source {
        self.inner.lock().active
    }

    /// The last viewport the host took from a device; `None` before any, and once the
    /// last session left.
    pub fn last_viewport(&self) -> Option<Viewport> {
        self.inner.lock().last_viewport.clone()
    }

    /// Stops the host: goodbye "quit" to every session (as the real host's Quit), the
    /// door closed, the runtime shut down. Idempotent; also run on drop.
    pub fn stop(&self) {
        let Some(runtime) = self.runtime.lock().unwrap_or_else(|e| e.into_inner()).take() else { return };
        {
            let mut st = self.inner.lock();
            self.inner.goodbye_all(&mut st, &Goodbye { reason: "quit".into(), ..Goodbye::default() });
            st.stopped = true;
            // Each goodbye is written and its session ended, for half a second at most.
            let deadline = Instant::now() + Duration::from_millis(500);
            while !st.clients.is_empty() {
                let Some(left) = deadline.checked_duration_since(Instant::now()) else { break };
                st = self.inner.changed.wait_timeout(st, left).unwrap_or_else(|e| e.into_inner()).0;
            }
        }
        let _ = self.inner.shutdown.send(true);
        if let Some(m) = self.mdns.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = m.daemon.unregister(&m.fullname);
            let _ = m.daemon.shutdown();
        }
        // Inside another runtime's async context a runtime may not be shut down by
        // waiting (tokio panics): it is left to wind down in the background there.
        if tokio::runtime::Handle::try_current().is_ok() {
            runtime.shutdown_background();
        } else {
            runtime.shutdown_timeout(Duration::from_millis(500));
        }
    }
}

impl Drop for TestHost {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
#[test]
fn the_host_is_send_and_sync() {
    fn check<T: Send + Sync>() {}
    check::<TestHost>();
}

/// "4829 1355 7208": a code as the Mac shows it.
pub fn grouped_code(code: &str) -> String {
    pairing::grouped(code)
}
