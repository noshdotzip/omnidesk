# ADR-0010: Dioxus for the control UI

- Status: **Accepted** for the control UI (prototyped and running 2026-09-03)
- Date: 2026-09-03

## Context

Ultidesk needs a control surface distinct from the projection surface: arranging the
virtual positions of monitors across machines, choosing which edge crosses to which
peer, cursor behaviour, audio routing and speaker selection, per-peer permissions, and
pairing. This is ordinary application UI with a lot of direct manipulation — dragging
monitor rectangles into a topology is the central interaction — and it is stateful,
long-lived, and mostly independent of the media plane.

ADR-0001 chose Electron plus Rust, but for the *projection* path specifically: proxy
windows hosting WebRTC video, where Chromium's media stack is the reason to be there at
all. The control UI has no such requirement. It needs no video pipeline, no WebRTC, and
no DOM-heavy rendering.

The project owner has asked for this control UI to be built in **Dioxus**, the Rust UI
framework, citing the Kopuz music player's Dioxus interface as the visual reference.
(That reference has not been reviewed yet; confirm the exact repository before treating
it as a spec.)

## Decision

Build the control UI — topology editor, cursor settings, audio routing, permissions,
pairing — in Dioxus. Keep Electron for the projection proxy windows, where Chromium's
WebRTC stack is load-bearing.

Marked Proposed rather than Accepted because it has not been prototyped, and because it
deliberately introduces a second UI stack. That cost is real and should be paid with
eyes open, not discovered later.

## What the prototype settled

A working display-arrangement editor now exists (`apps/control`), so two of the four
questions below are answered by measurement rather than argument.

**Windows ARM64 (question 3): confirmed native.** `dioxus 0.6` with the `desktop`
feature builds for `aarch64-pc-windows-msvc` and runs — a real window, correct title,
no stderr. ADR-0008 is satisfied; nothing here runs under Prism.

Building it did expose that the Windows toolchain was still on rustc 1.86 while the
Arch box was on 1.98. A transitive dependency requires 1.88, so the workspace would
not build until `rustup update stable` brought Windows to 1.98.1. The whole workspace
passes clippy `-D warnings` on the newer compiler.

**Renderer (question 2): it is the WebView.** The build pulls `webview2-com` and
`tao`, so Dioxus Desktop is Chromium-via-WebView2 on Windows. That is the footprint
concern this ADR raised, and it is real: choosing Dioxus does not avoid shipping a
browser engine, it swaps Electron's bundled one for the system one. What it does buy
is a Rust-native UI that shares `ultidesk-topology` directly, with no IPC or
duplicated geometry between the editor and the agent.

**Not yet verified: the Linux build.** Dioxus Desktop uses `wry`/`webkit2gtk` there,
which needs `webkit2gtk-4.1` development headers. Whether the Arch box has them is
unchecked — it went offline mid-session. If they are absent this is a second
toolchain dependency to install, and it should be recorded here before the ADR is
treated as settled on Linux.

## Open questions still outstanding

1. **Two UI stacks, or eventually one?** If Dioxus proves sufficient, does the
   projection window migrate to it too, retiring Electron? That depends on whether a
   Dioxus surface can host a low-latency H.264 stream without reimplementing what
   Chromium provides. Prototype the video path before assuming either answer.
2. **Which Dioxus renderer?** Desktop (WebView) reintroduces a browser engine and much
   of the footprint Electron was being avoided for; Blitz is native but far less mature.
   The choice materially changes the footprint argument.
3. ~~**Windows ARM64.**~~ Answered above: native, verified running.
4. **IPC shape.** The control UI mutates state the Rust agent owns. It should speak the
   existing IPC protocol rather than growing a parallel one — see ADR-0004 on keeping a
   single protocol schema.

## Consequences

- The topology editor gets a native, directly-manipulable surface instead of an
  Electron window, which suits dragging monitor rectangles.
- Two UI toolchains must be built, tested and shipped on both platforms until (1) is
  resolved.
- Nothing here changes the media plane: ADR-0003 (WebRTC) stands.
- **Linux picks up an X11 dependency.** `tao` pulls in `muda` for menus, and
  `libxdo-sys` emits `cargo:rustc-link-lib=xdo` unconditionally, so the control app
  will not link on Linux without `xdotool` installed (69 KiB). This is an X11
  automation library being linked into a Wayland application; it works under XWayland
  and costs nothing at runtime, but it is a real dependency the Electron path did not
  have, and worth naming rather than discovering at packaging time. Found 2026-09-04
  when the app first built against Arch — Windows had never surfaced it.
- **And an X11 dependency that can crash the app, not just fail to link.**
  `global-hotkey` is a non-optional dependency of `dioxus-desktop` on Linux — there is
  no feature flag to drop it, and `dioxus-desktop` constructs a `GlobalHotKeyManager`
  unconditionally. Its thread calls `XDefaultRootWindow` without checking that
  `XOpenDisplay` succeeded, so if X11 is unreachable the app segfaults at startup
  instead of losing hotkey support. A normal desktop launch is unaffected (the session
  provides `DISPLAY` and `XAUTHORITY`), but a pure Wayland session with no XWayland
  would hit it. The window itself is native Wayland; only the hotkey path needs X11.
  Worth revisiting if the control UI ever needs to run headless or in a minimal
  session.
