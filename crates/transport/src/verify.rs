//! Deciding whether the device on the other end is one this machine talks to.
//!
//! # What is replaced, and what is emphatically not
//! The web PKI answer — "does a certificate authority vouch for this name?" — is thrown
//! away entirely. There is no authority on a LAN with no coordinator, and a name is not
//! what identifies a machine here. In its place: the presented key must be in the pinned
//! set.
//!
//! What is **not** thrown away is the handshake signature check. Pinning a key proves
//! nothing on its own — anyone can copy a public key out of a packet capture and present
//! it. The proof that the other end *holds the private half* is TLS 1.3's
//! CertificateVerify signature, and both verifiers below run it through rustls' own
//! implementation. Skipping it while keeping the pin would be an authentication bypass
//! that looks, in the code, exactly like authentication.
//!
//! # Both directions
//! A peer relationship is symmetric: whichever machine dialled, both ends must prove
//! who they are. So the server demands a client certificate ([`ClientCertVerifier`]
//! with `client_auth_mandatory`) and applies the same pinning rule to it. Verifying only
//! the server would let any machine on the LAN drive this one's keyboard.

use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{verify_tls13_signature, CryptoProvider};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{DigitallySignedStruct, DistinguishedName, SignatureScheme};
use rustls_pki_types::{CertificateDer, ServerName, UnixTime};
use ultidesk_identity::PeerKey;

use crate::cert::peer_key;

/// Which devices this endpoint will complete a handshake with.
#[derive(Debug, Clone)]
pub enum TrustPolicy {
    /// Only these keys. The normal operating mode.
    Pinned(Vec<PeerKey>),
    /// Accept whatever key is presented, once, so that two machines that have never met
    /// can establish a channel over which to compare a pairing code.
    ///
    /// **This is not trust.** A connection accepted under this policy has proved only
    /// that the other end holds the key it presented — not that it is the machine the
    /// operator meant. Nothing may act on it until
    /// [`crate::PeerConnection::pairing_code`] has been compared on both screens and the
    /// key pinned. It exists as a separate variant, rather than an empty pinned set that
    /// happens to allow everything, so that "we are pairing" can never be the accidental
    /// state of a misconfigured endpoint.
    Pairing,
}

impl TrustPolicy {
    fn admits(&self, key: &PeerKey) -> bool {
        match self {
            TrustPolicy::Pinned(keys) => keys.contains(key),
            TrustPolicy::Pairing => true,
        }
    }

    fn refusal(&self, key: &PeerKey) -> rustls::Error {
        let count = match self {
            TrustPolicy::Pinned(keys) => keys.len(),
            TrustPolicy::Pairing => 0,
        };
        // The fingerprint and not the raw key: it is the form the operator was shown,
        // so it is the form they can act on. The count says whether the answer is
        // "unknown device" or "this machine trusts nobody yet", which are different
        // problems with different fixes.
        rustls::Error::General(format!(
            "peer {} is not paired with this device ({count} paired)",
            key.fingerprint()
        ))
    }
}

/// The pinning rule, in the shape rustls wants on each side of the handshake.
#[derive(Debug)]
pub struct PinnedPeers {
    policy: TrustPolicy,
    provider: Arc<CryptoProvider>,
}

impl PinnedPeers {
    pub fn new(policy: TrustPolicy, provider: Arc<CryptoProvider>) -> Self {
        PinnedPeers { policy, provider }
    }

    fn check(&self, end_entity: &CertificateDer<'_>) -> Result<(), rustls::Error> {
        let key = peer_key(end_entity)
            .map_err(|e| rustls::Error::General(format!("peer certificate rejected: {e}")))?;
        if !self.policy.admits(&key) {
            return Err(self.policy.refusal(&key));
        }
        Ok(())
    }

    /// Ed25519 and nothing else.
    ///
    /// Advertising the full menu would let a peer sign with RSA or ECDSA — which the
    /// pinning check would then have already accepted, because a certificate's key
    /// algorithm and its signature algorithm are separate fields. Narrowing here keeps
    /// the two from drifting apart.
    fn schemes() -> Vec<SignatureScheme> {
        vec![SignatureScheme::ED25519]
    }

    fn verify_1_3(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    /// TLS 1.2 is unreachable over QUIC, which mandates 1.3. Returning an error rather
    /// than `Ok` means that if some future path ever did reach it, it would fail closed.
    fn refuse_1_2() -> Result<HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::General(
            "TLS 1.2 is not accepted; this transport is QUIC/TLS 1.3 only".to_string(),
        ))
    }
}

impl ServerCertVerifier for PinnedPeers {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        // Deliberately unused. The name is `peer.ultidesk.invalid` for every device;
        // it identifies nothing and the key is what is checked. See `cert.rs`.
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        // Deliberately unused: a pinned self-signed key has no meaningful expiry, and
        // inventing one would only make two machines that still trust each other stop
        // talking on an arbitrary day.
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        self.check(end_entity)?;
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Self::refuse_1_2()
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.verify_1_3(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        Self::schemes()
    }
}

impl ClientCertVerifier for PinnedPeers {
    /// No hint list. Hints exist to help a client pick among several certificates from
    /// known issuers; a device has exactly one certificate and there are no issuers.
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    /// A connection without a client certificate is refused outright. This is the line
    /// that stops any machine on the LAN from driving this one.
    fn client_auth_mandatory(&self) -> bool {
        true
    }

    fn offer_client_auth(&self) -> bool {
        true
    }

    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<ClientCertVerified, rustls::Error> {
        self.check(end_entity)?;
        Ok(ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Self::refuse_1_2()
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.verify_1_3(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        Self::schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ultidesk_identity::Identity;

    fn key(seed: u8) -> PeerKey {
        Identity::from_secret_bytes([seed; 32]).public()
    }

    #[test]
    fn a_pinned_key_is_admitted_and_a_stranger_is_not() {
        let policy = TrustPolicy::Pinned(vec![key(1), key(2)]);
        assert!(policy.admits(&key(1)));
        assert!(policy.admits(&key(2)));
        assert!(!policy.admits(&key(3)));
    }

    #[test]
    fn an_empty_pinned_set_admits_nobody() {
        // The failure that would matter most: a machine that has lost its peer store
        // must refuse everything, not accept everything.
        let policy = TrustPolicy::Pinned(Vec::new());
        assert!(!policy.admits(&key(1)));
    }

    #[test]
    fn pairing_admits_anyone_which_is_why_it_is_a_separate_variant() {
        let policy = TrustPolicy::Pairing;
        assert!(policy.admits(&key(9)));
    }

    #[test]
    fn the_refusal_names_the_fingerprint_and_how_many_are_paired() {
        let policy = TrustPolicy::Pinned(vec![key(1)]);
        let message = policy.refusal(&key(4)).to_string();
        assert!(message.contains(&key(4).fingerprint()), "{message}");
        assert!(message.contains("1 paired"), "{message}");
    }

    #[test]
    fn only_ed25519_is_advertised() {
        assert_eq!(PinnedPeers::schemes(), vec![SignatureScheme::ED25519]);
    }

    #[test]
    fn tls_1_2_is_refused_rather_than_waved_through() {
        assert!(PinnedPeers::refuse_1_2().is_err());
    }
}
