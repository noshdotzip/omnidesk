# Ultidesk status

Honest snapshot of what is built, verified, and not. Updated 2026-07-31.

## Milestone position

Milestone 0 (feasibility + architecture) — partially complete. Milestones 1–10 not started.

## Verified (executed on `x86_64-pc-windows-msvc`, Rust 1.93, Node 22, pnpm 10)

- Cargo + pnpm workspaces build. Lockfiles committed.
- `cargo test --workspace` → **46 tests pass**.
- `cargo clippy --workspace --all-targets -- -D warnings` → clean. `cargo fmt --check` → clean.
- **Live window enumeration**: `ultidesk-agent enumerate` returned 8 real top-level windows.
- **Named-pipe local IPC**: loopback integration test connects, enforces the auth token,
  and round-trips Hello/Ping.
- **Projection state machine** and **input loop guard** and **coordinate mapping**: fully
  unit-tested; the TS mirrors pass parity tests (18 TS tests) and the app typechecks under
  strict TS settings.

## Implemented but NOT runtime-verified (needs manual GUI test)

- The Electron app: secure windows (nodeIntegration off, contextIsolation on, sandbox on,
  narrow preload, strict CSP, permission handler denies all), window picker, WebRTC
  `MediaBackend`, destination proxy window, dev loopback signaling broker, and the input
  forwarding path (proxy → main → agent → `SendInput`).
- Reason not verified: running the projection needs a live Electron process and a renderer
  bundling step (Vite) that is the immediate next task, and the GUI cannot be exercised in
  the current headless build environment. See [testing.md](testing.md) for the manual plan.
- Windows `SendInput` injection: the code compiles and the coordinate math is unit-tested,
  but injecting into a real application (and confirming UIPI blocks elevated targets) has
  not been manually run.

## Deliberately NOT built yet (no stubs, per engineering rules)

Device identity/pairing/discovery, the authenticated peer control channel (QUIC/TLS),
per-peer permission store + Work Device runtime enforcement, monitor topology editor UI,
KVM cursor edge-crossing, clipboard subsystem, file transfer, window handoff, Linux
backends, audio, game-provider integration, packaging/signing, emergency-release hotkey,
on-screen capture indicators, named-pipe ACL hardening.

## Known limitations / tracked debt

1. **Renderer bundling**: `pnpm --filter @ultidesk/desktop dev` intentionally errors until
   Vite is wired. HTML assets in `src/renderer` need a copy/bundle step to reach `dist`.
2. **Protocol codegen**: Rust/TS IPC types are hand-mirrored and test-guarded. Generated
   bindings (prost + protobufjs/ts-proto) must land in Milestone 1 before any peer
   protocol grows the surface — see ADR-0004.
3. **Named-pipe ACL**: the pipe is gated by a per-launch token but not yet ACL-restricted
   to the current user. Tracked in [threat-model.md](threat-model.md).
4. **`requestKeyframe`**: no direct renderer API; documented no-op for now.
5. **Keyboard**: only a minimal `KeyboardEvent.code` → PS/2 scancode map (letters/digits/
   space/enter/backspace/tab) for the Notepad demo. Full layouts/IME/AltGr are later work.
6. **Virtual screen bounds** in input mapping are a single-display placeholder pending the
   topology subsystem.

## Exact next step

Milestone 1 (secure device mesh): persistent Ed25519 identity + OS secret storage,
mDNS discovery + manual IP fallback, pairing with SAS verification code, pinned peers +
revocation, per-peer permissions, the authenticated QUIC control channel, capability
negotiation — which also replaces the dev loopback signaling broker. In parallel, a small
task to wire Vite so the Milestone-0 projection slice can be manually verified per
[testing.md](testing.md).
