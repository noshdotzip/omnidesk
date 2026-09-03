/**
 * MediaBackend abstraction (brief §6).
 *
 * Capture/encode/transport is deliberately behind this interface from day one so that
 * topology, input, and session logic never depend on Chromium-specific source ids.
 * The first implementation, [`ElectronWebRtcMediaBackend`], uses Electron's
 * desktopCapturer + WebRTC. Later backends (`WindowsGraphicsCaptureBackend`,
 * `PipeWirePortalBackend`, a native encoder backend) implement the same surface.
 *
 * Method names follow the brief's list (`list_sources`, `create_offer`, …) in camelCase.
 */

export interface MediaSource {
  /** Opaque, backend-specific id. Never leaks past this interface. */
  id: string;
  name: string;
  /** Native pixel size if known (used to seed letterbox math). */
  width?: number;
  height?: number;
}

export interface CaptureConstraints {
  maxWidth: number;
  maxHeight: number;
  maxFrameRate: number;
}

export interface MediaStats {
  codec?: string;
  frameWidth?: number;
  frameHeight?: number;
  framesPerSecond?: number;
  bitrateBps?: number;
  rttMs?: number;
  packetsLost?: number;
  jitterMs?: number;
  /**
   * Whether the sending pipeline is using a hardware encoder. `undefined` means the
   * runtime did not say — never conflate "unknown" with "software".
   */
  hardwareEncode?: boolean;
  /** Same question for the receiving pipeline. */
  hardwareDecode?: boolean;
  /** Raw runtime strings, kept for diagnostics (e.g. "MediaFoundationVideoEncodeAccelerator"). */
  encoderImplementation?: string;
  decoderImplementation?: string;
}

export interface MediaBackend {
  listSources(): Promise<MediaSource[]>;
  getSourceMetadata(sourceId: string): Promise<MediaSource | null>;
  startCapture(sourceId: string, constraints: CaptureConstraints): Promise<void>;
  createOffer(): Promise<RTCSessionDescriptionInit>;
  acceptOffer(offer: RTCSessionDescriptionInit): Promise<void>;
  applyAnswer(answer: RTCSessionDescriptionInit): Promise<void>;
  addIceCandidate(candidate: RTCIceCandidateInit): Promise<void>;
  setBitrate(bitsPerSecond: number): Promise<void>;
  setFramerate(framesPerSecond: number): Promise<void>;
  requestKeyframe(): Promise<void>;
  pause(): Promise<void>;
  resume(): Promise<void>;
  stop(): Promise<void>;
  getStats(): Promise<MediaStats>;
  /** Callbacks the session wiring subscribes to. */
  onIceCandidate(cb: (c: RTCIceCandidateInit) => void): void;
  onRemoteTrack(cb: (stream: MediaStream) => void): void;
}

/** A cumulative byte counter sample, used to turn WebRTC's counters into a rate. */
export interface ByteSample {
  bytes: number;
  timestampMs: number;
}

/**
 * Convert two cumulative byte-counter samples into a bitrate.
 *
 * Returns `undefined` when there is no usable delta — the first sample, a clock that
 * has not advanced, or a counter reset — rather than reporting a bogus zero or a
 * negative rate that would poison an adaptive-bitrate decision.
 */
export function computeBitrateBps(prev: ByteSample | null, next: ByteSample): number | undefined {
  if (prev === null) return undefined;
  const seconds = (next.timestampMs - prev.timestampMs) / 1000;
  if (seconds <= 0) return undefined;
  const deltaBytes = next.bytes - prev.bytes;
  if (deltaBytes < 0) return undefined;
  return Math.round((deltaBytes * 8) / seconds);
}

// Chromium reports the concrete implementation, e.g. "MediaFoundationVideoEncodeAccelerator"
// (hardware) or "SimulcastEncoderAdapter (libvpx, libvpx)" / "OpenH264" (software).
// Software is tested first so a hardware-sounding wrapper around a software codec
// (e.g. "ExternalEncoder (libvpx)") is still classified honestly as software.
const SOFTWARE_IMPLEMENTATION = /openh264|libvpx|libaom|ffmpeg|dav1d/i;
const HARDWARE_IMPLEMENTATION = /mediafoundation|d3d11|external(encoder|decoder)|hardware|qualcomm|nvenc|vaapi|videotoolbox/i;

/**
 * Decide whether an encoder/decoder implementation is hardware accelerated.
 *
 * This is the metric that matters most on Windows ARM64. If Chromium cannot reach the
 * Snapdragon MediaFoundation H.264 encoder it falls back to software OpenH264 *without
 * failing* — the projection still "works", just at a fraction of the framerate and a
 * multiple of the power draw. Making that fallback observable is the prerequisite for
 * treating it as a bug rather than as the baseline.
 *
 * `powerEfficientEncoder`/`powerEfficientDecoder` is the standardized signal and is
 * trusted first; the implementation name is only a fallback. Returns `undefined` when
 * neither signal is conclusive — callers must not read "unknown" as "software".
 */
export function classifyAcceleration(
  implementation: string | undefined,
  powerEfficient: boolean | undefined,
): boolean | undefined {
  if (typeof powerEfficient === "boolean") return powerEfficient;
  if (implementation === undefined || implementation.length === 0) return undefined;
  if (SOFTWARE_IMPLEMENTATION.test(implementation)) return false;
  if (HARDWARE_IMPLEMENTATION.test(implementation)) return true;
  return undefined;
}

/** H.264-first, LAN-only (host ICE), no TURN — matches the brief's media-plane rules. */
const RTC_CONFIG: RTCConfiguration = {
  iceServers: [], // LAN only: no STUN/TURN in the MVP
  iceTransportPolicy: "all",
};

/**
 * WebRTC backend using Electron/Chromium. Works both as source (captures a window and
 * sends) and as destination (receives a track). One instance per projection endpoint.
 *
 * NOTE: This runtime path requires a live Electron renderer and has NOT been executed
 * in the current build environment; it is covered by the manual test plan in
 * docs/testing.md, not by automated tests.
 */
export class ElectronWebRtcMediaBackend implements MediaBackend {
  private pc: RTCPeerConnection;
  private localStream: MediaStream | null = null;
  private videoSender: RTCRtpSender | null = null;
  private iceCb: ((c: RTCIceCandidateInit) => void) | null = null;
  private trackCb: ((s: MediaStream) => void) | null = null;
  /** Previous byte counter reading, so getStats reports a rate and not a total. */
  private prevByteSample: ByteSample | null = null;

  constructor(private readonly role: "source" | "destination") {
    this.pc = new RTCPeerConnection(RTC_CONFIG);
    this.pc.addEventListener("icecandidate", (ev) => {
      if (ev.candidate && this.iceCb) this.iceCb(ev.candidate.toJSON());
    });
    this.pc.addEventListener("track", (ev) => {
      const stream = ev.streams[0];
      if (stream && this.trackCb) this.trackCb(stream);
    });
  }

  onIceCandidate(cb: (c: RTCIceCandidateInit) => void): void {
    this.iceCb = cb;
  }
  onRemoteTrack(cb: (s: MediaStream) => void): void {
    this.trackCb = cb;
  }

  async listSources(): Promise<MediaSource[]> {
    // Source enumeration for capture is done in the main process (desktopCapturer) and
    // handed to the renderer via the preload bridge; see main/main.ts. A pure-renderer
    // backend cannot call desktopCapturer directly under contextIsolation.
    throw new Error("listSources is provided by the main process bridge, not the renderer backend");
  }

  async getSourceMetadata(): Promise<MediaSource | null> {
    return null;
  }

  /**
   * Begin capturing a desktop window as the local track. `sourceId` is a Chromium
   * `chromeMediaSourceId` obtained from the main process's desktopCapturer.
   */
  async startCapture(sourceId: string, constraints: CaptureConstraints): Promise<void> {
    if (this.role !== "source") throw new Error("only a source backend can startCapture");
    // Electron desktop capture uses the legacy chromeMediaSource constraints.
    const stream = await navigator.mediaDevices.getUserMedia({
      audio: false,
      video: {
        // @ts-expect-error Electron/Chromium desktop capture constraints are non-standard.
        mandatory: {
          chromeMediaSource: "desktop",
          chromeMediaSourceId: sourceId,
          maxWidth: constraints.maxWidth,
          maxHeight: constraints.maxHeight,
          maxFrameRate: constraints.maxFrameRate,
        },
      },
    });
    this.localStream = stream;
    const track = stream.getVideoTracks()[0];
    if (!track) throw new Error("desktop capture produced no video track");
    this.videoSender = this.pc.addTrack(track, stream);
  }

  async createOffer(): Promise<RTCSessionDescriptionInit> {
    const offer = await this.pc.createOffer();
    await this.pc.setLocalDescription(offer);
    return offer;
  }

  async acceptOffer(offer: RTCSessionDescriptionInit): Promise<void> {
    await this.pc.setRemoteDescription(offer);
    const answer = await this.pc.createAnswer();
    await this.pc.setLocalDescription(answer);
  }

  /** After acceptOffer, the local description is the answer to send back. */
  localAnswer(): RTCSessionDescriptionInit | null {
    return this.pc.localDescription ? this.pc.localDescription.toJSON() : null;
  }

  async applyAnswer(answer: RTCSessionDescriptionInit): Promise<void> {
    await this.pc.setRemoteDescription(answer);
  }

  async addIceCandidate(candidate: RTCIceCandidateInit): Promise<void> {
    await this.pc.addIceCandidate(candidate);
  }

  async setBitrate(bitsPerSecond: number): Promise<void> {
    if (!this.videoSender) return;
    const params = this.videoSender.getParameters();
    if (!params.encodings || params.encodings.length === 0) {
      params.encodings = [{}];
    }
    const first = params.encodings[0];
    if (first) first.maxBitrate = bitsPerSecond;
    await this.videoSender.setParameters(params);
  }

  async setFramerate(framesPerSecond: number): Promise<void> {
    if (!this.videoSender) return;
    const params = this.videoSender.getParameters();
    if (!params.encodings || params.encodings.length === 0) params.encodings = [{}];
    const first = params.encodings[0];
    if (first) first.maxFramerate = framesPerSecond;
    await this.videoSender.setParameters(params);
  }

  async requestKeyframe(): Promise<void> {
    // No direct renderer API to force a keyframe; a resolution/bitrate nudge triggers
    // one in practice. Documented limitation for the MVP.
  }

  async pause(): Promise<void> {
    this.localStream?.getVideoTracks().forEach((t) => (t.enabled = false));
  }
  async resume(): Promise<void> {
    this.localStream?.getVideoTracks().forEach((t) => (t.enabled = true));
  }

  async stop(): Promise<void> {
    this.localStream?.getTracks().forEach((t) => t.stop());
    this.localStream = null;
    this.videoSender = null;
    this.pc.close();
  }

  async getStats(): Promise<MediaStats> {
    const out: MediaStats = {};
    const report = await this.pc.getStats();

    // RTP stats reference their codec by id, so index the codec entries first.
    const codecMimeById = new Map<string, string>();
    report.forEach((s) => {
      if (s.type !== "codec") return;
      const mime = (s as Record<string, unknown>)["mimeType"];
      if (typeof mime === "string") codecMimeById.set(s.id, mime);
    });

    report.forEach((s) => {
      if (s.type === "outbound-rtp" || s.type === "inbound-rtp") {
        const r = s as Record<string, unknown>;
        if (typeof r["framesPerSecond"] === "number") out.framesPerSecond = r["framesPerSecond"] as number;
        if (typeof r["frameWidth"] === "number") out.frameWidth = r["frameWidth"] as number;
        if (typeof r["frameHeight"] === "number") out.frameHeight = r["frameHeight"] as number;
        if (typeof r["packetsLost"] === "number") out.packetsLost = r["packetsLost"] as number;
        if (typeof r["jitter"] === "number") out.jitterMs = (r["jitter"] as number) * 1000;

        const codecId = r["codecId"];
        if (typeof codecId === "string") {
          const mime = codecMimeById.get(codecId);
          if (mime !== undefined) out.codec = mime;
        }

        // Cumulative counters only become a rate when diffed against the last sample.
        const bytes = s.type === "outbound-rtp" ? r["bytesSent"] : r["bytesReceived"];
        if (typeof bytes === "number" && typeof r["timestamp"] === "number") {
          const sample: ByteSample = { bytes, timestampMs: r["timestamp"] as number };
          const bps = computeBitrateBps(this.prevByteSample, sample);
          if (bps !== undefined) out.bitrateBps = bps;
          this.prevByteSample = sample;
        }
      }

      if (s.type === "outbound-rtp") {
        const r = s as Record<string, unknown>;
        const impl = typeof r["encoderImplementation"] === "string" ? (r["encoderImplementation"] as string) : undefined;
        if (impl !== undefined) out.encoderImplementation = impl;
        const efficient = typeof r["powerEfficientEncoder"] === "boolean" ? (r["powerEfficientEncoder"] as boolean) : undefined;
        const hardware = classifyAcceleration(impl, efficient);
        if (hardware !== undefined) out.hardwareEncode = hardware;
      }

      if (s.type === "inbound-rtp") {
        const r = s as Record<string, unknown>;
        const impl = typeof r["decoderImplementation"] === "string" ? (r["decoderImplementation"] as string) : undefined;
        if (impl !== undefined) out.decoderImplementation = impl;
        const efficient = typeof r["powerEfficientDecoder"] === "boolean" ? (r["powerEfficientDecoder"] as boolean) : undefined;
        const hardware = classifyAcceleration(impl, efficient);
        if (hardware !== undefined) out.hardwareDecode = hardware;
      }

      if (s.type === "candidate-pair") {
        const r = s as Record<string, unknown>;
        if (r["nominated"] && typeof r["currentRoundTripTime"] === "number") {
          out.rttMs = (r["currentRoundTripTime"] as number) * 1000;
        }
      }
    });
    return out;
  }
}
