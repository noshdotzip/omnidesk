# Ultidesk status

Honest snapshot of what is built, verified, and not. Updated 2026-09-04.

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
- **Audio routing works BOTH WAYS** (verified 2026-09-03).
  Arch -> Windows: `audio-send` on
  Arch captures a PipeWire sink's *monitor* via `pw-record` and streams raw s16le
  PCM; `audio-recv` on Windows plays it through WASAPI (cpal). Measured: 1.4 MB /
  351,232 frames over an 8s run = 7.3s of audio at 48 kHz stereo, which is the
  stream duration, so nothing was dropped or misaligned. Clean disconnect.
  **Not confirmed audible**: the Arch box may have been playing silence, and nobody
  was listening on the Windows end. The byte and frame accounting proves the path,
  not the sound.
  Windows -> Arch: `audio-send` captures the default render endpoint via **WASAPI
  loopback** (`AUDCLNT_STREAMFLAGS_LOOPBACK`, direct COM — cpal does not expose it)
  and `audio-recv` on Arch plays through `pw-play`. Measured 2.6 MB / 645,120 frames
  = 13.4s at 48 kHz stereo, clean exit when the peer closed.
  The Windows capture reports the endpoint's **actual** mix format rather than the
  requested one, because shared mode does not negotiate; a 5.1 endpoint is rejected
  with guidance instead of being mislabelled as stereo.
  Uncompressed, so it needs ~1.5 Mbit/s — fine on the measured 167+ Mbit/s LAN.
  Neither direction is confirmed *audible*: the accounting proves the path, not the
  sound.
- **KVM handoff implemented, NOT yet run on real hardware** (`kvm-handoff`). Grabs
  the local pointer with a `WH_MOUSE_LL` hook when it reaches the right edge and
  forwards motion to the peer. Built on `core::kvm` (10 tests pinning that every
  release path is unconditional and that a release cannot be undone by the pointer
  resting on the edge) plus a `RegisterHotKey` emergency release the OS delivers
  independently of the hook. Compiles and passes tests on both platforms; the grab
  itself has never been exercised against a real desktop, and should first be tried
  with the short default deadline and a hand on Ctrl+Alt+Shift+U.
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

## Verified: zero-copy video capture (2026-09-03)

`ultidesk-agent cast-test start` against KDE Plasma 6.7.2:

```
node 103: frames=1 size=800x628 max_fps=144 dma-buf=1 mapped=0
  allocated DmaBuf — zero-copy capable
```

Getting there needed all three of these, and each was necessary but not sufficient
on its own — each was measured, not assumed:

1. **Do not set `StreamFlags::MAP_BUFFERS`.** It forces PipeWire to mmap every
   buffer, which defeats DMA-BUF outright. Removing it alone changed nothing.
2. **Advertise `SPA_PARAM_BUFFERS_dataType`** as a single combined bitmask including
   `SPA_DATA_DmaBuf`. Encoding it as enumerated alternatives instead produces
   `error alloc buffers: Invalid argument` and a stream that negotiates a format and
   then never allocates a buffer. Still yielded shared memory once fixed.
3. **Negotiate a DRM modifier.** `SPA_FORMAT_VIDEO_modifier`, MANDATORY and
   DONT_FIXATE, offering `DRM_FORMAT_MOD_INVALID`. This is the step that actually
   flips KWin to DMA-BUF.

The modifier-bearing format is offered *alongside* the plain one, so a compositor or
GPU that cannot do DMA-BUF still negotiates shared memory rather than failing. Slower
is acceptable; not working is not.

Buffer kind is read from the `add_buffer` callback, which fires at allocation before
any frame. That matters because compositors send frames on damage rather than on a
clock, so a static window produces none — and without this, "nothing moved" and
"negotiation failed" look identical.

## Verified: control UI — display arrangement and audio routing (2026-09-04)

Built with Dioxus 0.6 desktop ([ADR-0010](adrs/0010-dioxus-control-ui.md)) and running
**natively** on `aarch64-pc-windows-msvc`; the renderer is WebView2, confirmed by the
`webview2-com` dependency rather than assumed.

- `cargo test --workspace` -> **233 tests pass** on Windows ARM64 and on Arch x64.
  Clippy `-D warnings` and `cargo fmt --check` clean on both.
- **Display arrangement**: monitors drag and snap; overlaps are flagged; shared borders
  are listed. All geometry comes from `ultidesk-topology::layout` so the editor cannot
  disagree with the agent. Verified on screen that a 1109-tall and a 1080-tall monitor
  report **1080px** of shared border — the overlapping-span rule, not the taller edge.
- **Audio device enumeration**, native on both platforms
  ([ADR-0011](adrs/0011-audio-routing-loop-prevention.md)): 6 endpoints on Arch via the
  PipeWire registry (4 sinks, 2 sources, correct defaults) and 2 on Windows ARM64 via
  WASAPI. Exposed as `ultidesk-agent audio-devices`.
- The Linux walk needs **two** `sync` round-trips, measured rather than assumed: with
  one, 0 of 6 devices come back marked default; with two, the correct 2 do.
- **Feedback-loop refusal verified end-to-end in the UI**: selecting the same output as
  both source and sink disables "Add route" and explains why, naming the device rather
  than its GUID.

**Verified on Linux 2026-09-04**, once `xdotool` was installed (`muda`, pulled in by
`tao`, links `libxdo` unconditionally). The whole workspace — `ultidesk-control`
included — passes `cargo clippy --all-targets -D warnings`, `cargo fmt --check` and
233 tests on Arch.

The app runs on KDE Plasma Wayland as a **native Wayland window**, not through
XWayland (confirmed by `xdotool search` finding nothing). Both tabs render, and the
audio panel enumerates all six PipeWire endpoints through
`apps/control/src/devices.rs` — 4 sinks and 2 sources, with Speaker and Digital
Microphone marked default, and the "play on" column correctly excluding the
microphones.

One caveat found while testing, worth knowing before packaging: `global-hotkey`
(a non-optional dependency of `dioxus-desktop` on Linux, with no feature flag to
disable it) spawns a thread that calls `XDefaultRootWindow` without checking whether
`XOpenDisplay` succeeded. If X11 is unreachable the app **segfaults at startup**
rather than degrading. A normal desktop launch is fine because the session provides
`DISPLAY` and `XAUTHORITY`; it crashed only when launched over SSH with `DISPLAY` set
but `XAUTHORITY` missing. A pure Wayland session with no XWayland would hit the same
crash, so this is a real robustness limit of the dependency and not merely a testing
artefact.

The panel still cannot read a *peer's* devices — that needs the settings IPC — and
says so rather than showing a placeholder.

## Measured: the network link is the dominant latency cost (2026-09-04)

The two machines talk over a Wi-Fi link (Arch on `wlan0`, 5 GHz, via the Windows
machine's hosted network at `192.168.137.1`). Earlier work measured its *throughput*
at 167+ Mbit/s and treated the link as solved. Throughput is the wrong metric for a
KVM: what the operator feels is round-trip latency, and that turns out to be an order
of magnitude worse than assumed.

| Windows -> Arch ICMP | min | avg | max |
| --- | --- | --- | --- |
| Arch radio idle | 16 ms | ~150 ms | 519 ms |
| Arch radio kept busy | 5 ms | 20 ms | 42 ms |

The two rows differ only in whether the Arch machine was transmitting at the time.
Arch -> Windows in the same conditions averages 15 ms, so the penalty is one-directional.

That asymmetry is the signature of **Wi-Fi power save**: `iwconfig` reports
`Power Management: on`, and a sleeping client cannot receive until it next wakes, so
inbound packets queue at the access point. It is not signal quality -- the link is
-42 dBm at 68/70 with a 1.13 Gb/s negotiated rate.

Why it matters more than it looks:

- The old `kvm_mirror`, which waited for each `Injected` acknowledgement before sending
  the next update, was capped at **1/RTT ~= 7 pointer updates per second** on this link.
  It was not a demo of a slightly laggy KVM; it was a demo of an unusable one. This is
  what motivated `PeerSink`, and it makes the pipelining change worth roughly 20x on the
  achievable update rate here rather than the marginal gain it would be on a wired LAN.
- Uncompressed PCM audio (1.5 Mbit/s) has no jitter buffer, so 500 ms spikes are
  audible dropouts. This is a second reason to replace it with Opus/RTP, independent of
  bandwidth.
- Any figure quoted for input or projection latency is meaningless until power save is
  settled, because the link contributes more variance than everything else combined.

**Fixed 2026-09-04** with `sudo iw dev wlan0 set power_save off`. Re-measured
immediately afterwards, same direction and same link:

| Windows -> Arch ICMP | min | avg | max |
| --- | --- | --- | --- |
| Before (power save on) | 16 ms | ~150 ms | 519 ms |
| After (power save off) | 4 ms | 28 ms | 124 ms |

Roughly 5x on the average and 4x on the worst case, and the half-second outliers are
gone entirely (30 consecutive samples spanned 4-41 ms). 28 ms is still high for a
-42 dBm 5 GHz link, so there is more to find here, but it is no longer the dominant
term.

This does **not** survive a reboot. To persist it, a NetworkManager drop-in at
`/etc/NetworkManager/conf.d/wifi-powersave.conf` with `wifi.powersave = 2`. The Arch
machine also has an idle wired interface (`eno1`, state DOWN); a cable removes the
variable entirely and is worth preferring for any latency figure meant to be quoted.

The Arch machine also has an idle wired interface (`eno1`, state DOWN). If a cable is
available, that removes the variable entirely and is worth preferring for any latency
measurement that is meant to be quoted.

Note `iw` is not installed; the readings above came from `iwconfig` (net-tools) and
`ping`.

## Blocked

- ~~PipeWire video capture needs `clang`~~ — **resolved**: clang was installed, and
  `pipewire 0.10` (not 0.8, which fails against PipeWire 1.6.7 because bindgen emits
  `spa_pod_builder` as an opaque type) builds cleanly. Kept here only as the record of
  what the blocker was.
- **Historic:** PipeWire video capture needed `clang` on the Arch box. The ScreenCast
  portal is complete — `cast-test start` returns a real node id, a restore token and
  an authorised PipeWire fd — but turning that fd into frames needs a PipeWire
  client, and every route is closed on this machine:
  - `pipewire-rs` fails to build: `libspa-sys` runs bindgen, which panics with
    "Unable to find libclang". `clang` and `libclang.so` are absent (verified by
    building the crate, not just by probing).
  - GStreamer is installed but has neither `pipewiresrc` nor **any** H.264 encoder
    (`x264enc`, `vah264enc`, `vaapih264enc`, `openh264enc` all absent), so the CLI
    shim that worked for audio is not available for video.
  - `libpipewire-0.3` (1.6.7) and `libspa-0.2` headers *are* present, and VAAPI
    hardware exists (`/dev/dri/renderD128`), so only the toolchain is missing.

  Unblock with `sudo pacman -S clang` (and `gst-plugins-good`/`gstreamer-vaapi` if
  the CLI route is preferred later). Installing needs a password, so it cannot be
  done unattended.

  Note `reis` (the pure-Rust libei client, needed for InputCapture event delivery)
  builds fine without clang — only the video path is blocked.

## Planned / requested work

- **Dioxus control UI** — *partly built*, see the verified section above. The
  display-arrangement editor and the audio-routing panel exist and run natively on
  Windows ARM64. Still to do: cursor settings, per-peer permissions, pairing, and
  loading/persisting real state instead of an in-memory layout — all of which need
  the settings IPC surface. Visual reference: the Kopuz music player's Dioxus UI
  (not yet reviewed — confirm the repository before treating it as a spec).
- **libei client** (`reis`) so InputCapture actually delivers events; the portal
  arbitrates capture, it does not carry input.
- **PipeWire client** so ScreenCast's `OpenPipeWireRemote` fd becomes video frames.
- **Peer transport** (Milestone 1) — without it nothing crosses machines regardless
  of how well either backend works.
- **Audio transport quality** — the routing model and device selection are built
  (see above), but the stream itself is still uncompressed PCM over plaintext TCP and
  Linux playback still shells out to `pw-play`. Opus/RTP over the ADR-0002 transport
  is the target; PipeWire `rtp-sink`/`rtp-source` are present on the Arch box.

## Exact next step

Milestone 1 (secure device mesh): persistent Ed25519 identity + OS secret storage,
mDNS discovery + manual IP fallback, pairing with SAS verification code, pinned peers +
revocation, per-peer permissions, the authenticated QUIC control channel, capability
negotiation — which also replaces the dev loopback signaling broker. In parallel, a small
task to wire Vite so the Milestone-0 projection slice can be manually verified per
[testing.md](testing.md).
