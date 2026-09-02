# Ultidesk compatibility matrix

Rule: **never** replace "Untested" with "Supported" because an API exists. Only observed,
executed results count. Where the current dev environment cannot exercise a capability,
it stays "Untested" with exact manual steps recorded.

## Platform matrix

| Platform | Arch | Window enum | Input inject | Capture | Clipboard | File transfer | Window drag | Notes |
|---|---|---|---|---|---|---|---|---|
| Windows 11 | x64 | **Pass** | Untested | Untested | Untested | Untested | Untested | Enum verified live on `x86_64-pc-windows-msvc` (8 windows). `SendInput` code compiles + unit-tested for coord math, but injection into a real app not yet manually run. |
| Windows 11 | ARM64 | Untested | Untested | Untested | Untested | Untested | Untested | Build feasibility not yet attempted. |
| Arch KDE Wayland | x64 | Untested | Untested | Untested | Untested | Untested | Untested | Needs XDG ScreenCast/RemoteDesktop/InputCapture portal probes. |
| Arch GNOME Wayland | x64 | Untested | Untested | Untested | Untested | Untested | Untested | Portal versions differ from KDE; must be probed separately. |
| Arch X11 | x64 | Untested | Untested | Untested | Untested | Untested | Untested | XTest/XInput, XComposite, XFixes paths not built. |
| XWayland windows | x64 | Untested | Untested | Untested | Untested | Untested | Untested | — |

## What "Pass" means here

- **Windows x64 window enumeration**: `cargo run -p ultidesk-agent -- enumerate` was
  executed on the development machine and returned real top-level windows with valid PIDs,
  geometry, and handles, after applying the visible/unowned/not-tool/not-cloaked filter.

## Wayland compatibility fields to fill (per compositor + portal version)

ScreenCast · RemoteDesktop · InputCapture · Clipboard integration · persistent restore
permission · window-level vs. monitor-level capture. Build this from real runs on KDE
Plasma, GNOME, and a wlroots compositor; note when a compositor lacks a required portal
version rather than claiming universal support.

## Windows capture behavior to verify (Milestone 0 probes, not yet done)

For a captured window: behavior when **covered, resized, minimized, moved, closed**; and
that elevated targets fail input via UIPI (fail closed). Record measured results here.
