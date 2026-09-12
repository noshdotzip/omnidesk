//! The short code an operator compares across two screens when pairing.
//!
//! # What it is actually protecting against
//! The transport already proves that whoever is on the other end holds the private key
//! for the public key it presented. What it cannot prove is that the key belongs to the
//! machine the operator *meant* — on a first connection there is nothing to compare it
//! against, which is where a machine-in-the-middle lives: it completes one authenticated
//! session with each side using its own key, and both sides see a valid connection.
//!
//! The code below is derived from **both** public keys and from the TLS channel, so the
//! attacker's two sessions have two different keys and therefore two different codes.
//! A human reading six digits off one screen and comparing them on the other is what
//! closes it — this is the same shape as Bluetooth numeric comparison and ZRTP's SAS.
//!
//! # Why it is symmetric
//! The keys are sorted before hashing, so the initiator and the accepter compute the
//! same code. A scheme keyed by role produces two different codes on the two screens,
//! and the operator cannot tell that from an attack.
//!
//! # Why six digits and not four
//! Six digits is one guess in a million per attempt, and the attempt is not repeatable:
//! the operator either sees a match or tears the pairing down. Four digits is one in ten
//! thousand, which is within reach of a persistent attacker who can force repeated
//! pairing attempts. Digits rather than words so it can be read aloud over a phone in
//! any language.

use sha2::{Digest, Sha256};

use crate::key::PeerKey;

const SAS_LABEL: &[u8] = b"ultidesk-pairing-sas-v1";

/// Number of digits shown. Changing this changes the code both peers compute, so it is
/// a wire-visible constant, not a display preference.
pub const SAS_DIGITS: u32 = 6;

/// Derive the pairing code for a connection between two devices.
///
/// `channel_binding` should be TLS exported keying material for the live connection.
/// Passing an empty slice still yields a code that binds the two identities — enough to
/// detect a substituted key — but not one that binds *this* connection, so a relay that
/// re-used a genuine handshake would go unnoticed. Callers that can bind, must.
pub fn pairing_code(a: &PeerKey, b: &PeerKey, channel_binding: &[u8]) -> String {
    let (first, second) = if a <= b { (a, b) } else { (b, a) };

    let mut hasher = Sha256::new();
    hasher.update(SAS_LABEL);
    hasher.update(first.as_bytes());
    hasher.update(second.as_bytes());
    // Length-prefixed so that a binding of "ab" + a key cannot collide with "a" + a
    // different key: without it the fields run together and the digest stops
    // identifying which bytes came from where.
    hasher.update((channel_binding.len() as u64).to_be_bytes());
    hasher.update(channel_binding);
    let digest = hasher.finalize();

    // Take the value from the top eight bytes rather than one byte per digit: reducing
    // a whole 64-bit value modulo 10^6 leaves a bias far below what six digits can
    // express, whereas `digest[i] % 10` discards most of each byte and skews low.
    let mut value = u64::from_be_bytes(digest[..8].try_into().expect("8 bytes"));
    value %= 10u64.pow(SAS_DIGITS);

    let digits = format!("{value:0width$}", width = SAS_DIGITS as usize);
    // Grouped in threes; a six-digit run is read wrong often enough to matter when the
    // whole mechanism depends on a human comparing it correctly.
    format!("{} {}", &digits[..3], &digits[3..])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::key::Identity;

    fn key(seed: u8) -> PeerKey {
        Identity::from_secret_bytes([seed; 32]).public()
    }

    #[test]
    fn both_peers_compute_the_same_code_whichever_way_round_they_ask() {
        let (a, b) = (key(1), key(2));
        assert_eq!(
            pairing_code(&a, &b, b"channel"),
            pairing_code(&b, &a, b"channel"),
            "a role-dependent code would show two different numbers on the two screens"
        );
    }

    #[test]
    fn a_substituted_key_changes_the_code() {
        // This is the property the whole mechanism rests on: a machine in the middle
        // has to use its own key with at least one side.
        let (a, b, attacker) = (key(1), key(2), key(3));
        assert_ne!(
            pairing_code(&a, &b, b"channel"),
            pairing_code(&a, &attacker, b"channel")
        );
    }

    #[test]
    fn a_different_connection_gives_a_different_code() {
        let (a, b) = (key(1), key(2));
        assert_ne!(
            pairing_code(&a, &b, b"session-one"),
            pairing_code(&a, &b, b"session-two")
        );
    }

    #[test]
    fn the_channel_binding_cannot_be_shifted_into_the_keys() {
        // Without the length prefix, concatenation is ambiguous and two different
        // pairings could hash identically.
        let (a, b) = (key(1), key(2));
        assert_ne!(pairing_code(&a, &b, b"ab"), pairing_code(&a, &b, b"a"));
    }

    #[test]
    fn the_code_is_six_digits_in_two_groups() {
        let code = pairing_code(&key(4), &key(5), b"");
        assert_eq!(code.len(), 7, "{code}");
        assert_eq!(&code[3..4], " ");
        assert!(code.replace(' ', "").chars().all(|c| c.is_ascii_digit()));
    }

    #[test]
    fn a_low_value_keeps_its_leading_zeros() {
        // Formatting without the width would show "42" and the operator would compare a
        // two-character string against a six-character one.
        let mut seen_short = false;
        for seed in 0..40u8 {
            let code = pairing_code(&key(seed), &key(seed.wrapping_add(1)), b"");
            assert_eq!(code.replace(' ', "").len(), SAS_DIGITS as usize, "{code}");
            seen_short |= code.starts_with('0');
        }
        // Not an assertion about `seen_short` — 40 samples need not contain one. The
        // loop above is the real check; this keeps the intent visible.
        let _ = seen_short;
    }

    #[test]
    fn the_code_is_stable_across_runs() {
        // Pairing is compared across two machines and two processes, so the derivation
        // must not depend on anything but its inputs.
        assert_eq!(
            pairing_code(&key(9), &key(10), b"cb"),
            pairing_code(&key(9), &key(10), b"cb")
        );
    }
}
