# Ultidesk status

Honest snapshot of what is built, verified, and not. Updated 2026-09-09.

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

Discovery, the authenticated peer control channel (QUIC/TLS), per-peer permission store
+ Work Device runtime enforcement, monitor topology editor UI, KVM cursor edge-crossing,
clipboard subsystem, file transfer, window handoff, Linux backends, audio, game-provider
integration, packaging/signing, emergency-release hotkey, on-screen capture indicators,
named-pipe ACL hardening, OS secret storage for the device key.

Device **identity** and the pinning/pairing *logic* are now built - see below. What is
missing is everything that needs a network: nothing exchanges keys yet, so no peer has
ever been pinned by anything but a unit test.

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

- `cargo test --workspace` -> **258 tests pass** on Windows ARM64 and on Arch x64.
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

## Verified: real monitors and settings that stick (2026-09-09)

The control UI no longer shows a demo layout, and what the operator arranges survives
a restart.

**Monitors are read from the platform.** Enumerated through `tao`, which the toolkit
already provides on both platforms, rather than `EnumDisplayMonitors` plus a Wayland
output listener — two backends would be two chances to disagree about the coordinate
space, and agreeing with it is this editor's whole job.

- Windows ARM64 reads `\\.\DISPLAY1` as **2496x1664 at 120 Hz**, which is the
  1664x1109 logical desktop at its actual 150%% scale.
- Arch reads `eDP-1-0x82ED` at **1920x1080 with no refresh rate**, because `tao`
  documents `video_modes()` as unsupported on Linux and it always yields nothing.
  `refresh_rate` is therefore `Option`; filling in a plausible 60 Hz would put an
  unmeasured number on screen.

Positions and sizes are stored exactly as reported, deliberately **not** divided by
each monitor's scale factor. That division looks right on a single-monitor machine and
breaks as soon as two monitors differ: the virtual desktop is one coordinate space, so
a 100%% and a 150%% monitor sitting edge to edge share an exact boundary in it.
Rescaling each by its own factor turns that boundary into a gap or an overlap,
`adjacency` then reports no shared border, and the pointer can never cross. It would
only appear on mixed-DPI desks — which is exactly where nobody tests.

**Settings persist**, and doing so forced an identity bug into the open: the control
app minted a fresh random `DeviceId` on every launch, twice, so the Displays tab and
the Audio tab disagreed about which machine was "this machine". Nothing depended on it
until a saved route did. The id is now generated once and shared — a placeholder for
the Ed25519 identity in Milestone 1, not a substitute for it.

Restoring is a match keyed by **monitor name, never by index**. The parallel-walk
implementation is wrong the first time a monitor is unplugged: every monitor after it
shifts up one and inherits its neighbour's position, silently. Saved entries are hints
and never a source of monitors, so a saved name that is no longer attached is dropped
rather than resurrected as a ghost screen claiming an edge.

Verified end to end on both machines:

- Windows: dragged the peer, confirmed the position in `settings.json`, restarted, and
  it came back where it was left — overlapping, which the editor then flagged while
  correctly reporting no shared border.
- Arch: seeded `~/.config/ultidesk/settings.json` with the peer to the *left*, and it
  restored there with the shared border correctly reading `Left`. `XDG_CONFIG_HOME` is
  unset in that session, so this exercised the `HOME` fallback rather than the XDG
  path.

Saved audio routes are re-checked through `AudioRouting::add` on load rather than
trusted, so a stale file cannot reintroduce the feedback loop
[ADR-0011](adrs/0011-audio-routing-loop-prevention.md) exists to prevent.

Not done: the peer's screen and audio devices are still placeholders, labelled as not
connected. Both need the settings IPC, which does not exist. On Linux the agent has no
local IPC transport at all (`pipe.rs` is Windows-only), so that surface needs a Unix
socket before the control app can ask the agent anything.

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

## Verified: device identity (2026-09-09)

Executed on `aarch64-pc-windows-msvc`. A device is now an Ed25519 key pair and its
`DeviceId` is derived from the public key ([ADR-0012](adrs/0012-device-identity.md)),
replacing the random uuid that made a "peer" unnameable.

- `cargo test --workspace` -> **313 tests pass** (258 before). Clippy `-D warnings` and
  `cargo fmt --check` clean.
- `ultidesk-agent identity` run against a scratch config directory and then against the
  real one: created on the first run, `created:false` and byte-identical output on the
  second, so the id is stable across processes. The private key is never printed or
  logged.
- This machine: fingerprint `E7A0-CB0C-D78D-6E89-7BED`, key file at
  `%APPDATA%\\Ultidesk\\identity.json`.
- The control app shows the fingerprint in its header, because pairing works by an
  operator comparing that string against the other machine's.

New crate `ultidesk-identity`: `Identity`/`PeerKey`, the derived id, the pinned
`PeerStore`, the six-digit pairing code, and the file handling. 39 of the new tests are
its own. Pure Rust - deliberately no C toolchain, unlike the QUIC stack.

**Not verified**: nothing has been paired, because nothing connects. The pairing code
has never been computed against a real TLS channel binding - every test passes one in by
hand. The Arch machine has not run `identity` yet, so no second identity exists.

**Still a placeholder on Windows**: the key file is created `0600` on Unix and is merely
a normal file on Windows, which has no mode bits. Moving it into DPAPI / the Secret
Service is Milestone-1 work and has not been done.

## Verified: the authenticated peer transport (2026-09-09)

Executed on `aarch64-pc-windows-msvc`. `ultidesk-transport` is the ADR-0002 channel:
QUIC over TLS 1.3, mutually authenticated, pinned to the Ed25519 identities above. It
replaces the plaintext TCP transport for everything except a deliberate bench
comparison.

- `cargo test --workspace` -> **332 tests pass** (313 before). Clippy `-D warnings` and
  `cargo fmt --check` clean.
- **Pairing ran end to end** between two agents with separate identities and separate
  configuration directories: both sides displayed the same six digits (`709 328`), both
  pinned the other's key, and each recorded the other's fingerprint under an
  operator-chosen name.
- **A paired peer round-tripped Ping/Pong** through the real dispatcher over the
  authenticated channel: 0.9 / 2.2 / 3.7 ms min/avg/max over loopback.
- **An unpaired third identity was refused at the handshake**, by fingerprint, and never
  reached the dispatcher. The server logged
  `peer F5AE-... is not paired with this device (1 paired)` and the rejected client read
  the same text back through the TLS alert — so it learns which key to add rather than
  seeing "connection lost".
- Seven loopback integration tests pin the security properties: mutual authentication, an
  unpinned peer refused, an empty pinned set refusing everyone, the *dialling* side
  checking who answered, both ends deriving the same pairing code, two pairings never
  sharing a code, and the message-size cap.

Agent surface: `pair` (listen or dial), `peers` (list, `peers forget <key>` revokes),
`serve-peer`, `peer-ping`.

**Three things the tests found rather than the design**, all now fixed and worth knowing
before writing anything else on this channel:

1. `finish()` must wait for acknowledgement. Marking a stream finished and dropping the
   connection discards bytes still in flight; the peer reads "connection lost" instead of
   the reply that was, from the sender's side, definitely sent.
2. Streams are served concurrently. Draining them in sequence let one long-lived control
   stream block the next from ever being read.
3. A rejected peer is told why. `MessageStream` carries the connection so a stream error
   can report the connection's close reason.

**Not verified**: nothing has crossed between the two *machines* over this channel. Both
ends of every run above were on the Windows box, so it exercises the protocol and the
pinning but not the Wi-Fi link, and the Arch machine has not been paired. The 0.9-3.7 ms
figures are loopback and say nothing about the real link, where ICMP alone averages
28 ms after the power-save fix.

**Not built**: QUIC datagrams for pointer motion (everything is on the ordered control
stream today), discovery (an address is typed in), and per-peer permissions.

**Toolchain**: building this needs **clang** on `aarch64-pc-windows-msvc`. `ring`'s build
script overrides whatever compiler cc-rs found and asks for `clang` by name, because MSVC
cannot assemble its AArch64 sources. LLVM 22.1.8 was installed on the Windows machine for
this; the Arch machine already had clang.

## Verified: the two machines are paired and talking (2026-09-09)

The transport section above was measured entirely on loopback. This is the same code
between the two real machines, over the 5 GHz Wi-Fi link (Windows ARM64 at
`192.168.137.175`, Arch x64 at `192.168.137.9`, power save confirmed `off`, -45 dBm,
1.2 Gb/s negotiated).

- **Arch builds and passes the same suite.** `cargo build --workspace` clean,
  `cargo test --workspace` -> **332 tests**, the same total as Windows ARM64 despite a
  different distribution: Linux loses the named-pipe loopback test and gains the
  `0600` file-mode test. `cargo clippy --all-targets -D warnings` and `cargo fmt --check`
  clean on both — this time without needing a fix, unlike the last Linux run.
- **Arch has an identity**: fingerprint `7BAB-CD56-D896-A2A1-1BD0`, key file
  `~/.config/ultidesk/identity.json` created `-rw-------`. That is the Unix mode rule
  observed on a real machine rather than only in a unit test.
- **Pairing across the link.** Arch listened, Windows dialled. Both sides were held at
  the confirmation prompt so the codes could be compared *before* either was told to
  trust the other: both showed `264 511`, and each then pinned the other's key under an
  operator-chosen name. The keys stored match each machine's own reported public key.
- **Both directions carry traffic.** 8 Ping/Pong round trips each way through the real
  dispatcher over the authenticated channel:

  | direction | min | avg | max |
  | --- | --- | --- | --- |
  | Windows -> Arch | 7.8 ms | 39.3 ms | 48.1 ms |
  | Arch -> Windows | 8.3 ms | 26.6 ms | 46.9 ms |
  | ICMP, same window | 5 ms | 34 ms | 96 ms |

  The ICMP row is the point: the QUIC path is not the dominant cost. The link is, which
  is what the 2026-09-04 measurements already concluded — and it is why no input-latency
  figure quoted from this desk means anything until the machine is on the wire.
- **A stranger is refused across the link too.** A third identity on the Arch machine,
  given the same peer list so the only difference was that Windows had not pinned it, was
  refused during the handshake. Windows logged
  `peer 1728-E0B3-... is not paired with this device (1 paired)` and the stranger read the
  same text back through the TLS alert.
- The Arch agent chose **uinput** (`injector=uinput (no permission dialog)`), so it served
  the channel with nobody at the machine and no dialog anywhere.
- The Arch server identified its peer by the **name chosen at pairing**
  (`peer=windows-arm64`), not by an address or a uuid.

No inbound firewall rule had to be added on Windows: allow rules for
`ultidesk-agent.exe` already existed from the TCP-era work, and they name the binary
rather than a port or protocol, so they carried over to QUIC's UDP unchanged.

**Blocked at the time of this run, and resolved the next day**: `sudo` on the Arch
machine requires a password, so `sudo usermod -aG input $USER` could not be run from
here. It was run by hand on 2026-09-10 — see the section below.

## Verified: local IPC on Linux, and evdev unblocked (2026-09-10)

`sudo usermod -aG input $USER` was run on the Arch machine, which clears the last
outstanding permission blocker: `ultidesk-agent input-devices` now enumerates the real
hardware — `SYNA32A4:00 06CB:CE17 Mouse` at `/dev/input/event7` and
`AT Translated Set 2 keyboard` at `/dev/input/event3`. The capture path has still never
been *run*; grabbing a keyboard on a machine nobody is sitting at is not something to do
in passing.

`crates/agent/src/unix_socket.rs` gives the agent local IPC on Linux, which it had none
of. Verified live on Arch:

- Socket at `/run/user/1000/ultidesk/agent.sock`, mode `srw-------`, in a directory the
  session provides as `drwx------`. The handshake file beside it is `-rw-------`, because
  it carries the token.
- Authentication enforced through the same `Session` the named pipe uses: `Ping` before
  `Hello` refused, a wrong token refused, the real token accepted, then `Ping` -> `Pong`.
- `EnumerateWindows` returns `[]` and `InjectMouseMove` returns
  `input_unsupported` — honest answers rather than silence, since neither capability
  exists in this build on Linux.
- A second agent refuses to start: *"another Ultidesk agent is already listening ...;
  stop it first"*.
- `SIGTERM` removes the socket file.

**Test counts diverge on purpose now**: **341** on Windows ARM64 and **347** on Arch. The
difference is the six Unix-socket tests, which cannot compile on Windows. Clippy
`-D warnings` and `cargo fmt --check` clean on both.

### Two bugs the unit tests could not have caught

Recorded because both are about the *shape* of the tests rather than the code:

1. **`tokio::net::UnixListener::bind` needs a running runtime** and panics with "there is
   no reactor running" otherwise. `serve` created the listener before building the
   runtime, and every test is a `#[tokio::test]` — so a runtime always existed exactly
   where the precondition was being exercised. The binary panicked on its first real
   launch. `bind` is now `async`, which never awaits but makes calling it outside a
   runtime a compile error.
2. **`Drop` never ran.** The socket file survived every `pkill`, because a server whose
   body is `loop { accept }` only returns if `accept` fails, and `SIGTERM` kills the
   process mid-loop. Nothing broke — the next launch detected the file as stale and
   replaced it, which is the path working as designed — but a leftover socket makes a
   directory listing claim an agent is running. `serve` now selects on `SIGTERM`/`SIGINT`.

**What this does not do yet**: the request set is unchanged, so the control app still has
nothing new to ask for. Monitors, audio devices and topology are the settings IPC, and
that is the next piece.

## Verified: a machine can read a peer's audio devices (2026-09-10)

The first settings-IPC message, and the first time one machine learns something real
about the other instead of showing a placeholder. `ListAudioDevices` is answered by the
same `Session` all four transports share, so it works over the named pipe, the Unix
socket, the dev TCP path and the QUIC channel without any of them knowing about it.

Both directions, over the Wi-Fi link, against real hardware:

- **Windows -> Arch**: 6 PipeWire endpoints — two HDMI outputs, the chipset speaker, and
  two microphones — every one labelled `2301d188-…`, the Arch machine's derived device
  id.
- **Arch -> Windows**: 3 WASAPI endpoints — `Headphones (AirPods)`,
  `Speakers (Qualcomm(R) Aqstic(TM) …)` and the microphone array — every one labelled
  `4d2b951b-…`, the Windows machine's derived id.

**The answer is checked against the identity that was authenticated.** The peer fills in
the owning `DeviceId` itself, so nothing stops a compromised one from labelling its
endpoints with another machine's id — which would silently re-point a saved audio route
at a device on a third machine. It cannot, and only because the id is derived from the
public key ([ADR-0012](adrs/0012-device-identity.md)): the receiver knows which key
completed the handshake and computes the id that key is entitled to. Against a random
uuid there would be nothing to compare. One forged entry rejects the whole list rather
than being filtered out.

That refusal is unit-tested rather than demonstrated live — making a peer lie would mean
shipping a lying agent — but the check runs on every one of the live calls above, and
passing it is why they returned at all.

**350 tests on Windows ARM64, 356 on Arch.** Clippy `-D warnings` and `cargo fmt --check`
clean on both.

Two seams this needed, recorded because the next messages will use them:

- `Backends` bundles what a session can be asked about. Injection is something a peer
  *does to* this machine; audio is something the machine *reports about itself*. One
  trait for both would leave a future per-peer permission check unable to tell them
  apart. Bundled rather than passed one argument at a time, because monitors and topology
  are next and a dispatcher signature that grows per capability drags four transports and
  every test with it.
- The platform-DTO-to-`AudioDevice` mapping moved into the platform crate that owns the
  DTO. It was duplicated in the control app, and it encodes which field a saved route is
  keyed on — two copies are two chances for the editor and the agent to disagree about
  that, and the disagreement would surface as saved routes quietly failing to match.

**What is still missing before the control app's peer panel can be real**: the app talks
to no agent. It enumerates its own devices in-process, and asking a *peer* means asking
the local agent to relay — which is a message that does not exist, because the answer
would then carry a device id the local agent cannot vouch for. That relay, and its
verification story, is the next decision.

## Verified: per-peer permissions, enforced across the link (2026-09-10)

A paired peer could previously send every request the dispatcher accepts. Pairing is now
a decision about *what*, not only *who*: three permissions, each with a request behind it,
enforced on the machine that owns the capability
([permissions.md](permissions.md)).

Demonstrated between the two machines, with Arch serving and Windows asking:

- Arch revoked `read-devices` from the Windows peer. Windows asked anyway and was refused:
  `arch-x64 refused the request (not_permitted): this device does not allow "read-devices"
  for the peer on this session` — the message names the permission, so an operator knows
  which grant to change rather than only that something failed.
- `peer-ping` kept working while that revocation was in force, which is the point of
  leaving liveness ungated.
- Arch granted it back; the same call returned all 6 endpoints.
- The Arch agent logs what each peer may do as it connects:
  `peer connected peer=windows-arm64 allowed=["control-input", "read-devices", "list-windows"]`.

**366 tests on Windows ARM64, 372 on Arch.** Clippy `-D warnings` and `cargo fmt --check`
clean on both.

Both machines' `peers.json` were v1 and migrated on this run: every peer kept the
unrestricted access pairing used to mean, the grant was written back once so it is
visible and revocable, and the warning stopped repeating on the next launch. Defaulting
those peers to nothing would have broken a working setup with no explanation; within v2 a
missing or unrecognised field still reads as `false`, so a store this build cannot fully
understand grants less than intended rather than more.

`required_permission` has no wildcard arm, so adding a request fails to compile until
someone decides what it costs — a `_ => None` would let the next message ship ungated and
do it silently.

## Decided and built: how two machines' screens are arranged (2026-09-10)

The starting arrangement is a **strip, left to right, in the order machines connected**
(`ultidesk_topology::arrange`), and whatever the operator drags is remembered.

- The first machine keeps the desktop its platform reports, so a single-machine desk is
  untouched and matches what the OS shows.
- Each machine after it is translated so its left edge meets the previous machine's right
  edge. The two end up sharing a full-height border, so the pointer can cross before
  anyone has arranged anything.
- Each machine moves as **one block**, never monitor by monitor. Within a machine the
  virtual desktop is one coordinate space and two screens edge to edge share an exact
  boundary; nudging them independently turns that into a gap, `adjacency` reports no
  shared border, and the pointer stops crossing *within* that machine. A translation
  preserves differences, so a block move cannot do that.

**This settles the coordinate-space question**, which was blocking the monitor message and
could not be settled by measurement on the hardware here. Placing machines by their
reported absolute positions would mix physical and logical space. Nothing does: the offset
comes from the previous block's own measured width, so only distances *within* one machine
are ever compared, and those are self-consistent whatever the platform means by them.

What is left of the problem, and deliberately left: relative **sizes** are still not
comparable across machines, so a 150%-scaled desktop is drawn larger than an unscaled one
of the same physical size. That is a display problem, not a correctness one, and the
obvious fix — rescaling each monitor by its own factor — is exactly what would break the
internal adjacency above.

**Saved layouts are keyed by device *and* name.** `eDP-1` is a name two laptops both
report; matching on it alone would have applied one machine's saved position to the
other's screen, silently, and only on desks where both name their panels the same. An
entry written before layouts could hold two machines carries no device and still matches
by name — what it meant when it was written — and re-saving upgrades it, so the looser
match applies once and then stops.

**378 tests on Windows ARM64, 384 on Arch.** Clippy `-D warnings` and `cargo fmt --check`
clean on both.

Not done: the peer in the editor is still a placeholder screen, because `ListMonitors`
does not exist. The arrangement rule does not depend on that — it takes whatever monitors
it is given — so the placeholder is already positioned by the same code a real peer will
be.

## Verified: a machine can read a peer's monitors (2026-09-10)

`ListMonitors` completes the settings-IPC query set that exists today. Each platform crate
gained a **windowless** enumerator, because the agent is headless and the control app's
toolkit needs a window: `EnumDisplayMonitors` on Windows, `wl_output` + `xdg_output` on
Wayland. Neither raises a permission dialog.

Both directions, over the Wi-Fi link, against real hardware:

- **Windows -> Arch**: `eDP-1`, 1920x1080 logical, 1920x1080 mode, scale 1.0, and
  **144.028 Hz** — a refresh rate the toolkit path could never report on Linux, where
  `tao`'s `video_modes()` is unsupported and always yields nothing.
- **Arch -> Windows**: `\\.\DISPLAY1`, 2496x1664 at scale 1.5, primary.

Every monitor is labelled with the sending machine's derived device id and checked against
the key that completed the handshake, the same as audio endpoints. That check now has
**one** implementation over a trait rather than one per answer type: two copies of a
security check are two chances for one to be forgotten when the next answer is added, and
the forgotten one is the exploitable one.

### The second enumerator immediately caught a real bug

The whole reason `apps/control/src/monitors.rs` warns against two backends is that they
can disagree. They did, on the first run:

| | reported | scale |
|---|---|---|
| agent (`EnumDisplayMonitors`) | 1664x1109 | 1.0 |
| control app (`tao`) | 2496x1664 | 1.5 |

Neither was wrong for its own process. The agent had never declared DPI awareness, so
Windows handed it every rectangle **divided by the scale factor** and answered 96 DPI for
every monitor — a lie that is entirely self-consistent and undetectable from inside the
process. 1664 x 1.5 = 2496 is the whole explanation.

Self-consistency is not enough here: the agent tells a *peer* this machine's geometry,
reads positions the control app saved, and injects pointer coordinates in that same space.
Two processes on one machine differing by a factor of 1.5 puts the pointer in the wrong
place, and only on scaled displays — the ones most likely to be someone's actual laptop.
The agent now declares per-monitor-v2 awareness before reading anything
(`ultidesk_platform_windows::dpi`) and agrees with the toolkit.

On Wayland the equivalent trap is different and was avoided by design rather than caught:
`wl_output` reports a position in the compositor's global space and a size in *physical*
pixels, which on a scaled output are different spaces. `xdg-output-unstable-v1` reports
both logically, so that is what is read, and a compositor without it is refused rather
than guessed at. This one could not have been caught by testing here — the Arch machine
has a single 1.0-scale output, where the two spaces coincide.

**392 tests on Windows ARM64.** Clippy `-D warnings` and `cargo fmt --check` clean on both
machines.

Note for anyone reproducing the Linux run over SSH: an SSH shell is not part of the
graphical session, so the agent must be given `XDG_RUNTIME_DIR` and `WAYLAND_DISPLAY`
explicitly. Started normally inside the session it inherits both, and the error when it
cannot connect says so.

## Verified: the relay (2026-09-10)

The control app speaks only to its own agent — it has no identity, holds no pinned keys,
and a second connection from a second process would mean a second thing to authenticate
and two answers that could disagree. `AskPeer` closes that: the agent puts the question to
the peer and hands the answer back.

Demonstrated on the Windows machine against the Arch peer:

- With no address known: `peer_address_unknown: arch-x64 has never been reached from this
  device, so there is no address to try; discovery does not exist yet`. A specific answer,
  not a failure to connect.
- One direct `peer-monitors` teaches the address, which `peers` now shows as
  `last seen at 192.168.137.9:45872`.
- `ask-peer monitors` then finds the peer on its own: `from 7BAB-CD56-D896-A2A1-1BD0`,
  `eDP-1`, 1920 wide. `ask-peer devices` returns all 6 endpoints through the same path.

**Relaying is not a permission, and cannot be granted.** A peer able to relay would reach
a third machine it was never paired with, wearing this machine's trust to do it — so the
gate grew a second axis, `LocalOnly`, that no grant can express. A peer allowed everything
is still refused, which is pinned by a test. Belt and braces: the peer-facing transports
are built with no relay context at all, so a mistake in the gate leaves nothing to hop
with.

**The ownership check stays where it can be made.** Only the party that completed the
handshake knows which key was proved, and that is the agent. So the check runs before the
answer is relayed at all, and the reply carries the peer's key so the answer says whose it
is. That is a statement about a check already performed rather than one the caller is
expected to repeat: a control app that could not trust its own agent has already lost,
since that agent injects its input and holds its private key.

**403 tests on Windows ARM64, 409 on Arch.** Clippy `-D warnings` and `cargo fmt --check`
clean on both.

A structural note worth keeping: `Session::dispatch` now returns a *decision* rather than
always a reply. `AskPeer` cannot be answered without network I/O, and the gate is
deliberately synchronous so that authentication and permissions stay unit-testable without
a runtime — so it authorises and hands the work back to the transport that has one. The
alternative, intercepting the message in each transport, would need the gate repeated per
transport, and the copy that was forgotten would be the way in.

The stored address is a **hint, never an identity**. Nothing compares addresses; the key
alone decides who a connection is. A missing address therefore has no security meaning,
which is why adding the field needed no schema bump — unlike a missing permission, where
absence had to be read as "granted nothing".

## Verified: the control app asks its agent (2026-09-10)

The peer panels were placeholders, and not for want of an answer — the agent has been able
to report a peer's monitors and endpoints for two commits. They were placeholders for want
of a *message the app could name*, because the wire types lived inside the agent binary.

`ultidesk-ipc` is now a library: the message types, the handshake descriptor, and a client
that finds the agent, authenticates and asks. What deliberately did **not** move is the
gate — enforcement belongs on the machine providing the capability, and a client knowing
the shape of a request has never been what authorises it.

Verified against live agents on both machines, over both transports:

| | asked | got |
|---|---|---|
| Windows, named pipe | `ask monitors` | `\\.\DISPLAY1`, 2496x1664, scale 1.5 |
| Windows, named pipe | `ask devices` | 3 endpoints |
| Windows -> Arch, relayed | `ask-peer monitors` | `eDP-1`, 1920 wide, 144.028 Hz |
| Windows -> Arch, relayed | `ask-peer devices` | 6 endpoints |
| Arch, Unix socket | `ask monitors` | `eDP-1`, 144.028 Hz |
| Arch, Unix socket | `ask devices` | 6 endpoints |

`ask` and `ask-peer` go through the *running* agent rather than doing the work in-process,
which is what makes them a test of the path the UI depends on rather than of a parallel
one.

**The control app now reads both machines through that one connection — including its
own.** That is the point rather than a convenience. Two enumerators on one machine already
disagreed by a factor of 1.5 (see the DPI section above); the bug is fixed, but the class
of bug only goes away when there is one source, and the agent's numbers are the ones a
peer is told. The window toolkit stays as the fallback for when no agent is running.

Both panels degrade in halves rather than all at once: this machine's endpoints stay real
when the peer is asleep, a peer that cannot be reached keeps its placeholder rather than
vanishing and silently dropping the arrangement made of it, and "not paired" and "paired
but unreachable" are different messages because they have different fixes.

A stale handshake file — one left behind by an agent that died — used to surface as *"the
system cannot find the file specified"*, describing a pipe nobody had asked about. It now
reads as `no agent is running on this machine (a handshake file names …, but nothing is
listening)`.

**403 tests on Windows ARM64, 409 on Arch.** Clippy `-D warnings` and `cargo fmt --check`
clean on both.

**Not verified**: the control app's *window*. Everything above exercises the same client
code the UI calls, but nobody has watched the editor draw a peer's real screen — that
needs someone at a desk, and on the Arch machine a GUI launched over SSH hits the
`global-hotkey` X11 crash recorded in ADR-0010.

## Exact next step

**Look at it.** Every piece of the settings surface is now built and exercised
head-lessly, and the one thing nobody has done is open the control app and see a peer's
real monitor drawn next to the local one. That wants a person at one of the machines; on
Arch it also wants a real session rather than SSH, because of the `global-hotkey` crash in
[ADR-0010](adrs/0010-dioxus-control-ui.md).

After that, the next unbuilt thing is the one the whole settings surface was for:
**wiring the KVM together as a daemon** (next.md item 3). Everything it needs now exists —
an authenticated channel, a topology both machines agree on, evdev capture unblocked, and
uinput injection that needs no dialog. What is missing is the assembly: something that
watches the pointer, notices it hit a shared edge, and hands over.
