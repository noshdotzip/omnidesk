/**
 * Dev loopback signaling broker.
 *
 * Relays WebRTC signaling (offer/answer/ice) and lifecycle messages between the two
 * windows that make up one projection in this single-process dev mode. In production
 * this is replaced by the authenticated peer control channel (Rust QUIC) — see
 * docs/protocols.md. The broker only ever moves data between two local webContents;
 * it performs no capture, encoding, or network I/O.
 */

import { webContents } from "electron";

interface Group {
  projectionId: string;
  members: Set<number>; // webContents ids
}

export class SignalingBroker {
  private byProjection = new Map<string, Group>();
  private memberToProjection = new Map<number, string>();

  register(projectionId: string, webContentsId: number): void {
    let group = this.byProjection.get(projectionId);
    if (!group) {
      group = { projectionId, members: new Set() };
      this.byProjection.set(projectionId, group);
    }
    group.members.add(webContentsId);
    this.memberToProjection.set(webContentsId, projectionId);
  }

  /** Relay a signaling message from `fromId` to the other member(s) of its projection. */
  relay(fromId: number, message: unknown): void {
    const projectionId = this.memberToProjection.get(fromId);
    if (!projectionId) return;
    const group = this.byProjection.get(projectionId);
    if (!group) return;
    for (const memberId of group.members) {
      if (memberId === fromId) continue;
      const wc = webContents.fromId(memberId);
      wc?.send("ultidesk:signal", message);
    }
  }

  relayClose(projectionId: string): void {
    const group = this.byProjection.get(projectionId);
    if (!group) return;
    for (const memberId of group.members) {
      const wc = webContents.fromId(memberId);
      wc?.send("ultidesk:signal", { kind: "close", projectionId });
      this.memberToProjection.delete(memberId);
    }
    this.byProjection.delete(projectionId);
  }
}
