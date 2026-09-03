# Ultidesk compatibility matrix

Rule: **never** replace "Untested" with "Supported" because an API exists. Only observed,
executed results count. Where the current dev environment cannot exercise a capability,
it stays "Untested" with exact manual steps recorded.

## Platform matrix

| Platform | Arch | Window enum | Input inject | Capture | Clipboard | File transfer | Window drag | Notes |
|---|---|---|---|---|---|---|---|---|
| Windows 11 | x64 | **Pass** | Untested | Untested | Untested | Untested | Untested | Enum verified live on `x86_64-pc-windows-msvc` (8 windows). `SendInput` code compiles + unit-tested for coord math, but injection into a real app not yet manually run. |
| Windows 11 | ARM64 | **Pass** | Untested | Untested | Untested | Untested | Untested | Enum verified live on native `aarch64-pc-windows-msvc` (8 windows, Qualcomm ARMv8). Whole toolchain native, no Prism — see ADR-0008. `SendInput` unit-tested only, same as x64. |
| Arch KDE Wayland | x64 | **N/A by design** | **Pass** | Negotiates to Start-ready | Not implemented | Not implemented | Not implemented | Builds and tests clean on `x86_64-unknown-linux-gnu` (58 tests, clippy/fmt clean). **Capability probing implemented and executed** (`ultidesk-agent probe`). Window *enumeration* is impossible on Wayland and is not a gap to close — see [ADR-0009](adrs/0009-wayland-portal-picker.md). Capture/input still unimplemented; `serve` exits 1 ("Windows-only"). |
| Arch GNOME Wayland | x64 | Untested | Untested | Untested | Untested | Untested | Untested | Portal versions differ from KDE; must be probed separately. |
| Arch X11 | x64 | Untested | Untested | Untested | Untested | Untested | Untested | XTest/XInput, XComposite, XFixes paths not built. |
| XWayland windows | x64 | Untested | Untested | Untested | Untested | Untested | Untested | — |

## What "Pass" means here

- **Windows x64 window enumeration**: `cargo run -p ultidesk-agent -- enumerate` was
  executed on the development machine and returned real top-level windows with valid PIDs,
  geometry, and handles, after applying the visible/unowned/not-tool/not-cloaked filter.
- **Windows ARM64 window enumeration**: the same command, executed on a Qualcomm
  Snapdragon (ARMv8) Windows 11 machine against a natively built ARM64 binary (verified
  `AA64` in the PE header, not an x64 image under Prism). Returned 8 real top-level
  windows across 6 processes with plausible geometry and non-zero handles.

## Windows ARM64 native-build status (measured 2026-09-02)

Host: Windows 11 Pro 26200, Qualcomm Snapdragon (ARMv8), `aarch64-pc-windows-msvc`.

| Component | Result |
|---|---|
| `cargo test --workspace` | 46/46 pass (identical count to x64) |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean |
| `cargo fmt --check` | clean |
| `ultidesk-agent.exe` | ARM64 PE image |
| Node / Electron 33.3.1 (Chrome 130) | ARM64, incl. all bundled Chromium DLLs |
| esbuild, rollup native addon | ARM64 (see ADR-0008 for the trap here) |
| `@ultidesk/desktop` vitest + `tsc --noEmit` | 29/29 pass, typecheck clean |

Still untested on ARM64 for the same reason as x64 — they need the GUI that
[status.md](status.md) tracks — are input injection, capture, clipboard, file transfer,
and window drag. **No ARM64 capability is marked Pass on the strength of the build
succeeding.**


## Arch KDE Wayland portal capability probe (measured 2026-09-02)

Host: Arch Linux, kernel 7.1.2, Intel i5-10300H, KDE Plasma **Wayland** session
(`kwin_wayland` + `plasmashell` on seat0), `xdg-desktop-portal` 1.22.1,
`xdg-desktop-portal-kde` 6.7.2. Read-only D-Bus property reads — no session was
created, so no permission dialogs were raised on the user's desktop.

| Portal interface | Version | Advertised capability |
|---|---|---|
| `org.freedesktop.portal.ScreenCast` | 4 | `AvailableSourceTypes=7` → MONITOR | WINDOW | VIRTUAL |
| `org.freedesktop.portal.RemoteDesktop` | 2 | `AvailableDeviceTypes=7` → KEYBOARD | POINTER | TOUCHSCREEN |
| `org.freedesktop.portal.InputCapture` | 2 | `SupportedCapabilities=7` → KEYBOARD | POINTER | TOUCHSCREEN |
| `org.freedesktop.portal.Clipboard` | 1 | present |
| `org.freedesktop.portal.FileTransfer` | 1 | present (on the Documents object, not Desktop) |
| `org.freedesktop.portal.GlobalShortcuts` | 2 | present |

Audio: PipeWire 1.6.7 + WirePlumber, with `pipewire`, `pipewire-pulse` and
`wireplumber` all active. `module-rtp-sink`, `module-rtp-source` and
`module-pulse-tunnel` are installed, so network audio transport needs no extra
packages.

**What this does and does not mean.** It means the *platform* can support
window-level capture, input injection, edge-crossing input capture, clipboard, and
network audio. It does **not** mean Ultidesk can: none of these portals is called by
any code in the tree today. Per the matrix above, `WINDOW` in `AvailableSourceTypes`
is a property of KDE, not a capability of this project.

`InputCapture` v2 being present is the notable find: it is the correct primitive for
KVM edge-crossing (grab pointer/keyboard at a screen edge and forward the stream)
rather than a `RemoteDesktop` workaround, and it removes the main open question from
ADR-0005's Linux plan.

## KDE Plasma 6.7.2 portal results (updated 2026-09-03)

All three portals now work. An earlier revision of this file recorded InputCapture as
"accepted but never answered"; that was true of 2026-09-02 and is **superseded**. Two
separate faults were tangled together, and both are fixed:

1. **`session_handle` type.** RemoteDesktop and ScreenCast return it as a string
   (`s`); InputCapture returns a real object path (`o`). Assuming a string produced a
   bare `incorrect type` from zvariant naming neither the portal nor the field.
   `portal_call::session_handle` now accepts both shapes in one place.
2. **Barrier wire shape.** `SetPointerBarriers` takes `aa{sv}` — an array of plain
   dicts with the id carried inside as `barrier_id` — not `a(ua{sv})`. The portal
   rejected the struct form with an exact signature diff, which is what made it
   fixable:
   `Type of message, "(oa{sv}a(ua{sv})u)", does not match expected type "(oa{sv}aa{sv}u)"`.

A third issue was self-inflicted and worth recording: every error was wrapped as
"could not reach the session bus", including ones with nothing to do with connecting.
That sent debugging at the connection for far too long. `PortalError` now separates
`Connect` from `Bus`.

### Barrier coordinate convention (measured, not read off the spec)

The portal reports rejections by id and explains nothing, so the convention was
established by submitting candidate encodings together and seeing which came back in
`failed_barriers`. On a single 1920x1080 zone at (0,0):

| Candidate | Position | Result |
|---|---|---|
| all-inclusive | `(1919, 0, 1919, 1079)` | rejected |
| all-exclusive | `(1920, 0, 1920, 1080)` | rejected |
| inclusive x, exclusive y | `(1919, 0, 1919, 1080)` | rejected |
| **boundary x, inclusive y** | `(1920, 0, 1920, 1079)` | **accepted** |

The rule is **mixed**: the coordinate *perpendicular* to the barrier is the boundary
line (`x + width`), while the extent *along* it is an inclusive pixel range
(`y .. y + height - 1`). Verified to generalise — all four edges submitted together
were accepted:

| Edge | Position |
|---|---|
| Left | `(0, 0, 0, 1079)` |
| Right | `(1920, 0, 1920, 1079)` |
| Top | `(0, 0, 1919, 0)` |
| Bottom | `(0, 1080, 1919, 1080)` |

This convention is why the right edge of one monitor and the left edge of the monitor
beside it are the *same line* — the property edge crossing depends on. It is pinned by
unit tests in `input_capture.rs`.

### Verified working

| Portal | Result |
|---|---|
| `RemoteDesktop` | **Pass** — pointer moved via `NotifyPointerMotion`; `restore_token` issued and re-used, second run needed no prompt |
| `InputCapture` | `CreateSession` -> `GetZones` -> `SetPointerBarriers` all succeed; barrier accepted. Event delivery still needs a libei client |
| `ScreenCast` | `CreateSession` + `SelectSources` succeed in ~4 ms; `Start` (the picker) not exercised |

## Measured link between the two dev machines (2026-09-02)

Windows ARM64 laptop <-> Arch x64 laptop, over the Windows ICS WiFi subnet
(`192.168.137.0/24`, Arch on `wlan0`). TCP with Nagle disabled, 500 round trips of an
8-byte frame, then 32 MiB transferred each way.

| Metric | Result |
|---|---|
| RTT p50 | 7.1 ms |
| RTT p90 | 41.6 ms |
| RTT p99 | 87.2 ms |
| RTT max | 292.2 ms |
| Jitter (mean abs. deviation from median) | 15.2 ms |
| Throughput, Windows -> Arch | 167 Mbit/s |
| Throughput, Arch -> Windows | 237 Mbit/s |

**Bandwidth is not the constraint; latency is.** ADR-0003 targets 1080p60 at
~5-30 Mbit/s, and this link has 5-8x that headroom in both directions. But a p90 of
41 ms and a p99 of 87 ms on an 8-byte round trip is a WiFi/power-save profile, not a
usable interactive budget: KVM pointer motion is perceptible above roughly 30 ms, so
the *least* bandwidth-hungry feature is the one this link threatens first.

Re-measure on wired Ethernet before drawing conclusions about the design, and treat
any KVM or projection latency budget as unvalidated until then. These numbers
characterize this particular WiFi path, not Ultidesk.

## Wayland compatibility fields to fill (per compositor + portal version)

ScreenCast · RemoteDesktop · InputCapture · Clipboard integration · persistent restore
permission · window-level vs. monitor-level capture. Build this from real runs on KDE
Plasma, GNOME, and a wlroots compositor; note when a compositor lacks a required portal
version rather than claiming universal support.

## Windows capture behavior to verify (Milestone 0 probes, not yet done)

For a captured window: behavior when **covered, resized, minimized, moved, closed**; and
that elevated targets fail input via UIPI (fail closed). Record measured results here.
