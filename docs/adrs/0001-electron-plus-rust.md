# ADR-0001: Electron/TypeScript app + Rust user-session agent

- Status: Accepted
- Date: 2026-07-31

## Context

Ultidesk needs rich cross-platform UI (tray, pairing, topology editor, proxy windows,
WebRTC playback, diagnostics) and low-level OS access (window enumeration, global input
capture/injection, clipboard, portals, secret storage, LAN transport). No single stack is
great at both. Desktop capture, clipboard, global input, and interactive windows are all
**session-specific**, so a privileged service is the wrong home for them.

## Decision

Two processes in the logged-in interactive user session:

- **Electron/TypeScript** owns UI, WebRTC media negotiation/playback, and the first
  Chromium-based capture path.
- **Rust** owns identity, discovery, pairing, native window/monitor enumeration, input
  capture/injection, clipboard, file transfer, session/lock detection, secret storage, and
  sanitized logging.

They communicate over authenticated local IPC. This is a pragmatic starting point, not
irreversible. If startup/privileged features later need a service, it stays **separate**
from the user-session agent with a minimal authenticated API.

## Consequences

- Fast UI iteration; strong systems code where it matters; clear security boundary
  (renderer → preload → main → IPC → agent).
- Two languages ⇒ a shared protocol contract must be generated, not hand-forked
  (see ADR-0004).
- Electron's footprint and security surface require strict settings (sandbox, CSP,
  contextIsolation) — enforced in the app.
