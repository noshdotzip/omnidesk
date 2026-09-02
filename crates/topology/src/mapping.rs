//! Pure coordinate mapping.
//!
//! Two independent problems live here:
//!
//! * **Letterbox pointer mapping** ([`map_proxy_to_source`]): the MVP scales the source
//!   window's captured client region into the destination proxy preserving aspect
//!   ratio, producing black bars. A click in the bars must map to *nothing* (never to
//!   a source pixel); a click on content must map to the correct normalized source
//!   position regardless of proxy size.
//!
//! * **Edge crossing** ([`map_edge_crossing`]): when the cursor leaves one display's
//!   edge, its normalized position along that edge maps to the same normalized
//!   position along the neighbouring display's edge, so crossing between a 4K and a
//!   1080p screen lands where the user expects.

/// A size in pixels (or logical units). Width/height are expected to be > 0 for
/// meaningful mapping; zero/negative inputs yield `None`/degenerate results rather
/// than panicking.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Size {
    pub w: f64,
    pub h: f64,
}

impl Size {
    pub fn new(w: f64, h: f64) -> Self {
        Self { w, h }
    }
    fn is_valid(&self) -> bool {
        self.w.is_finite() && self.h.is_finite() && self.w > 0.0 && self.h > 0.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

impl Point {
    pub fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
}

/// A point normalized to `[0,1] x [0,1]` within the source client region. The source
/// side multiplies this by its real client size (and adds the client origin) to get a
/// device pixel — that final step happens on the source, which alone knows the live
/// window geometry and DPI.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NormPoint {
    pub u: f64,
    pub v: f64,
}

/// A rectangle (used for the displayed content area inside a letterboxed proxy).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ContentRect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// Compute the rectangle, in proxy content coordinates, actually covered by the
/// scaled source image (the non-black region). Returns `None` for invalid sizes.
pub fn letterbox_content_rect(proxy: Size, source: Size) -> Option<ContentRect> {
    if !proxy.is_valid() || !source.is_valid() {
        return None;
    }
    let scale = (proxy.w / source.w).min(proxy.h / source.h);
    let disp_w = source.w * scale;
    let disp_h = source.h * scale;
    let x = (proxy.w - disp_w) / 2.0;
    let y = (proxy.h - disp_h) / 2.0;
    Some(ContentRect {
        x,
        y,
        w: disp_w,
        h: disp_h,
    })
}

/// Map a pointer position in the proxy content area to a normalized source position.
///
/// Returns `None` when the point falls in the letterbox bars (or inputs are invalid),
/// so the caller never sends a bogus coordinate to the source. The result is clamped
/// to `[0,1]` to absorb sub-pixel rounding right at the content edge.
pub fn map_proxy_to_source(proxy_pt: Point, proxy: Size, source: Size) -> Option<NormPoint> {
    let rect = letterbox_content_rect(proxy, source)?;
    // Small epsilon so a click exactly on the content border is accepted.
    let eps = 1e-6;
    if proxy_pt.x < rect.x - eps
        || proxy_pt.x > rect.x + rect.w + eps
        || proxy_pt.y < rect.y - eps
        || proxy_pt.y > rect.y + rect.h + eps
    {
        return None;
    }
    let u = ((proxy_pt.x - rect.x) / rect.w).clamp(0.0, 1.0);
    let v = ((proxy_pt.y - rect.y) / rect.h).clamp(0.0, 1.0);
    Some(NormPoint { u, v })
}

/// Which edge of a display the cursor is crossing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Left,
    Right,
    Top,
    Bottom,
}

impl Edge {
    /// The edge on the destination that a crossing arrives at (the opposite side).
    pub fn opposite(self) -> Edge {
        match self {
            Edge::Left => Edge::Right,
            Edge::Right => Edge::Left,
            Edge::Top => Edge::Bottom,
            Edge::Bottom => Edge::Top,
        }
    }
    /// True if this edge runs vertically (Left/Right) — crossing varies the Y position.
    pub fn is_vertical(self) -> bool {
        matches!(self, Edge::Left | Edge::Right)
    }
}

/// Map the position along a source edge to the position along the destination edge,
/// preserving the normalized fraction. `pos_along` and the returned value are in the
/// same axis as the edge (Y for Left/Right edges, X for Top/Bottom). Differing edge
/// lengths (e.g. 4K vs 1080p) are handled by the normalization.
///
/// Returns the destination-axis coordinate, clamped into `[0, to_len]`.
pub fn map_edge_crossing(pos_along: f64, from_len: f64, to_len: f64) -> f64 {
    if !(from_len.is_finite() && from_len > 0.0 && to_len.is_finite() && to_len > 0.0) {
        return 0.0;
    }
    let t = (pos_along / from_len).clamp(0.0, 1.0);
    (t * to_len).clamp(0.0, to_len)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-6, "expected {b}, got {a}");
    }

    #[test]
    fn same_aspect_has_no_bars_and_maps_linearly() {
        let proxy = Size::new(1920.0, 1080.0);
        let source = Size::new(1920.0, 1080.0);
        let rect = letterbox_content_rect(proxy, source).unwrap();
        approx(rect.x, 0.0);
        approx(rect.y, 0.0);
        approx(rect.w, 1920.0);
        approx(rect.h, 1080.0);
        let mid = map_proxy_to_source(Point::new(960.0, 540.0), proxy, source).unwrap();
        approx(mid.u, 0.5);
        approx(mid.v, 0.5);
    }

    #[test]
    fn wide_source_in_tall_proxy_pillarboxes_and_rejects_side_bars() {
        // 1920x1080 source shown in a 1000x1000 proxy -> bars top & bottom.
        let proxy = Size::new(1000.0, 1000.0);
        let source = Size::new(1920.0, 1080.0);
        let rect = letterbox_content_rect(proxy, source).unwrap();
        // scale = min(1000/1920, 1000/1080) = 1000/1920 = 0.520833
        approx(rect.w, 1000.0);
        approx(rect.h, 1080.0 * (1000.0 / 1920.0));
        // A click near the very top (in the black bar) maps to nothing.
        assert!(map_proxy_to_source(Point::new(500.0, 5.0), proxy, source).is_none());
        // A click at the content center maps to source center.
        let cy = rect.y + rect.h / 2.0;
        let c = map_proxy_to_source(Point::new(500.0, cy), proxy, source).unwrap();
        approx(c.u, 0.5);
        approx(c.v, 0.5);
    }

    #[test]
    fn tall_source_in_wide_proxy_letterboxes_sides() {
        let proxy = Size::new(1600.0, 900.0);
        let source = Size::new(900.0, 1600.0); // portrait source
        let rect = letterbox_content_rect(proxy, source).unwrap();
        // scale = min(1600/900, 900/1600) = 900/1600 = 0.5625
        approx(rect.h, 900.0);
        approx(rect.w, 900.0 * 0.5625);
        // Left black bar rejected.
        assert!(map_proxy_to_source(Point::new(2.0, 450.0), proxy, source).is_none());
        // Content left edge accepted, maps to u≈0.
        let left = map_proxy_to_source(Point::new(rect.x, 450.0), proxy, source).unwrap();
        approx(left.u, 0.0);
    }

    #[test]
    fn corners_map_to_unit_corners() {
        let proxy = Size::new(800.0, 600.0);
        let source = Size::new(800.0, 600.0);
        let tl = map_proxy_to_source(Point::new(0.0, 0.0), proxy, source).unwrap();
        approx(tl.u, 0.0);
        approx(tl.v, 0.0);
        let br = map_proxy_to_source(Point::new(800.0, 600.0), proxy, source).unwrap();
        approx(br.u, 1.0);
        approx(br.v, 1.0);
    }

    #[test]
    fn resize_preserves_mapping_of_content_center() {
        let source = Size::new(1280.0, 720.0);
        for proxy in [
            Size::new(640.0, 360.0),
            Size::new(1920.0, 1080.0),
            Size::new(1000.0, 1400.0),
        ] {
            let rect = letterbox_content_rect(proxy, source).unwrap();
            let center = Point::new(rect.x + rect.w / 2.0, rect.y + rect.h / 2.0);
            let n = map_proxy_to_source(center, proxy, source).unwrap();
            approx(n.u, 0.5);
            approx(n.v, 0.5);
        }
    }

    #[test]
    fn invalid_sizes_map_to_none() {
        assert!(letterbox_content_rect(Size::new(0.0, 100.0), Size::new(10.0, 10.0)).is_none());
        assert!(map_proxy_to_source(
            Point::new(1.0, 1.0),
            Size::new(100.0, 100.0),
            Size::new(0.0, 10.0)
        )
        .is_none());
    }

    #[test]
    fn edge_crossing_preserves_fraction_across_different_lengths() {
        // Exit 4K screen (2160 tall) at 3/4 down -> arrive on 1080 screen at 3/4 down.
        let y = map_edge_crossing(1620.0, 2160.0, 1080.0);
        approx(y, 810.0);
    }

    #[test]
    fn edge_crossing_clamps_out_of_range() {
        approx(map_edge_crossing(-50.0, 1080.0, 720.0), 0.0);
        approx(map_edge_crossing(99999.0, 1080.0, 720.0), 720.0);
    }

    #[test]
    fn edge_crossing_degenerate_lengths_are_safe() {
        approx(map_edge_crossing(10.0, 0.0, 720.0), 0.0);
        approx(map_edge_crossing(10.0, 1080.0, 0.0), 0.0);
    }

    #[test]
    fn edge_opposite_and_orientation() {
        assert_eq!(Edge::Left.opposite(), Edge::Right);
        assert_eq!(Edge::Top.opposite(), Edge::Bottom);
        assert!(Edge::Left.is_vertical());
        assert!(!Edge::Top.is_vertical());
    }
}
