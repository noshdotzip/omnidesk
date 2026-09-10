//! `ultidesk-identity` — who this machine is, and which machines it trusts.
//!
//! Everything cross-machine in Ultidesk depends on a question this crate answers:
//! *which device is on the other end?* Until now the answer was a random uuid stored in
//! the settings file, which is to say there was no answer — a uuid is a label anyone can
//! copy, so a "peer" could not be named, saved, or trusted, and the control app's peer
//! panels were placeholders because there was nothing real to show.
//!
//! Here a device is an **Ed25519 key pair**. Its [`DeviceId`](ultidesk_core::DeviceId)
//! is derived from the public key rather than drawn at random, so it cannot be claimed
//! by a machine that does not hold the private half. Trust is a pinned list of public
//! keys ([`PeerStore`]), established once through a code the operator compares on both
//! screens ([`sas::pairing_code`]).
//!
//! # What this crate is not
//! It does no networking and opens no sockets. Proving possession of a key is the
//! transport's job (ADR-0002: QUIC with TLS 1.3, pinned to these identities); this crate
//! supplies the key material, the pinning decision and the pairing code, so that the
//! parts which must be right independently of any network can be tested without one.
//!
//! Ed25519 is used through `ed25519-dalek` and no cryptography is invented here — the
//! only original construction is the domain-separated derivation of the device id, the
//! fingerprint and the pairing code, each of which is pinned by a test.

/// Small-state-file handling: atomic replacement, and `0600` for files holding a
/// secret. Public because the agent's handshake file has the same two needs — it carries
/// the local IPC token — and a second copy of the rule is a second chance to get the
/// mode wrong.
pub mod file;
mod hex;
pub mod key;
pub mod peers;
pub mod sas;
pub mod store;

#[cfg(test)]
mod test_support;

pub use key::{Identity, PeerKey};
pub use peers::{PairedPeer, PeerStore, Permissions, PinError, PinOutcome, UnknownPermission};
pub use sas::pairing_code;
pub use store::{load_or_create, IdentityError, LoadedIdentity};
