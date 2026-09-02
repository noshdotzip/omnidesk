/**
 * Narrow preload bridge. This is the ONLY surface the renderer can use to reach the
 * main process; no Node, fs, or shell APIs are exposed. Every method maps to a single
 * validated IPC channel. (brief §7)
 */

import { contextBridge, ipcRenderer } from "electron";
import type { ProxyInputEvent, SignalMessage, WindowDto } from "../shared/protocol.js";

export interface CaptureSourceDescriptor {
  id: string;
  name: string;
  thumbnailDataUrl: string;
}

const api = {
  listCaptureSources(): Promise<CaptureSourceDescriptor[]> {
    return ipcRenderer.invoke("ultidesk:list-capture-sources");
  },
  enumerateWindows(): Promise<WindowDto[]> {
    return ipcRenderer.invoke("ultidesk:enumerate-windows");
  },
  startProjection(projectionId: string, sourceRect: WindowDto | null): Promise<boolean> {
    return ipcRenderer.invoke("ultidesk:start-projection", projectionId, sourceRect);
  },
  sendSignal(message: SignalMessage): void {
    ipcRenderer.send("ultidesk:signal", message);
  },
  onSignal(cb: (message: SignalMessage) => void): void {
    ipcRenderer.on("ultidesk:signal", (_e, message: SignalMessage) => cb(message));
  },
  sendProxyInput(input: ProxyInputEvent): void {
    ipcRenderer.send("ultidesk:proxy-input", input);
  },
};

export type UltideskApi = typeof api;

contextBridge.exposeInMainWorld("ultidesk", api);
