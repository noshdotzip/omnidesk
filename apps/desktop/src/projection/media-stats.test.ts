import { describe, it, expect } from "vitest";
import { classifyAcceleration, computeBitrateBps, type ByteSample } from "./media-backend";

// These are the pure halves of the WebRTC stats plumbing. The RTCPeerConnection side
// needs a live Electron renderer (docs/testing.md); the classification and rate math
// do not, and they are what the ARM64 hardware-encode question actually hinges on.

describe("classifyAcceleration", () => {
  it("trusts the standardized powerEfficient signal over the implementation name", () => {
    // Name says software, but the runtime explicitly says power-efficient: believe it.
    expect(classifyAcceleration("libvpx", true)).toBe(true);
    expect(classifyAcceleration("MediaFoundationVideoEncodeAccelerator", false)).toBe(false);
  });

  it("recognises Windows hardware encoders by name when powerEfficient is absent", () => {
    expect(classifyAcceleration("MediaFoundationVideoEncodeAccelerator", undefined)).toBe(true);
    expect(classifyAcceleration("D3D11VideoDecoder", undefined)).toBe(true);
    expect(classifyAcceleration("ExternalDecoder", undefined)).toBe(true);
  });

  it("recognises software fallbacks by name", () => {
    expect(classifyAcceleration("OpenH264", undefined)).toBe(false);
    expect(classifyAcceleration("libvpx", undefined)).toBe(false);
    expect(classifyAcceleration("SimulcastEncoderAdapter (libvpx, libvpx)", undefined)).toBe(false);
  });

  it("classifies a hardware-sounding wrapper around a software codec as software", () => {
    // The failure mode this guards: reporting "hardware" for a software encoder just
    // because Chromium wrapped it in an adapter with an accelerator-ish name.
    expect(classifyAcceleration("ExternalEncoder (libvpx)", undefined)).toBe(false);
  });

  it("returns undefined when neither signal is conclusive, never a false 'software'", () => {
    expect(classifyAcceleration(undefined, undefined)).toBeUndefined();
    expect(classifyAcceleration("", undefined)).toBeUndefined();
    expect(classifyAcceleration("SomeEncoderWeHaveNotSeen", undefined)).toBeUndefined();
  });
});

describe("computeBitrateBps", () => {
  const at = (bytes: number, timestampMs: number): ByteSample => ({ bytes, timestampMs });

  it("converts a byte delta over a time delta into bits per second", () => {
    // 125_000 bytes in 1s = 1_000_000 bit/s.
    expect(computeBitrateBps(at(0, 1000), at(125_000, 2000))).toBe(1_000_000);
  });

  it("handles sub-second sampling intervals", () => {
    expect(computeBitrateBps(at(1_000, 1000), at(26_000, 1200))).toBe(1_000_000);
  });

  it("returns undefined for the first sample", () => {
    expect(computeBitrateBps(null, at(125_000, 2000))).toBeUndefined();
  });

  it("returns undefined rather than dividing by a clock that has not advanced", () => {
    expect(computeBitrateBps(at(0, 1000), at(125_000, 1000))).toBeUndefined();
    expect(computeBitrateBps(at(0, 2000), at(125_000, 1000))).toBeUndefined();
  });

  it("returns undefined on a counter reset instead of a negative rate", () => {
    expect(computeBitrateBps(at(500_000, 1000), at(1_000, 2000))).toBeUndefined();
  });

  it("reports zero for a stalled stream whose clock still advances", () => {
    // Distinct from 'unknown': the stream is genuinely sending nothing.
    expect(computeBitrateBps(at(4_096, 1000), at(4_096, 2000))).toBe(0);
  });
});
