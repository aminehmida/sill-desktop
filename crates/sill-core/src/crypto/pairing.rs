//! Pairing and identity math from `Sources/StreamProtocol` (Pairing.swift, RemoteIdentity.swift),
//! byte for byte: the Mac checks every proof this side makes, and this side checks the Mac's.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ring::{digest, hmac, pbkdf2, signature};
use std::num::NonZeroU32;

/// base64url without padding (RFC 4648 §5).
pub mod base64url {
    use super::*;

    pub fn encode(d: &[u8]) -> String {
        URL_SAFE_NO_PAD.encode(d)
    }

    /// `None` for anything that is not canonical unpadded base64url.
    pub fn decode(text: &str) -> Option<Vec<u8>> {
        if !text.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') || text.len() % 4 == 1 {
            return None;
        }
        let d = URL_SAFE_NO_PAD.decode(text).ok()?;
        if encode(&d) == text { Some(d) } else { None }
    }
}

pub fn sha256(d: &[u8]) -> [u8; 32] {
    digest::digest(&digest::SHA256, d).as_ref().try_into().unwrap()
}

/// Bytes from hex; panics on anything else (constants and tests only).
pub fn hex(s: &str) -> Vec<u8> {
    (0..s.len() / 2).map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).expect("hex")).collect()
}

pub fn to_hex(d: &[u8]) -> String {
    d.iter().map(|b| format!("{b:02x}")).collect()
}

/// Constant-time equality.
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// A P-256 SubjectPublicKeyInfo.
pub mod spki {
    use super::*;

    /// `SEQUENCE { SEQUENCE { ecPublicKey, prime256v1 }, BIT STRING (0x00 ‖ point) }`: 26 bytes,
    /// then the point.
    pub const PREFIX: [u8; 26] = [
        0x30, 0x59, 0x30, 0x13, 0x06, 0x07, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01, 0x06, 0x08, 0x2a, 0x86, 0x48, 0xce, 0x3d,
        0x03, 0x01, 0x07, 0x03, 0x42, 0x00,
    ];

    pub fn der(point: &[u8]) -> Vec<u8> {
        let mut v = PREFIX.to_vec();
        v.extend_from_slice(point);
        v
    }

    /// SHA-256 of the SPKI of a 65-byte uncompressed P-256 point: every pin compares this.
    pub fn fingerprint(point: &[u8]) -> [u8; 32] {
        sha256(&der(point))
    }

    /// The uncompressed point of a DER SubjectPublicKeyInfo; `None` for any key that is not P-256.
    pub fn point_of_der(spki: &[u8]) -> Option<&[u8]> {
        if spki.len() == 91 && spki[..26] == PREFIX && spki[26] == 4 { Some(&spki[26..]) } else { None }
    }

    /// The SubjectPublicKeyInfo of an X.509 certificate (DER), as its bytes: a walk down
    /// `Certificate → tbsCertificate → [version] serial sigAlg issuer validity subject spki`.
    /// Nothing else about the certificate is looked at.
    pub fn of_certificate(cert: &[u8]) -> Option<&[u8]> {
        let (tag, cert_body, _) = tlv(cert)?;
        if tag != 0x30 {
            return None;
        }
        let (tag, mut tbs, _) = tlv(cert_body)?;
        if tag != 0x30 {
            return None;
        }
        // [0] EXPLICIT version, optional.
        let (tag, _, rest) = tlv(tbs)?;
        if tag == 0xA0 {
            tbs = rest;
        }
        // serialNumber, signature, issuer, validity, subject.
        for _ in 0..5 {
            let (_, _, rest) = tlv(tbs)?;
            tbs = rest;
        }
        let (tag, body, rest) = tlv(tbs)?;
        if tag != 0x30 {
            return None;
        }
        let _ = body;
        Some(&tbs[..tbs.len() - rest.len()])
    }

    /// The pin of a certificate's key: `None` when it is not a P-256 key.
    pub fn fingerprint_of_certificate(cert: &[u8]) -> Option<[u8; 32]> {
        let spki = of_certificate(cert)?;
        point_of_der(spki).map(fingerprint)
    }

    /// One DER TLV: its tag, its contents, and what follows it.
    fn tlv(d: &[u8]) -> Option<(u8, &[u8], &[u8])> {
        if d.len() < 2 {
            return None;
        }
        let tag = d[0];
        let (len, header) = if d[1] < 0x80 {
            (d[1] as usize, 2)
        } else {
            let n = (d[1] & 0x7F) as usize;
            if n == 0 || n > 4 || d.len() < 2 + n {
                return None;
            }
            let mut len = 0usize;
            for &b in &d[2..2 + n] {
                len = (len << 8) | b as usize;
            }
            (len, 2 + n)
        };
        if d.len() < header + len {
            return None;
        }
        Some((tag, &d[header..header + len], &d[header + len..]))
    }
}

/// The Mac ID: Crockford base32 of the fingerprint's first 10 bytes, 16 characters.
pub mod mac_id {
    const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

    pub fn make(fingerprint: &[u8]) -> String {
        if fingerprint.len() < 10 {
            return String::new();
        }
        let mut out = String::with_capacity(16);
        let (mut buffer, mut bits) = (0u32, 0u32);
        for &b in &fingerprint[..10] {
            buffer = (buffer << 8) | u32::from(b);
            bits += 8;
            while bits >= 5 {
                bits -= 5;
                out.push(ALPHABET[((buffer >> bits) & 31) as usize] as char);
            }
            buffer &= (1 << bits) - 1;
        }
        out
    }

    pub fn is_well_formed(t: &str) -> bool {
        t.len() == 16 && t.bytes().all(|c| ALPHABET.contains(&c))
    }
}

/// The 12-digit typed code with its Damm check digit (`PairingCode`).
pub mod pairing_code {
    const TABLE: [[u8; 10]; 10] = [
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

    /// What a typed code is.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum Check {
        /// The 12 digits, separators dropped.
        Ok(String),
        /// Not 12 digits.
        Length,
        /// 12 digits whose check digit does not hold.
        Typo,
    }

    /// Spaces, hyphens and no-break spaces are dropped first.
    pub fn check(input: &str) -> Check {
        let kept: String = input.chars().filter(|c| !matches!(c, ' ' | '-' | '\u{a0}')).collect();
        if kept.chars().count() != 12 || !kept.bytes().all(|c| c.is_ascii_digit()) {
            return Check::Length;
        }
        let interim = kept.bytes().fold(0usize, |i, c| TABLE[i][(c - b'0') as usize] as usize);
        if interim == 0 { Check::Ok(kept) } else { Check::Typo }
    }

    /// The Damm check digit of `digits`.
    pub fn check_digit(digits: &[u8]) -> u8 {
        digits.iter().fold(0usize, |i, &d| TABLE[i][d as usize] as usize) as u8
    }

    /// "1234 5678 9012".
    pub fn grouped(code: &str) -> String {
        code.as_bytes().chunks(4).map(|c| String::from_utf8_lossy(c).into_owned()).collect::<Vec<_>>().join(" ")
    }
}

/// The proofs of one pairing exchange (`PairingProof`). Each side takes the other's fingerprint
/// from the TLS session, never from a message.
pub mod pairing_proof {
    use super::*;

    const LABEL: &str = "sill-pair-v1";
    const ROUNDS: u32 = 600_000;

    /// The typed path's key: PBKDF2-HMAC-SHA256(code, "sill-pair-v1" ‖ fp_mac, 600,000, 32).
    pub fn code_key(code: &str, mac_fingerprint: &[u8]) -> [u8; 32] {
        let mut salt = LABEL.as_bytes().to_vec();
        salt.extend_from_slice(mac_fingerprint);
        let mut out = [0u8; 32];
        pbkdf2::derive(pbkdf2::PBKDF2_HMAC_SHA256, NonZeroU32::new(ROUNDS).unwrap(), &salt, code.as_bytes(), &mut out);
        out
    }

    fn message(role: &str, first: &[u8], second: &[u8]) -> Vec<u8> {
        let mut m = format!("{LABEL} {role}").into_bytes();
        m.push(0);
        m.extend_from_slice(first);
        m.extend_from_slice(second);
        m
    }

    pub fn device_proof(key: &[u8], device_fingerprint: &[u8], mac_fingerprint: &[u8]) -> Vec<u8> {
        hmac_sha256(key, &message("device", device_fingerprint, mac_fingerprint))
    }

    pub fn mac_proof(key: &[u8], mac_fingerprint: &[u8], device_fingerprint: &[u8]) -> Vec<u8> {
        hmac_sha256(key, &message("mac", mac_fingerprint, device_fingerprint))
    }

    pub fn is_valid_mac_proof(proof: &[u8], key: &[u8], mac_fingerprint: &[u8], device_fingerprint: &[u8]) -> bool {
        constant_time_eq(proof, &mac_proof(key, mac_fingerprint, device_fingerprint))
    }
}

pub(crate) fn hmac_sha256(key: &[u8], message: &[u8]) -> Vec<u8> {
    hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, key), message).as_ref().to_vec()
}

/// The Bonjour TXT record's `r` (`RecognitionTag`): base64url of p ‖ HMAC-SHA256(recognitionKey,
/// "sill-tag-v1" ‖ p)[0..<6], p 6 fresh bytes per registration. Recognises a saved Mac on a
/// network whatever its Bonjour name; replayable, so a hint, never authentication (a saved Mac is
/// always dialed pinned to its key).
pub mod recognition_tag {
    use super::*;

    pub fn matches(tag: &str, recognition_key: &[u8]) -> bool {
        let Some(bytes) = base64url::decode(tag) else { return false };
        if bytes.len() != 12 || recognition_key.is_empty() {
            return false;
        }
        let mut msg = b"sill-tag-v1".to_vec();
        msg.extend_from_slice(&bytes[..6]);
        let mac = hmac_sha256(recognition_key, &msg);
        constant_time_eq(&mac[..6], &bytes[6..12])
    }
}

/// The QR code's content: `sill://pair?v=1&m=…&k=…&s=…&n=…&p=…&a=…` (`PairLink`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairLink {
    pub mac_id: String,
    pub fingerprint: Vec<u8>,
    pub secret: Vec<u8>,
    pub name: String,
    pub port: u16,
    pub addresses: Vec<String>,
}

impl PairLink {
    /// The link, or why it is none, in words to show.
    pub fn parse(text: &str) -> Result<PairLink, String> {
        let t = text.trim();
        let not_a_link = || "That isn’t a Sill pairing link.".to_string();
        let damaged = || "The pairing link is damaged.".to_string();
        const SCHEME: &str = "sill://pair";
        if t.len() < SCHEME.len() || !t.is_char_boundary(SCHEME.len()) || !t[..SCHEME.len()].eq_ignore_ascii_case(SCHEME) {
            return Err(not_a_link());
        }
        let rest = &t[SCHEME.len()..];
        if !(rest.is_empty() || rest.starts_with('?') || rest.starts_with("/?")) {
            return Err(not_a_link());
        }
        let query = rest.split_once('?').map(|x| x.1).unwrap_or("");
        // Values are percent-encoded down to RFC 3986's unreserved characters and ":", so a "+"
        // never stands for a space.
        let items: Vec<(String, String)> = query
            .split('&')
            .filter(|kv| !kv.is_empty())
            .map(|kv| {
                let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
                (percent_decode(k), percent_decode(v))
            })
            .collect();
        let all = |key: &str| -> Vec<&str> { items.iter().filter(|(k, _)| k == key).map(|(_, v)| v.as_str()).collect() };
        let single = |key: &str| -> Option<&str> {
            let a = all(key);
            if a.len() == 1 && !a[0].is_empty() { Some(a[0]) } else { None }
        };
        if single("v") != Some("1") {
            return Err("This pairing link is from a newer Sill. Update Sill on this device.".to_string());
        }
        let m = single("m").filter(|m| mac_id::is_well_formed(m)).ok_or_else(damaged)?;
        let fp = single("k").and_then(base64url::decode).filter(|d| d.len() == 32).ok_or_else(damaged)?;
        let secret = single("s").and_then(base64url::decode).filter(|d| d.len() == 16).ok_or_else(damaged)?;
        let port = single("p")
            .filter(|p| p.len() <= 5 && p.bytes().all(|c| c.is_ascii_digit()))
            .and_then(|p| p.parse::<u32>().ok())
            .filter(|p| (1..=65535).contains(p))
            .ok_or_else(damaged)? as u16;
        if m != mac_id::make(&fp) {
            return Err(damaged());
        }
        let mut name = safe_text::label(all("n").first().copied().unwrap_or(""), 64);
        if name.is_empty() {
            name = "Mac".to_string();
        }
        Ok(PairLink {
            mac_id: m.to_string(),
            fingerprint: fp,
            secret,
            name,
            port,
            addresses: all("a").into_iter().take(5).map(str::to_string).collect(),
        })
    }
}

/// `%XX` decoding; a malformed escape leaves the text as it is.
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let hex = b.get(i + 1..i + 3).and_then(|h| std::str::from_utf8(h).ok()).and_then(|h| u8::from_str_radix(h, 16).ok());
            match hex {
                Some(v) => {
                    out.push(v);
                    i += 3;
                }
                None => return s.to_string(),
            }
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

/// Kind 18: the fingerprint of the key that signed the MacInfo bytes, when the signature holds.
pub fn verify_mac_info(info: &[u8], point: &[u8], sig: &[u8]) -> Option<[u8; 32]> {
    if point.len() != 65 || point[0] != 4 {
        return None;
    }
    signature::UnparsedPublicKey::new(&signature::ECDSA_P256_SHA256_ASN1, point).verify(info, sig).ok()?;
    Some(spki::fingerprint(point))
}

/// `SafeText.label` (SafeText.swift), the same rules: whitespace of every kind becomes one space,
/// other control and format characters (bidirectional overrides, zero-width characters) are
/// removed, runs of spaces collapse, the ends are trimmed, at most `limit` characters are kept.
pub mod safe_text {
    /// General category Cf (format), as of Unicode 15.
    fn is_format(c: char) -> bool {
        matches!(c as u32,
            0x00AD | 0x0600..=0x0605 | 0x061C | 0x06DD | 0x070F | 0x0890..=0x0891 | 0x08E2 | 0x180E
            | 0x200B..=0x200F | 0x202A..=0x202E | 0x2060..=0x2064 | 0x2066..=0x206F | 0xFEFF | 0xFFF9..=0xFFFB
            | 0x110BD | 0x110CD | 0x13430..=0x1343F | 0x1BCA0..=0x1BCA3 | 0x1D173..=0x1D17A | 0xE0001 | 0xE0020..=0xE007F)
    }

    fn is_private_or_nonchar(c: char) -> bool {
        let v = c as u32;
        matches!(v, 0xE000..=0xF8FF | 0xF0000..=0xFFFFD | 0x100000..=0x10FFFD | 0xFDD0..=0xFDEF) || (v & 0xFFFE) == 0xFFFE
    }

    pub fn label(s: &str, limit: usize) -> String {
        let mut out = String::new();
        let mut pending_space = false;
        for c in s.chars() {
            // U+001C…U+001F are separators too (and controls either way: never kept).
            if c.is_whitespace() || matches!(c, '\u{1c}'..='\u{1f}') {
                pending_space = !out.is_empty();
                continue;
            }
            if c.is_control() || is_format(c) || is_private_or_nonchar(c) {
                continue;
            }
            if pending_space {
                out.push(' ');
                pending_space = false;
            }
            out.push(c);
        }
        if out.chars().count() > limit {
            out = out.chars().take(limit).collect();
            while out.ends_with(' ') {
                out.pop();
            }
        }
        out
    }
}
