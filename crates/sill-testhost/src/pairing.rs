//! Pairing: the typed code, the proofs, the Mac ID, the Bonjour recognition tag and the
//! pairing window's rules.
//!
//! Read from `Sources/StreamProtocol/Pairing.swift`, `RemoteIdentity.swift` (`Base64URL`,
//! `MacID`) and `Sources/SillHost/PairingWindow.swift`; the unit tests below use the
//! vectors of `Tests/checks/addresses/main.swift` and `docs/remote-access-plan.md`.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ring::rand::SecureRandom;
use ring::{digest, hmac, pbkdf2};
use std::collections::HashMap;
use std::num::NonZeroU32;

/// Damm's quasigroup of order 10 (`PairingCode.table`).
const DAMM: [[u8; 10]; 10] = [
    [0, 3, 1, 7, 5, 9, 8, 6, 4, 2],
    [7, 0, 9, 2, 1, 5, 4, 8, 6, 3],
    [4, 2, 0, 6, 8, 7, 1, 3, 5, 9],
    [1, 7, 5, 0, 9, 8, 3, 4, 2, 6],
    [6, 1, 2, 3, 0, 4, 5, 9, 7, 8],
    [3, 6, 7, 4, 2, 0, 9, 5, 8, 1],
    [5, 8, 6, 9, 7, 2, 0, 1, 3, 4],
    [8, 9, 4, 5, 3, 6, 2, 0, 1, 7],
    [9, 4, 3, 8, 6, 1, 7, 2, 0, 5],
    [2, 5, 8, 1, 4, 3, 6, 7, 9, 0],
];

/// A pairing code's length: 11 random digits and one Damm check digit.
pub const CODE_LENGTH: usize = 12;
/// PBKDF2 rounds of the typed path (`PairingProof.rounds`).
pub const PBKDF2_ROUNDS: u32 = 600_000;
const LABEL: &str = "sill-pair-v1";

fn damm(digits: impl Iterator<Item = u8>) -> u8 {
    digits.fold(0u8, |interim, d| DAMM[interim as usize][d as usize])
}

/// A fresh code (`PairingCode.generate`): 11 uniformly random digits by rejection
/// sampling, then the check digit.
pub fn generate_code() -> String {
    let rng = ring::rand::SystemRandom::new();
    let mut digits: Vec<u8> = Vec::with_capacity(CODE_LENGTH);
    while digits.len() < CODE_LENGTH - 1 {
        let mut bytes = [0u8; 16];
        rng.fill(&mut bytes).expect("the system's random source");
        for b in bytes {
            if b < 250 && digits.len() < CODE_LENGTH - 1 {
                digits.push(b % 10);
            }
        }
    }
    let check = damm(digits.iter().copied());
    digits.push(check);
    digits.iter().map(|d| char::from(b'0' + d)).collect()
}

/// `PairingCode.check`: spaces, hyphens and no-break spaces removed, exactly 12 ASCII
/// digits, the Damm check valid. `None` for anything else.
pub fn check_code(input: &str) -> Option<String> {
    let kept: String = input.chars().filter(|c| !matches!(c, ' ' | '-' | '\u{00A0}')).collect();
    if kept.len() != CODE_LENGTH || !kept.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    (damm(kept.bytes().map(|b| b - b'0')) == 0).then_some(kept)
}

/// "4829 1355 7208" (`PairingCode.grouped`).
pub fn grouped(code: &str) -> String {
    code.as_bytes()
        .chunks(4)
        .map(|c| String::from_utf8_lossy(c).into_owned())
        .collect::<Vec<_>>()
        .join(" ")
}

/// The typed path's key: PBKDF2-HMAC-SHA256(the 12 ASCII digits, "sill-pair-v1" ‖ fp_mac,
/// 600,000 rounds, 32 bytes).
pub fn code_key(code: &str, mac_fingerprint: &[u8; 32]) -> [u8; 32] {
    let mut salt = LABEL.as_bytes().to_vec();
    salt.extend_from_slice(mac_fingerprint);
    let mut out = [0u8; 32];
    pbkdf2::derive(
        pbkdf2::PBKDF2_HMAC_SHA256,
        NonZeroU32::new(PBKDF2_ROUNDS).unwrap(),
        &salt,
        code.as_bytes(),
        &mut out,
    );
    out
}

fn proof_message(who: &str, first: &[u8; 32], second: &[u8; 32]) -> Vec<u8> {
    let mut m = format!("{LABEL} {who}").into_bytes();
    m.push(0);
    m.extend_from_slice(first);
    m.extend_from_slice(second);
    m
}

fn proof(key: &[u8], who: &str, first: &[u8; 32], second: &[u8; 32]) -> [u8; 32] {
    let tag = hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, key), &proof_message(who, first, second));
    tag.as_ref().try_into().unwrap()
}

/// proof_D = HMAC-SHA256(key, "sill-pair-v1 device" ‖ 0x00 ‖ fp_device ‖ fp_mac).
#[cfg_attr(not(test), allow(dead_code))]
pub fn device_proof(key: &[u8], fp_device: &[u8; 32], fp_mac: &[u8; 32]) -> [u8; 32] {
    proof(key, "device", fp_device, fp_mac)
}

/// proof_M = HMAC-SHA256(key, "sill-pair-v1 mac" ‖ 0x00 ‖ fp_mac ‖ fp_device).
pub fn mac_proof(key: &[u8], fp_mac: &[u8; 32], fp_device: &[u8; 32]) -> [u8; 32] {
    proof(key, "mac", fp_mac, fp_device)
}

/// The Mac's check of proof_D, in constant time (`PairingProof.isValidDeviceProof`).
pub fn is_valid_device_proof(received: &[u8], key: &[u8], fp_device: &[u8; 32], fp_mac: &[u8; 32]) -> bool {
    let key = hmac::Key::new(hmac::HMAC_SHA256, key);
    hmac::verify(&key, &proof_message("device", fp_device, fp_mac), received).is_ok()
}

/// base64url without padding (`Base64URL.encode`).
pub fn b64url(data: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(data)
}

/// `Base64URL.decode`: canonical unpadded base64url only.
pub fn b64url_decode(text: &str) -> Option<Vec<u8>> {
    let d = URL_SAFE_NO_PAD.decode(text).ok()?;
    (b64url(&d) == text).then_some(d)
}

/// The Mac ID (`MacID.make`): Crockford base32 of the fingerprint's first 10 bytes.
pub fn mac_id(fingerprint: &[u8; 32]) -> String {
    const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let mut out = String::with_capacity(16);
    let (mut buffer, mut bits) = (0u32, 0u32);
    for b in &fingerprint[..10] {
        buffer = (buffer << 8) | u32::from(*b);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(char::from(ALPHABET[((buffer >> bits) & 31) as usize]));
        }
        buffer &= (1 << bits) - 1;
    }
    out
}

/// The Bonjour TXT record's `r` (`RecognitionTag.make`): base64url of the 6-byte nonce
/// and the first 6 bytes of HMAC-SHA256(recognitionKey, "sill-tag-v1" ‖ nonce).
pub fn recognition_tag(recognition_key: &[u8; 32], nonce: Option<[u8; 6]>) -> String {
    let p = nonce.unwrap_or_else(|| {
        let mut p = [0u8; 6];
        ring::rand::SystemRandom::new().fill(&mut p).expect("the system's random source");
        p
    });
    let mut message = b"sill-tag-v1".to_vec();
    message.extend_from_slice(&p);
    let mac = hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, recognition_key), &message);
    let mut tag = p.to_vec();
    tag.extend_from_slice(&mac.as_ref()[..6]);
    b64url(&tag)
}

/// SHA-256.
pub fn sha256(data: &[u8]) -> [u8; 32] {
    digest::digest(&digest::SHA256, data).as_ref().try_into().unwrap()
}

/// Why a window is closed (`PairingWindow.CloseReason`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseReason {
    Used,
    Expired,
    Stopped,
    Cancelled,
    Withdrawn,
    None,
}

/// What a window made of one proof (`PairingWindow.Verdict`).
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    /// The proof holds: proof_M to send. The window is now closed (used).
    Accept([u8; 32]),
    /// A wrong proof; this many are still taken.
    Reject(u32),
    /// Not now: nothing counted.
    Busy(f64),
    /// No window, or it just closed.
    Closed(CloseReason),
}

/// An open window (`PairingWindow.Open`).
#[derive(Debug, Clone)]
pub struct Open {
    pub secret: [u8; 16],
    pub code: String,
    pub code_key: Option<[u8; 32]>,
    pub expires_at: f64,
    pub failures: u32,
    pub next_allowed_at: f64,
    pub last_attempt_by_source: HashMap<String, f64>,
    /// The key whose ask opened it; `None` when the host's user did.
    pub by_device: Option<[u8; 32]>,
}

/// One pairing window's rules (`PairingWindow`): open for `lifetime` seconds, for one
/// device, at most five wrong proofs, spaced out.
#[derive(Debug, Clone)]
pub enum Window {
    Closed(CloseReason),
    Open(Box<Open>),
}

/// `PairingWindow.maxFailures`.
pub const MAX_FAILURES: u32 = 5;
/// `PairingWindow.sourceSpacing`: the same source may try once per this many seconds.
pub const SOURCE_SPACING: f64 = 5.0;
/// `PairingWindow.spacing`: after the 1st to 4th wrong proof the next is taken this much later.
pub const SPACING: [f64; 4] = [1.0, 2.0, 4.0, 8.0];

impl Window {
    pub fn open(secret: [u8; 16], code: String, now: f64, lifetime: f64, by_device: Option<[u8; 32]>) -> Window {
        Window::Open(Box::new(Open {
            secret,
            code,
            code_key: None,
            expires_at: now + lifetime,
            failures: 0,
            next_allowed_at: 0.0,
            last_attempt_by_source: HashMap::new(),
            by_device,
        }))
    }

    pub fn current(&self) -> Option<&Open> {
        match self {
            Window::Open(w) => Some(w),
            Window::Closed(_) => None,
        }
    }

    pub fn is_open(&self) -> bool {
        self.current().is_some()
    }

    /// K for `code`, once derived; ignored when the window has moved on to another code.
    pub fn set_code_key(&mut self, key: [u8; 32], code: &str) {
        if let Window::Open(w) = self {
            if w.code == code {
                w.code_key = Some(key);
            }
        }
    }

    pub fn close(&mut self, reason: CloseReason) {
        if self.is_open() {
            *self = Window::Closed(reason);
        }
    }

    /// `PairingWindow.tryProof`. `spacing` false leaves out the two "busy" rules (after a
    /// wrong proof, and per source), a test host's shortcut.
    #[allow(clippy::too_many_arguments)]
    pub fn try_proof(
        &mut self,
        method: &str,
        proof: &[u8],
        fp_device: &[u8; 32],
        fp_mac: &[u8; 32],
        source: &str,
        now: f64,
        spacing: bool,
    ) -> Verdict {
        let w = match self {
            Window::Open(w) => w,
            Window::Closed(r) => return Verdict::Closed(*r),
        };
        if now >= w.expires_at {
            *self = Window::Closed(CloseReason::Expired);
            return Verdict::Closed(CloseReason::Expired);
        }
        if spacing {
            if now < w.next_allowed_at {
                return Verdict::Busy(w.next_allowed_at - now);
            }
            if let Some(last) = w.last_attempt_by_source.get(source) {
                if now - last < SOURCE_SPACING {
                    return Verdict::Busy(SOURCE_SPACING - (now - last));
                }
            }
        }
        let key: Option<Vec<u8>> = match method {
            "qr" => Some(w.secret.to_vec()),
            "code" => match w.code_key {
                Some(k) => Some(k.to_vec()),
                None => return Verdict::Busy(1.0),
            },
            _ => None, // an unknown method never matches: a wrong proof
        };
        w.last_attempt_by_source.insert(source.to_string(), now);
        if let Some(key) = key {
            if is_valid_device_proof(proof, &key, fp_device, fp_mac) {
                let m = mac_proof(&key, fp_mac, fp_device);
                *self = Window::Closed(CloseReason::Used);
                return Verdict::Accept(m);
            }
        }
        w.failures += 1;
        if w.failures >= MAX_FAILURES {
            *self = Window::Closed(CloseReason::Stopped);
            return Verdict::Closed(CloseReason::Stopped);
        }
        w.next_allowed_at = now + SPACING[(w.failures as usize - 1).min(SPACING.len() - 1)];
        Verdict::Reject(MAX_FAILURES - w.failures)
    }
}

/// Kind 20's reason for a proof the window turned away closed (`PairingWindow.closedReason`).
pub fn closed_reason(reason: CloseReason, stopped_by_this_proof: bool) -> &'static str {
    if reason == CloseReason::Stopped && stopped_by_this_proof {
        "stopped"
    } else if reason == CloseReason::Expired {
        "expired"
    } else {
        "closed"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn damm_codes() {
        assert_eq!(check_code("4829 1355 7208").as_deref(), Some("482913557208"));
        assert_eq!(check_code("482913557209"), None);
        assert_eq!(check_code("48291355720"), None);
        for _ in 0..200 {
            let c = generate_code();
            assert_eq!(check_code(&c), Some(c.clone()));
            // every single-digit substitution is caught
            let mut bytes = c.clone().into_bytes();
            bytes[3] = b'0' + (bytes[3] - b'0' + 1) % 10;
            assert_eq!(check_code(std::str::from_utf8(&bytes).unwrap()), None);
        }
        assert_eq!(grouped("482913557208"), "4829 1355 7208");
    }

    #[test]
    fn mac_id_vector() {
        let fp = sha256(b"example");
        assert_eq!(&fp[..10], &[0x50, 0xd8, 0x58, 0xe0, 0x98, 0x5e, 0xcc, 0x7f, 0x60, 0x41]);
        assert_eq!(mac_id(&fp), "A3C5HR4RBV67YR21");
    }

    #[test]
    fn recognition_tag_vector() {
        assert_eq!(recognition_tag(&[0x11; 32], Some([0xa1, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6])), "oaKjpKWmL4V47Ctm");
    }

    #[test]
    fn window_vectors() {
        let fp_mac = sha256(b"mac");
        let fp_dev = sha256(b"device");
        let secret = [0x22u8; 16];
        let qr_d = device_proof(&secret, &fp_dev, &fp_mac);
        let wrong = [7u8; 32];
        let code = "482913557208".to_string();

        let mut w = Window::Closed(CloseReason::None);
        assert_eq!(w.try_proof("qr", &qr_d, &fp_dev, &fp_mac, "a", 0.0, true), Verdict::Closed(CloseReason::None));
        w = Window::open(secret, code.clone(), 100.0, 300.0, None);
        match w.try_proof("qr", &qr_d, &fp_dev, &fp_mac, "a", 101.0, true) {
            Verdict::Accept(m) => assert_eq!(b64url(&m), "XAX-uZPsq_Ax8fvI_sV7Ex0a-ROi19rI-FW-ykPh9TE"),
            v => panic!("{v:?}"),
        }
        assert_eq!(w.try_proof("qr", &qr_d, &fp_dev, &fp_mac, "b", 102.0, true), Verdict::Closed(CloseReason::Used));

        w = Window::open(secret, code.clone(), 200.0, 300.0, None);
        assert_eq!(w.try_proof("qr", &qr_d, &fp_dev, &wrong, "a", 200.0, true), Verdict::Reject(4));
        assert!(matches!(w.try_proof("qr", &qr_d, &fp_dev, &fp_mac, "b", 200.5, true), Verdict::Busy(r) if (r - 0.5).abs() < 1e-9));
        assert!(matches!(w.try_proof("qr", &wrong, &fp_dev, &fp_mac, "a", 203.0, true), Verdict::Busy(_)));
        assert_eq!(w.try_proof("qr", &wrong, &fp_dev, &fp_mac, "b", 201.1, true), Verdict::Reject(3));
        assert_eq!(w.try_proof("qr", &wrong, &fp_dev, &fp_mac, "c", 203.2, true), Verdict::Reject(2));
        assert_eq!(w.try_proof("qr", &wrong, &fp_dev, &fp_mac, "d", 207.3, true), Verdict::Reject(1));
        assert_eq!(w.try_proof("qr", &wrong, &fp_dev, &fp_mac, "e", 215.4, true), Verdict::Closed(CloseReason::Stopped));
        assert_eq!(w.try_proof("qr", &qr_d, &fp_dev, &fp_mac, "f", 230.0, true), Verdict::Closed(CloseReason::Stopped));

        w = Window::open(secret, code.clone(), 300.0, 5.0, None);
        assert_eq!(w.try_proof("qr", &qr_d, &fp_dev, &fp_mac, "a", 305.01, true), Verdict::Closed(CloseReason::Expired));

        // typed code: busy before K, then accepted with the vector's proof_M
        w = Window::open(secret, code.clone(), 800.0, 300.0, None);
        let k = code_key(&code, &fp_mac);
        let code_d = device_proof(&k, &fp_dev, &fp_mac);
        assert_eq!(w.try_proof("code", &code_d, &fp_dev, &fp_mac, "a", 800.05, true), Verdict::Busy(1.0));
        w.set_code_key(k, "000000000000");
        assert_eq!(w.try_proof("code", &code_d, &fp_dev, &fp_mac, "a", 800.1, true), Verdict::Busy(1.0));
        w.set_code_key(k, &code);
        match w.try_proof("code", &code_d, &fp_dev, &fp_mac, "a", 800.2, true) {
            Verdict::Accept(m) => assert_eq!(b64url(&m), "6BmGWC9XT8J8OY8phhq9bASgoSnoWpRxBeqZtvRrWmk"),
            v => panic!("{v:?}"),
        }
        w = Window::open(secret, code, 900.0, 300.0, None);
        assert_eq!(w.try_proof("icloud", &qr_d, &fp_dev, &fp_mac, "a", 900.0, true), Verdict::Reject(4));
    }
}
