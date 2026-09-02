# Ultidesk testing

## Automated (runs today)

- **Rust** unit + integration tests: `cargo test --workspace` (46 tests).
  - `core::projection` — projection state machine: happy path, input gating to
    `RemoteActive`, first-frame-timeout rollback, disconnect fault, failed/successful
    handoff, return-to-source, `Close` from every state, illegal-transition rejection.
  - `core::input_guard` — loop prevention: own-origin, recaptured-own-injection,
    different-agent-injection, duplicate, hop limit, forward-stamp TTL, bounded LRU.
  - `topology::mapping` — letterbox rects, pillarbox/letterbox bar rejection, resize
    invariance, edge-crossing fraction preservation + clamping.
  - `platform-windows::inject` — absolute virtual-desktop coordinate conversion.
  - `agent::ipc` — auth gating, wrong token, protocol mismatch, held-input tracking +
    release-all, blocked-injection surfacing.
  - `agent::pipe` — **named-pipe loopback**: real client connects, auth is enforced,
    Hello/Ping round-trips (Windows only).
- **TypeScript** (`pnpm --filter @ultidesk/desktop test`, 18 tests): projection-state and
  coordinate-mapping **parity** with the Rust authoritative logic; typecheck via `tsc`.
- **Lint/format**: `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --check`.

## Live probe (runs today)

`cargo run -p ultidesk-agent -- enumerate` prints capturable windows as JSON — a real,
no-elevation feasibility check of Win32 enumeration.

## Planned test suites (per brief §24)

Property-based topology tests · protocol fuzzing/malformed-input · fault injection ·
two-process loopback dev mode · real multi-device hardware · manual security checklists.
Required cases are enumerated in the brief §24 (topology, input, projection, clipboard,
file, security) and become tasks as each subsystem lands.

## Manual projection test plan (GUI — not runnable headless in this environment)

The end-to-end WebRTC projection path is implemented but requires a live Electron process
and a renderer bundling step (Vite) that is the next task. Once bundling is wired, verify:

1. `cargo build -p ultidesk-agent` (produces `target/debug/ultidesk-agent.exe`).
2. Launch the desktop app; it spawns the agent, reads the handshake, and authenticates.
3. Open Notepad. In the Ultidesk source window, confirm Notepad appears in the picker.
4. Select it. A destination proxy window opens and shows **live** video (type in Notepad
   and watch the proxy update — it must not be a static screenshot).
5. Click inside the proxy on a content area → the click lands at the correct spot in
   Notepad. Click in a letterbox bar → nothing is sent to the source.
6. Type letters/space/enter in the proxy → text appears in Notepad.
7. Resize the proxy → pointer mapping stays correct (no offset).
8. Close the proxy → projection stops but **Notepad keeps running**.
9. Kill the agent / disconnect abruptly → no keys/buttons remain stuck (release-all fires).
10. Try to project an **elevated** app and inject → input fails closed (UIPI), reported
    clearly, not bypassed.
11. Inspect logs → no typed keys, window titles, clipboard, frames, or token present.

Record results in [compatibility.md](compatibility.md). Until these are run, the
projection capability stays "Untested".
