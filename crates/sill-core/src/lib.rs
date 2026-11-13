//! The protocol core of the Sill desktop client: Sill streams a Mac's windows to other devices,
//! and this crate speaks the device's half of its wire protocol (Sill for Mac 0.6.1), with no
//! GUI, decoder or audio device of its own.
//!
//! * [`protocol`]: framing, message kinds, JSON payloads, blobs, pure policies.
//! * [`crypto`]: pairing math, the device's identity, where its key is kept.
//! * [`net`]: the doors' TLS, pairing, the session, the saved Macs, discovery.
//! * [`client`]: [`Client`], the handle a GUI drives.

pub mod client;
pub mod crypto;
pub mod net;
pub mod protocol;

pub use client::{Client, ClientConfig, ClientEvent, ClientState, MouseButton, Phase, VideoFeedback, VideoSink};
