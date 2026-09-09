//! `ultidesk-transport` — the authenticated peer channel of [ADR-0002].
//!
//! QUIC over TLS 1.3, mutually authenticated, pinned to the Ed25519 device identities in
//! `ultidesk-identity`. It replaces the plaintext TCP transport in
//! `crates/agent/src/tcp.rs`, which carried every keystroke in the clear behind a shared
//! token and was never anything but a bench tool.
//!
//! Three properties are the whole point, and each is enforced in a named place rather
//! than assumed:
//!
//! - **The other end is who it claims to be.** Its certificate must carry a pinned
//!   public key ([`TrustPolicy`]), *and* it must prove possession of the matching
//!   private key through the TLS 1.3 CertificateVerify signature. `verify.rs` does both;
//!   doing only the first would be an authentication bypass that reads like
//!   authentication.
//! - **Both ends prove it.** Client certificates are mandatory. Verifying only the
//!   dialled machine would let anything on the LAN drive this one's keyboard.
//! - **A first meeting is not trust.** [`TrustPolicy::Pairing`] completes a channel with
//!   an unknown key so a six-digit code can be compared on both screens, and is a
//!   distinct variant precisely so it cannot be the accidental state of a misconfigured
//!   endpoint.
//!
//! # What this crate does not decide
//! It has no opinion about the bytes it carries. Framing is length-prefixed and bounded
//! ([`MessageStream`]); what goes inside is the agent's protocol. Keeping the two apart
//! is what lets the security properties above be tested over a loopback socket without a
//! second machine, a desktop, or any input being injected anywhere.
//!
//! [ADR-0002]: ../../../docs/adrs/0002-control-transport.md

mod cert;
mod endpoint;
mod frame;
mod verify;

use std::net::SocketAddr;

pub use cert::SERVER_NAME;
pub use endpoint::{PeerConnection, PeerEndpoint};
pub use frame::MessageStream;
pub use verify::TrustPolicy;

#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("could not bind {addr}: {source}")]
    Bind {
        addr: SocketAddr,
        #[source]
        source: std::io::Error,
    },
    #[error("TLS configuration failed: {0}")]
    Tls(String),
    #[error("certificate problem: {0}")]
    Certificate(String),
    /// Covers the refusal that matters most: a peer whose key is not pinned. The message
    /// carries the fingerprint so the operator can tell "wrong machine" from "not paired
    /// yet".
    #[error("handshake failed: {0}")]
    Handshake(String),
    #[error("stream error: {0}")]
    Stream(String),
    #[error("message is {len} bytes, over the {limit}-byte limit")]
    MessageTooLarge { len: usize, limit: usize },
}
