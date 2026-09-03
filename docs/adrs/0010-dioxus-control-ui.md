# ADR-0010: Dioxus for the control UI

- Status: **Proposed** (requested 2026-09-03; not implemented)
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

## Open questions to settle before accepting

1. **Two UI stacks, or eventually one?** If Dioxus proves sufficient, does the
   projection window migrate to it too, retiring Electron? That depends on whether a
   Dioxus surface can host a low-latency H.264 stream without reimplementing what
   Chromium provides. Prototype the video path before assuming either answer.
2. **Which Dioxus renderer?** Desktop (WebView) reintroduces a browser engine and much
   of the footprint Electron was being avoided for; Blitz is native but far less mature.
   The choice materially changes the footprint argument.
3. **Windows ARM64.** Ultidesk is native-only on ARM64 (ADR-0008). Whichever renderer is
   chosen must have a genuinely native aarch64 build, and any native dependency it pulls
   in has to be checked the same way esbuild and rollup were.
4. **IPC shape.** The control UI mutates state the Rust agent owns. It should speak the
   existing IPC protocol rather than growing a parallel one — see ADR-0004 on keeping a
   single protocol schema.

## Consequences

- The topology editor gets a native, directly-manipulable surface instead of an
  Electron window, which suits dragging monitor rectangles.
- Two UI toolchains must be built, tested and shipped on both platforms until (1) is
  resolved.
- Nothing here changes the media plane: ADR-0003 (WebRTC) stands.
