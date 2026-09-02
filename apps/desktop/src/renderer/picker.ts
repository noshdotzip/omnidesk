/**
 * Source picker (renderer). Lists capturable windows, and on selection starts a
 * projection: opens a destination proxy window (via the main process) and streams the
 * selected window to it over WebRTC. This window is the projection SOURCE.
 *
 * Not covered by automated tests — requires a live Electron process (see docs/testing.md).
 */

import { ElectronWebRtcMediaBackend } from "../projection/media-backend.js";
import type { WindowDto } from "../shared/protocol.js";

const sourcesEl = () => document.getElementById("sources") as HTMLDivElement;
const statusEl = () => document.getElementById("status") as HTMLDivElement;

interface CaptureSourceDescriptor {
  id: string;
  name: string;
  thumbnailDataUrl: string;
}

function setStatus(text: string): void {
  statusEl().textContent = text;
}

async function refresh(): Promise<void> {
  setStatus("Enumerating windows…");
  const [caps, windows] = await Promise.all([
    window.ultidesk.listCaptureSources(),
    window.ultidesk.enumerateWindows(),
  ]);
  render(caps, windows);
  setStatus(`${caps.length} capturable window(s). Select one to project.`);
}

function render(caps: CaptureSourceDescriptor[], windows: WindowDto[]): void {
  const container = sourcesEl();
  container.innerHTML = "";
  for (const cap of caps) {
    const btn = document.createElement("button");
    btn.className = "source";
    const img = document.createElement("img");
    img.src = cap.thumbnailDataUrl;
    const label = document.createElement("span");
    label.textContent = cap.name;
    btn.append(img, label);
    btn.addEventListener("click", () => {
      void startProjection(cap, windows);
    });
    container.append(btn);
  }
}

async function startProjection(cap: CaptureSourceDescriptor, windows: WindowDto[]): Promise<void> {
  const projectionId = crypto.randomUUID();
  // Best-effort match to the agent's native window record for input mapping.
  const rect = windows.find((w) => w.title === cap.name) ?? null;
  setStatus(`Starting projection of "${cap.name}"…`);
  await window.ultidesk.startProjection(projectionId, rect);

  const backend = new ElectronWebRtcMediaBackend("source");
  backend.onIceCandidate((candidate) => {
    window.ultidesk.sendSignal({ kind: "ice", projectionId, candidate });
  });

  window.ultidesk.onSignal(async (msg) => {
    if (msg.projectionId !== projectionId) return;
    if (msg.kind === "answer") {
      await backend.applyAnswer({ type: "answer", sdp: msg.sdp });
    } else if (msg.kind === "ice") {
      await backend.addIceCandidate(msg.candidate);
    } else if (msg.kind === "first-frame") {
      setStatus(`Projected "${cap.name}" — remote first frame received.`);
    } else if (msg.kind === "close") {
      setStatus(`Projection of "${cap.name}" closed. Source window still running locally.`);
      await backend.stop();
    }
  });

  await backend.startCapture(cap.id, { maxWidth: 1920, maxHeight: 1080, maxFrameRate: 60 });
  const offer = await backend.createOffer();
  if (offer.sdp) {
    window.ultidesk.sendSignal({ kind: "offer", projectionId, sdp: offer.sdp });
  }
}

document.getElementById("refresh")?.addEventListener("click", () => void refresh());
void refresh();
