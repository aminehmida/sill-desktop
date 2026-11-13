//! Pairing and identity: the Mac ID, SPKI pins, the typed code, the proofs, the recognition tag,
//! `sill://pair` links (`Pairing.swift`, `RemoteIdentity.swift`), and this device's own key.

pub mod identity;
pub mod keystore;
pub mod pairing;

pub use identity::*;
pub use keystore::*;
pub use pairing::*;
