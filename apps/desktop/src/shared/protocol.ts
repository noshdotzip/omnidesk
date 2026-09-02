/**
 * Wire/IPC message types.
 *
 * `Ipc*` mirror the Rust agent's `crates/agent/src/ipc.rs` tagged enums exactly (serde
 * `#[serde(tag = "type")]`), so JSON produced here deserializes there and vice versa.
 * See docs/protocols.md and ADR-0004 for how these stay in sync (and the plan to move
 * to generated bindings before any peer-to-peer protocol ships).
 */

export const PROTOCOL_VERSION = 1;

// ---- Local IPC: desktop app -> Rust agent -----------------------------------

export interface VirtualScreenDto {
  left: number;
  top: number;
  width: number;
  height: number;
}

export type MouseButtonDto = "left" | "right" | "middle";

export type IpcRequest =
  | { type: "Hello"; token: string; protocol_version: number }
  | { type: "Ping" }
  | { type: "EnumerateWindows" }
  | { type: "InjectMouseMove"; screen_x: number; screen_y: number; virtual_screen: VirtualScreenDto }
  | { type: "InjectMouseButton"; button: MouseButtonDto; down: boolean }
  | { type: "InjectKey"; scancode: number; down: boolean }
  | { type: "ReleaseAllInput" };

export interface WindowDto {
  hwnd: number;
  title: string;
  process_id: number;
  left: number;
  top: number;
  right: number;
  bottom: number;
}

export type IpcResponse =
  | { type: "HelloOk"; agent_version: string; protocol_version: number }
  | { type: "Pong" }
  | { type: "Windows"; windows: WindowDto[] }
  | { type: "Injected" }
  | { type: "Released"; count: number }
  | { type: "Error"; code: string; message: string };

// ---- Projection signaling (dev loopback broker) -----------------------------
//
// In this MVP slice, source and destination are two windows in ONE Electron process,
// brokered by the main process. This is the "explicit loopback development peer" the
// brief allows: it is dev-only and must be replaced by the authenticated peer control
// channel in Milestone 1. It never touches the network.

export type SignalMessage =
  | { kind: "offer"; projectionId: string; sdp: string }
  | { kind: "answer"; projectionId: string; sdp: string }
  | { kind: "ice"; projectionId: string; candidate: RTCIceCandidateInit }
  | { kind: "first-frame"; projectionId: string }
  | { kind: "close"; projectionId: string };

/** A pointer/key event forwarded from a proxy back toward its source window. */
export type ProxyInputEvent =
  | { kind: "pointer-move"; projectionId: string; u: number; v: number }
  | { kind: "pointer-button"; projectionId: string; button: MouseButtonDto; down: boolean; u: number; v: number }
  | { kind: "key"; projectionId: string; scancode: number; down: boolean };
