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
  unit-tested; the TS mirrors pass parity tests and the app typechecks under
  strict TS settings.

## Verified on Windows ARM64 (executed 2026-09-02, Rust 1.86, Node 22, Electron 33.3.1)

Host: Windows 11 Pro 26200 on a Qualcomm ARMv8 CPU, built and run **natively** as
`aarch64-pc-windows-msvc`. Nothing in this list ran under Prism (ADR-0008).

- `cargo test --workspace` -> **46 tests pass**, the same count as x64.
- `cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --check` clean.
- `ultidesk-agent.exe` is an ARM64 PE image; **live window enumeration** returned 8 real
  top-level windows across 6 processes.
- Desktop app: **29 TS tests pass** (18 existing + 11 new media-stats tests) and
  `tsc --noEmit` is clean, with esbuild/rollup running as ARM64 native binaries.
- Electron 33.3.1 / Chromium 130 resolves to the ARM64 build, bundled DLLs included.
- **Release-binary benchmark** (`scripts/bench-agent.mjs`, driving the real named-pipe
  IPC): cold start 17.2 ms and idle working set ~9,960 KB, versus 37.6 ms and ~20,280 KB
  for the same code cross-built as x64 and run under Prism (~2x on both). Steady-state Ping RTT showed no
  reliable difference between the two — the IPC path is kernel-bound, not translation-
  bound. Full table and caveats in [ADR-0008](adrs/0008-windows-arm64-native.md).

What this does **not** establish: input injection, capture, clipboard, transfer, and
window drag are still Untested on ARM64 for exactly the same reason as on x64 — they
need the GUI. Building natively is not evidence that they work.

## Verified on Arch Linux x64 (executed 2026-09-02, Rust 1.98, Node 22)

Host: Arch Linux, kernel 7.1.2, Intel i5-10300H, `x86_64-unknown-linux-gnu`, in a live
KDE Plasma Wayland session.

- `cargo build --workspace` succeeds; `cargo test --workspace` → **45 tests pass**.
  That is 45 and not 46 by design: the named-pipe loopback IPC test lives behind
  `#[cfg(windows)] mod pipe` and does not exist on Linux.
- `cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --check` clean
  — but only after a fix. On the unmodified tree clippy **failed on Linux** with 13
  dead-code errors: with no Linux transport, nothing constructs `Endpoint`, `Session`,
  `IpcRequest`/`IpcResponse` or the injector methods. The repo's own gate
  (`pnpm agent:check`) had only ever been run on Windows. Fixed with a scoped
  `#![cfg_attr(not(windows), allow(dead_code))]` on the two affected modules.
- Desktop app: **29 TS tests pass**, `tsc --noEmit` clean, install selects
  `@esbuild/linux-x64` + `@rollup/rollup-linux-x64-gnu`.
- **ScreenCast session implemented and negotiated live** (`screen_cast.rs`,
  `ultidesk-agent cast-test`): `CreateSession` + `SelectSources` both succeed against
  KDE in ~4 ms, requesting WINDOW sources only with metadata cursor. Stops before
  `Start`, which raises the compositor's picker (ADR-0009). No frames yet —
  `OpenPipeWireRemote` returns an fd that needs a PipeWire client to become video.
- **InputCapture barrier geometry implemented** (`crates/platform-linux/src/input_capture.rs`).
  Zone/edge/barrier math with 7 unit tests, including the inclusive-coordinate rule: a
  barrier at `x + width` instead of `x + width - 1` lands outside the zone, and the
  portal answers by silently listing it in `failed_barriers` — the KVM edge then never
  fires and there is no error to chase. **The D-Bus session for InputCapture is not
  written yet**, and even once it is, actual event delivery needs a libei client
  (`ConnectToEIS` + the `reis` crate). No input has been captured.
- **Real-cursor mirroring works** (`kvm-mirror`, verified 2026-09-03). The Windows
  agent reads its actual pointer via `GetCursorPos`, maps it proportionally onto the
  peer's screen with `topology::map_edge_crossing`, and forwards it. Verified across
  a genuine resolution mismatch: local 1664x1109 -> remote 1920x1080. The Arch agent
  logged the connection for exactly the mirror window and zero injection errors.
  Mirroring deliberately **does not take over the local pointer** — nothing is
  swallowed or warped — so it is safe to run on a machine someone is using. True
  handoff needs low-level hooks and an emergency-release hotkey; neither exists yet,
  and neither should be built without that hotkey.
- **CROSS-MACHINE KVM WORKS** (verified 2026-09-03). Windows ARM64 drove the Arch
  x64 Wayland pointer over the network end to end:
  `ultidesk-agent kvm-demo 192.168.137.9:45872 <token>` completed a handshake and
  five absolute moves, each answered `Injected`, then `ReleaseAllInput`. The Arch
  agent logged one peer connect/disconnect and no injection errors. Its portal
  session was restored from a saved grant in ~35 ms with **no prompt**.
  Path: Windows client -> plaintext TCP -> `Session::handle` -> `PortalInjector` ->
  RemoteDesktop `NotifyPointerMotion`.
  **Caveat:** the portal accepted every call and returned no error; nobody was
  watching the Arch screen, so the cursor movement itself is inferred from the
  portal's acceptance plus the earlier visually-confirmed `inject-test`, not
  observed directly in this run.
  **The transport is NOT secure** — plaintext TCP behind a token, explicitly not the
  ADR-0002 channel. See `crates/agent/src/tcp.rs`.
- **Input injection on Wayland WORKS** (verified 2026-09-03). `ultidesk-agent
  inject-test` completed `CreateSession` -> `SelectDevices` -> `Start`, the KDE
  prompt was granted, and the pointer traced a 40px square via
  `NotifyPointerMotion`. A `restore_token` was returned and re-used: a later run
  completed in ~2s with no prompt at all, which is what makes a KVM usable daily.
- **InputCapture works end to end** (verified 2026-09-03): `CreateSession` ->
  `GetZones` -> `SetPointerBarriers`, barrier accepted, real display reported
  (1920x1080 at 0,0). Event *delivery* still needs a libei client.
- **RemoteDesktop session lifecycle implemented** (`crates/platform-linux/src/remote_desktop.rs`,
  driven by `ultidesk-agent inject-test`). `CreateSession` -> `SelectDevices` -> `Start`
  all execute against the live KDE portal; instrumented logs and `xdg-desktop-portal-kde`
  journal entries confirm the handshake reaches KDE's permission lookup in ~7 ms.
  **Injection itself is still unproven**: `Start` blocks on a permission dialog that has
  not yet been accepted, so no pointer event has been delivered. Reaching the dialog is
  not the same as injecting, and the compatibility matrix reflects that.
- **`ultidesk-platform-linux` added**: portal capability probing over D-Bus (zbus),
  executed against the live KDE Plasma Wayland session via `ultidesk-agent probe`. It
  correctly reports ScreenCast v4 (monitor+window+virtual), RemoteDesktop v2 and
  InputCapture v2 (keyboard+pointer+touchscreen), Clipboard v1. 13 new unit tests.
  Capture and input injection are **not** implemented — they return `Unsupported`.

**The Linux build has no Linux capability.** Executed against a live Plasma session,
`ultidesk-agent enumerate` returns `[]` and `ultidesk-agent serve` exits 1 with
"the IPC server transport is currently Windows-only". The binary is a valid ELF
x86-64 executable that does nothing useful yet. Building is not evidence of function;
see [compatibility.md](compatibility.md) for the portal probe of what the platform
*could* support.

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
7. **ARM64 toolchain is guarded, not self-healing**: `scripts/check-native-arch.mjs` runs
   at `preinstall` and hard-fails when the package manager or Node is emulated, because
   the x64 `pnpm.exe` silently installs x64 esbuild/rollup on an ARM64 machine. Install
   with `corepack pnpm install`. See ADR-0008.
8. **Hardware video encode is unconfirmed**: `MediaStats` now reports
   `hardwareEncode`/`hardwareDecode` and the raw implementation strings, but nothing has
   read them yet — that needs the GUI. A software OpenH264 fallback on ARM64 would be a
   serious performance bug and is the first thing to check once the GUI runs.

## Planned / requested work

- **Dioxus control UI** — topology editor (dragging monitor rectangles into their
  virtual arrangement), cursor settings, audio routing and speaker selection,
  per-peer permissions, pairing. Requested 2026-09-03; see
  [ADR-0010](adrs/0010-dioxus-control-ui.md), recorded as Proposed rather than
  Accepted because it adds a second UI stack alongside Electron and the renderer
  choice is unresolved. Visual reference: the Kopuz music player's Dioxus UI (not
  yet reviewed — confirm the repository before treating it as a spec).
- **libei client** (`reis`) so InputCapture actually delivers events; the portal
  arbitrates capture, it does not carry input.
- **PipeWire client** so ScreenCast's `OpenPipeWireRemote` fd becomes video frames.
- **Peer transport** (Milestone 1) — without it nothing crosses machines regardless
  of how well either backend works.
- **Audio routing** — PipeWire `rtp-sink`/`rtp-source` already present on the Arch
  box; the one goal needing no portal permission.

## Exact next step

Milestone 1 (secure device mesh): persistent Ed25519 identity + OS secret storage,
mDNS discovery + manual IP fallback, pairing with SAS verification code, pinned peers +
revocation, per-peer permissions, the authenticated QUIC control channel, capability
negotiation — which also replaces the dev loopback signaling broker. In parallel, a small
task to wire Vite so the Milestone-0 projection slice can be manually verified per
[testing.md](testing.md).
