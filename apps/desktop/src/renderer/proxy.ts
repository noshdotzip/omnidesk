/**
 * Destination proxy (renderer). Displays the projected source window's stream and
 * forwards input back to the source. This window is the projection DESTINATION.
 *
 * Coordinate mapping uses the shared `mapProxyToSource` so clicks in the letterbox bars
 * are discarded and content clicks map to the correct normalized source position.
 *
 * Not covered by automated tests — requires a live Electron process (see docs/testing.md).
 */

import { ElectronWebRtcMediaBackend } from "../projection/media-backend.js";
import { mapProxyToSource, type Size } from "../topology/mapping.js";
import type { MouseButtonDto } from "../shared/protocol.js";

const params = new URLSearchParams(location.search);
const projectionId = params.get("projectionId") ?? "";

const video = () => document.getElementById("stream") as HTMLVideoElement;
const badge = () => document.getElementById("badge") as HTMLDivElement;

const backend = new ElectronWebRtcMediaBackend("destination");
let firstFrameSent = false;

backend.onIceCandidate((candidate) => {
  window.ultidesk.sendSignal({ kind: "ice", projectionId, candidate });
});

backend.onRemoteTrack((stream) => {
  const v = video();
  v.srcObject = stream;
  void v.play();
});

window.ultidesk.onSignal(async (msg) => {
  if (msg.projectionId !== projectionId) return;
  if (msg.kind === "offer") {
    await backend.acceptOffer({ type: "offer", sdp: msg.sdp });
    const answer = backend.localAnswer();
    if (answer?.sdp) {
      window.ultidesk.sendSignal({ kind: "answer", projectionId, sdp: answer.sdp });
    }
  } else if (msg.kind === "ice") {
    await backend.addIceCandidate(msg.candidate);
  } else if (msg.kind === "close") {
    await backend.stop();
    badge().textContent = "Disconnected — source application is still running on its device.";
  }
});

function sourceSize(): Size {
  const v = video();
  return { w: v.videoWidth, h: v.videoHeight };
}
function proxySize(): Size {
  const v = video();
  return { w: v.clientWidth, h: v.clientHeight };
}

function pointerToNorm(ev: MouseEvent): { u: number; v: number } | null {
  const v = video();
  const rect = v.getBoundingClientRect();
  const pt = { x: ev.clientX - rect.left, y: ev.clientY - rect.top };
  return mapProxyToSource(pt, proxySize(), sourceSize());
}

function mouseButton(ev: MouseEvent): MouseButtonDto {
  return ev.button === 2 ? "right" : ev.button === 1 ? "middle" : "left";
}

// Minimal KeyboardEvent.code -> PS/2 set-1 scan code map (enough to type into Notepad
// for the MVP demo). Full layout/IME handling is a documented later milestone.
const SCANCODES: Record<string, number> = {
  KeyA: 0x1e, KeyB: 0x30, KeyC: 0x2e, KeyD: 0x20, KeyE: 0x12, KeyF: 0x21, KeyG: 0x22,
  KeyH: 0x23, KeyI: 0x17, KeyJ: 0x24, KeyK: 0x25, KeyL: 0x26, KeyM: 0x32, KeyN: 0x31,
  KeyO: 0x18, KeyP: 0x19, KeyQ: 0x10, KeyR: 0x13, KeyS: 0x1f, KeyT: 0x14, KeyU: 0x16,
  KeyV: 0x2f, KeyW: 0x11, KeyX: 0x2d, KeyY: 0x15, KeyZ: 0x2c,
  Space: 0x39, Enter: 0x1c, Backspace: 0x0e, Tab: 0x0f,
  Digit1: 0x02, Digit2: 0x03, Digit3: 0x04, Digit4: 0x05, Digit5: 0x06,
  Digit6: 0x07, Digit7: 0x08, Digit8: 0x09, Digit9: 0x0a, Digit0: 0x0b,
};

function wireInput(): void {
  const v = video();
  v.addEventListener("mousemove", (ev) => {
    const n = pointerToNorm(ev);
    if (n) window.ultidesk.sendProxyInput({ kind: "pointer-move", projectionId, u: n.u, v: n.v });
  });
  const onButton = (ev: MouseEvent, down: boolean) => {
    const n = pointerToNorm(ev);
    if (n) {
      window.ultidesk.sendProxyInput({
        kind: "pointer-button",
        projectionId,
        button: mouseButton(ev),
        down,
        u: n.u,
        v: n.v,
      });
    }
  };
  v.addEventListener("mousedown", (ev) => onButton(ev, true));
  v.addEventListener("mouseup", (ev) => onButton(ev, false));
  v.addEventListener("contextmenu", (ev) => ev.preventDefault());

  // Only forward keys while the proxy is focused; OS shortcuts (Alt+Tab) stay local.
  const onKey = (ev: KeyboardEvent, down: boolean) => {
    const scancode = SCANCODES[ev.code];
    if (scancode === undefined) return;
    ev.preventDefault();
    window.ultidesk.sendProxyInput({ kind: "key", projectionId, scancode, down });
  };
  window.addEventListener("keydown", (ev) => onKey(ev, true));
  window.addEventListener("keyup", (ev) => onKey(ev, false));

  v.addEventListener("playing", () => {
    if (!firstFrameSent) {
      firstFrameSent = true;
      window.ultidesk.sendSignal({ kind: "first-frame", projectionId });
      badge().textContent = "Remote — projected from source device";
    }
  });
}

wireInput();
