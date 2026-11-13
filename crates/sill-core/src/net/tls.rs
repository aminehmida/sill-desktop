//! The doors' TLS (RemoteTLS.swift): TLS 1.3 only, this device's certificate presented, the Mac's
//! key pinned inside the handshake (a mismatch ends it before this device's certificate is sent),
//! one ALPN, SNI "sill" so no MagicDNS name crosses the network in clear, and no resumption.

use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ring::signature;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, SignatureScheme};
use tokio::net::TcpStream;
use tokio::task::JoinSet;
use tokio::time::Instant;
use tokio_rustls::client::TlsStream;
use tokio_rustls::TlsConnector;

use super::saved_macs::Endpoint;
use crate::crypto::{constant_time_eq, spki, DeviceIdentity};

/// A session: the catalog, frames and input.
pub const SESSION_ALPN: &str = "sill/1";
/// Pairing: exactly one kind 19 from the device and one kind 20 back, then close.
pub const PAIRING_ALPN: &str = "sill-pair/1";
/// The SNI every client sends. The host ignores it.
pub const SERVER_NAME: &str = "sill";
/// One attempt's connect and handshake timeout.
pub const DIAL_TIMEOUT: Duration = Duration::from_secs(10);
/// All of a Mac's addresses together.
pub const DIAL_OVERALL: Duration = Duration::from_secs(20);

/// Why a dial failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DialReason {
    Timeout,
    Refused,
    Dns,
    /// Another key answered than the one this device pinned.
    WrongMac,
    /// The Mac refused this device's key.
    RefusedKey,
    NotSill,
    Network,
}

/// Why a dial failed, in words the connect screen can show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DialError {
    pub reason: DialReason,
    pub message: String,
}

impl DialError {
    pub fn new(reason: DialReason, message: impl Into<String>) -> DialError {
        DialError { reason, message: message.into() }
    }
}

impl fmt::Display for DialError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for DialError {}

/// What a dial ends with: a TLS 1.3 stream with the negotiated protocol and the Mac's key.
pub struct TlsConnection {
    pub stream: TlsStream<TcpStream>,
    /// SHA-256 of the SubjectPublicKeyInfo of the key that answered.
    pub mac_fingerprint: [u8; 32],
    pub host: String,
    pub port: u16,
}

#[derive(Default)]
struct PinState {
    peer: Option<[u8; 32]>,
    mismatch: bool,
}

/// Pins the Mac's key: the whole of trust. Nothing else about its certificate is looked at.
#[derive(Debug)]
struct PinVerifier {
    pin: Option<Vec<u8>>,
    state: Arc<Mutex<PinState>>,
}

impl fmt::Debug for PinState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PinState(mismatch: {})", self.mismatch)
    }
}

impl ServerCertVerifier for PinVerifier {
    fn verify_server_cert(
        &self, end_entity: &CertificateDer<'_>, _intermediates: &[CertificateDer<'_>], _server_name: &ServerName<'_>,
        _ocsp_response: &[u8], _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let fp = spki::fingerprint_of_certificate(end_entity.as_ref())
            .ok_or_else(|| rustls::Error::General("not a P-256 key".into()))?;
        let mut state = self.state.lock().unwrap();
        state.peer = Some(fp);
        if let Some(pin) = &self.pin {
            if !constant_time_eq(pin, &fp) {
                state.mismatch = true;
                return Err(rustls::Error::General("pin mismatch".into()));
            }
        }
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self, _message: &[u8], _cert: &CertificateDer<'_>, _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::General("TLS 1.3 only".into()))
    }

    fn verify_tls13_signature(
        &self, message: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_p256_signature(message, cert.as_ref(), dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![SignatureScheme::ECDSA_NISTP256_SHA256]
    }
}

/// The handshake's CertificateVerify, checked against the certificate's own P-256 key.
pub(crate) fn verify_p256_signature(message: &[u8], cert: &[u8], dss: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
    if dss.scheme != SignatureScheme::ECDSA_NISTP256_SHA256 {
        return Err(rustls::Error::General("not an ECDSA P-256 signature".into()));
    }
    let point = spki::of_certificate(cert).and_then(spki::point_of_der).ok_or_else(|| rustls::Error::General("not a P-256 key".into()))?;
    signature::UnparsedPublicKey::new(&signature::ECDSA_P256_SHA256_ASN1, point)
        .verify(message, dss.signature())
        .map_err(|_| rustls::Error::General("the handshake signature does not hold".into()))?;
    Ok(HandshakeSignatureValid::assertion())
}

fn client_config(identity: &DeviceIdentity, alpn: &str, pin: Option<&[u8]>, state: Arc<Mutex<PinState>>) -> Result<ClientConfig, rustls::Error> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let chain = vec![CertificateDer::from(identity.certificate_der().to_vec())];
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(identity.pkcs8_der().to_vec()));
    let mut config = ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(PinVerifier { pin: pin.map(<[u8]>::to_vec), state }))
        .with_client_auth_cert(chain, key)?;
    config.alpn_protocols = vec![alpn.as_bytes().to_vec()];
    // Resumption would skip the certificates; a fresh context per dial has nothing to resume anyway.
    config.resumption = rustls::client::Resumption::disabled();
    config.enable_sni = true;
    Ok(config)
}

/// One address: TCP, then TLS 1.3 with `alpn`, pinned to `pin` when given.
pub async fn dial(identity: &DeviceIdentity, host: &str, port: u16, alpn: &str, pin: Option<&[u8]>, timeout: Duration) -> Result<TlsConnection, DialError> {
    let target = host.to_string();
    let addrs: Vec<std::net::SocketAddr> = match tokio::time::timeout(timeout, tokio::net::lookup_host((target.as_str(), port))).await {
        Ok(Ok(a)) => a.collect(),
        Ok(Err(_)) => Vec::new(),
        Err(_) => return Err(DialError::new(DialReason::Timeout, format!("{host} didn’t answer."))),
    };
    if addrs.is_empty() {
        return Err(DialError::new(DialReason::Dns, format!("Couldn’t find {host}. Is Tailscale on?")));
    }
    let deadline = Instant::now() + timeout;
    let mut last: Option<DialError> = None;
    let mut tcp = None;
    for addr in addrs {
        match tokio::time::timeout_at(deadline, TcpStream::connect(addr)).await {
            Ok(Ok(s)) => {
                tcp = Some(s);
                break;
            }
            Ok(Err(e)) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
                last = Some(DialError::new(
                    DialReason::Refused,
                    format!("{host} refused the connection. Is Remote Access on in Sill on the Mac?"),
                ));
            }
            Ok(Err(e)) if e.kind() == std::io::ErrorKind::TimedOut => {
                last = Some(DialError::new(DialReason::Timeout, format!("{host} didn’t answer.")));
            }
            Ok(Err(e)) => last = Some(DialError::new(DialReason::Network, format!("Couldn’t reach {host}: {e}"))),
            Err(_) => {
                last = Some(DialError::new(DialReason::Timeout, format!("{host} didn’t answer.")));
                break;
            }
        }
    }
    let Some(tcp) = tcp else {
        return Err(last.unwrap_or_else(|| DialError::new(DialReason::Network, format!("Couldn’t reach {host}."))));
    };
    let _ = tcp.set_nodelay(true);
    {
        let sock = socket2::SockRef::from(&tcp);
        let _ = sock.set_keepalive(true);
        // DSCP AF41, interactive video.
        #[cfg(unix)]
        if tcp.peer_addr().map(|a| a.is_ipv4()).unwrap_or(false) {
            let _ = sock.set_tos(0x88);
        }
    }
    let state = Arc::new(Mutex::new(PinState::default()));
    let config = client_config(identity, alpn, pin, state.clone()).map_err(|e| DialError::new(DialReason::Network, e.to_string()))?;
    let connector = TlsConnector::from(Arc::new(config));
    let name = ServerName::try_from(SERVER_NAME).expect("a DNS name");
    let stream = match tokio::time::timeout_at(deadline, connector.connect(name, tcp)).await {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => {
            if state.lock().unwrap().mismatch {
                return Err(DialError::new(DialReason::WrongMac, format!("The Mac at {host} isn’t the one this device paired with.")));
            }
            return Err(DialError::new(DialReason::NotSill, format!("{host} didn’t answer as Sill: {e}")));
        }
        Err(_) => return Err(DialError::new(DialReason::Timeout, format!("{host} didn’t answer."))),
    };
    let negotiated = stream.get_ref().1.alpn_protocol().map(|p| String::from_utf8_lossy(p).into_owned());
    if negotiated.as_deref() != Some(alpn) {
        return Err(DialError::new(
            DialReason::NotSill,
            format!("{host} didn’t answer as Sill (ALPN {}).", negotiated.as_deref().unwrap_or("none")),
        ));
    }
    let fp = state.lock().unwrap().peer.ok_or_else(|| DialError::new(DialReason::NotSill, "The Mac sent no P-256 key."))?;
    Ok(TlsConnection { stream, mac_fingerprint: fp, host: host.to_string(), port })
}

fn rank(r: DialReason) -> u8 {
    match r {
        DialReason::WrongMac => 0,
        DialReason::RefusedKey => 1,
        DialReason::Refused => 2,
        DialReason::NotSill => 3,
        DialReason::Network => 4,
        DialReason::Dns => 5,
        DialReason::Timeout => 6,
    }
}

/// Several addresses of one Mac at once (`RemoteConnector`): attempts start 1 s apart, or at once
/// when the one before fails; the first to finish its TLS handshake wins and the rest are closed.
/// `overall` in all; the most telling failure is the one reported.
pub async fn dial_any(identity: &DeviceIdentity, endpoints: &[Endpoint], alpn: &str, pin: Option<&[u8]>, overall: Duration) -> Result<TlsConnection, DialError> {
    if endpoints.is_empty() {
        return Err(DialError::new(DialReason::Network, "No address to dial."));
    }
    let deadline = Instant::now() + overall;
    let mut set: JoinSet<Result<TlsConnection, DialError>> = JoinSet::new();
    let mut next = 0;
    let mut best: Option<DialError> = None;
    let mut next_start = Instant::now();
    loop {
        if next < endpoints.len() && (set.is_empty() || Instant::now() >= next_start) {
            let e = endpoints[next].clone();
            next += 1;
            let (identity, alpn, pin) = (identity.clone(), alpn.to_string(), pin.map(<[u8]>::to_vec));
            set.spawn(async move { dial(&identity, &e.host, e.port, &alpn, pin.as_deref(), DIAL_TIMEOUT).await });
            next_start = Instant::now() + Duration::from_secs(1);
            continue;
        }
        let wake = if next < endpoints.len() { next_start.min(deadline) } else { deadline };
        tokio::select! {
            joined = set.join_next() => {
                match joined {
                    Some(Ok(Ok(conn))) => return Ok(conn),
                    Some(Ok(Err(e))) => {
                        if best.as_ref().map(|b| rank(e.reason) < rank(b.reason)).unwrap_or(true) {
                            best = Some(e);
                        }
                        // The next address at once.
                        next_start = Instant::now();
                    }
                    Some(Err(e)) => {
                        if best.is_none() {
                            best = Some(DialError::new(DialReason::Network, e.to_string()));
                        }
                        next_start = Instant::now();
                    }
                    None => {}
                }
                if set.is_empty() && next >= endpoints.len() {
                    return Err(best.unwrap_or_else(|| DialError::new(DialReason::Timeout, "The Mac didn’t answer.")));
                }
            }
            _ = tokio::time::sleep_until(wake) => {
                if Instant::now() >= deadline {
                    return Err(best.unwrap_or_else(|| DialError::new(DialReason::Timeout, "The Mac didn’t answer.")));
                }
            }
        }
    }
}
