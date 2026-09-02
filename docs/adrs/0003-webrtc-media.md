# ADR-0003: WebRTC for the media plane (H.264-first, LAN ICE, no TURN)

- Status: Accepted
- Date: 2026-07-31

## Context

Projection needs low-latency, adaptive, hardware-accelerated video between two LAN peers,
with congestion control and packet-loss recovery already solved. Building a bespoke RTP
pipeline for the MVP would be wasteful and risky.

## Decision

Use **WebRTC** for the initial video stream:

- **H.264 first** for broad hardware encode/decode compatibility.
- Host/LAN ICE candidates only; **no STUN/TURN** in the MVP (no relay, no Internet).
- Adaptive resolution/bitrate/framerate (~5–30 Mbps), 1080p60 target, low-latency playback,
  keyframe requests after loss/resize.
- SDP/ICE exchanged over the authenticated control channel (ADR-0002); media is refused if
  not tied to the expected paired identity.
- Behind the `MediaBackend` interface so Chromium specifics never leak into topology/input/
  session logic; later native backends (Windows.Graphics.Capture, PipeWire) implement the
  same surface.

## Consequences

- Reuses a battle-tested congestion/loss stack; hardware codecs where Chromium exposes them.
- Chromium desktop capture has limits (per-window audio, protected/DRM/exclusive-fullscreen
  surfaces, no direct keyframe-force API) — documented as limitations, not worked around.
- The dev loopback signaling broker used in Milestone 0 is dev-only and replaced by the
  authenticated control channel in Milestone 1.
