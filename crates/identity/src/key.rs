//! This machine's Ed25519 identity, and the public half of a peer's.
//!
//! # Why the device id is derived rather than random
//! Until now a [`DeviceId`] was a random uuid minted on first run and stored beside
//! the settings. That worked because nothing checked it. It stops working the moment
//! two machines authenticate each other: a random id says nothing about *who* is on
//! the other end, so a peer could claim any id it liked and the name in the UI would
//! be whatever it asserted.
//!
//! Here the id is a function of the public key. A device cannot present an id it does
//! not hold the private key for, and re-keying a machine produces a visibly different
//! id rather than silently inheriting the old one's trust.

use ed25519_dalek::{SigningKey, VerifyingKey};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};
use ultidesk_core::DeviceId;
use uuid::Uuid;

use crate::hex;

/// Domain separation. Every digest in this crate is prefixed with its own label so a
/// hash computed for one purpose can never be presented as another — a device id and
/// a fingerprint are derived from the same 32 bytes and must not collide.
const DEVICE_ID_LABEL: &[u8] = b"ultidesk-device-id-v1";
const FINGERPRINT_LABEL: &[u8] = b"ultidesk-fingerprint-v1";

/// The public half of a device identity: a raw Ed25519 verifying key.
///
/// Ordered so that two peers can sort a pair of keys identically and derive the same
/// pairing code regardless of which of them initiated the connection.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PeerKey([u8; 32]);

impl PeerKey {
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        PeerKey(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Parse the 64-character lower-case hex form written to disk and shown in logs.
    pub fn parse(text: &str) -> Option<Self> {
        let bytes = hex::decode(text)?;
        let bytes: [u8; 32] = bytes.try_into().ok()?;
        Some(PeerKey(bytes))
    }

    /// The key as a curve point, or `None` if these 32 bytes are not one this device
    /// should ever talk to.
    ///
    /// Two separate refusals, and the second matters more than it looks:
    ///
    /// - **Not a point at all.** 32 arbitrary bytes need not decode, and a peer that
    ///   presents garbage should be refused at the door rather than at the first
    ///   signature check, where the failure reads as "signature invalid" and sends the
    ///   operator looking in the wrong place.
    /// - **A point of small order.** These *do* decode — the all-zero encoding is a
    ///   valid point, which is exactly why checking only the first condition is not
    ///   enough. Under a small-order public key the permissive Ed25519 verification
    ///   equation accepts signatures nobody had to produce, so a device pinned to one
    ///   would trust anything. Nothing legitimate ever generates one.
    pub fn verifying_key(&self) -> Option<VerifyingKey> {
        let key = VerifyingKey::from_bytes(&self.0).ok()?;
        if key.is_weak() {
            return None;
        }
        Some(key)
    }

    /// The routing id derived from this key.
    ///
    /// Encoded as an RFC 4122 version 8 uuid — the version reserved for
    /// vendor-specific derivation — so that it is honestly labelled as *derived*
    /// rather than passing itself off as a random v4.
    pub fn device_id(&self) -> DeviceId {
        let mut hasher = Sha256::new();
        hasher.update(DEVICE_ID_LABEL);
        hasher.update(self.0);
        let digest = hasher.finalize();

        let mut bytes = [0u8; 16];
        bytes.copy_from_slice(&digest[..16]);
        bytes[6] = (bytes[6] & 0x0f) | 0x80; // version 8
        bytes[8] = (bytes[8] & 0x3f) | 0x80; // RFC 4122 variant
        DeviceId::from_uuid(Uuid::from_bytes(bytes))
    }

    /// A short form for a human to compare across two screens.
    ///
    /// Hex rather than a word list or base32: the alphabet is one every operator
    /// already reads without a legend. 80 bits, grouped in fours so the eye can check
    /// a group at a time instead of scanning twenty characters.
    pub fn fingerprint(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(FINGERPRINT_LABEL);
        hasher.update(self.0);
        let digest = hasher.finalize();

        let text = hex::encode(&digest[..10]).to_uppercase();
        text.as_bytes()
            .chunks(4)
            .map(|c| std::str::from_utf8(c).expect("hex is ascii"))
            .collect::<Vec<_>>()
            .join("-")
    }
}

impl std::fmt::Display for PeerKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", hex::encode(&self.0))
    }
}

impl std::fmt::Debug for PeerKey {
    /// Prints the fingerprint, not the raw key.
    ///
    /// A public key is not a secret, but a log line carrying 64 hex characters is
    /// unreadable, and the fingerprint is what the operator was shown during pairing —
    /// so it is the form that can actually be matched against something.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "PeerKey({})", self.fingerprint())
    }
}

impl Serialize for PeerKey {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&hex::encode(&self.0))
    }
}

impl<'de> Deserialize<'de> for PeerKey {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let text = String::deserialize(d)?;
        PeerKey::parse(&text).ok_or_else(|| {
            serde::de::Error::custom("expected 64 hex characters of Ed25519 public key")
        })
    }
}

/// This machine's key pair.
///
/// Holds the private key, so it is deliberately not `Clone`, not `Serialize`, and its
/// `Debug` shows only the public fingerprint.
pub struct Identity {
    signing: SigningKey,
}

impl Identity {
    /// Mint a new identity from the operating system's entropy source.
    pub fn generate() -> Self {
        // `OsRng` reads the OS CSPRNG on every call and cannot be seeded by us, which
        // is the point: an identity generated from a reproducible RNG would be
        // reproducible by anyone who knew the seed.
        let signing = SigningKey::generate(&mut rand_core::OsRng);
        Identity { signing }
    }

    pub fn from_secret_bytes(secret: [u8; 32]) -> Self {
        Identity {
            signing: SigningKey::from_bytes(&secret),
        }
    }

    /// The private scalar, for the store to persist. Crate-private on purpose: the
    /// only two things that may see it are the key file and the TLS layer.
    pub(crate) fn secret_bytes(&self) -> [u8; 32] {
        self.signing.to_bytes()
    }

    pub fn public(&self) -> PeerKey {
        PeerKey(self.signing.verifying_key().to_bytes())
    }

    pub fn device_id(&self) -> DeviceId {
        self.public().device_id()
    }

    pub fn fingerprint(&self) -> String {
        self.public().fingerprint()
    }

    /// PKCS#8 DER, the form a TLS stack wants when it is handed a private key.
    ///
    /// Exported here rather than reconstructed by the transport so that exactly one
    /// place in the workspace touches the private key's encoding.
    pub fn to_pkcs8_der(&self) -> Vec<u8> {
        use ed25519_dalek::pkcs8::EncodePrivateKey;
        self.signing
            .to_pkcs8_der()
            // Encoding a key we already hold cannot fail for any reason a caller could
            // act on; a failure here would mean the key is not a key.
            .expect("an Ed25519 signing key always encodes as PKCS#8")
            .as_bytes()
            .to_vec()
    }
}

impl std::fmt::Debug for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Identity({})", self.fingerprint())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_device_id_is_a_function_of_the_key() {
        let id = Identity::from_secret_bytes([7u8; 32]);
        let same = Identity::from_secret_bytes([7u8; 32]);
        assert_eq!(id.device_id(), same.device_id());

        let other = Identity::from_secret_bytes([8u8; 32]);
        assert_ne!(
            id.device_id(),
            other.device_id(),
            "a different key must not resolve to the same device"
        );
    }

    #[test]
    fn the_derived_uuid_is_labelled_as_derived() {
        // Version 8 is the RFC 4122 slot for a vendor-derived uuid. Claiming v4 would
        // assert randomness this id does not have.
        let id = Identity::from_secret_bytes([3u8; 32]).device_id();
        assert_eq!(id.as_uuid().get_version_num(), 8);
        assert_eq!(id.as_uuid().get_variant(), uuid::Variant::RFC4122);
    }

    #[test]
    fn the_fingerprint_and_the_device_id_do_not_share_a_digest() {
        // Both are SHA-256 over the same 32 bytes; only the domain label separates
        // them, so this is the test that the label is actually applied.
        let key = Identity::from_secret_bytes([5u8; 32]).public();
        let fingerprint_hex = key.fingerprint().replace('-', "").to_lowercase();
        let id_hex = hex::encode(key.device_id().as_uuid().as_bytes());
        assert!(!id_hex.starts_with(&fingerprint_hex[..8]));
    }

    #[test]
    fn the_fingerprint_is_grouped_and_stable() {
        let key = Identity::from_secret_bytes([1u8; 32]).public();
        let fp = key.fingerprint();
        assert_eq!(fp.len(), 24, "20 hex characters in 5 groups of 4");
        assert_eq!(fp.matches('-').count(), 4);
        assert_eq!(fp, key.fingerprint());
    }

    #[test]
    fn a_peer_key_round_trips_through_text_and_json() {
        let key = Identity::generate().public();
        assert_eq!(PeerKey::parse(&key.to_string()), Some(key));

        let json = serde_json::to_string(&key).unwrap();
        let back: PeerKey = serde_json::from_str(&json).unwrap();
        assert_eq!(back, key);
    }

    #[test]
    fn a_malformed_peer_key_is_refused_rather_than_padded() {
        assert_eq!(PeerKey::parse("00"), None, "too short");
        assert_eq!(PeerKey::parse(&"0".repeat(65)), None, "odd length");
        assert!(serde_json::from_str::<PeerKey>("\"nonsense\"").is_err());
    }

    #[test]
    fn thirty_two_arbitrary_bytes_are_not_necessarily_a_public_key() {
        // Decodes as hex, does not decode as a curve point.
        assert!(PeerKey::from_bytes([2u8; 32]).verifying_key().is_none());
        assert!(Identity::generate().public().verifying_key().is_some());
    }

    #[test]
    fn a_small_order_key_is_refused_even_though_it_decodes() {
        // The trap: all-zero bytes *are* a valid point, so a check that only asked
        // "does this decode" would pin a key under which signature verification
        // accepts forgeries.
        let zero = PeerKey::from_bytes([0u8; 32]);
        assert!(VerifyingKey::from_bytes(zero.as_bytes()).is_ok());
        assert!(zero.verifying_key().is_none());
    }

    #[test]
    fn debug_output_never_carries_the_private_key() {
        // A panic message or a stray `dbg!` must not be able to leak the identity.
        let id = Identity::from_secret_bytes([9u8; 32]);
        let printed = format!("{id:?}");
        assert!(!printed.contains(&hex::encode(&id.secret_bytes())));
        assert!(printed.contains(&id.fingerprint()));
    }

    #[test]
    fn the_pkcs8_export_round_trips_back_to_the_same_identity() {
        use ed25519_dalek::pkcs8::DecodePrivateKey;
        let id = Identity::generate();
        let der = id.to_pkcs8_der();
        let back = SigningKey::from_pkcs8_der(&der).unwrap();
        assert_eq!(back.to_bytes(), id.secret_bytes());
    }
}
