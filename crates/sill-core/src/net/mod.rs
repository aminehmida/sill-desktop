//! The network: the doors' TLS, pairing, the session, the saved Macs, discovery at home.

pub mod connection_list;
pub mod discovery_policy;
pub mod home_discovery;
pub mod pairer;
pub mod saved_macs;
pub mod session;
pub mod tls;

pub use connection_list::ConnectionList;
pub use discovery_policy::{DiscoveryPolicy, Due, ReconnectClock, ReconnectStep};
pub use home_discovery::{HomeDiscovery, HomeDoor, HomeMac, SERVICE_TYPE};
pub use pairer::{AskOutcome, PairOutcome};
pub use saved_macs::{DialOrder, Endpoint, SavedMac, SavedMacs};
pub use session::{LinkStats, Session, SessionListener};
pub use tls::{DialError, DialReason, TlsConnection, PAIRING_ALPN, SERVER_NAME, SESSION_ALPN};
