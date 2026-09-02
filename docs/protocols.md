# Ultidesk protocols

Two contracts exist: the **local IPC** (desktop app ↔ agent) and the **peer protocol**
(device ↔ device). Only local IPC is implemented in this slice.

## Versioning

`PROTOCOL_VERSION` (currently `1`) is defined once in `crates/core/src/protocol.rs` and
mirrored in `apps/desktop/src/shared/protocol.ts` and `protocol/ultidesk.proto`. Peers
exchange it in `Hello` and refuse to proceed on mismatch unless capability negotiation
covers the delta. The agent IPC also checks it during `Hello` and returns
`protocol_mismatch` on disagreement (tested).

## Local IPC (implemented)

Transport: Windows named pipe, newline-delimited JSON, one response per request in order.
Auth: the agent generates a random pipe name + per-launch token, writes them to a
per-user handshake file, and prints the file path on stdout. The desktop app reads it,
connects, and must send `Hello { token }` before any other command (enforced + tested).

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
