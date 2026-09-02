# ADR-0002: Control transport — direct Rust↔Rust QUIC over TLS 1.3

- Status: Accepted (target); not yet implemented
- Date: 2026-07-31

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
