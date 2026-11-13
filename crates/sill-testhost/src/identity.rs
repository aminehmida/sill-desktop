//! A P-256 identity: the key, a self-signed certificate made from it, and its pin (the
//! SHA-256 of its SubjectPublicKeyInfo).
//!
//! Read from `Sources/StreamProtocol/RemoteIdentity.swift` (`SPKI`, `SelfSignedCertificate`)
//! and `Remote.swift` (`SignedMacInfo`). The real host builds its certificate by hand
//! (v3, no extensions, a random 16-hex-character CN, valid 2026 to 9999); this one comes
//! from `rcgen` with a random CN, which is equivalent for every peer: only the key is
//! pinned, names and dates mean nothing.

use crate::pairing::sha256;
use ring::rand::{SecureRandom, SystemRandom};
use ring::signature::{EcdsaKeyPair, KeyPair, ECDSA_P256_SHA256_ASN1, ECDSA_P256_SHA256_ASN1_SIGNING};
use std::io;

/// `SEQUENCE { SEQUENCE { OID ecPublicKey, OID prime256v1 }, BIT STRING (0x00 ‖` — the 26
/// bytes before a P-256 key's 65-byte uncompressed point in its SubjectPublicKeyInfo.
const P256_SPKI_PREFIX: [u8; 26] = [
    0x30, 0x59, 0x30, 0x13, 0x06, 0x07, 0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x02, 0x01, 0x06, 0x08, 0x2A, 0x86, 0x48,
    0xCE, 0x3D, 0x03, 0x01, 0x07, 0x03, 0x42, 0x00,
];

/// The SubjectPublicKeyInfo DER of a 65-byte uncompressed P-256 point (`SPKI.der`).
pub fn spki_der(point: &[u8; 65]) -> Vec<u8> {
    let mut d = P256_SPKI_PREFIX.to_vec();
    d.extend_from_slice(point);
    d
}

/// The pin of a point: SHA-256 of its SubjectPublicKeyInfo (`SPKI.fingerprint`).
pub fn fingerprint_of_point(point: &[u8; 65]) -> [u8; 32] {
    sha256(&spki_der(point))
}

/// One DER element: its tag, its content, and what follows it.
fn tlv(d: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let (&tag, rest) = d.split_first()?;
    let (&first, rest) = rest.split_first()?;
    let (len, rest) = if first < 0x80 {
        (first as usize, rest)
    } else {
        let n = (first & 0x7F) as usize;
        if n == 0 || n > 4 || rest.len() < n {
            return None;
        }
        let len = rest[..n].iter().fold(0usize, |a, b| (a << 8) | *b as usize);
        (len, &rest[n..])
    };
    if rest.len() < len {
        return None;
    }
    Some((tag, &rest[..len], &rest[len..]))
}

/// The 65-byte P-256 point of an X.509 certificate's key, or `None` when the certificate
/// does not parse or its key is anything but an uncompressed P-256 point. A tiny DER
/// walk: Certificate → tbsCertificate → (version) serial, signature, issuer, validity,
/// subject → subjectPublicKeyInfo. Nothing else of the certificate is looked at, as on
/// the real host (`SPKI.fingerprint(of: SecCertificate)`).
pub fn p256_point_of_certificate(cert: &[u8]) -> Option<[u8; 65]> {
    let (tag, certificate, _) = tlv(cert)?;
    if tag != 0x30 {
        return None;
    }
    let (tag, mut tbs, _) = tlv(certificate)?;
    if tag != 0x30 {
        return None;
    }
    // [0] EXPLICIT version, when present.
    if tbs.first() == Some(&0xA0) {
        tbs = tlv(tbs)?.2;
    }
    // serialNumber, signature, issuer, validity, subject.
    for _ in 0..5 {
        tbs = tlv(tbs)?.2;
    }
    let (tag, content, rest) = tlv(tbs)?;
    if tag != 0x30 {
        return None;
    }
    let whole = &tbs[..tbs.len() - rest.len()];
    let _ = content;
    if whole.len() != P256_SPKI_PREFIX.len() + 65 || whole[..26] != P256_SPKI_PREFIX || whole[26] != 0x04 {
        return None;
    }
    whole[26..].try_into().ok()
}

/// The pin of a certificate's key when it is P-256, else `None`.
pub fn fingerprint_of_certificate(cert: &[u8]) -> Option<[u8; 32]> {
    p256_point_of_certificate(cert).map(|p| fingerprint_of_point(&p))
}

/// Verifies an ASN.1 (DER) ECDSA P-256 / SHA-256 signature over `message`.
pub fn verify_p256(point: &[u8; 65], message: &[u8], signature: &[u8]) -> bool {
    ring::signature::UnparsedPublicKey::new(&ECDSA_P256_SHA256_ASN1, point)
        .verify(message, signature)
        .is_ok()
}

/// A key and the certificate made from it, ready for TLS.
pub struct Identity {
    /// The private key, PKCS#8 DER.
    pub pkcs8: Vec<u8>,
    key: EcdsaKeyPair,
    /// The public key's uncompressed point (04 ‖ X ‖ Y).
    pub point: [u8; 65],
    /// SHA-256 of the SubjectPublicKeyInfo: this identity's pin.
    pub fingerprint: [u8; 32],
    /// The self-signed certificate, DER. Rebuilt at every start from the key, as the real
    /// host does: the pin is the key.
    pub cert_der: Vec<u8>,
}

fn other(e: impl std::fmt::Display) -> io::Error {
    io::Error::other(e.to_string())
}

impl Identity {
    /// A fresh P-256 identity.
    pub fn generate() -> io::Result<Identity> {
        let rng = SystemRandom::new();
        let doc = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &rng).map_err(other)?;
        Identity::from_pkcs8(doc.as_ref())
    }

    /// The identity of a stored key.
    pub fn from_pkcs8(pkcs8: &[u8]) -> io::Result<Identity> {
        let rng = SystemRandom::new();
        let key = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, pkcs8, &rng).map_err(other)?;
        let point: [u8; 65] = key.public_key().as_ref().try_into().map_err(|_| other("not a P-256 point"))?;
        let mut cn = [0u8; 8];
        rng.fill(&mut cn).map_err(other)?;
        let cn: String = cn.iter().map(|b| format!("{b:02x}")).collect();
        let rc_key = rcgen::KeyPair::try_from(pkcs8).map_err(other)?;
        let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).map_err(other)?;
        params.distinguished_name = rcgen::DistinguishedName::new();
        params.distinguished_name.push(rcgen::DnType::CommonName, cn);
        let cert_der = params.self_signed(&rc_key).map_err(other)?.der().to_vec();
        let fingerprint = fingerprint_of_point(&point);
        if fingerprint_of_certificate(&cert_der) != Some(fingerprint) {
            return Err(other("the certificate's key is not the identity's"));
        }
        Ok(Identity { pkcs8: pkcs8.to_vec(), key, point, fingerprint, cert_der })
    }

    /// A DER ECDSA P-256 / SHA-256 signature over `message` (`.ecdsaSignatureMessageX962SHA256`).
    pub fn sign(&self, message: &[u8]) -> Vec<u8> {
        self.key
            .sign(&SystemRandom::new(), message)
            .map(|s| s.as_ref().to_vec())
            .unwrap_or_default()
    }
}

/// 32 random bytes.
pub fn random32() -> [u8; 32] {
    let mut b = [0u8; 32];
    SystemRandom::new().fill(&mut b).expect("the system's random source");
    b
}

/// 16 random bytes.
pub fn random16() -> [u8; 16] {
    let mut b = [0u8; 16];
    SystemRandom::new().fill(&mut b).expect("the system's random source");
    b
}

/// A random UUID (version 4), upper case as Swift's `UUID().uuidString`.
pub fn uuid() -> String {
    let mut b = random16();
    b[6] = (b[6] & 0x0F) | 0x40;
    b[8] = (b[8] & 0x3F) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02X}")).collect();
    format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_round_trip() {
        let a = Identity::generate().unwrap();
        let b = Identity::from_pkcs8(&a.pkcs8).unwrap();
        assert_eq!(a.fingerprint, b.fingerprint);
        assert_eq!(p256_point_of_certificate(&a.cert_der), Some(a.point));
        let sig = a.sign(b"hello");
        assert!(verify_p256(&a.point, b"hello", &sig));
        assert!(!verify_p256(&a.point, b"hellO", &sig));
        assert_ne!(Identity::generate().unwrap().fingerprint, a.fingerprint);
    }

    #[test]
    fn der_walker_refuses_garbage() {
        assert_eq!(p256_point_of_certificate(&[]), None);
        assert_eq!(p256_point_of_certificate(&[0x30, 0x82, 0xFF, 0xFF, 1, 2, 3]), None);
        assert_eq!(p256_point_of_certificate(&[0x30, 0x03, 0x30, 0x01, 0x00]), None);
    }

    #[test]
    fn uuid_shape() {
        let u = uuid();
        assert_eq!(u.len(), 36);
        assert_eq!(u.as_bytes()[14], b'4');
    }
}
