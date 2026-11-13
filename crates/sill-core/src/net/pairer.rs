//! Pairing at the Mac's doors: one connection with ALPN `sill-pair/1`, one kind 19 out, one kind
//! 20 back, then the Mac closes it. By the QR link the Mac is pinned to the link's key before a
//! byte is sent; by the typed code nothing is pinned, and the proofs bind both keys (a relay in
//! the middle makes this device's proof name the relay's key).

use std::time::Duration;

use tokio::io::AsyncWriteExt;

use super::home_discovery::HomeMac;
use super::saved_macs::{Endpoint, SavedMac, SavedMacs};
use super::tls::{self, DialError, TlsConnection, DIAL_OVERALL, DIAL_TIMEOUT, PAIRING_ALPN};
use crate::crypto::{base64url, mac_id, pairing_proof, safe_text, DeviceIdentity, PairLink};
use crate::protocol::{self, kind, payloads, MessageReader, PairResult};

/// How a pairing ended.
#[derive(Debug, Clone, PartialEq)]
#[allow(clippy::large_enum_variant)]
pub enum PairOutcome {
    /// Saved; `endpoint` is the address that answered.
    Paired { mac: SavedMac, endpoint: Endpoint },
    /// Why not, in words to show.
    Failed(String),
}

/// How a home door's "pair me" ended.
#[derive(Debug, Clone, PartialEq)]
pub enum AskOutcome {
    /// The Mac shows a code now: type it (pinned to this key).
    Shown { mac_fingerprint: [u8; 32] },
    Refused(String),
}

/// Who pairs: this device's key and what it calls itself.
pub struct PairingDevice<'a> {
    pub identity: &'a DeviceIdentity,
    pub name: &'a str,
    pub model: &'a str,
    /// Whether a loopback address is worth saving (tests against a host on the same computer).
    pub allow_loopback: bool,
}

const ANSWER_TIMEOUT: Duration = Duration::from_secs(15);

/// By the typed code. `pin`: after an ask at a home door, the key that ask saw (the code the Mac
/// shows is for that connection's Mac); else none, and the proofs bind both keys.
pub async fn by_code(device: &PairingDevice<'_>, endpoint: &Endpoint, code: &str, store: &SavedMacs, pin: Option<&[u8]>) -> PairOutcome {
    let conn = match tls::dial(device.identity, &endpoint.host, endpoint.port, PAIRING_ALPN, pin, DIAL_TIMEOUT).await {
        Ok(c) => c,
        Err(e) => return PairOutcome::Failed(e.message),
    };
    // PBKDF2's 600,000 rounds are a fraction of a second of pure computing: off the async workers.
    let (code_owned, fp) = (code.to_string(), conn.mac_fingerprint);
    let key = match tokio::task::spawn_blocking(move || pairing_proof::code_key(&code_owned, &fp)).await {
        Ok(k) => k,
        Err(e) => return PairOutcome::Failed(format!("Pairing failed: {e}")),
    };
    // A home door's address changes with the network: only a remote one is worth typing again.
    let typed = if pin.is_none() { vec![endpoint.to_string()] } else { Vec::new() };
    exchange(device, conn, &key, "code", endpoint.clone(), store, typed, None).await
}

/// The home door's "pair me" (kind 19 "ask"): the Mac opens its pairing window by itself and
/// answers "shown", or says why not. Taking any key; the code then goes only to the key this ask
/// saw. `cancel`: the device's Cancel, which closes the window the ask opened.
pub async fn ask(device: &PairingDevice<'_>, found: &HomeMac, cancel: bool, pin: Option<&[u8]>) -> AskOutcome {
    let name = &found.service_name;
    let mut conn = match tls::dial(device.identity, &found.host, found.port, PAIRING_ALPN, pin, DIAL_TIMEOUT).await {
        Ok(c) => c,
        Err(e) => return AskOutcome::Refused(e.message),
    };
    let req = payloads::pair_request(if cancel { "cancel" } else { "ask" }, "", device.name, device.model);
    if let Err(e) = send(&mut conn, &req).await {
        return AskOutcome::Refused(format!("Pairing failed: {e}"));
    }
    let Some(msg) = answer(&mut conn).await else {
        return AskOutcome::Refused(format!("{name} closed the connection without answering."));
    };
    let _ = conn.stream.shutdown().await;
    let Some(r) = PairResult::parse(&msg.payload) else {
        return AskOutcome::Refused(format!("{name}’s answer couldn’t be read."));
    };
    match r.reason.as_deref() {
        Some("shown") => AskOutcome::Shown { mac_fingerprint: conn.mac_fingerprint },
        Some("openOnMac") => AskOutcome::Refused(format!("In Sill on {name}, choose Pair iPhone or iPad…, then tap it again.")),
        Some("locked") => AskOutcome::Refused(format!("Unlock {name}, then tap it again.")),
        Some("busy") => AskOutcome::Refused(format!("{name} is pairing another device. Try again in a minute.")),
        Some("closed") => AskOutcome::Refused(if cancel { "Cancelled.".to_string() } else { format!("{name} isn’t taking pairing requests right now.") }),
        other => AskOutcome::Refused(
            r.message
                .as_deref()
                .map(|m| safe_text::label(m, 300))
                .unwrap_or_else(|| format!("{name} didn’t show a code ({}).", other.unwrap_or("null"))),
        ),
    }
}

/// By a `sill://pair` link. `also`: an address typed beside it, dialed first.
pub async fn by_link(device: &PairingDevice<'_>, link: &PairLink, store: &SavedMacs, also: Option<Endpoint>) -> PairOutcome {
    let mut endpoints: Vec<Endpoint> = Vec::new();
    for e in also.into_iter().chain(link.addresses.iter().filter_map(|a| Endpoint::parse(a, link.port))) {
        if !endpoints.contains(&e) {
            endpoints.push(e);
        }
    }
    if endpoints.is_empty() {
        return PairOutcome::Failed("The pairing link names no address. Type the Mac’s address instead.".to_string());
    }
    let conn = match tls::dial_any(device.identity, &endpoints, PAIRING_ALPN, Some(&link.fingerprint), DIAL_OVERALL).await {
        Ok(c) => c,
        Err(DialError { message, .. }) => return PairOutcome::Failed(message),
    };
    let endpoint = Endpoint::new(conn.host.clone(), conn.port);
    // The link's addresses are the Mac's own list: keep them to dial until its kind 18 says more,
    // and its port, the Mac's remote port.
    let typed = endpoints.iter().map(Endpoint::to_string).collect();
    exchange(device, conn, &link.secret, "qr", endpoint, store, typed, Some(link.port)).await
}

async fn send(conn: &mut TlsConnection, json: &str) -> std::io::Result<()> {
    conn.stream.write_all(&protocol::encode_json(kind::PAIR_REQUEST, json)).await?;
    conn.stream.flush().await
}

async fn answer(conn: &mut TlsConnection) -> Option<protocol::Message> {
    let mut reader = MessageReader::new(&mut conn.stream, || {});
    tokio::time::timeout(ANSWER_TIMEOUT, reader.next()).await.ok()?.ok()
}

#[allow(clippy::too_many_arguments)]
async fn exchange(
    device: &PairingDevice<'_>, mut conn: TlsConnection, key: &[u8], method: &str, endpoint: Endpoint, store: &SavedMacs,
    typed: Vec<String>, link_port: Option<u16>,
) -> PairOutcome {
    let failed = |m: &str| PairOutcome::Failed(m.to_string());
    let device_fp = device.identity.fingerprint();
    let proof = pairing_proof::device_proof(key, device_fp, &conn.mac_fingerprint);
    let req = payloads::pair_request(method, &base64url::encode(&proof), device.name, device.model);
    if let Err(e) = send(&mut conn, &req).await {
        return PairOutcome::Failed(format!("Pairing failed: {e}"));
    }
    let Some(msg) = answer(&mut conn).await else {
        return failed("The Mac closed the connection without answering.");
    };
    let _ = conn.stream.shutdown().await;
    if msg.kind != kind::PAIR_RESULT || msg.payload.len() > protocol::MAX_PAIRING_PAYLOAD {
        return failed("The Mac answered something other than a pairing result.");
    }
    let Some(r) = PairResult::parse(&msg.payload) else { return failed("The Mac’s answer couldn’t be read.") };
    if !r.ok {
        return PairOutcome::Failed(refusal(&r));
    }
    let mac_proof = r.proof.as_deref().and_then(base64url::decode);
    if !mac_proof.map(|p| pairing_proof::is_valid_mac_proof(&p, key, &conn.mac_fingerprint, device_fp)).unwrap_or(false) {
        return failed("The Mac’s answer didn’t prove it knows the code. Nothing was saved.");
    }
    let id = mac_id::make(&conn.mac_fingerprint);
    if r.mac_id.as_deref() != Some(id.as_str()) {
        return failed("The Mac’s answer named another key. Nothing was saved.");
    }
    let existing = store.by_id(&id);
    // What this pairing dialed first (the address that answered, then the link's or the typed
    // one), then what an earlier pairing kept: never a link-local or loopback address, which
    // names nothing from elsewhere.
    let mut keep: Vec<String> = Vec::new();
    let candidates = std::iter::once(endpoint.to_string())
        .chain(typed)
        .chain(existing.iter().flat_map(|e| e.typed_addresses.iter().cloned()));
    for a in candidates {
        let keepable = Endpoint::parse(&a, endpoint.port).map(|e| SavedMacs::keepable(&e.host, device.allow_loopback)).unwrap_or(false);
        if keepable && !keep.contains(&a) {
            keep.push(a);
        }
    }
    keep.truncate(8);
    // The link's port is the Mac's remote port until a verified kind 18 says otherwise.
    let port = existing
        .as_ref()
        .filter(|e| e.info_issued_at > 0.0)
        .map(|e| e.remote_port)
        .or(link_port.filter(|p| *p >= 1))
        .or(existing.as_ref().map(|e| e.remote_port))
        .unwrap_or(endpoint.port);
    let mut name = safe_text::label(r.name.as_deref().unwrap_or(""), 64);
    if name.is_empty() {
        name = existing.as_ref().map(|e| e.name.clone()).unwrap_or_else(|| "Mac".to_string());
    }
    let mac = SavedMac {
        mac_id: id,
        fingerprint: conn.mac_fingerprint.to_vec(),
        name,
        recognition_key: r.recognition_key.clone(),
        remote_port: port,
        addresses: existing.as_ref().map(|e| e.addresses.clone()).unwrap_or_default(),
        typed_addresses: keep,
        info_issued_at: existing.as_ref().map(|e| e.info_issued_at).unwrap_or(0.0),
        last_worked: Some(endpoint.key()),
        paired_at: protocol::now(),
        last_connected_at: existing.as_ref().and_then(|e| e.last_connected_at),
        revoked: false,
    };
    store.upsert(mac.clone());
    PairOutcome::Paired { mac, endpoint }
}

/// Kind 20's refusals in this device's words (`PairResult.knownReasons`), else the Mac's own.
pub fn refusal(r: &PairResult) -> String {
    match r.reason.as_deref() {
        Some("code") => format!(
            "That code didn’t match.{}",
            r.tries_left.map(|n| format!(" {n} {} left.", if n == 1 { "try" } else { "tries" })).unwrap_or_default()
        ),
        Some("closed") => "The Mac isn’t showing a pairing code. In Sill on the Mac, choose Pair iPhone or iPad…".to_string(),
        Some("expired") => "That code expired. Choose New Code on the Mac.".to_string(),
        Some("stopped") => "The Mac stopped pairing after too many wrong codes. Open its pairing window again.".to_string(),
        Some("busy") => format!(
            "The Mac is busy.{}",
            r.retry_after.map(|s| format!(" Try again in {} s.", (s as i64).max(1))).unwrap_or_else(|| " Try again shortly.".to_string())
        ),
        Some("locked") => "Unlock the Mac, then try again.".to_string(),
        Some("shown") | Some("openOnMac") => "Choose Pair iPhone or iPad… in Sill on the Mac, then enter its code.".to_string(),
        other => r
            .message
            .as_deref()
            .map(|m| safe_text::label(m, 300))
            .filter(|m| !m.is_empty())
            .unwrap_or_else(|| format!("The Mac refused ({}).", other.unwrap_or("no reason"))),
    }
}
