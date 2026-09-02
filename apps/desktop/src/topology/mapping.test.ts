import { describe, it, expect } from "vitest";
import {
  letterboxContentRect,
  mapProxyToSource,
  mapEdgeCrossing,
  oppositeEdge,
} from "./mapping";

const approx = (a: number, b: number) => expect(Math.abs(a - b)).toBeLessThan(1e-6);

describe("letterbox mapping (parity with Rust topology)", () => {
  it("maps linearly with no bars for equal aspect", () => {
    const proxy = { w: 1920, h: 1080 };
    const source = { w: 1920, h: 1080 };
    const rect = letterboxContentRect(proxy, source)!;
    approx(rect.x, 0);
    approx(rect.w, 1920);
    const mid = mapProxyToSource({ x: 960, y: 540 }, proxy, source)!;
    approx(mid.u, 0.5);
    approx(mid.v, 0.5);
  });

  it("rejects clicks in the pillarbox bars", () => {
    const proxy = { w: 1000, h: 1000 };
    const source = { w: 1920, h: 1080 };
    expect(mapProxyToSource({ x: 500, y: 5 }, proxy, source)).toBeNull();
    const rect = letterboxContentRect(proxy, source)!;
    const c = mapProxyToSource({ x: 500, y: rect.y + rect.h / 2 }, proxy, source)!;
    approx(c.u, 0.5);
    approx(c.v, 0.5);
  });

  it("maps content center to source center across many proxy sizes (resize safety)", () => {
    const source = { w: 1280, h: 720 };
    for (const proxy of [
      { w: 640, h: 360 },
      { w: 1920, h: 1080 },
      { w: 1000, h: 1400 },
    ]) {
      const rect = letterboxContentRect(proxy, source)!;
      const n = mapProxyToSource({ x: rect.x + rect.w / 2, y: rect.y + rect.h / 2 }, proxy, source)!;
      approx(n.u, 0.5);
      approx(n.v, 0.5);
    }
  });

  it("returns null on invalid sizes", () => {
    expect(letterboxContentRect({ w: 0, h: 100 }, { w: 10, h: 10 })).toBeNull();
    expect(mapProxyToSource({ x: 1, y: 1 }, { w: 100, h: 100 }, { w: 0, h: 10 })).toBeNull();
  });
});

describe("edge crossing (parity with Rust topology)", () => {
  it("preserves the fraction across different lengths", () => {
    approx(mapEdgeCrossing(1620, 2160, 1080), 810);
  });
  it("clamps out of range", () => {
    approx(mapEdgeCrossing(-50, 1080, 720), 0);
    approx(mapEdgeCrossing(99999, 1080, 720), 720);
  });
  it("is safe for degenerate lengths", () => {
    approx(mapEdgeCrossing(10, 0, 720), 0);
  });
  it("opposite edges", () => {
    expect(oppositeEdge("Left")).toBe("Right");
    expect(oppositeEdge("Top")).toBe("Bottom");
  });
});
