# ADR-0012: A device is an Ed25519 key pair, and its id is derived from it

Status: **Accepted** (2026-09-09)

## Context

Every cross-machine feature rests on one question: *which device is on the other end?*
Until now the answer was a random uuid, minted on first run and stored in the settings
file. That was adequate while nothing checked it, and it stopped being adequate the
moment two machines had to authenticate each other:

- A uuid is a label, not a proof. Any machine can present any id.
- So a peer could not be *named* or *saved* — there was nothing to save that meant
  anything the next time. This is why every peer panel in the control app was a
  placeholder, and why [ADR-0002](0002-control-transport.md)'s pinned-identity transport
  had nothing to pin to.

[ADR-0002](0002-control-transport.md) already committed to "pinned paired-device
identity" and Ed25519. This ADR decides the shape of that identity, how the existing
`DeviceId` relates to it, and how trust is established and stored.

## Decision

**A device is an Ed25519 key pair**, generated on first run and stored in the per-user
configuration directory. `ed25519-dalek` is used for the primitive; nothing here
implements cryptography.

**`DeviceId` is derived from the public key**, not drawn at random:
`SHA-256("ultidesk-device-id-v1" || pubkey)`, truncated to 16 bytes and stamped as an
RFC 4122 **version 8** uuid. Version 8 is the slot reserved for vendor-specific
derivation, so the id is honestly labelled as derived rather than passing itself off as
random. The existing `DeviceId` type is unchanged, so nothing that routes by id had to
be rewritten — but an id can no longer be claimed by a machine that does not hold the
private half.

**Trust is a pinned list of public keys**, keyed by key and never by name. A name is
whatever the other end says it is; two machines may claim the same one and a hostile one
will claim yours. The name exists to be shown to the operator.

**Pairing is confirmed by a six-digit code** derived from both public keys — sorted, so
both ends compute the same one — plus the TLS channel binding. A machine in the middle
must use its own key with at least one side, which changes that side's code.

**Three failure directions are decided deliberately, and they are not the same:**

| What is damaged | Response | Why |
|---|---|---|
| The identity file | **Refuse and stop**, leaving the bytes on disk | Silently re-keying changes who this machine *is* and un-pairs every peer, with no error anywhere. The operator would see "not paired" on both machines and have no reason to suspect the key. |
| The peer store | Load empty, move the file aside | Trusting nobody fails closed and is immediately visible. Carrying on with a partial parse would silently trust or distrust one specific peer, which is not. |
| The settings file | Start fresh (unchanged from before) | Losing an arrangement is annoying and recoverable. |

**Small-order public keys are rejected.** They decode as valid points — the all-zero
encoding is one — so a check that only asked "does this parse" would let a device pin a
key under which the permissive Ed25519 verification equation accepts signatures nobody
produced.

## Consequences

- The control app and the agent agree on this machine's id, because both derive it from
  the same key file. Config-directory resolution moved into `ultidesk-core::paths` so
  there is one rule rather than one per process.
- Settings files written before this change name a random id. `Settings::adopt_device_id`
  re-keys them, rewriting only the audio endpoints that belonged to *this* machine — a
  blanket replacement would drag a peer's endpoints onto the local id.
- The key is stored as hex in `identity.json`, created `0600` on Unix. **Windows has no
  mode bits and the file is not yet ACL-restricted**; that is the same outstanding
  hardening the named pipe has, tracked in [threat-model.md](../threat-model.md).
  Moving the key into DPAPI / the Secret Service is Milestone-1 work and this ADR does
  not claim it.
- Nothing here opens a socket. Proving possession of a key is the transport's job; this
  layer supplies the material, the pinning decision and the pairing code, so the parts
  that must be right independently of any network are testable without one.

## Alternatives rejected

**Keep the random uuid and authenticate separately.** Two identities for one device —
the uuid it routes under and the key it authenticates with — and every place that has
both has to keep them in agreement. The failure is silent whenever they diverge.

**A UUIDv5 over the key.** v5 is defined as SHA-1 in a namespace. Using v5 while
computing SHA-256 would be a false claim about the encoding; v8 exists precisely for
this.

**Fingerprint as a word list (PGP-style) or base32.** Rejected for hex: the alphabet
needs no legend, and the fingerprint is read off one screen and compared on another
rather than typed.

**A four-digit pairing code.** One in ten thousand per attempt is within reach of an
attacker who can force repeated pairing attempts. Six digits, grouped in threes.

**Certificate chains / a local CA.** Ultidesk is LAN-only with no coordinator
(ADR-0002). A CA adds an issuing authority, a revocation surface and an expiry story to
solve a problem two pinned keys already solve.
