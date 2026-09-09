//! Wrapping the device key in the X.509 envelope TLS insists on carrying, and reading
//! it back out.
//!
//! # The certificate is a container, not a trust statement
//! TLS 1.3 has no way to present a bare public key without an extension neither `rustls`
//! nor most stacks implement, so the device's Ed25519 key travels inside a self-signed
//! certificate. Nothing about that certificate is trusted: not its issuer, not its
//! subject, not its validity window, not its SANs. The only thing read out of it is the
//! 32-byte public key, which is then compared against the pinned set.
//!
//! Saying that plainly matters, because the usual X.509 instincts are all wrong here:
//!
//! - **Expiry is not checked.** A pinned key is the identity; an expiry date on a
//!   self-signed wrapper would only invent a day on which two machines that still trust
//!   each other stop talking. Revocation is `PeerStore::forget`, which is a decision
//!   someone makes rather than a clock running out.
//! - **The subject name is not checked.** The client passes a fixed server name because
//!   `quinn` requires one; it identifies nothing, and the verifier ignores it.
//!
//! # Why the key is checked on the way out
//! `peer_key` re-validates the bytes as a curve point through
//! [`PeerKey::verifying_key`], so a certificate carrying 32 bytes that are not a usable
//! key is rejected here rather than becoming a `PeerKey` that can be compared, stored
//! and displayed but never used.

use rustls_pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use ultidesk_identity::{Identity, PeerKey};

use crate::TransportError;

/// The name the client asks for and the certificate carries.
///
/// `.invalid` is reserved by RFC 2606 precisely so that it can never resolve to a real
/// host. It is a placeholder for an API that demands a name, not an identity — the
/// identity is the key.
pub const SERVER_NAME: &str = "peer.ultidesk.invalid";

/// Build the certificate and private key this device presents, in both directions.
///
/// The same pair is used as the server certificate and as the client certificate: a
/// peer is a peer, and which end dialled is an accident of who moved their mouse first.
pub fn certificate_for(
    identity: &Identity,
) -> Result<(CertificateDer<'static>, PrivateKeyDer<'static>), TransportError> {
    let pkcs8 = identity.to_pkcs8_der();
    let key_pair = rcgen::KeyPair::from_pkcs8_der_and_sign_algo(
        &PrivatePkcs8KeyDer::from(pkcs8.clone()),
        &rcgen::PKCS_ED25519,
    )
    .map_err(|e| TransportError::Certificate(format!("could not load the device key: {e}")))?;

    let mut params = rcgen::CertificateParams::new(vec![SERVER_NAME.to_string()])
        .map_err(|e| TransportError::Certificate(format!("bad certificate parameters: {e}")))?;
    // The fingerprint, so that a certificate dumped by a packet capture or an error
    // message can be matched against what the operator was shown. It is a label; the
    // key beside it is what is actually compared.
    params.distinguished_name = rcgen::DistinguishedName::new();
    params.distinguished_name.push(
        rcgen::DnType::CommonName,
        format!("ultidesk {}", identity.fingerprint()),
    );

    let cert = params
        .self_signed(&key_pair)
        .map_err(|e| TransportError::Certificate(format!("could not self-sign: {e}")))?;

    Ok((
        cert.der().clone(),
        PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(pkcs8)),
    ))
}

/// Read the device key out of a presented certificate.
///
/// A certificate whose key is any other algorithm is refused rather than coerced: this
/// project has exactly one identity algorithm, and a peer offering something else is not
/// a peer this build can pin.
pub fn peer_key(cert: &CertificateDer<'_>) -> Result<PeerKey, TransportError> {
    use x509_parser::prelude::FromDer;

    let (_rest, parsed) = x509_parser::certificate::X509Certificate::from_der(cert.as_ref())
        .map_err(|e| TransportError::Certificate(format!("unparseable certificate: {e}")))?;

    let spki = parsed.public_key();
    if spki.algorithm.algorithm != x509_parser::oid_registry::OID_SIG_ED25519 {
        return Err(TransportError::Certificate(format!(
            "peer key algorithm is {}, not Ed25519",
            spki.algorithm.algorithm
        )));
    }

    let raw = spki.subject_public_key.data.as_ref();
    let bytes: [u8; 32] = raw.try_into().map_err(|_| {
        TransportError::Certificate(format!(
            "peer key is {} bytes, not the 32 an Ed25519 key has",
            raw.len()
        ))
    })?;

    let key = PeerKey::from_bytes(bytes);
    if key.verifying_key().is_none() {
        return Err(TransportError::Certificate(
            "peer key is not a usable Ed25519 point".to_string(),
        ));
    }
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_certificate_carries_the_device_key_unchanged() {
        // The whole mechanism: what goes in as an identity comes back out as the same
        // key, so pinning can compare it against a stored one.
        let identity = Identity::generate();
        let (cert, _key) = certificate_for(&identity).unwrap();
        assert_eq!(peer_key(&cert).unwrap(), identity.public());
    }

    #[test]
    fn two_devices_produce_different_certificates() {
        let a = Identity::generate();
        let b = Identity::generate();
        let (cert_a, _) = certificate_for(&a).unwrap();
        let (cert_b, _) = certificate_for(&b).unwrap();
        assert_ne!(peer_key(&cert_a).unwrap(), peer_key(&cert_b).unwrap());
    }

    #[test]
    fn a_certificate_is_reproducible_from_the_same_identity() {
        // Not byte-identical — serial numbers and validity differ per call — but the
        // key it carries must not.
        let identity = Identity::from_secret_bytes([21u8; 32]);
        let (first, _) = certificate_for(&identity).unwrap();
        let (second, _) = certificate_for(&identity).unwrap();
        assert_eq!(peer_key(&first).unwrap(), peer_key(&second).unwrap());
    }

    #[test]
    fn rubbish_is_refused_rather_than_parsed_into_a_key() {
        let junk = CertificateDer::from(vec![0x30, 0x00, 0xff, 0xff]);
        assert!(peer_key(&junk).is_err());
    }
}
