/**
 * TypeScript mirror of `ultidesk-topology::mapping`.
 *
 * The proxy renderer needs the *same* letterbox pointer math the Rust source uses, so
 * a click inside the proxy `<video>` maps to the exact normalized source position and
 * clicks in the black bars are discarded. Kept in lockstep with
 * `crates/topology/src/mapping.rs`; parity is checked in `mapping.test.ts`.
 */

export interface Size {
  w: number;
  h: number;
}
export interface Point {
  x: number;
  y: number;
}
/** Normalized `[0,1]×[0,1]` position within the source client region. */
export interface NormPoint {
  u: number;
  v: number;
}
export interface ContentRect {
  x: number;
  y: number;
  w: number;
  h: number;
}

function valid(s: Size): boolean {
  return Number.isFinite(s.w) && Number.isFinite(s.h) && s.w > 0 && s.h > 0;
}

function clamp(v: number, lo: number, hi: number): number {
  return Math.min(hi, Math.max(lo, v));
}

/** The non-black content rectangle when `source` is scaled into `proxy` preserving aspect ratio. */
export function letterboxContentRect(proxy: Size, source: Size): ContentRect | null {
  if (!valid(proxy) || !valid(source)) return null;
  const scale = Math.min(proxy.w / source.w, proxy.h / source.h);
  const dispW = source.w * scale;
  const dispH = source.h * scale;
  return {
    x: (proxy.w - dispW) / 2,
    y: (proxy.h - dispH) / 2,
    w: dispW,
    h: dispH,
  };
}

/**
 * Map a pointer in the proxy content area to a normalized source position. Returns null
 * when the point lands in the letterbox bars (or on invalid input), so the caller never
 * sends a bogus coordinate to the source.
 */
export function mapProxyToSource(proxyPt: Point, proxy: Size, source: Size): NormPoint | null {
  const rect = letterboxContentRect(proxy, source);
  if (rect === null) return null;
  const eps = 1e-6;
  if (
    proxyPt.x < rect.x - eps ||
    proxyPt.x > rect.x + rect.w + eps ||
    proxyPt.y < rect.y - eps ||
    proxyPt.y > rect.y + rect.h + eps
  ) {
    return null;
  }
  return {
    u: clamp((proxyPt.x - rect.x) / rect.w, 0, 1),
    v: clamp((proxyPt.y - rect.y) / rect.h, 0, 1),
  };
}

export type Edge = "Left" | "Right" | "Top" | "Bottom";

export function oppositeEdge(e: Edge): Edge {
  switch (e) {
    case "Left":
      return "Right";
    case "Right":
      return "Left";
    case "Top":
      return "Bottom";
    case "Bottom":
      return "Top";
  }
}

/** Map a position along a source edge to the destination edge, preserving the fraction. */
export function mapEdgeCrossing(posAlong: number, fromLen: number, toLen: number): number {
  if (!(Number.isFinite(fromLen) && fromLen > 0 && Number.isFinite(toLen) && toLen > 0)) {
    return 0;
  }
  const t = clamp(posAlong / fromLen, 0, 1);
  return clamp(t * toLen, 0, toLen);
}
