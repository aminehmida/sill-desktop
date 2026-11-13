//! The host tried with a minimal raw device of its own: tokio-rustls, the 14-byte header
//! and JSON, nothing of this workspace's client.

use crate::identity::{verify_p256, Identity};
use crate::pairing::{self, b64url};
use crate::tls::{testclient::client_config, PAIRING_ALPN, SESSION_ALPN};
use crate::wire::{self, kind, Header};
use crate::{DeviceEvent, Door, HostConfig, InputEvent, Source, TestHost};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use rustls::pki_types::ServerName;
use serde_json::{json, Value};
use std::io;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_rustls::client::TlsStream;
use tokio_rustls::TlsConnector;

const WAIT: Duration = Duration::from_secs(5);

fn block<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(f)
}

async fn dial(host: &TestHost, device: &Identity, alpn: &str, pin: Option<[u8; 32]>) -> io::Result<TlsStream<TcpStream>> {
    let tcp = TcpStream::connect(host.address()).await?;
    TlsConnector::from(client_config(device, alpn, pin)).connect(ServerName::try_from("sill").unwrap(), tcp).await
}

async fn read_message(s: &mut TlsStream<TcpStream>) -> io::Result<(Header, Vec<u8>)> {
    let mut h = [0u8; wire::HEADER_LEN];
    s.read_exact(&mut h).await?;
    let header = wire::parse_header(&h);
    let mut payload = vec![0u8; header.len];
    s.read_exact(&mut payload).await?;
    Ok((header, payload))
}

/// One kind 19 on a pairing connection and its kind 20; `None` when the door let none through.
async fn pair_request(host: &TestHost, device: &Identity, body: Value) -> Option<Value> {
    let mut s = dial(host, device, PAIRING_ALPN, Some(host.fingerprint())).await.ok()?;
    s.write_all(&wire::message(kind::PAIR_REQUEST, wire::now(), false, body.to_string().as_bytes())).await.ok()?;
    let (h, payload) = timeout(WAIT, read_message(&mut s)).await.ok()?.ok()?;
    assert_eq!(h.kind, kind::PAIR_RESULT);
    // The host closes after its answer.
    let mut rest = Vec::new();
    assert_eq!(timeout(WAIT, s.read_to_end(&mut rest)).await.ok()?.ok()?, 0);
    serde_json::from_slice(&payload).ok()
}

fn code_request(host: &TestHost, device: &Identity, code: &str) -> Value {
    let key = pairing::code_key(code, &host.fingerprint());
    let proof = pairing::device_proof(&key, &device.fingerprint, &host.fingerprint());
    json!({"v": 1, "method": "code", "proof": b64url(&proof), "name": "Test iPad", "model": "iPad14,1"})
}

fn qr_request(host: &TestHost, device: &Identity, secret: &[u8]) -> Value {
    let proof = pairing::device_proof(secret, &device.fingerprint, &host.fingerprint());
    json!({"v": 1, "method": "qr", "proof": b64url(&proof), "name": "Test iPad", "model": null})
}

async fn pair(host: &TestHost, device: &Identity) {
    let secret = host.pairing_secret().unwrap();
    let r = pair_request(host, device, qr_request(host, device, &secret)).await.unwrap();
    assert_eq!(r["ok"], json!(true), "{r}");
}

struct Session {
    s: TlsStream<TcpStream>,
    /// Kinds read while waiting for another, in order.
    skipped: Vec<u8>,
}

impl Session {
    async fn open(host: &TestHost, device: &Identity) -> Session {
        let s = dial(host, device, SESSION_ALPN, Some(host.fingerprint())).await.unwrap();
        Session { s, skipped: Vec::new() }
    }

    async fn send(&mut self, kind: u8, payload: &[u8]) {
        self.s.write_all(&wire::message(kind, wire::now(), false, payload)).await.unwrap();
    }

    async fn send_json(&mut self, kind: u8, v: Value) {
        self.send(kind, v.to_string().as_bytes()).await;
    }

    async fn next(&mut self) -> (Header, Vec<u8>) {
        timeout(WAIT, read_message(&mut self.s)).await.expect("a message within 5 s").expect("the connection stays open")
    }

    /// The next message of `kind`, others skipped (and remembered).
    async fn until(&mut self, kind: u8) -> (Header, Vec<u8>) {
        loop {
            let (h, p) = self.next().await;
            if h.kind == kind {
                return (h, p);
            }
            self.skipped.push(h.kind);
        }
    }

    async fn until_json(&mut self, kind: u8, accept: impl Fn(&Value) -> bool) -> Value {
        loop {
            let (_, p) = self.until(kind).await;
            let v: Value = serde_json::from_slice(&p).expect("JSON");
            if accept(&v) {
                return v;
            }
        }
    }

    /// The catalog a session gets on connect, in order, up to the last thumbnail.
    async fn catalog(&mut self) -> Vec<(Header, Vec<u8>)> {
        let mut all = Vec::new();
        let mut thumbnails = 0;
        while thumbnails < 3 {
            let m = self.next().await;
            if m.0.kind == kind::THUMBNAIL {
                thumbnails += 1;
            }
            all.push(m);
        }
        all
    }
}

#[test]
fn rejects_a_code_without_a_valid_check_digit() {
    let bad = HostConfig { code: Some("482913557209".into()), ..HostConfig::default() };
    assert_eq!(TestHost::start(bad).err().map(|e| e.kind()), Some(io::ErrorKind::InvalidInput));
    let good = HostConfig { code: Some("4829 1355 7208".into()), ..HostConfig::default() };
    let host = TestHost::start(good).unwrap();
    assert_eq!(host.pairing_code(), "482913557208");
    assert_eq!(crate::grouped_code(&host.pairing_code()), "4829 1355 7208");
    assert_eq!(host.mac_id().len(), 16);
    assert!(host.pair_link().unwrap().starts_with(&format!("sill://pair?v=1&m={}&k=", host.mac_id())));
}

#[test]
fn pairs_with_the_code_then_serves_a_session() {
    let host = TestHost::start(HostConfig { name: "Mac & mini".into(), ..HostConfig::default() }).unwrap();
    let device = Identity::generate().unwrap();
    let code = host.pairing_code();
    block(async {
        // Pair with the typed code.
        let r = pair_request(&host, &device, code_request(&host, &device, &code)).await.unwrap();
        assert_eq!(r["ok"], json!(true), "{r}");
        assert_eq!(r["macID"], json!(host.mac_id()));
        assert_eq!(r["name"], json!("Mac & mini"));
        assert_eq!(pairing::b64url_decode(r["recognitionKey"].as_str().unwrap()).unwrap(), host.recognition_key());
        let key = pairing::code_key(&code, &host.fingerprint());
        let proof_m = pairing::mac_proof(&key, &host.fingerprint(), &device.fingerprint);
        assert_eq!(r["proof"], json!(b64url(&proof_m)));
        assert!(r.get("reason").is_none() && r.get("method").is_none());
        let paired = host.paired_devices();
        assert_eq!(paired.len(), 1);
        assert_eq!((paired[0].name.as_str(), paired[0].method.as_str()), ("Test iPad", "code"));
        assert_eq!(paired[0].fingerprint, b64url(&device.fingerprint));
        // A fresh window opened by itself, with another code.
        assert_ne!(host.pairing_code(), code);

        // The session: hello first, then the catalog in the real host's order.
        let mut s = Session::open(&host, &device).await;
        s.send_json(kind::HELLO, json!({"appVersion": "0.6.1", "build": "1", "protocol": 1, "device": "iPad (iPad14,1)"})).await;
        let catalog = s.catalog().await;
        let kinds: Vec<u8> = catalog.iter().map(|m| m.0.kind).collect();
        assert_eq!(kinds, [2, 16, 18, 4, 4, 4, 4, 4, 5, 14, 3, 3, 3]);

        let list: Value = serde_json::from_slice(&catalog[0].1).unwrap();
        assert_eq!(list["macName"], json!("Mac & mini"));
        assert_eq!(list["active"], json!({"none": {}}));
        assert_eq!(list["hostVersion"], json!("0.6.1"));
        assert_eq!((&list["protocol"], &list["gestures"], &list["clipboard"], &list["keyRepeat"]), (&json!(1), &json!(1), &json!(1), &json!(1)));
        assert_eq!(list["launchID"].as_str().unwrap().len(), 36);
        assert!(list.get("locked").is_none());
        let windows = list["windows"].as_array().unwrap();
        assert_eq!(windows.len(), 3);
        assert_eq!(windows[0], json!({"id": 101, "title": "Untitled.txt", "appName": "TextEdit", "bundleID": "com.apple.TextEdit", "width": 800, "height": 600}));

        let settings: Value = serde_json::from_slice(&catalog[1].1).unwrap();
        assert_eq!(settings["settings"]["maxFPS"], json!(120));
        assert_eq!(settings["sessionPaired"], json!(true));
        assert!(settings.get("stream").is_none() && settings.get("answering").is_none());

        // Kind 18: signed by the host's key over the exact `info` bytes.
        let signed: Value = serde_json::from_slice(&catalog[2].1).unwrap();
        let info = STANDARD.decode(signed["info"].as_str().unwrap()).unwrap();
        let point: [u8; 65] = STANDARD.decode(signed["key"].as_str().unwrap()).unwrap().try_into().unwrap();
        let sig = STANDARD.decode(signed["sig"].as_str().unwrap()).unwrap();
        assert!(verify_p256(&point, &info, &sig));
        assert_eq!(crate::identity::fingerprint_of_point(&point), host.fingerprint());
        let info: Value = serde_json::from_slice(&info).unwrap();
        assert_eq!((&info["v"], &info["macID"]), (&json!(1), &json!(host.mac_id())));

        // An icon: bundle id length, bundle id, PNG. A thumbnail: window id, JPEG.
        let icon = &catalog[3].1;
        let n = u16::from_be_bytes([icon[0], icon[1]]) as usize;
        assert_eq!(&icon[2..2 + n], b"com.apple.TextEdit");
        assert_eq!(&icon[2 + n..2 + n + 4], &[0x89, b'P', b'N', b'G']);
        let apps: Value = serde_json::from_slice(&catalog[8].1).unwrap();
        assert_eq!(apps[4], json!({"name": "Notes", "bundleID": "com.apple.Notes"}));
        let shape = &catalog[9].1;
        assert_eq!(f32::from_bits(u32::from_be_bytes(shape[8..12].try_into().unwrap())), 16.0);
        assert_eq!(&shape[16..20], &[0x89, b'P', b'N', b'G']);
        let thumb = &catalog[10].1;
        assert_eq!(u32::from_be_bytes(thumb[..4].try_into().unwrap()), 101);
        assert_eq!(&thumb[4..6], &[0xFF, 0xD8]);

        // No video before a pick. A ping comes back unchanged, time stamp included.
        s.s.write_all(&wire::message(kind::PING, 42.5, false, &[1, 2, 3, 4, 5, 6, 7, 8])).await.unwrap();
        let (h, p) = s.next().await;
        assert_eq!((h.kind, h.timestamp, p.as_slice()), (kind::PONG, 42.5, &[1u8, 2, 3, 4, 5, 6, 7, 8][..]));
        assert_eq!(host.frames_sent(), 0);

        // Pick the Desktop: the list says so, the settings say what runs, then the
        // parameter sets and the first keyframe.
        s.send_json(kind::VIEWPORT, json!({"width": 1024, "height": 768, "scale": null, "fps": 60})).await;
        s.send_json(kind::SELECT_SOURCE, json!({"desktop": {}})).await;
        let list = s.until_json(kind::WINDOW_LIST, |_| true).await;
        assert_eq!(list["active"], json!({"desktop": {}}));
        let running = s.until_json(kind::HOST_SETTINGS, |v| v.get("stream").is_some()).await;
        assert_eq!(running["stream"], json!({"width": 1280, "height": 720, "fps": 60, "mbps": 15, "onVirtualDisplay": false}));
        let (h, sets) = s.until(kind::PARAMETER_SETS).await;
        assert!(h.keyframe);
        assert_eq!(&sets[..2], &[4, 3]);
        // A session waiting for its keyframe gets the sets once more, then the keyframe.
        let (h2, sets2) = s.next().await;
        assert_eq!((h2.kind, &sets2), (kind::PARAMETER_SETS, &sets));
        let (h3, frame) = s.next().await;
        assert_eq!((h3.kind, h3.keyframe, h3.timestamp), (kind::FRAME, true, h.timestamp));
        let first_len = u32::from_be_bytes(frame[..4].try_into().unwrap()) as usize;
        assert!(first_len + 4 <= frame.len());
        let (h4, delta) = s.until(kind::FRAME).await;
        assert!(!h4.keyframe);
        assert_eq!((delta[4] >> 1) & 0x3F, 1); // a trailing picture's slice
        assert!(!s.skipped.contains(&kind::FRAME));

        // A keyframe request: the next frame messages are the sets and a keyframe.
        s.send(kind::KEYFRAME_REQUEST, &[]).await;
        let before = host.keyframes_sent();
        loop {
            let (h, _) = s.until(kind::FRAME).await;
            if h.keyframe {
                break;
            }
        }
        assert!(host.keyframes_sent() > before || host.keyframes_sent() >= 2);
        assert!(host
            .wait_for(|e| matches!(e, DeviceEvent::KeyframeRequest { honoured: true, .. }), WAIT)
            .is_some());

        // Settings: one answer carrying the token; a value no menu offers is refused.
        s.send_json(kind::CHANGE_SETTINGS, json!({"token": 7, "bitrate": 25_000_000, "maxFPS": 75})).await;
        let answer = s.until_json(kind::HOST_SETTINGS, |v| v.get("answering").is_some()).await;
        assert_eq!((&answer["answering"], &answer["settings"]["bitrate"], &answer["settings"]["maxFPS"]), (&json!(7), &json!(25_000_000), &json!(120)));

        // Input, a gesture, stats, a window command: recorded as decoded.
        s.send_json(kind::INPUT, json!({"pointer": {"_0": "leftDown", "x": 0.25, "y": 0.75}})).await;
        s.send_json(kind::INPUT, json!({"key": {"hidUsage": 40, "down": true, "modifiers": 0}})).await;
        s.send_json(kind::GESTURE, json!({"gesture": "swipeUp", "fingers": 3})).await;
        s.send_json(kind::CLIENT_STATS, json!({"fps": 60, "frameAgeMs": 9, "rttMs": 1, "device": "iPad (iPad14,1)"})).await;
        s.send_json(kind::INPUT, json!({"pointer": {"_0": "hover", "x": 0.25, "y": 0.75}})).await;
        assert!(host.wait_for(|e| matches!(e, DeviceEvent::Undecodable { kind: 8, .. }), WAIT).is_some());
        let events = host.events();
        assert!(events.contains(&DeviceEvent::Input { conn: 2, input: InputEvent::Pointer { action: "leftDown".into(), x: 0.25, y: 0.75 } }));
        assert!(events.contains(&DeviceEvent::Input { conn: 2, input: InputEvent::Key { hid_usage: 40, down: true, modifiers: 0 } }));
        assert!(events.contains(&DeviceEvent::Gesture { conn: 2, gesture: "swipeUp".into(), fingers: Some(3) }));
        assert!(events.contains(&DeviceEvent::SelectSource { conn: 2, source: Source::Desktop }));
        assert!(events.iter().any(|e| matches!(e, DeviceEvent::Hello { hello, .. } if hello.app_version.as_deref() == Some("0.6.1"))));
        assert!(events.iter().any(|e| matches!(e, DeviceEvent::Viewport { viewport, refused: None, .. } if viewport.width == 1024.0)));
        assert!(events.iter().any(|e| matches!(e, DeviceEvent::ClientStats { stats, .. } if stats.frame_age_ms == 9)));
        assert_eq!(host.last_viewport().map(|v| v.height), Some(768.0));
        assert_eq!(host.active_source(), Source::Desktop);

        // The Mac's pointer and a new cursor shape.
        host.push_mac_pointer(Some((0.12345, 0.5)));
        let pointer = s.until_json(kind::MAC_POINTER, |_| true).await;
        assert_eq!(pointer, json!({"inside": true, "seen": 3, "x": 0.1235, "y": 0.5}));
        host.push_cursor_shape();
        let (_, shape) = s.until(kind::CURSOR_SHAPE).await;
        assert_eq!(f32::from_bits(u32::from_be_bytes(shape[..4].try_into().unwrap())), 8.0);

        // A goodbye, then the end of the connection.
        host.send_goodbye("busy", Some("Try later."));
        let bye = s.until_json(kind::GOODBYE, |_| true).await;
        assert_eq!(bye, json!({"reason": "busy", "message": "Try later."}));
        let mut rest = Vec::new();
        let _ = timeout(WAIT, s.s.read_to_end(&mut rest)).await.expect("the host closes after its goodbye");
    });
    assert!(host.wait_for(|e| matches!(e, DeviceEvent::SessionEnded { conn: 2 }), WAIT).is_some());
    assert_eq!(host.session_count(), 0);
    assert_eq!(host.active_source(), Source::None);
    host.stop();
}

#[test]
fn refuses_an_unpaired_key_inside_the_handshake() {
    let host = TestHost::start(HostConfig::default()).unwrap();
    let stranger = Identity::generate().unwrap();
    block(async {
        // TLS 1.3: the client's own handshake may finish before the server judged its
        // certificate; the refusal then arrives at its first read. Never a Sill byte.
        let outcome = match dial(&host, &stranger, SESSION_ALPN, Some(host.fingerprint())).await {
            Err(e) => Err(e),
            Ok(mut s) => {
                let _ = s.write_all(&wire::message(kind::HELLO, wire::now(), false, b"{}")).await;
                timeout(WAIT, read_message(&mut s)).await.expect("an answer within 5 s").map(|_| ())
            }
        };
        let error = outcome.expect_err("an unpaired key gets no session");
        assert!(format!("{error:?}").contains("BadCertificate"), "{error:?}");
        // A protocol the door does not offer fails the handshake too.
        assert!(dial(&host, &stranger, "sill/2", Some(host.fingerprint())).await.is_err());
        // A device that pins another Mac's key never sends its certificate.
        assert!(dial(&host, &stranger, SESSION_ALPN, Some([9; 32])).await.is_err());
    });
    let refused = host.wait_for(|e| matches!(e, DeviceEvent::HandshakeRefused { alpn: Some(a), .. } if a == "sill/1"), WAIT);
    match refused {
        Some(DeviceEvent::HandshakeRefused { fingerprint, reason, .. }) => {
            assert_eq!(fingerprint, Some(b64url(&stranger.fingerprint)));
            assert!(reason.starts_with("unpaired"));
        }
        other => panic!("{other:?}"),
    }
    assert!(!host.events().iter().any(|e| matches!(e, DeviceEvent::SessionStarted { .. })));
}

#[test]
fn counts_wrong_codes_and_stops_at_the_fifth() {
    let host = TestHost::start(HostConfig::default()).unwrap();
    let device = Identity::generate().unwrap();
    let other = Identity::generate().unwrap();
    block(async {
        // A wrong typed code (valid check digit, another code) and wrong QR secrets.
        let wrong_code = if host.pairing_code() == "482913557208" { "000000000000" } else { "482913557208" };
        let r = pair_request(&host, &device, code_request(&host, &device, wrong_code)).await.unwrap();
        assert_eq!(r, json!({"ok": false, "reason": "code", "triesLeft": 4}));
        // A later generation's request is "closed" and costs no try; so is an unknown `v`'s ask.
        let mut later = qr_request(&host, &device, &[1; 16]);
        later["v"] = json!(2);
        assert_eq!(pair_request(&host, &device, later).await.unwrap(), json!({"ok": false, "reason": "closed"}));
        // A proof made for another device's key is wrong, an unknown method too.
        let secret = host.pairing_secret().unwrap();
        let r = pair_request(&host, &device, qr_request(&host, &other, &secret)).await.unwrap();
        assert_eq!(r, json!({"ok": false, "reason": "code", "triesLeft": 3}));
        let mut unknown = qr_request(&host, &device, &secret);
        unknown["method"] = json!("icloud");
        assert_eq!(pair_request(&host, &device, unknown).await.unwrap()["triesLeft"], json!(2));
        assert_eq!(pair_request(&host, &device, qr_request(&host, &device, &[3; 16])).await.unwrap()["triesLeft"], json!(1));
        // The fifth stops the window; after it even the right proof is "closed".
        let r = pair_request(&host, &device, qr_request(&host, &device, &[4; 16])).await.unwrap();
        assert_eq!(r, json!({"ok": false, "reason": "stopped"}));
        let r = pair_request(&host, &device, qr_request(&host, &device, &secret)).await.unwrap();
        assert_eq!(r, json!({"ok": false, "reason": "closed"}));
        assert!(host.paired_devices().is_empty());
        assert_eq!(host.pairing_code(), "");

        // A kind 19 that does not decode, or another kind first: closed without an answer.
        let mut s = dial(&host, &device, PAIRING_ALPN, None).await.unwrap();
        s.write_all(&wire::message(kind::PAIR_REQUEST, wire::now(), false, br#"{"v":1,"method":"qr"}"#)).await.unwrap();
        let mut rest = Vec::new();
        assert_eq!(timeout(WAIT, s.read_to_end(&mut rest)).await.unwrap().unwrap_or(0), 0);

        // A new window on the Mac: the right proof pairs.
        let code = host.new_pairing_code();
        assert_eq!(pairing::check_code(&code), Some(code.clone()));
        pair(&host, &device).await;
    });
    let answers: Vec<String> = host
        .events()
        .into_iter()
        .filter_map(|e| match e {
            DeviceEvent::PairRequest { answer, .. } => Some(answer),
            _ => None,
        })
        .collect();
    assert_eq!(answers, ["code", "closed", "code", "code", "code", "stopped", "closed", "ok"]);
    assert!(host.events().iter().any(|e| matches!(e, DeviceEvent::PairingAbandoned { .. })));
}

#[test]
fn spaces_out_proofs_when_asked_to() {
    let host = TestHost::start(HostConfig { pairing_spacing: true, ..HostConfig::default() }).unwrap();
    let device = Identity::generate().unwrap();
    block(async {
        let r = pair_request(&host, &device, qr_request(&host, &device, &[1; 16])).await.unwrap();
        assert_eq!(r["triesLeft"], json!(4));
        let secret = host.pairing_secret().unwrap();
        let r = pair_request(&host, &device, qr_request(&host, &device, &secret)).await.unwrap();
        assert_eq!(r["reason"], json!("busy"));
        let after = r["retryAfter"].as_f64().unwrap();
        assert!(after > 0.0 && after <= 1.0, "{after}");
    });
}

#[test]
fn asks_and_cancels_at_the_home_door() {
    let host = TestHost::start(HostConfig { pairing_open: false, ..HostConfig::default() }).unwrap();
    let device = Identity::generate().unwrap();
    block(async {
        assert_eq!(host.pairing_code(), "");
        let ask = json!({"v": 1, "method": "ask", "proof": "", "name": "Test iPad", "model": "iPad14,1"});
        // An ask needs no window: it opens one, "shown"; a second ask finds it open.
        assert_eq!(pair_request(&host, &device, ask.clone()).await.unwrap(), json!({"ok": false, "reason": "shown"}));
        let code = host.pairing_code();
        assert_eq!(code.len(), 12);
        assert_eq!(pair_request(&host, &device, ask.clone()).await.unwrap()["reason"], json!("shown"));
        assert_eq!(host.pairing_code(), code);
        // The asker's cancel withdraws it ("closed" whatever happened); a fresh one opens.
        let cancel = json!({"v": 1, "method": "cancel", "proof": "", "name": "Test iPad", "model": null});
        assert_eq!(pair_request(&host, &device, cancel).await.unwrap(), json!({"ok": false, "reason": "closed"}));
        assert_ne!(host.pairing_code(), code);
        // Locked: "locked".
        host.set_locked(true, false);
        assert_eq!(pair_request(&host, &device, ask).await.unwrap()["reason"], json!("locked"));
    });
}

#[test]
fn the_remote_door_takes_pairing_only_with_a_window_and_says_remote_off() {
    let config = HostConfig { door: Door::Remote, remote_access: false, ..HostConfig::default() };
    let host = TestHost::start(config).unwrap();
    let device = Identity::generate().unwrap();
    block(async {
        let ask = json!({"v": 1, "method": "ask", "proof": "", "name": "Test iPad", "model": null});
        assert_eq!(pair_request(&host, &device, ask.clone()).await.unwrap(), json!({"ok": false, "reason": "closed"}));
        pair(&host, &device).await;
        // Paired, Remote Access off: goodbye "remoteOff", then closed.
        let mut s = Session::open(&host, &device).await;
        s.send_json(kind::HELLO, json!({})).await;
        let (h, p) = s.next().await;
        assert_eq!(h.kind, kind::GOODBYE);
        assert_eq!(serde_json::from_slice::<Value>(&p).unwrap(), json!({"reason": "remoteOff"}));
        // No window: `sill-pair/1` fails its handshake.
        host.set_pairing_open(false);
        assert!(pair_request(&host, &device, ask).await.is_none());
    });
}

#[test]
fn the_device_gate_says_update() {
    let config = HostConfig { minimum_device_version: Some("1.2".into()), require_pairing: false, ..HostConfig::default() };
    let host = TestHost::start(config).unwrap();
    let device = Identity::generate().unwrap();
    block(async {
        // Too old: kind 22 "update" first and alone, never the catalog.
        let mut s = Session::open(&host, &device).await;
        s.send_json(kind::HELLO, json!({"appVersion": "1.1.9", "device": "iPad (iPad14,1)"})).await;
        let (h, p) = s.next().await;
        assert_eq!(h.kind, kind::GOODBYE);
        assert_eq!(
            serde_json::from_slice::<Value>(&p).unwrap(),
            json!({"reason": "update", "minimumVersion": "1.2", "reconnect": false,
                   "message": "Update Sill on your iPad to keep using Test Mac. It needs version 1.2 or later."})
        );
        // Another message first (a device from before the hello): refused the same way.
        let mut s = Session::open(&host, &device).await;
        s.send_json(kind::SELECT_SOURCE, json!({"desktop": {}})).await;
        let (h, _) = s.next().await;
        assert_eq!(h.kind, kind::GOODBYE);
        // New enough: served, unpaired at an open door.
        let mut s = Session::open(&host, &device).await;
        s.send_json(kind::HELLO, json!({"appVersion": "v1.2.0"})).await;
        let (h, _) = s.next().await;
        assert_eq!(h.kind, kind::WINDOW_LIST);
        let state = s.until_json(kind::HOST_SETTINGS, |_| true).await;
        assert_eq!(state["sessionPaired"], json!(false));
    });
    let refused = host.events().iter().filter(|e| matches!(e, DeviceEvent::SessionRefused { reason, .. } if reason == "update")).count();
    assert_eq!(refused, 2);
}

#[test]
fn clipboard_menus_and_the_lock() {
    let host = TestHost::start(HostConfig::default()).unwrap();
    let (a, b) = (Identity::generate().unwrap(), Identity::generate().unwrap());
    block(async {
        pair(&host, &a).await;
        pair(&host, &b).await;
        let mut sa = Session::open(&host, &a).await;
        let mut sb = Session::open(&host, &b).await;
        sa.send_json(kind::HELLO, json!({})).await;
        sb.send_json(kind::HELLO, json!({})).await;
        sa.catalog().await;
        sb.catalog().await;

        // The clipboard: `{}` subscribes; what the Mac held before is never sent.
        host.set_clipboard_text("before");
        sa.send_json(kind::CLIPBOARD, json!({})).await;
        sb.send_json(kind::CLIPBOARD, json!({})).await;
        // Both subscriptions in before the Mac copies: the two connections are read independently.
        assert!(host.wait_for(|e| matches!(e, DeviceEvent::Clipboard { conn: 3, text: None, stop: false }), WAIT).is_some());
        assert!(host.wait_for(|e| matches!(e, DeviceEvent::Clipboard { conn: 4, text: None, stop: false }), WAIT).is_some());
        host.set_clipboard_text("copied on the Mac");
        assert_eq!(sa.until_json(kind::CLIPBOARD, |_| true).await, json!({"text": "copied on the Mac"}));
        assert_eq!(sb.until_json(kind::CLIPBOARD, |_| true).await, json!({"text": "copied on the Mac"}));
        // A device's text becomes the Mac's and goes to the other subscriber only; the
        // 16-byte ping behind it is the sender's receipt.
        sa.send_json(kind::CLIPBOARD, json!({"text": "from A"})).await;
        sa.send(kind::PING, &[7u8; 16]).await;
        assert_eq!(sb.until_json(kind::CLIPBOARD, |_| true).await, json!({"text": "from A"}));
        let (_, receipt) = sa.until(kind::PONG).await;
        assert_eq!(receipt, [7u8; 16]);
        assert!(!sa.skipped.contains(&kind::CLIPBOARD));
        assert_eq!(host.clipboard_text().as_deref(), Some("from A"));
        // Stop: nothing more to B.
        sb.send_json(kind::CLIPBOARD, json!({"stop": true})).await;
        assert!(host.wait_for(|e| matches!(e, DeviceEvent::Clipboard { conn: 4, stop: true, .. }), WAIT).is_some());
        host.set_clipboard_text("only A hears this");
        assert_eq!(sa.until_json(kind::CLIPBOARD, |_| true).await["text"], json!("only A hears this"));

        // The menus: a kind 27 without an id subscribes and brings the top level (empty
        // while nothing streams); a pick moves the version and sends it again.
        sa.send_json(kind::FETCH_MENU, json!({"token": 1})).await;
        let top = sa.until_json(kind::MAC_MENU, |_| true).await;
        assert_eq!(top, json!({"version": 1, "menus": [], "answering": 1}));
        sa.send_json(kind::SELECT_SOURCE, json!({"window": {"_0": 101}})).await;
        let top = sa.until_json(kind::MAC_MENU, |_| true).await;
        assert_eq!((&top["version"], &top["app"], &top["bundleID"]), (&json!(2), &json!("TextEdit"), &json!("com.apple.TextEdit")));
        assert_eq!(top["menus"][1], json!({"id": "2", "title": "File", "submenu": true}));
        assert!(top.get("answering").is_none());
        sa.send_json(kind::FETCH_MENU, json!({"version": 2, "id": "2", "title": "File", "token": 2})).await;
        let file = sa.until_json(kind::MAC_MENU, |v| v["answering"] == json!(2)).await;
        assert_eq!(file["menu"], json!("2"));
        assert_eq!(file["items"][0], json!({"id": "2.0", "title": "New", "key": "⌘N"}));
        assert_eq!(file["items"][2], json!({"separator": true}));
        assert_eq!(file["items"][4], json!({"id": "2.4", "title": "Save…", "key": "⌘S", "enabled": false}));
        // An old version: "The menus changed…", nothing read.
        sa.send_json(kind::FETCH_MENU, json!({"version": 1, "id": "2", "title": "File", "token": 3})).await;
        let stale = sa.until_json(kind::MAC_MENU, |v| v["answering"] == json!(3)).await;
        assert_eq!((&stale["items"], &stale["note"]), (&json!([]), &json!("The menus changed. Open the menu again.")));
        sa.send_json(kind::PRESS_MENU_ITEM, json!({"version": 2, "id": "2.0", "title": "New", "token": 4})).await;
        assert_eq!(sa.until_json(kind::MAC_MENU, |v| v["answering"] == json!(4)).await, json!({"version": 2, "answering": 4, "pressed": true}));
        sa.send_json(kind::PRESS_MENU_ITEM, json!({"version": 2, "id": "2.4", "title": "Save…", "token": 5})).await;
        assert_eq!(sa.until_json(kind::MAC_MENU, |v| v["answering"] == json!(5)).await["pressed"], json!(false));
        // B never subscribed: its fetch is "The menus changed…".
        sb.send_json(kind::FETCH_MENU, json!({"version": 2, "id": "2", "title": "File", "token": 1})).await;
        assert_eq!(sb.until_json(kind::MAC_MENU, |_| true).await["note"], json!("The menus changed. Open the menu again."));

        // The lock: the list loses its windows and `clipboard`, the window gives way to
        // the Desktop; a window pick is refused; the unlock brings the window back.
        host.set_locked(true, true);
        let locked = sb.until_json(kind::WINDOW_LIST, |v| v.get("locked").is_some()).await;
        assert_eq!((&locked["locked"], &locked["offConsole"], &locked["windows"]), (&json!(true), &json!(true), &json!([])));
        assert_eq!(locked["active"], json!({"desktop": {}}));
        assert!(locked.get("clipboard").is_none());
        sb.send_json(kind::SELECT_SOURCE, json!({"window": {"_0": 102}})).await;
        assert!(host.wait_for(|e| matches!(e, DeviceEvent::Refused { conn: 4, kind: 6, .. }), WAIT).is_some());
        host.set_locked(false, false);
        let unlocked = sb.until_json(kind::WINDOW_LIST, |v| v.get("locked").is_none()).await;
        assert_eq!(unlocked["active"], json!({"window": {"_0": 101}}));
        assert_eq!(unlocked["windows"].as_array().map(Vec::len), Some(3));

        // A window closed from the device leaves the list; streaming, nothing streams.
        sb.send_json(kind::WINDOW_COMMAND, json!({"id": 101, "action": "close"})).await;
        let list = sb.until_json(kind::WINDOW_LIST, |v| v["windows"].as_array().map(Vec::len) == Some(2)).await;
        assert_eq!(list["active"], json!({"none": {}}));
        // An app launched gets a window, which is picked.
        sb.send_json(kind::LAUNCH_APP, json!({"bundleID": "com.apple.Notes"})).await;
        let list = sb.until_json(kind::WINDOW_LIST, |v| v["active"] == json!({"window": {"_0": 104}})).await;
        assert_eq!(list["windows"][2]["appName"], json!("Notes"));

        // Remove on the Mac: "removed" to both, and the next dial fails its handshake.
        host.forget_devices();
        assert_eq!(sa.until_json(kind::GOODBYE, |_| true).await, json!({"reason": "removed"}));
        assert_eq!(sb.until_json(kind::GOODBYE, |_| true).await, json!({"reason": "removed"}));
    });
    assert!(host.wait_for(|e| matches!(e, DeviceEvent::SessionEnded { conn: 4 }), WAIT).is_some());
    block(async {
        if let Ok(mut s) = dial(&host, &a, SESSION_ALPN, Some(host.fingerprint())).await {
            assert!(timeout(WAIT, read_message(&mut s)).await.unwrap().is_err());
        }
    });
}

#[test]
fn sends_the_tone_only_to_a_device_that_plays_it() {
    let host = TestHost::start(HostConfig { send_audio: true, require_pairing: false, ..HostConfig::default() }).unwrap();
    let (a, b) = (Identity::generate().unwrap(), Identity::generate().unwrap());
    block(async {
        let mut plays = Session::open(&host, &a).await;
        plays.send_json(kind::HELLO, json!({"audio": ["opus", "aac-eld"]})).await;
        let mut silent = Session::open(&host, &b).await;
        silent.send_json(kind::HELLO, json!({})).await;
        assert!(host.wait_for(|e| matches!(e, DeviceEvent::Hello { conn: 2, .. }), WAIT).is_some());
        plays.send_json(kind::SELECT_SOURCE, json!({"desktop": {}})).await;
        // The format first (type 1, JSON), then packets (type 2) numbered from 0.
        let (_, format) = plays.until(kind::AUDIO).await;
        assert_eq!(format[0], 1);
        let format: Value = serde_json::from_slice(&format[1..]).unwrap();
        assert_eq!((&format["codec"], &format["sampleRate"], &format["channels"]), (&json!("aac-eld"), &json!(48000), &json!(2)));
        assert_eq!((&format["framesPerPacket"], &format["primingFrames"], &format["source"]), (&json!(512), &json!(256), &json!("test")));
        assert_eq!(STANDARD.decode(format["cookie"].as_str().unwrap()).unwrap().len(), 41);
        for seq in 0..3u32 {
            let (_, p) = plays.until(kind::AUDIO).await;
            assert_eq!(p[0], 2);
            assert_eq!(u16::from_be_bytes([p[1], p[2]]), format["epoch"].as_u64().unwrap() as u16);
            assert_eq!(u32::from_be_bytes(p[3..7].try_into().unwrap()), seq);
            assert_eq!((p[7], p[8]), (u8::from(seq == 0), 1));
            assert_eq!(u16::from_be_bytes([p[9], p[10]]) as usize, p.len() - 11);
        }
        // The other device gets the picture and no sound.
        for _ in 0..40 {
            let (h, _) = silent.next().await;
            assert_ne!(h.kind, kind::AUDIO);
        }
        // Send Audio off from a device: the sound stops for every device.
        silent.send_json(kind::CHANGE_SETTINGS, json!({"token": 1, "sendAudio": false})).await;
        silent.until_json(kind::HOST_SETTINGS, |v| v["answering"] == json!(1)).await;
        let off = plays.until_json(kind::HOST_SETTINGS, |v| v["settings"]["sendAudio"] == json!(false)).await;
        assert!(off.get("answering").is_none());
    });
}

#[test]
fn keeps_its_identity_and_trust_list_in_a_directory() {
    let dir = std::env::temp_dir().join(format!("sill-testhost-{}", crate::identity::uuid()));
    let config = HostConfig { dir: Some(dir.clone()), code: Some("482913557208".into()), ..HostConfig::default() };
    let device = Identity::generate().unwrap();
    let first = TestHost::start(config.clone()).unwrap();
    let (fingerprint, key) = (first.fingerprint(), first.recognition_key());
    block(pair(&first, &device));
    // A fixed code: the window that reopens after a pairing has it again.
    assert_eq!(first.pairing_code(), "482913557208");
    first.stop();
    let second = TestHost::start(config).unwrap();
    assert_eq!((second.fingerprint(), second.recognition_key()), (fingerprint, key));
    assert_eq!(second.paired_devices().len(), 1);
    block(async {
        let mut s = Session::open(&second, &device).await;
        assert_eq!(s.next().await.0.kind, kind::WINDOW_LIST);
    });
    // Another host is another Mac.
    assert_ne!(TestHost::start(HostConfig::default()).unwrap().fingerprint(), fingerprint);
    drop(second);
    let _ = std::fs::remove_dir_all(dir);
}

/// Not a check: writes what a device receives as Annex B, for a decoder outside to try.
#[test]
#[ignore]
fn dump_stream_for_a_decoder() {
    let Ok(path) = std::env::var("SILL_TESTHOST_DUMP") else { return };
    let host = TestHost::start(HostConfig { require_pairing: false, ..HostConfig::default() }).unwrap();
    let device = Identity::generate().unwrap();
    let mut out = Vec::new();
    block(async {
        let mut s = Session::open(&host, &device).await;
        s.send_json(kind::SELECT_SOURCE, json!({"window": {"_0": 102}})).await;
        let mut frames = 0;
        while frames < 150 {
            let (h, p) = s.next().await;
            if h.kind == kind::PARAMETER_SETS {
                let mut at = 2;
                for _ in 0..p[1] {
                    let n = u16::from_be_bytes([p[at], p[at + 1]]) as usize;
                    out.extend_from_slice(&[0, 0, 0, 1]);
                    out.extend_from_slice(&p[at + 2..at + 2 + n]);
                    at += 2 + n;
                }
            } else if h.kind == kind::FRAME {
                frames += 1;
                let mut at = 0;
                while at < p.len() {
                    let n = u32::from_be_bytes(p[at..at + 4].try_into().unwrap()) as usize;
                    out.extend_from_slice(&[0, 0, 0, 1]);
                    out.extend_from_slice(&p[at + 4..at + 4 + n]);
                    at += 4 + n;
                }
            }
        }
    });
    std::fs::write(path, out).unwrap();
}

#[test]
fn an_open_door_serves_any_key_ticks_after_input_and_shows_a_code_when_asked() {
    let config = HostConfig { require_pairing: false, pairing_open: false, ..HostConfig::default() };
    let host = TestHost::start(config).unwrap();
    let device = Identity::generate().unwrap();
    block(async {
        let mut s = Session::open(&host, &device).await;
        s.send_json(kind::HELLO, json!({})).await;
        s.catalog().await;
        // Nothing streams and nothing was typed: no tick. Input makes the session live.
        s.send_json(kind::INPUT, json!({"scroll": {"x": 0.5, "y": 0.5, "dx": 0.0, "dy": -0.1}})).await;
        let (h, p) = s.until(kind::TICK).await;
        assert!(p.is_empty() && !h.keyframe);
        assert!(s.skipped.is_empty(), "{:?}", s.skipped);
        // Kind 21 from an unpaired session: a window opens for it.
        assert_eq!(host.pairing_code(), "");
        s.send(kind::PAIRING_WANTED, &[]).await;
        assert!(host.wait_for(|e| matches!(e, DeviceEvent::PairingWanted { .. }), WAIT).is_some());
        assert_eq!(host.pairing_code().len(), 12);
        // A viewport out of bounds is refused whole; a kind nobody knows is skipped.
        s.send_json(kind::VIEWPORT, json!({"width": 1e300, "height": 600})).await;
        s.send(200, b"?").await;
        assert!(host.wait_for(|e| matches!(e, DeviceEvent::Unknown { kind: 200, len: 1, .. }), WAIT).is_some());
        assert!(host.events().iter().any(|e| matches!(e, DeviceEvent::Viewport { refused: Some(_), .. })));
        assert_eq!(host.last_viewport(), None);
        // A message announcing more than 1 MiB closes the session.
        let mut big = wire::message(kind::INPUT, wire::now(), false, &[]);
        big[10..14].copy_from_slice(&((1u32 << 20) + 1).to_be_bytes());
        s.s.write_all(&big).await.unwrap();
        let mut rest = Vec::new();
        let _ = timeout(WAIT, s.s.read_to_end(&mut rest)).await.expect("the host closes it");
    });
    assert!(host.wait_for(|e| matches!(e, DeviceEvent::SessionEnded { .. }), WAIT).is_some());
}
