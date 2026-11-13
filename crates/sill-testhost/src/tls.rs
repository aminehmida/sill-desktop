//! The door's TLS, as `Sources/StreamProtocol/RemoteTLS.swift` sets it up for a host:
//! TLS 1.3 only, a self-signed P-256 certificate, a client certificate required, the two
//! application protocols `sill/1` and `sill-pair/1` offered together, no tickets and no
//! resumption. Trust is the SHA-256 of the peer key's SubjectPublicKeyInfo and the
//! negotiated protocol, decided inside the handshake (`DoorPolicy.trusts`): a key the
//! door does not trust fails the handshake with a `bad_certificate` alert, which a
//! TLS 1.3 client sees on its first read after its own side reported ready.

use crate::identity::{fingerprint_of_certificate, p256_point_of_certificate, verify_p256, Identity};
use rustls::client::danger::HandshakeSignatureValid;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{CertificateError, DigitallySignedStruct, DistinguishedName, Error, ServerConfig, SignatureScheme};
use std::sync::Arc;

/// A session: the catalog, frames and input (`RemoteTLS.sessionALPN`).
pub const SESSION_ALPN: &str = "sill/1";
/// Pairing: one kind 19 from the device, one kind 20 back (`RemoteTLS.pairingALPN`).
pub const PAIRING_ALPN: &str = "sill-pair/1";

/// The protocol a host picks from what a client offers: its own order, `sill/1` first
/// (`RemoteTLS.serverALPNs`). A real client offers exactly one.
pub fn choose_alpn<'a>(offered: impl Iterator<Item = &'a [u8]>) -> Option<&'static str> {
    let offered: Vec<&[u8]> = offered.collect();
    [SESSION_ALPN, PAIRING_ALPN].into_iter().find(|p| offered.contains(&p.as_bytes()))
}

/// Decides trust from the peer's pin (`None`: its key is not P-256).
pub type TrustFn = Arc<dyn Fn(Option<[u8; 32]>) -> bool + Send + Sync>;

struct PinVerifier {
    trust: TrustFn,
}

impl std::fmt::Debug for PinVerifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PinVerifier")
    }
}

impl ClientCertVerifier for PinVerifier {
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<ClientCertVerified, Error> {
        if (self.trust)(fingerprint_of_certificate(end_entity.as_ref())) {
            Ok(ClientCertVerified::assertion())
        } else {
            // `bad_certificate` on the wire: what a device reads as "the peer refused ours" (-9825).
            Err(Error::InvalidCertificate(CertificateError::BadEncoding))
        }
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        Err(Error::General("TLS 1.3 only".into()))
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        verify_p256_tls13(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![SignatureScheme::ECDSA_NISTP256_SHA256]
    }
}

/// A TLS 1.3 CertificateVerify by a P-256 key, checked with ring directly.
pub fn verify_p256_tls13(
    message: &[u8],
    cert: &CertificateDer<'_>,
    dss: &DigitallySignedStruct,
) -> Result<HandshakeSignatureValid, Error> {
    let point = p256_point_of_certificate(cert.as_ref()).ok_or(Error::InvalidCertificate(CertificateError::BadEncoding))?;
    if dss.scheme != SignatureScheme::ECDSA_NISTP256_SHA256 || !verify_p256(&point, message, dss.signature()) {
        return Err(Error::InvalidCertificate(CertificateError::BadSignature));
    }
    Ok(HandshakeSignatureValid::assertion())
}

/// The host's TLS configuration for one connection; `trust` is that connection's verify
/// block, which already knows the protocol the client asked for.
pub fn server_config(identity: &Identity, trust: TrustFn) -> Result<Arc<ServerConfig>, Error> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_client_cert_verifier(Arc::new(PinVerifier { trust }))
        .with_single_cert(
            vec![CertificateDer::from(identity.cert_der.clone())],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(identity.pkcs8.clone())),
        )?;
    config.alpn_protocols = vec![SESSION_ALPN.as_bytes().to_vec(), PAIRING_ALPN.as_bytes().to_vec()];
    // No tickets, no resumption: a removed device must not come back on an old ticket.
    config.send_tls13_tickets = 0;
    config.session_storage = Arc::new(rustls::server::NoServerSessionStorage {});
    Ok(Arc::new(config))
}

/// A minimal device-side TLS configuration, for this crate's own tests only.
#[cfg(test)]
pub mod testclient {
    use super::*;
    use rustls::client::danger::{ServerCertVerified, ServerCertVerifier};
    use rustls::pki_types::ServerName;
    use rustls::ClientConfig;

    #[derive(Debug)]
    struct Pin(Option<[u8; 32]>);

    impl ServerCertVerifier for Pin {
        fn verify_server_cert(
            &self,
            end_entity: &CertificateDer<'_>,
            _intermediates: &[CertificateDer<'_>],
            _server_name: &ServerName<'_>,
            _ocsp: &[u8],
            _now: UnixTime,
        ) -> Result<ServerCertVerified, Error> {
            let fp = fingerprint_of_certificate(end_entity.as_ref());
            if fp.is_some() && (self.0.is_none() || self.0 == fp) {
                Ok(ServerCertVerified::assertion())
            } else {
                Err(Error::InvalidCertificate(CertificateError::BadEncoding))
            }
        }
        fn verify_tls12_signature(
            &self,
            _m: &[u8],
            _c: &CertificateDer<'_>,
            _d: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, Error> {
            Err(Error::General("TLS 1.3 only".into()))
        }
        fn verify_tls13_signature(
            &self,
            m: &[u8],
            c: &CertificateDer<'_>,
            d: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, Error> {
            verify_p256_tls13(m, c, d)
        }
        fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
            vec![SignatureScheme::ECDSA_NISTP256_SHA256]
        }
    }

    /// A device's configuration: its identity, one protocol, the host pinned (or any P-256 host).
    pub fn client_config(device: &Identity, alpn: &str, pin: Option<[u8; 32]>) -> Arc<ClientConfig> {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut config = ClientConfig::builder_with_provider(provider)
            .with_protocol_versions(&[&rustls::version::TLS13])
            .unwrap()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(Pin(pin)))
            .with_client_auth_cert(
                vec![CertificateDer::from(device.cert_der.clone())],
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(device.pkcs8.clone())),
            )
            .unwrap();
        config.alpn_protocols = vec![alpn.as_bytes().to_vec()];
        config.resumption = rustls::client::Resumption::disabled();
        Arc::new(config)
    }
}
