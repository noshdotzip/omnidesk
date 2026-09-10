# Ultidesk protocols

Two contracts exist: the **local IPC** (desktop app ↔ agent) and the **peer protocol**
(device ↔ device). The local IPC and the peer *transport* are implemented; the wider peer
message schema below is not.

## Versioning

`PROTOCOL_VERSION` (currently `1`) is defined once in `crates/core/src/protocol.rs` and
mirrored in `apps/desktop/src/shared/protocol.ts` and `protocol/ultidesk.proto`. Peers
exchange it in `Hello` and refuse to proceed on mismatch unless capability negotiation
covers the delta. The agent IPC also checks it during `Hello` and returns
`protocol_mismatch` on disagreement (tested).

## Local IPC (implemented)

Transport: a **Windows named pipe** or a **Unix socket**, newline-delimited JSON, one
response per request in order. The agent writes its address and a per-launch token to a
handshake file (`agent-endpoint.json`) in the session's runtime directory; the client
reads that file and connects. The address is one field, `endpoint_path`, because a client
opens both kinds the same way.

On Linux the socket lives in `$XDG_RUNTIME_DIR/ultidesk/`, which the session provides
mode `0700`. That directory permission is enforced by the kernel on every `connect()`,
which makes it a stronger control than the named pipe has — the pipe is token-gated only,
and ACL restriction is still tracked in [threat-model.md](threat-model.md). The token is
required on both.

A socket file left behind by a crashed agent is detected by probing it: a successful
connect means a live agent and the new one refuses to start rather than stealing the
path; `ECONNREFUSED` means it is stale and safe to replace.
Auth: the agent generates a per-launch token, writes it with the address to the
handshake file, and prints that file's path on stdout. The desktop app reads it,
connects, and must send `Hello { token }` before any other command (enforced + tested).
The pipe name carries a random suffix because the pipe namespace is machine-global and a
fixed name can be squatted; the Unix socket does not need one, because the directory is
already per-user — there the path needs to be *findable*, not unguessable.

Request/response shapes are the serde-tagged enums in `crates/agent/src/ipc.rs`, mirrored
by the discriminated unions in `apps/desktop/src/shared/protocol.ts`:

- `Hello { token, protocol_version }` → `HelloOk { agent_version, protocol_version }` | `Error`
- `Ping` → `Pong`
- `EnumerateWindows` → `Windows { windows: WindowDto[] }`
- `InjectMouseMove { screen_x, screen_y, virtual_screen }` → `Injected` | `Error`
- `InjectMouseButton { button, down }` → `Injected` | `Error`
- `InjectKey { scancode, down }` → `Injected` | `Error`
- `ReleaseAllInput` → `Released { count }`

Safety: bounded message size (`MAX_MESSAGE_BYTES`), and the agent releases all held
input if the connection drops.

## Peer transport (implemented)

Transport: **QUIC over TLS 1.3** (`crates/transport`, [ADR-0002](adrs/0002-control-transport.md)),
mutually authenticated and pinned to Ed25519 device identities
([ADR-0012](adrs/0012-device-identity.md)). One UDP socket serves both roles: a peer
dials and accepts on the same endpoint.

Authentication is two checks, both required and both run in each direction:

1. the presented certificate carries a public key in this device's pinned set, and
2. the peer proves possession of the matching private key through the TLS 1.3
   CertificateVerify signature.

The certificate itself is only a container for the key — issuer, subject, SANs and
validity window are all ignored, and there is no CA.

Version negotiation is **ALPN** (`ultidesk/<PROTOCOL_VERSION>`), so a version mismatch is
a refused handshake rather than an error exchanged over a channel that should not have
opened. There is consequently no `Hello` on this transport: `Session::for_authenticated_peer`
starts authenticated, and a `Hello` arriving anyway is refused rather than offering a
second, weaker way in.

Framing: a 4-byte big-endian length followed by the payload, on a bidirectional QUIC
stream. The length is checked against `MAX_MESSAGE_BYTES` *before* the buffer is
allocated. Newline framing was not carried over from the local IPC because it only works
while the payload is newline-free.

Messages are the same `IpcRequest`/`IpcResponse` enums as the local IPC, deliberately: the
logic deciding what a peer may do has one implementation rather than one per transport.

Pairing: both machines run `ultidesk-agent pair` (one listens, one dials), compare a
six-digit code derived from both public keys and the TLS channel binding, and pin each
other's key. `ultidesk-agent peers forget <key>` revokes.

**Superseded:** `crates/agent/src/tcp.rs` (`serve-peer-dev`) is newline-delimited JSON over
plaintext TCP behind a shared token. It is kept only to compare the two on a bench.

## Peer protocol (canonical schema, not yet implemented)

Canonical reference: `protocol/ultidesk.proto`. Envelope fields: `protocol_version`,
`message_type`, `message_id`, `session_id`, `sender_device_id`, `recipient_device_id`,
`monotonic_timestamp`, `payload_length`, `payload`. Message families: Hello/capability,
pairing, permission changes, topology, input leases, input events, clipboard offers/
requests, file offers/accept/progress, projection offers/authorization, WebRTC signaling,
projection state changes, handoff prepare/commit/abort, source lock, session termination,
diagnostics.

Untrusted fields — never trust remote enum values, lengths, counts, paths, device names,
window titles, image dimensions, codec params, file metadata, sequence numbers, or
session references. Validate and bound everything; reject malformed input safely.

## Input event fields

Every input message carries: `protocol_version`, `session_id`, `lease_id`, `event_id`,
`origin_device_id`, `target_device_id`, `sequence_number`, `monotonic_timestamp`,
`event_type`, `modifier_state`, `hop_count`, `payload`. Pointer motion may use
unreliable/unordered delivery; key/button transitions, lease state, clipboard commands,
and session commands use reliable/ordered delivery.

## Keeping the mirrors in sync (ADR-0004)

Until generated bindings land (Milestone 1, before any peer protocol ships), the Rust and
TS mirrors of the local IPC are hand-kept in lockstep and guarded by tests. The projection
state machine and coordinate mapping have unit tests on **both** sides asserting identical
behavior, so drift is caught. Adding real peer messages requires switching to codegen
first — do not grow the hand-mirrored surface.
