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
12. Read `MediaBackend.getStats()` and record `hardwareEncode`, `hardwareDecode`, and the
    raw `encoderImplementation`/`decoderImplementation` strings.

Record results in [compatibility.md](compatibility.md). Until these are run, the
projection capability stays "Untested".

## Manual test: audio routing (Arch -> Windows)

On **Windows** (the receiver), start it first so the port is listening:

```bash
./target/debug/ultidesk-agent.exe audio-recv 0.0.0.0:45873 120
```

On **Arch**, find a sink and stream its monitor — the monitor of an *output*, so what
the machine is playing, not a microphone:

```bash
pw-cli ls Node | grep -B2 'Audio/Sink'
./target/debug/ultidesk-agent audio-send <windows-ip>:45873 <sink-name>.monitor 48000 2
```

Play something on Arch and it should come out of the Windows speakers. The receiver
prints bytes and frames on disconnect; frames / rate should equal the stream
duration — if it is short, audio was dropped.

The last argument to `audio-recv` is the latency cap in milliseconds. Lower it for
tighter sync, raise it if the audio breaks up: the measured WiFi link has ~15 ms mean
absolute jitter, so a cap below about 50 ms will glitch on it.

Capturing the sink itself rather than its `.monitor` silently sends the wrong thing.

## Manual test: KVM handoff (Windows ARM64 -> Arch Wayland)

**This grabs your pointer.** While control is on the peer, the Windows cursor stops
moving. Read the release routes before running it.

Start the peer server on Arch (see below), then on Windows:

```bash
./target/debug/ultidesk-agent.exe kvm-handoff <arch-ip>:45872 <token> 1920 1080 20
```

Move the pointer to the **right edge** of the Windows screen. Control hands over at
the matching height on the Arch screen; the Windows cursor freezes and Arch's moves.

Three independent ways back, each working if the others are broken:

1. **Ctrl+Alt+Shift+U** — the emergency hotkey. Registered with `RegisterHotKey`, so
   the OS delivers it even if the forwarding loop is wedged or blocked on a dead
   socket.
2. **Kill the peer** (or pull the network). Any send/receive failure releases.
3. **Wait.** The session ends after the deadline (20s by default) regardless.

Walking the remote pointer back off the peer's left edge also returns control.

The keyboard **is** grabbed by default, so keystrokes go to the peer. Pass `false` as
a seventh argument to hook the mouse only.

A low-level keyboard hook runs *before* the OS dispatches registered hotkeys, so a
hook that swallowed everything would swallow the emergency combination too. The hook
therefore never swallows anything while Ctrl+Alt+Shift are held together, and reports
the release directly. That in-hook route is the one that works when the keyboard is
grabbed; `RegisterHotKey` remains as an independent second route for when it is not.

If the pointer ever stays stuck after all three routes, that is a serious bug: the
state machine in `core::kvm` asserts it cannot happen, so capture the exact sequence.

## Manual test: mirror the real pointer (Windows ARM64 -> Arch Wayland)

Start the peer server on Arch as described below, then on Windows:

```bash
./target/debug/ultidesk-agent.exe kvm-mirror <arch-ip>:45872 <token> 1920 1080 15
```

Move the mouse during the window. The Arch cursor should follow, proportionally —
the two screens need not be the same size or aspect. The command prints how many
updates it sent; a still pointer sends exactly one (its initial position), so a
count of 1 means the path works but nobody moved the mouse.

This does not grab local input. The Windows cursor keeps behaving normally.

## Manual test: cross-machine KVM (Windows ARM64 -> Arch Wayland)

Proves the whole path: peer transport, session auth, absolute-to-relative pointer
translation, and portal injection.

On the **Arch** machine (or over SSH with the Plasma session environment exported):

```bash
export XDG_RUNTIME_DIR=/run/user/1000
export DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus
export XDG_CURRENT_DESKTOP=KDE
export ULTIDESK_PEER_TOKEN=pick-a-token
export ULTIDESK_RESTORE_TOKEN=<a grant printed by an earlier run>   # optional, skips the prompt
./target/debug/ultidesk-agent serve-peer-dev 0.0.0.0:45872
```

Approve the KDE prompt if `ULTIDESK_RESTORE_TOKEN` was not supplied. Then on
**Windows**:

```bash
./target/debug/ultidesk-agent.exe kvm-demo <arch-ip>:45872 pick-a-token 200
```

Expected: `peer handshake accepted`, five `moved remote pointer to (x, y)` lines,
and the Arch cursor tracing a 200px square. **Watch the Arch screen** — the portal
accepting a call is not proof the cursor moved, and only a human can close that gap.

Motion only: no clicks and no keystrokes, since those would land in whatever window
has focus on the remote machine.

Failure modes:

- `peer rejected the handshake` -> the tokens do not match.
- `could not connect` -> check the bind address and that nothing filters port 45872.
- `timed out waiting for a response to Start` on the server -> the KDE prompt was
  never answered; supply a `ULTIDESK_RESTORE_TOKEN` or approve it at the machine.

## Manual test: Linux input injection (`ultidesk-agent inject-test`)

Must be run **while sitting at the Arch machine** — it raises a KDE permission dialog
and blocks on it. Over SSH the dialog appears on the physical screen, not in your shell.

```bash
cargo build -p ultidesk-agent
./target/debug/ultidesk-agent inject-test
```

Expected: three `portal step N/3` log lines, a KDE prompt to allow remote control,
then the pointer traces a 40px square and returns to its starting point. The command
prints a `restore_token=` line on success; a later run passed that token should not
prompt again.

Reading the failure modes apart:

- Hangs at `step 3/3`, then `timed out waiting for a response to Start` after 120s →
  the dialog was never answered. Not a code fault.
- `the user declined the Start permission request` → dialog was dismissed.
- Stops at step 1 or 2 → a real bug in the Request/Response handshake, since neither of
  those steps involves the user. Check `journalctl --user` for `xdg-desktop-portal-kde`
  lines; the absence of a `MegaAuth` permission-lookup line means the call never
  reached the KDE backend. Note the journal prints **local time** while the agent logs
  **UTC** — comparing them naively will show a false "no activity".

It does not click or type: injecting buttons or keystrokes into whatever window holds
focus would be a hazard, not a test.

### Step 12 on Windows ARM64 (ADR-0008)

Step 12 is not a nice-to-have on ARM64, it is the performance test. Chromium falls back
to software H.264 (OpenH264) *without failing* when it cannot reach the Snapdragon
MediaFoundation encoder: the projection still runs, at a fraction of the framerate and a
multiple of the power draw. Expect `hardwareEncode === true` with an
`encoderImplementation` naming MediaFoundation.

- `hardwareEncode === false` → **a bug to file**, not an acceptable baseline. Capture the
  implementation string and `chrome://gpu` before changing anything.
- `hardwareEncode === undefined` → the runtime reported neither `powerEfficientEncoder`
  nor a recognised implementation name. That is "unknown", not "software"; widen the
  patterns in `classifyAcceleration` rather than recording a guess.
