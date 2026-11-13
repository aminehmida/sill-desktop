//! This device's identity at the Mac's doors (RemoteIdentity.swift): one P-256 key per install
//! with a self-signed certificate. Trust is the key alone, pinned as the SHA-256 of its
//! SubjectPublicKeyInfo; the certificate's names and dates mean nothing to either side.

use std::io;
use std::sync::Arc;

use ring::rand::{SecureRandom, SystemRandom};
use ring::signature::{EcdsaKeyPair, KeyPair as _, ECDSA_P256_SHA256_ASN1_SIGNING};

use super::keystore::KeyStore;
use super::pairing::{spki, to_hex};

/// The key store's name of the private key (PKCS#8 DER).
pub const IDENTITY_KEY_NAME: &str = "device-identity";
/// The key store's name of the certificate (X.509 DER). Not secret; kept beside its key so the
/// same certificate is presented at every launch.
pub const IDENTITY_CERT_NAME: &str = "device-certificate";

/// A P-256 key and its self-signed certificate.
#[derive(Clone)]
pub struct DeviceIdentity {
    pkcs8: Arc<Vec<u8>>,
    certificate: Arc<Vec<u8>>,
    point: [u8; 65],
    fingerprint: [u8; 32],
}

impl std::fmt::Debug for DeviceIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "DeviceIdentity({})", to_hex(&self.fingerprint[..6]))
    }
}

fn other(e: impl std::fmt::Display) -> io::Error {
    io::Error::other(e.to_string())
}

impl DeviceIdentity {
    /// A fresh key and certificate, stored nowhere.
    pub fn generate() -> io::Result<DeviceIdentity> {
        let rng = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &rng).map_err(other)?;
        Self::from_pkcs8(pkcs8.as_ref(), None)
    }

    /// From a stored key, with its stored certificate when that still names the key (a new one is
    /// made otherwise).
    pub fn from_pkcs8(pkcs8: &[u8], certificate: Option<&[u8]>) -> io::Result<DeviceIdentity> {
        let rng = SystemRandom::new();
        let pair = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, pkcs8, &rng).map_err(other)?;
        let point: [u8; 65] = pair.public_key().as_ref().try_into().map_err(|_| other("the device key is not P-256"))?;
        let fingerprint = spki::fingerprint(&point);
        let certificate = match certificate.filter(|c| spki::fingerprint_of_certificate(c) == Some(fingerprint)) {
            Some(c) => c.to_vec(),
            None => self_signed(pkcs8, &rng)?,
        };
        Ok(DeviceIdentity { pkcs8: Arc::new(pkcs8.to_vec()), certificate: Arc::new(certificate), point, fingerprint })
    }

    /// The identity in `store`, made and saved there when it has none.
    pub fn load_or_create(store: &dyn KeyStore) -> io::Result<DeviceIdentity> {
        if let Some(pkcs8) = store.load(IDENTITY_KEY_NAME)? {
            let cert = store.load(IDENTITY_CERT_NAME)?;
            let id = Self::from_pkcs8(&pkcs8, cert.as_deref())?;
            if cert.as_deref() != Some(id.certificate_der()) {
                store.save(IDENTITY_CERT_NAME, id.certificate_der())?;
            }
            return Ok(id);
        }
        let id = Self::generate()?;
        store.save(IDENTITY_KEY_NAME, &id.pkcs8)?;
        store.save(IDENTITY_CERT_NAME, &id.certificate)?;
        Ok(id)
    }

    /// Whether `store` holds a key already: saved Macs are worthless without the key they trust.
    pub fn exists(store: &dyn KeyStore) -> bool {
        matches!(store.load(IDENTITY_KEY_NAME), Ok(Some(_)))
    }

    /// The private key, PKCS#8 DER.
    pub fn pkcs8_der(&self) -> &[u8] {
        &self.pkcs8
    }

    /// The self-signed certificate, X.509 DER.
    pub fn certificate_der(&self) -> &[u8] {
        &self.certificate
    }

    /// The public key's uncompressed point (04 ‖ X ‖ Y).
    pub fn public_point(&self) -> &[u8; 65] {
        &self.point
    }

    /// SHA-256 of the key's SubjectPublicKeyInfo: what a Mac pins.
    pub fn fingerprint(&self) -> &[u8; 32] {
        &self.fingerprint
    }
}

/// CN a random name, a random positive serial, valid 2026 to 2100: as the Android client's.
fn self_signed(pkcs8: &[u8], rng: &SystemRandom) -> io::Result<Vec<u8>> {
    let key = rcgen::KeyPair::try_from(pkcs8).map_err(other)?;
    let mut cn = [0u8; 8];
    rng.fill(&mut cn).map_err(other)?;
    let mut serial = [0u8; 16];
    rng.fill(&mut serial).map_err(other)?;
    serial[0] &= 0x7F;
    serial[15] |= 1;
    let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).map_err(other)?;
    params.distinguished_name = rcgen::DistinguishedName::new();
    params.distinguished_name.push(rcgen::DnType::CommonName, to_hex(&cn));
    params.serial_number = Some(rcgen::SerialNumber::from_slice(&serial));
    params.not_before = rcgen::date_time_ymd(2026, 1, 1);
    params.not_after = rcgen::date_time_ymd(2100, 1, 1);
    let cert = params.self_signed(&key).map_err(other)?;
    Ok(cert.der().to_vec())
}
