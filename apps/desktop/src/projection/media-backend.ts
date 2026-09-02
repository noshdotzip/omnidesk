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
  hardwareEncode?: boolean;
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
    report.forEach((s) => {
      if (s.type === "outbound-rtp" || s.type === "inbound-rtp") {
        const r = s as Record<string, unknown>;
        if (typeof r["framesPerSecond"] === "number") out.framesPerSecond = r["framesPerSecond"] as number;
        if (typeof r["frameWidth"] === "number") out.frameWidth = r["frameWidth"] as number;
        if (typeof r["frameHeight"] === "number") out.frameHeight = r["frameHeight"] as number;
        if (typeof r["packetsLost"] === "number") out.packetsLost = r["packetsLost"] as number;
        if (typeof r["jitter"] === "number") out.jitterMs = (r["jitter"] as number) * 1000;
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
