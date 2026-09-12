# ADR-0002: Control transport — direct Rust↔Rust QUIC over TLS 1.3

- Status: **Accepted; implemented 2026-09-09** (`crates/transport`)
- Date: 2026-07-31, implementation recorded 2026-09-09

## Context

Ultidesk is LAN-only for the MVP. The control plane must carry reliable control/clipboard/
state/session messages and high-frequency, latency-sensitive pointer motion, be mutually
authenticated to a pinned device identity, and never involve a cloud coordinator or relay.

## Decision

Direct peer-to-peer **QUIC** with **TLS 1.3** and pinned paired-device identity:

- Reliable streams for control, clipboard metadata, state transitions, session commands,
  and (dedicated streams) file data.
- Datagrams / an unordered channel for pointer motion.
- Application-level protocol version + capability negotiation.
- SDP/ICE/DTLS fingerprints for the media plane are exchanged over this authenticated
  channel; unauthenticated media sessions are refused.

We do **not** invent cryptography; we use vetted QUIC/TLS libraries and Ed25519 identities.

## Consequences

- One connection multiplexes ordered control and unordered input — good latency behavior.
- Requires a mature Rust QUIC stack; the choice of library is a Milestone-1 sub-decision.
- Until implemented, the projection slice uses a dev loopback broker (ADR-0003 context)
  that never touches the network and is replaced in Milestone 1.

## What was actually built (2026-09-09)

`quinn` 0.11 over `rustls` 0.23 with the **ring** provider, `rcgen` for the self-signed
certificate that carries the device's Ed25519 key ([ADR-0012](0012-device-identity.md)),
and a custom verifier that pins by raw public key on both sides of the handshake.

Decisions this ADR did not anticipate, recorded because they are load-bearing:

- **The certificate is a container, not a trust statement.** TLS 1.3 has no practical way
  to present a bare public key, so the identity travels inside a self-signed wrapper
  whose issuer, subject, SANs and validity window are all ignored. Only the 32-byte key
  is read out. Expiry in particular is *deliberately* unchecked: a pinned key is the
  identity, and an expiry date would only invent a day on which two machines that still
  trust each other stop talking.
- **Pinning does not replace the signature check.** The verifier does both — the key must
  be pinned *and* the peer must prove possession through the TLS 1.3 CertificateVerify
  signature. Doing only the first is an authentication bypass that reads like
  authentication.
- **Client certificates are mandatory**, so the check is symmetric. A peer relationship
  has no client and no server; which end dialled is an accident.
- **Version negotiation moved into ALPN**, derived from `PROTOCOL_VERSION`. A mismatch is
  then a refused handshake rather than an error message exchanged over a channel that
  should not have opened, and the QUIC path needs no `Hello` at all.
- **`TrustPolicy::Pairing` is a distinct variant**, not an empty pinned set that happens
  to admit everything, so "accept anyone" cannot be the accidental state of a
  misconfigured endpoint.

### Not built yet

- **Datagrams for pointer motion.** The ADR calls for an unordered channel and QUIC
  provides one; today everything rides the ordered control stream. It matters once the
  KVM daemon streams motion, not before.
- **Media-plane fingerprint exchange.** There is no media plane yet (ADR-0003).
- **Discovery.** A peer's address is typed in. mDNS is Milestone-1 work that this channel
  now has something to authenticate.
