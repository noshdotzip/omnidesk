# Ultidesk architecture

## The one idea that shapes everything

**Processes do not move.** When a user "moves a window to another computer", the process,
its memory, handles, credentials, and domain membership all stay on the **source**
computer. Ultidesk captures the selected source window, streams its frames directly to
the **destination**, and shows them in a local **proxy window**. Input on the proxy is
forwarded back to the source. We call this **Window Projection**.

Consequences that the whole design must honor:

- Every projection has exactly one authoritative **source** device that owns the process.
- Closing a proxy disconnects the projection; it does **not** close the source app.
  Closing the source app is a separate, explicitly labeled action.
- If the destination crashes or the network drops, the source app keeps running and is
  locally recoverable; all remotely-held input is released.
- Moving a projection from B to C does **not** re-encode B's video. The original source A
  negotiates a **direct** A→C stream; B holds its proxy until C confirms a first frame,
  then closes. No generational transcoding.

## Process model

Two processes per machine, both in the **logged-in interactive user session** (desktop
capture, clipboard, global input, and interactive windows are all session-specific — a
privileged Windows service cannot do them, so we do not start with one):

1. **Rust user-session agent** (`crates/agent`) — device identity, discovery, pairing,
   authenticated peer connections, native window enumeration, input capture/injection,
   clipboard, file transfer, session/lock detection, secret storage, sanitized logs, and
   (later) native capture backends.
2. **Electron/TypeScript desktop app** (`apps/desktop`) — tray/settings/pairing UI,
   topology editor, window picker, destination proxy windows, WebRTC media negotiation
   and playback, diagnostics, notifications, permission prompts, packaging.

They communicate over **authenticated local IPC** (Windows named pipe, per-launch token;
Unix domain socket on Linux). The renderer never talks to the agent directly:

```
renderer → validated preload bridge → Electron main → authenticated local IPC → Rust agent
```

## Subsystems (one shared identity/permission/session/logging model)

1. Secure device mesh — discovery, pairing, identity, permission, session, revocation.
2. Shared input / KVM — topology, edge crossing, keyboard forwarding, input leases,
   emergency release, stuck-input cleanup.
3. Window Projection — enumeration, window families, capture, encode/transport, proxy
   windows, remote input, focus/resize.
4. Seamless window handoff — drag-boundary detection, pre-negotiation, direct A→C
   handoff, rollback.
5. Shared clipboard — text first, then HTML/images/file offers, versioning, loop
   prevention, sensitive-device controls.
6. File transfer — explicit send/approval, chunking, resume, integrity, safe destination.

## Media abstraction

Capture/encode/transport sits behind `MediaBackend`
(`apps/desktop/src/projection/media-backend.ts`) from day one, so topology, input, and
session logic never depend on Chromium source ids. First implementation is
`ElectronWebRtcMediaBackend`; later `WindowsGraphicsCaptureBackend`,
`PipeWirePortalBackend`, and a native-encoder backend implement the same surface.

## Where the risky logic lives (and how it is verified)

The correctness-critical, platform-independent logic is deliberately pulled into pure,
unit-tested Rust with a TypeScript mirror so both ends agree:

| Concern | Rust (authoritative) | TS mirror | Verified by |
|---|---|---|---|
| Projection lifecycle | `core::projection` | `projection/state.ts` | unit tests both sides |
| Input loop prevention | `core::input_guard` | (agent-side only) | Rust unit tests |
| Letterbox pointer mapping | `topology::mapping` | `topology/mapping.ts` | unit tests both sides |
| Edge crossing | `topology::mapping` | `topology/mapping.ts` | unit tests both sides |
| Local IPC contract | `agent::ipc` | `shared/protocol.ts` | conformance (see protocols.md) |

## Networking (LAN-only MVP)

- Discovery: mDNS/DNS-SD (`_ultidesk._udp.local`), with manual IP fallback. Advertises
  only non-sensitive metadata (never usernames, window titles, app lists, clipboard).
- Control plane (production direction): direct Rust↔Rust QUIC, TLS 1.3, pinned paired
  identity; reliable streams for control/clipboard/state, datagrams for pointer motion.
- Media plane: direct WebRTC, H.264 first, host/LAN ICE, **no TURN**, SDP/ICE exchanged
  over the authenticated control channel. No cloud coordinator, no relay, no silent
  Internet connection.

See [protocols.md](protocols.md), [threat-model.md](threat-model.md), and the ADRs.

## Current slice vs. production

This Milestone-0 slice substitutes a **dev loopback signaling broker** in the Electron
main process (two windows in one process) for the not-yet-built authenticated peer
control channel. It is dev-only, never touches the network, and is replaced in
Milestone 1. See [status.md](status.md).
