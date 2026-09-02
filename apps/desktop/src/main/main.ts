/**
 * Ultidesk desktop main process.
 *
 * Security posture (brief §7): every window is created with nodeIntegration:false,
 * contextIsolation:true, sandbox:true, and a narrow preload bridge. Permission-relevant
 * actions are re-checked here / in the agent, never trusted from the renderer.
 *
 * This slice runs a *dev loopback* projection: the "source" picker window and the
 * "destination" proxy window are two windows in this one process, brokered here. This
 * stands in for the authenticated peer control channel until Milestone 1. It is
 * dev-only and never touches the network.
 *
 * NOTE: This runtime path requires a live Electron process and Vite bundling of the
 * renderer (tracked in docs/status.md); it has NOT been executed in the current build
 * environment. See docs/testing.md for the manual test plan.
 */

import { app, BrowserWindow, ipcMain, desktopCapturer, session } from "electron";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { AgentClient } from "./agent-client.js";
import { SignalingBroker } from "./signaling.js";
import type { ProxyInputEvent, VirtualScreenDto, WindowDto } from "../shared/protocol.js";

const __dirname = dirname(fileURLToPath(import.meta.url));

interface ActiveProjection {
  projectionId: string;
  sourceWebContentsId: number;
  destWebContentsId: number | null;
  /** Source window screen rect, used to map normalized proxy coords to screen pixels. */
  sourceRect: WindowDto | null;
}

const projections = new Map<string, ActiveProjection>();
const broker = new SignalingBroker();
let agent: AgentClient | null = null;
let pickerWindow: BrowserWindow | null = null;

function agentBinaryPath(): string {
  if (process.env["ULTIDESK_AGENT_BIN"]) return process.env["ULTIDESK_AGENT_BIN"];
  // Dev default: workspace target dir relative to apps/desktop/dist/main.
  const exe = process.platform === "win32" ? "ultidesk-agent.exe" : "ultidesk-agent";
  return join(__dirname, "..", "..", "..", "..", "target", "debug", exe);
}

function secureWebPreferences(): Electron.WebPreferences {
  return {
    nodeIntegration: false,
    contextIsolation: true,
    sandbox: true,
    preload: join(__dirname, "..", "preload", "preload.js"),
  };
}

function createPickerWindow(): void {
  pickerWindow = new BrowserWindow({
    width: 480,
    height: 640,
    title: "Ultidesk — Source",
    webPreferences: secureWebPreferences(),
  });
  void pickerWindow.loadFile(join(__dirname, "..", "renderer", "picker.html"));
  pickerWindow.on("closed", () => (pickerWindow = null));
}

function createProxyWindow(projection: ActiveProjection): BrowserWindow {
  const win = new BrowserWindow({
    width: 960,
    height: 600,
    title: "Ultidesk — Projected (remote)",
    webPreferences: secureWebPreferences(),
  });
  projection.destWebContentsId = win.webContents.id;
  void win.loadFile(join(__dirname, "..", "renderer", "proxy.html"), {
    query: { projectionId: projection.projectionId },
  });
  win.on("closed", () => {
    // Closing the proxy disconnects projection but never closes the source app.
    broker.relayClose(projection.projectionId);
    projections.delete(projection.projectionId);
  });
  return win;
}

function registerIpc(): void {
  // Harden: block permission requests we do not use.
  session.defaultSession.setPermissionRequestHandler((_wc, _perm, cb) => cb(false));

  // desktopCapturer must run in the main process; return sanitized source descriptors.
  ipcMain.handle("ultidesk:list-capture-sources", async () => {
    const sources = await desktopCapturer.getSources({
      types: ["window"],
      thumbnailSize: { width: 320, height: 200 },
    });
    return sources.map((s) => ({
      id: s.id,
      name: s.name,
      thumbnailDataUrl: s.thumbnail.toDataURL(),
    }));
  });

  ipcMain.handle("ultidesk:enumerate-windows", async (): Promise<WindowDto[]> => {
    if (!agent) return [];
    const res = await agent.request({ type: "EnumerateWindows" });
    return res.type === "Windows" ? res.windows : [];
  });

  // Start a projection: register it and open a destination proxy window.
  ipcMain.handle(
    "ultidesk:start-projection",
    (ev, projectionId: string, sourceRect: WindowDto | null) => {
      const projection: ActiveProjection = {
        projectionId,
        sourceWebContentsId: ev.sender.id,
        destWebContentsId: null,
        sourceRect,
      };
      projections.set(projectionId, projection);
      broker.register(projectionId, ev.sender.id);
      const proxy = createProxyWindow(projection);
      broker.register(projectionId, proxy.webContents.id);
      return true;
    },
  );

  // Signaling relay between the two windows of a projection (dev loopback broker).
  ipcMain.on("ultidesk:signal", (ev, message: unknown) => {
    broker.relay(ev.sender.id, message);
  });

  // Input from a proxy -> map to source screen coords -> agent injection.
  ipcMain.on("ultidesk:proxy-input", async (_ev, input: ProxyInputEvent) => {
    await handleProxyInput(input);
  });
}

async function handleProxyInput(input: ProxyInputEvent): Promise<void> {
  if (!agent) return;
  const projection = projections.get(input.projectionId);
  const rect = projection?.sourceRect ?? null;

  // Virtual screen is a single-display placeholder for the slice; real multi-monitor
  // virtual-desktop bounds come from the topology subsystem in a later milestone.
  const virtual_screen: VirtualScreenDto = { left: 0, top: 0, width: 1920, height: 1080 };

  const toScreen = (u: number, v: number): { x: number; y: number } | null => {
    if (!rect) return null;
    return {
      x: Math.round(rect.left + u * (rect.right - rect.left)),
      y: Math.round(rect.top + v * (rect.bottom - rect.top)),
    };
  };

  switch (input.kind) {
    case "pointer-move": {
      const p = toScreen(input.u, input.v);
      if (p) await agent.request({ type: "InjectMouseMove", screen_x: p.x, screen_y: p.y, virtual_screen });
      break;
    }
    case "pointer-button": {
      const p = toScreen(input.u, input.v);
      if (p) await agent.request({ type: "InjectMouseMove", screen_x: p.x, screen_y: p.y, virtual_screen });
      await agent.request({ type: "InjectMouseButton", button: input.button, down: input.down });
      break;
    }
    case "key":
      await agent.request({ type: "InjectKey", scancode: input.scancode, down: input.down });
      break;
  }
}

async function bootstrap(): Promise<void> {
  registerIpc();
  try {
    agent = await AgentClient.spawnAndConnect(agentBinaryPath());
  } catch (err) {
    // The UI must still open and clearly report that the agent is unavailable rather
    // than silently degrading.
    console.error("[ultidesk] agent unavailable:", err);
  }
  createPickerWindow();
}

app.whenReady().then(bootstrap);

app.on("window-all-closed", () => {
  if (process.platform !== "darwin") app.quit();
});

app.on("before-quit", () => {
  // Never leave input held on the source if we tear down.
  void agent?.releaseAllInput();
  agent?.dispose();
});
