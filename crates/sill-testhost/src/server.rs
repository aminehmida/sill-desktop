//! The door: accepting connections, the TLS handshake, and what each application protocol
//! gets (`Sources/SillHost/Door.swift`, `StreamServer.swift`).
//!
//! Left out of the real door, on purpose: the per-source places, their costs and the
//! 300 s backoff (`DoorPolicy.Places`), which would make a test that provokes five
//! refusals from 127.0.0.1 wait five minutes; the origin rules (VPN, internet, peer-to-peer
//! Wi-Fi); and the USB cable's pairing.

use crate::events::{DeviceEvent, Goodbye, Hello};
use crate::host::{Inner, Out};
use crate::pairing::b64url;
use crate::tls::{self, TrustFn, PAIRING_ALPN, SESSION_ALPN};
use crate::wire::{self, kind, Header};
use std::io;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, Notify};
use tokio::time::timeout;
use tokio_rustls::LazyConfigAcceptor;

/// `DoorPolicy.handshakeDeadline`: from accept to the end of the TLS handshake.
const HANDSHAKE_DEADLINE: Duration = Duration::from_secs(4);
/// `DoorPolicy.requestDeadline`: from ready to a pairing connection's one kind 19.
const REQUEST_DEADLINE: Duration = Duration::from_secs(4);
/// `DeviceGate.firstMessageDeadline`.
const FIRST_MESSAGE_DEADLINE: Duration = Duration::from_secs(2);
/// `DeviceGate.closeWait`: how long a refused device has to close its side.
const CLOSE_WAIT: Duration = Duration::from_secs(1);
/// How long a closed session or an answered pairing connection is drained before it is
/// dropped (the real host cancels once the message is handed over, or after 0.25 s).
const DRAIN: Duration = Duration::from_millis(250);

/// One whole message: its header, then its payload, refused when it announces more than `max`.
async fn read_message<R: AsyncRead + Unpin>(r: &mut R, max: usize) -> io::Result<(Header, Vec<u8>)> {
    let mut h = [0u8; wire::HEADER_LEN];
    r.read_exact(&mut h).await?;
    let header = wire::parse_header(&h);
    if header.len > max {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "the message is over the limit"));
    }
    let mut payload = vec![0u8; header.len];
    r.read_exact(&mut payload).await?;
    Ok((header, payload))
}

/// Reads and drops what the peer still sends until it closes, for `within` at most.
async fn drain<R: AsyncRead + Unpin>(r: &mut R, within: Duration) {
    let _ = timeout(within, async {
        let mut buf = [0u8; 4096];
        while matches!(r.read(&mut buf).await, Ok(n) if n > 0) {}
    })
    .await;
}

/// The goodbye and close of a connection that was never registered
/// (`StreamServer.closeWithGoodbye`): the message, this side's end (close_notify, FIN),
/// then whatever the device still sends read and dropped until it closes too, at most 1 s.
async fn close_with_goodbye<R, W>(rd: &mut R, wr: &mut W, goodbye: &Goodbye)
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let payload = serde_json::to_vec(goodbye).unwrap_or_default();
    let _ = wr.write_all(&wire::message(kind::GOODBYE, wire::now(), false, &payload)).await;
    let _ = wr.flush().await;
    let _ = wr.shutdown().await;
    drain(rd, CLOSE_WAIT).await;
}

/// Accepts connections until the host stops.
pub(crate) async fn accept_loop(inner: Arc<Inner>, listener: TcpListener) {
    let mut shutdown = inner.shutdown.subscribe();
    loop {
        tokio::select! {
            _ = shutdown.changed() => return,
            accepted = listener.accept() => {
                if let Ok((tcp, peer)) = accepted {
                    tokio::spawn(connection(inner.clone(), tcp, peer));
                }
            }
        }
    }
}

/// The stream's clock: frames, sound packets and ticks, each when it is due.
pub(crate) async fn pacer(inner: Arc<Inner>) {
    let mut shutdown = inner.shutdown.subscribe();
    loop {
        let next = inner.pace(Instant::now());
        tokio::select! {
            _ = shutdown.changed() => return,
            _ = inner.pacer.notified() => {}
            _ = async {
                match next {
                    Some(at) => tokio::time::sleep_until(at.into()).await,
                    None => std::future::pending::<()>().await,
                }
            } => {}
        }
    }
}

async fn connection(inner: Arc<Inner>, tcp: TcpStream, peer: SocketAddr) {
    let conn = inner.next_conn();
    let _ = tcp.set_nodelay(true);
    let refused = |alpn: Option<&str>, fingerprint: Option<[u8; 32]>, reason: String| {
        inner.log.push(DeviceEvent::HandshakeRefused {
            conn,
            alpn: alpn.map(str::to_string),
            fingerprint: fingerprint.map(|f| b64url(&f)),
            reason,
        });
    };
    let deadline = tokio::time::Instant::now() + HANDSHAKE_DEADLINE;
    // The ClientHello first: the verify block must know the protocol the device asks for.
    let acceptor = LazyConfigAcceptor::new(rustls::server::Acceptor::default(), tcp);
    let start = match tokio::time::timeout_at(deadline, acceptor).await {
        Ok(Ok(start)) => start,
        Ok(Err(e)) => return refused(None, None, format!("no TLS 1.3 ClientHello: {e}")),
        Err(_) => return refused(None, None, "the handshake timed out".into()),
    };
    let alpn = start.client_hello().alpn().and_then(tls::choose_alpn);
    // What the verify block saw: `Some(None)` for a key that is not P-256.
    let seen: Arc<Mutex<Option<Option<[u8; 32]>>>> = Arc::new(Mutex::new(None));
    let trust: TrustFn = {
        let (inner, seen) = (inner.clone(), seen.clone());
        Arc::new(move |fp| {
            *seen.lock().unwrap() = Some(fp);
            inner.trusts(alpn, fp)
        })
    };
    let config = match tls::server_config(&inner.identity, trust) {
        Ok(c) => c,
        Err(e) => return refused(alpn, None, format!("TLS setup: {e}")),
    };
    // Fallible: a refused handshake hands the socket back, so it can be closed in order.
    let accepted = tokio::time::timeout_at(deadline, start.into_stream(config).into_fallible()).await;
    let key = *seen.lock().unwrap();
    let tls = match accepted {
        Ok(Ok(tls)) => tls,
        Ok(Err((e, mut tcp))) => {
            // The alert is written; end this side and read what the device still sends before
            // closing. Dropping the socket with unread bytes resets the connection, and the
            // reset can overtake the alert (it does on Windows): the device then sees a lost
            // connection where it should see the refusal.
            let _ = tcp.shutdown().await;
            drain(&mut tcp, DRAIN).await;
            let reason = match (alpn, key) {
                (None, _) => format!("no application protocol in common: {e}"),
                (_, Some(Some(_))) => "unpaired: the door does not trust this key".to_string(),
                (_, Some(None)) => "the key is not P-256".to_string(),
                (_, None) => format!("the handshake failed: {e}"),
            };
            return refused(alpn, key.flatten(), reason);
        }
        Err(_) => return refused(alpn, key.flatten(), "the handshake timed out".into()),
    };
    let Some(Some(fp)) = key else { return refused(alpn, None, "no client certificate".into()) };
    match alpn {
        Some(SESSION_ALPN) => session(inner, conn, tls, fp).await,
        Some(PAIRING_ALPN) => pairing(inner, conn, tls, fp, peer).await,
        _ => {}
    }
}

/// `sill-pair/1`: exactly one kind 19 of at most 4 KiB within 4 s, one kind 20 back, then
/// closed (`Door.readPairRequest`).
async fn pairing<S: AsyncRead + AsyncWrite + Unpin>(inner: Arc<Inner>, conn: u64, tls: S, fp: [u8; 32], peer: SocketAddr) {
    let (mut rd, mut wr) = tokio::io::split(tls);
    let abandoned = |reason: &str| inner.log.push(DeviceEvent::PairingAbandoned { conn, reason: reason.into() });
    let payload = match timeout(REQUEST_DEADLINE, read_message(&mut rd, wire::MAX_PAIRING_PAYLOAD)).await {
        Ok(Ok((h, payload))) if h.kind == kind::PAIR_REQUEST && h.len > 0 => payload,
        Ok(Ok(_)) => return abandoned("the first message is not a kind 19 with a payload"),
        Ok(Err(e)) => return abandoned(&format!("no kind 19: {e}")),
        Err(_) => return abandoned("no kind 19 within 4 s"),
    };
    let Some(answer) = inner.judge_pairing(conn, &payload, fp, &peer.ip().to_string()) else { return };
    let _ = wr.write_all(&wire::message(kind::PAIR_RESULT, wire::now(), false, &answer)).await;
    let _ = wr.flush().await;
    let _ = wr.shutdown().await;
    drain(&mut rd, DRAIN).await;
}

/// `sill/1`: judged again at ready, through the device gate, then served
/// (`Door.admitSession`, `StreamServer.gate`, `serve` and `receiveLoop`).
async fn session<S: AsyncRead + AsyncWrite + Unpin + Send + 'static>(inner: Arc<Inner>, conn: u64, tls: S, fp: [u8; 32]) {
    let (mut rd, mut wr) = tokio::io::split(tls);
    match inner.session_at_ready(&fp) {
        Ok(()) => {}
        Err(None) => {
            return inner.log.push(DeviceEvent::HandshakeRefused {
                conn,
                alpn: Some(SESSION_ALPN.into()),
                fingerprint: Some(b64url(&fp)),
                reason: "unpaired: removed since the handshake".into(),
            })
        }
        Err(Some(reason)) => {
            inner.log.push(DeviceEvent::SessionRefused { conn, reason: reason.into() });
            let goodbye = Goodbye { reason: reason.into(), ..Goodbye::default() };
            return close_with_goodbye(&mut rd, &mut wr, &goodbye).await;
        }
    }
    // The device gate, with a floor above "0": the session's first message must be a hello
    // the floor admits, within 2 s; any other device gets the update goodbye, nothing
    // before it, and is never registered.
    let mut first_hello: Option<Hello> = None;
    if inner.gate_is_on() {
        let hello = match timeout(FIRST_MESSAGE_DEADLINE, read_message(&mut rd, wire::MAX_CLIENT_PAYLOAD)).await {
            Ok(Ok((h, payload))) if h.kind == kind::HELLO => serde_json::from_slice::<Hello>(&payload).ok(),
            _ => None,
        };
        if let Some(goodbye) = inner.gate_refusal(hello.as_ref()) {
            inner.log.push(DeviceEvent::SessionRefused { conn, reason: goodbye.reason.clone() });
            return close_with_goodbye(&mut rd, &mut wr, &goodbye).await;
        }
        first_hello = hello;
    }

    let (tx, mut rx) = mpsc::unbounded_channel::<Out>();
    let inflight = Arc::new(AtomicUsize::new(0));
    let stop = Arc::new(Notify::new());
    inner.register(conn, fp, tx, inflight.clone(), stop.clone());
    if let Some(hello) = first_hello {
        inner.took_hello(conn, hello);
    }

    // The writer: one message at a time, in order; `inflight` falls as each is handed to
    // the connection (the real host's `contentProcessed`).
    let mut writer = tokio::spawn({
        let stop = stop.clone();
        async move {
            'outer: while let Some(mut out) = rx.recv().await {
                loop {
                    match out {
                        Out::Data(bytes, counted) => {
                            let result = wr.write_all(&bytes).await;
                            if counted {
                                inflight.fetch_sub(1, Ordering::SeqCst);
                            }
                            if result.is_err() {
                                break 'outer;
                            }
                        }
                        Out::Close => {
                            let _ = wr.flush().await;
                            let _ = wr.shutdown().await;
                            break 'outer;
                        }
                    }
                    match rx.try_recv() {
                        Ok(more) => out = more,
                        Err(_) => break,
                    }
                }
                if wr.flush().await.is_err() {
                    break;
                }
            }
            stop.notify_one();
        }
    });

    loop {
        tokio::select! {
            _ = stop.notified() => {
                // The host closed this session (a goodbye) or its writes failed: what the
                // device still sends is read and dropped for a moment, so the goodbye is
                // not chased by a reset.
                drain(&mut rd, DRAIN).await;
                break;
            }
            message = read_message(&mut rd, wire::MAX_CLIENT_PAYLOAD) => match message {
                Ok((h, payload)) => inner.on_message(conn, h, payload),
                // The end of the connection, a read error, or a message over 1 MiB.
                Err(_) => break,
            }
        }
    }
    inner.unregister(conn);
    if timeout(CLOSE_WAIT, &mut writer).await.is_err() {
        writer.abort();
    }
}
