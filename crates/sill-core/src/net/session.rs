//! One session connection to a Mac (`StreamClient`'s connection half): the hello first, a writer
//! task so nobody else waits on the socket, a reader task, pings every 0.25 s on the monotonic
//! clock, a report every second, and liveness: no byte for max(6 s, 4 × the worst recent round
//! trip) ends it (a payload's every 256 KB counts, so a slow keyframe is not silence).

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

use tokio::io::{AsyncWriteExt, BufWriter};
use tokio::sync::{mpsc, watch};

use super::tls::TlsConnection;
use crate::protocol::{self, kind, payloads, Message, MessageReader, ReadError};

/// One second of what this device saw, for the Mac's log (`ClientStats`) and a HUD. `-1`: no
/// sample that second.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LinkStats {
    pub fps: i32,
    pub frame_age_ms: i32,
    pub frame_age_max_ms: i32,
    pub rtt_ms: i32,
    pub rtt_max_ms: i32,
}

/// How a session ended.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SessionEnd {
    /// `None` for a clean end.
    pub why: Option<String>,
    /// The Mac refused this device's key: a TLS alert before its first byte (upstream's -9825,
    /// -9829 right after `.ready`). The host refuses an unpaired key inside the handshake, which a
    /// TLS 1.3 client only learns at its first read.
    pub key_refused: bool,
}

/// What a session tells its owner. Called on the session's own tasks: never block.
pub trait SessionListener: Send + Sync + 'static {
    /// On the reader task. Frames go straight to the decoder from here.
    fn on_message(&self, session: &Arc<Session>, msg: Message);
    /// Once, on whichever task noticed the end.
    fn on_ended(&self, session: &Arc<Session>, end: SessionEnd);
    fn on_stats(&self, _session: &Arc<Session>, _stats: LinkStats) {}
    /// `ClientStats`' sound fields for the second just ended (`audioBehindMs`, `audioLate`);
    /// `None`: none played.
    fn sound_stats(&self) -> Option<(i32, i32)> {
        None
    }
}

/// Seconds on this process's monotonic clock.
pub fn monotonic() -> f64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs_f64()
}

fn monotonic_ms() -> u64 {
    (monotonic() * 1000.0) as u64
}

/// Pointer moves go out at most this often, the newest one.
const MOVE_INTERVAL_MS: u64 = 8;

struct Inner {
    pending_move: Option<String>,
    flush_scheduled: bool,
    last_move_at: u64,
    frames: i32,
    ages: Vec<i32>,
    rtts: Vec<i32>,
}

/// A live session. Cheap to share; every method may be called from any thread.
pub struct Session {
    me: Weak<Session>,
    /// The address that answered.
    pub host: String,
    pub port: u16,
    /// SHA-256 of the SubjectPublicKeyInfo of the key that answered.
    pub mac_fingerprint: [u8; 32],
    device_label: String,
    listener: Arc<dyn SessionListener>,
    runtime: tokio::runtime::Handle,
    out: mpsc::UnboundedSender<Vec<u8>>,
    stop: watch::Sender<bool>,
    ended: AtomicBool,
    inner: Mutex<Inner>,
    last_received_at: AtomicU64,
    received_any: AtomicBool,
    inputs_on_session: AtomicU64,
    worst_recent_rtt_ms: AtomicI64,
    last_stats: Mutex<Option<LinkStats>>,
}

impl Session {
    /// Starts the session on `runtime`: the hello is the first message on the connection, before
    /// anything else (Compatibility.swift).
    pub fn start(
        conn: TlsConnection, device_label: String, hello: String, listener: Arc<dyn SessionListener>, runtime: &tokio::runtime::Handle,
    ) -> Arc<Session> {
        let (out, rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let (stop, stop_rx) = watch::channel(false);
        let session = Arc::new_cyclic(|me| Session {
            me: me.clone(),
            host: conn.host.clone(),
            port: conn.port,
            mac_fingerprint: conn.mac_fingerprint,
            device_label,
            listener,
            runtime: runtime.clone(),
            out,
            stop,
            ended: AtomicBool::new(false),
            inner: Mutex::new(Inner { pending_move: None, flush_scheduled: false, last_move_at: 0, frames: 0, ages: Vec::new(), rtts: Vec::new() }),
            last_received_at: AtomicU64::new(monotonic_ms()),
            received_any: AtomicBool::new(false),
            inputs_on_session: AtomicU64::new(0),
            worst_recent_rtt_ms: AtomicI64::new(0),
            last_stats: Mutex::new(None),
        });
        let _ = session.out.send(protocol::encode_json(kind::HELLO, &hello));
        let (read, write) = tokio::io::split(conn.stream);
        runtime.spawn(write_loop(session.clone(), write, rx, stop_rx.clone()));
        runtime.spawn(read_loop(session.clone(), read, stop_rx.clone()));
        runtime.spawn(timer_loop(session.clone(), stop_rx));
        session
    }

    /// Queues one message. Nothing after the end.
    pub fn send(&self, kind: u8, payload: &[u8]) {
        let mut inner = self.inner.lock().unwrap();
        if self.is_ended() {
            return;
        }
        if kind == kind::INPUT {
            self.flush_move_locked(&mut inner);
        }
        let _ = self.out.send(protocol::encode(kind, payload));
        if kind == kind::INPUT {
            self.inputs_on_session.fetch_add(1, Ordering::SeqCst);
        }
    }

    pub fn send_json(&self, kind: u8, json: &str) {
        self.send(kind, json.as_bytes());
    }

    /// Input (kind 8). Pointer moves go out at most every 8 ms, the newest one; any other input
    /// first flushes a waiting move.
    pub fn send_input(&self, json: &str, is_move: bool) {
        let mut inner = self.inner.lock().unwrap();
        if self.is_ended() {
            return;
        }
        if !is_move {
            self.flush_move_locked(&mut inner);
            self.put_input(json);
            return;
        }
        let now = monotonic_ms();
        if inner.pending_move.is_none() && now.saturating_sub(inner.last_move_at) >= MOVE_INTERVAL_MS {
            self.put_input(json);
            inner.last_move_at = now;
            return;
        }
        inner.pending_move = Some(json.to_string());
        if !inner.flush_scheduled {
            inner.flush_scheduled = true;
            let delay = MOVE_INTERVAL_MS.saturating_sub(now.saturating_sub(inner.last_move_at));
            let me = self.me.clone();
            self.runtime.spawn(async move {
                tokio::time::sleep(Duration::from_millis(delay)).await;
                if let Some(s) = me.upgrade() {
                    let mut inner = s.inner.lock().unwrap();
                    inner.flush_scheduled = false;
                    if !s.is_ended() {
                        s.flush_move_locked(&mut inner);
                    }
                }
            });
        }
    }

    fn put_input(&self, json: &str) {
        let _ = self.out.send(protocol::encode_json(kind::INPUT, json));
        self.inputs_on_session.fetch_add(1, Ordering::SeqCst);
    }

    fn flush_move_locked(&self, inner: &mut Inner) {
        if let Some(m) = inner.pending_move.take() {
            self.put_input(&m);
            inner.last_move_at = monotonic_ms();
        }
    }

    /// Whether a coalesced move still waits (a kind 26 is stale then).
    pub fn move_waiting(&self) -> bool {
        self.inner.lock().unwrap().pending_move.is_some()
    }

    /// Kind 8 messages sent on this connection: a kind 26 whose `seen` is lower is stale.
    pub fn inputs_on_session(&self) -> u64 {
        self.inputs_on_session.load(Ordering::SeqCst)
    }

    /// The worst round trip of the last second that had a pong; 0 until one does.
    pub fn worst_recent_rtt_ms(&self) -> i64 {
        self.worst_recent_rtt_ms.load(Ordering::Relaxed)
    }

    pub fn last_stats(&self) -> Option<LinkStats> {
        *self.last_stats.lock().unwrap()
    }

    /// A frame arrived: its age (host clock to this one, approximate across devices).
    pub fn note_frame(&self, host_timestamp: f64) {
        let age = ((protocol::now() - host_timestamp) * 1000.0).clamp(0.0, i32::MAX as f64) as i32;
        let mut inner = self.inner.lock().unwrap();
        inner.frames += 1;
        inner.ages.push(age);
    }

    /// Ends the session and closes its connection.
    pub fn close(&self, why: Option<&str>) {
        self.end(SessionEnd { why: why.map(str::to_string), key_refused: false });
    }

    pub fn is_ended(&self) -> bool {
        self.ended.load(Ordering::SeqCst)
    }

    /// Coming back after a pause in which nothing was read: that silence is not the Mac's.
    pub fn reset_liveness(&self) {
        self.last_received_at.store(monotonic_ms(), Ordering::Relaxed);
    }

    fn end(&self, end: SessionEnd) {
        {
            let _inner = self.inner.lock().unwrap();
            if self.ended.swap(true, Ordering::SeqCst) {
                return;
            }
        }
        let _ = self.stop.send(true);
        if let Some(me) = self.me.upgrade() {
            self.listener.on_ended(&me, end);
        }
    }

    fn tick(&self) {
        if self.is_ended() {
            return;
        }
        self.send(kind::PING, &payloads::ping(monotonic()));
        let limit = 6_000u64.max(4 * self.worst_recent_rtt_ms().max(0) as u64);
        let silent = monotonic_ms().saturating_sub(self.last_received_at.load(Ordering::Relaxed));
        if silent > limit {
            self.end(SessionEnd { why: Some(format!("connection silent for {} s", silent / 1000)), key_refused: false });
        }
    }

    fn close_second(&self) {
        if self.is_ended() {
            return;
        }
        let stats = {
            let mut inner = self.inner.lock().unwrap();
            inner.ages.sort_unstable();
            inner.rtts.sort_unstable();
            let (a, r) = (&inner.ages, &inner.rtts);
            let stats = LinkStats {
                fps: inner.frames,
                frame_age_ms: if a.is_empty() { -1 } else { a[a.len() / 2] },
                frame_age_max_ms: a.last().copied().unwrap_or(-1),
                rtt_ms: if r.is_empty() { -1 } else { r[r.len() / 2] },
                rtt_max_ms: r.last().copied().unwrap_or(-1),
            };
            if let Some(worst) = r.last() {
                self.worst_recent_rtt_ms.store(i64::from(*worst), Ordering::Relaxed);
            }
            inner.frames = 0;
            inner.ages.clear();
            inner.rtts.clear();
            stats
        };
        *self.last_stats.lock().unwrap() = Some(stats);
        let sound = self.listener.sound_stats();
        self.send_json(
            kind::CLIENT_STATS,
            &payloads::client_stats(
                stats.fps, stats.frame_age_ms, stats.rtt_ms, &self.device_label, stats.frame_age_max_ms, stats.rtt_max_ms,
                sound.map(|s| s.0), sound.map(|s| s.1),
            ),
        );
        if let Some(me) = self.me.upgrade() {
            self.listener.on_stats(&me, stats);
        }
    }
}

type ReadHalf = tokio::io::ReadHalf<tokio_rustls::client::TlsStream<tokio::net::TcpStream>>;
type WriteHalf = tokio::io::WriteHalf<tokio_rustls::client::TlsStream<tokio::net::TcpStream>>;

async fn write_loop(session: Arc<Session>, write: WriteHalf, mut rx: mpsc::UnboundedReceiver<Vec<u8>>, mut stop: watch::Receiver<bool>) {
    let mut out = BufWriter::with_capacity(64 * 1024, write);
    let result: std::io::Result<()> = async {
        loop {
            let first = tokio::select! {
                m = rx.recv() => m,
                _ = stop.changed() => None,
            };
            let Some(mut m) = first else { break };
            loop {
                out.write_all(&m).await?;
                match rx.try_recv() {
                    Ok(next) => m = next,
                    Err(_) => break,
                }
            }
            out.flush().await?;
        }
        // What was queued before the end still goes (a last message, then close_notify).
        while let Ok(m) = rx.try_recv() {
            out.write_all(&m).await?;
        }
        out.flush().await?;
        Ok(())
    }
    .await;
    let _ = tokio::time::timeout(Duration::from_secs(1), out.shutdown()).await;
    if let Err(e) = result {
        session.end(SessionEnd { why: Some(format!("write failed: {e}")), key_refused: false });
    }
}

/// A TLS alert from the peer, other than its closing one.
fn alert_received(e: &std::io::Error) -> bool {
    match e.get_ref().and_then(|r| r.downcast_ref::<rustls::Error>()) {
        Some(rustls::Error::AlertReceived(a)) => *a != rustls::AlertDescription::CloseNotify,
        _ => false,
    }
}

async fn read_loop(session: Arc<Session>, read: ReadHalf, mut stop: watch::Receiver<bool>) {
    let s = session.clone();
    let mut reader = MessageReader::new(read, move || {
        s.last_received_at.store(monotonic_ms(), Ordering::Relaxed);
        s.received_any.store(true, Ordering::Relaxed);
    });
    loop {
        let next = tokio::select! {
            m = reader.next() => m,
            _ = stop.changed() => return,
        };
        match next {
            Ok(m) => match m.kind {
                kind::PONG if m.payload.len() == 8 => {
                    let sent = f64::from_bits(u64::from_be_bytes(m.payload[..8].try_into().unwrap()));
                    let rtt = ((monotonic() - sent) * 1000.0).clamp(0.0, i32::MAX as f64) as i32;
                    session.inner.lock().unwrap().rtts.push(rtt);
                }
                kind::TICK => {}
                // A pong of another length is a clipboard Paste's receipt (16 random bytes).
                _ => session.listener.on_message(&session, m),
            },
            Err(ReadError::Eof) => {
                session.end(SessionEnd::default());
                return;
            }
            Err(ReadError::Io(e)) => {
                let key_refused = !session.received_any.load(Ordering::Relaxed) && alert_received(&e);
                let why = if session.is_ended() { None } else { Some(format!("read failed: {e}")) };
                session.end(SessionEnd { why, key_refused });
                return;
            }
        }
    }
}

async fn timer_loop(session: Arc<Session>, mut stop: watch::Receiver<bool>) {
    let mut ticks = tokio::time::interval(Duration::from_millis(250));
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut n = 0u32;
    loop {
        tokio::select! {
            _ = ticks.tick() => {}
            _ = stop.changed() => return,
        }
        if session.is_ended() {
            return;
        }
        session.tick();
        n += 1;
        #[allow(clippy::manual_is_multiple_of)]
        if n % 4 == 0 {
            session.close_second();
        }
    }
}
