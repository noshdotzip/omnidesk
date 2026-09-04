//! Arranging monitors across devices into one virtual desktop.
//!
//! This is the model behind the topology editor: where each screen sits relative to the
//! others, which screens touch, and where along a shared border the pointer may cross.
//!
//! It is pure geometry so it can be unit-tested without a GUI, and so the agent and the
//! editor cannot disagree about the layout. An editor that snaps monitors one way while
//! the agent computes crossings another way produces a desk where the pointer vanishes
//! at an edge that looks correct on screen.

use crate::monitor::Monitor;
use serde::{Deserialize, Serialize};

/// A rectangle in the shared logical coordinate space.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Rect {
    pub fn right(&self) -> f64 {
        self.x + self.width
    }
    pub fn bottom(&self) -> f64 {
        self.y + self.height
    }

    /// Whether two rectangles share interior area.
    ///
    /// Touching edges do **not** overlap: two monitors side by side have
    /// `a.right() == b.x`, which is the arrangement the whole design depends on.
    pub fn overlaps(&self, other: &Rect) -> bool {
        self.x < other.right()
            && other.x < self.right()
            && self.y < other.bottom()
            && other.y < self.bottom()
    }
}

/// Which side of a monitor a neighbour sits on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Side {
    Left,
    Right,
    Top,
    Bottom,
}

impl Side {
    /// The side the pointer arrives on when it leaves through this one.
    pub fn opposite(self) -> Side {
        match self {
            Side::Left => Side::Right,
            Side::Right => Side::Left,
            Side::Top => Side::Bottom,
            Side::Bottom => Side::Top,
        }
    }
}

/// A shared border between two monitors, and the stretch of it the pointer can use.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Adjacency {
    /// Which side of the first monitor the second sits on.
    pub side: Side,
    /// Start of the overlapping span, along the shared border.
    pub span_start: f64,
    /// End of the overlapping span.
    pub span_end: f64,
}

impl Adjacency {
    pub fn span(&self) -> f64 {
        self.span_end - self.span_start
    }
}

/// Monitors arranged into one virtual desktop.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Layout {
    pub monitors: Vec<Monitor>,
}

/// How close two edges must be, in logical pixels, before the editor snaps them.
pub const DEFAULT_SNAP: f64 = 24.0;

fn rect_of(m: &Monitor) -> Rect {
    Rect {
        x: m.logical_x,
        y: m.logical_y,
        width: m.logical_width,
        height: m.logical_height,
    }
}

impl Layout {
    pub fn new(monitors: Vec<Monitor>) -> Self {
        Layout { monitors }
    }

    pub fn rect(&self, index: usize) -> Option<Rect> {
        self.monitors.get(index).map(rect_of)
    }

    /// Bounding box of every monitor: the virtual desktop.
    ///
    /// `None` for an empty layout rather than a zero rect, because a zero-sized desktop
    /// would silently divide by zero in every mapping that consumes it.
    pub fn bounds(&self) -> Option<Rect> {
        let first = self.monitors.first()?;
        let mut min_x = first.logical_x;
        let mut min_y = first.logical_y;
        let mut max_x = first.right();
        let mut max_y = first.bottom();
        for m in self.monitors.iter().skip(1) {
            min_x = min_x.min(m.logical_x);
            min_y = min_y.min(m.logical_y);
            max_x = max_x.max(m.right());
            max_y = max_y.max(m.bottom());
        }
        Some(Rect {
            x: min_x,
            y: min_y,
            width: max_x - min_x,
            height: max_y - min_y,
        })
    }

    /// Pairs of monitors that overlap, which is always a misconfiguration.
    ///
    /// Overlapping screens make a pointer position ambiguous: the same logical
    /// coordinate belongs to two displays, and which one wins depends on iteration
    /// order. The editor should refuse to save a layout with any of these.
    pub fn overlapping_pairs(&self) -> Vec<(usize, usize)> {
        let mut out = Vec::new();
        for i in 0..self.monitors.len() {
            for j in (i + 1)..self.monitors.len() {
                if rect_of(&self.monitors[i]).overlaps(&rect_of(&self.monitors[j])) {
                    out.push((i, j));
                }
            }
        }
        out
    }

    /// How two monitors touch, if they do.
    ///
    /// Requires a shared border **and** a non-zero overlap along it. Two screens meeting
    /// only at a corner are not adjacent: there is nowhere for the pointer to cross, and
    /// treating that as a border creates an edge that swallows the cursor.
    pub fn adjacency(&self, a: usize, b: usize) -> Option<Adjacency> {
        let ra = self.rect(a)?;
        let rb = self.rect(b)?;

        // Vertical border: spans overlap in y.
        let y_start = ra.y.max(rb.y);
        let y_end = ra.bottom().min(rb.bottom());
        if y_end > y_start {
            if (ra.right() - rb.x).abs() < f64::EPSILON {
                return Some(Adjacency {
                    side: Side::Right,
                    span_start: y_start,
                    span_end: y_end,
                });
            }
            if (rb.right() - ra.x).abs() < f64::EPSILON {
                return Some(Adjacency {
                    side: Side::Left,
                    span_start: y_start,
                    span_end: y_end,
                });
            }
        }

        // Horizontal border: spans overlap in x.
        let x_start = ra.x.max(rb.x);
        let x_end = ra.right().min(rb.right());
        if x_end > x_start {
            if (ra.bottom() - rb.y).abs() < f64::EPSILON {
                return Some(Adjacency {
                    side: Side::Bottom,
                    span_start: x_start,
                    span_end: x_end,
                });
            }
            if (rb.bottom() - ra.y).abs() < f64::EPSILON {
                return Some(Adjacency {
                    side: Side::Top,
                    span_start: x_start,
                    span_end: x_end,
                });
            }
        }

        None
    }

    /// Where a monitor should land when dragged to `(x, y)`, snapped to its neighbours.
    ///
    /// Snapping considers both *butting* (this screen's edge against a neighbour's
    /// opposite edge) and *aligning* (both left edges flush, both tops flush). Butting
    /// is what creates a usable border; aligning is what stops a two-monitor desk
    /// looking subtly crooked.
    ///
    /// Returns the position unchanged when nothing is within `threshold`.
    pub fn snap(&self, index: usize, x: f64, y: f64, threshold: f64) -> (f64, f64) {
        let Some(moving) = self.monitors.get(index) else {
            return (x, y);
        };
        let w = moving.logical_width;
        let h = moving.logical_height;

        let mut best_x: Option<(f64, f64)> = None; // (distance, snapped)
        let mut best_y: Option<(f64, f64)> = None;

        let consider = |cand: f64, actual: f64, slot: &mut Option<(f64, f64)>| {
            let d = (cand - actual).abs();
            if d <= threshold && slot.map_or(true, |(best, _)| d < best) {
                *slot = Some((d, cand));
            }
        };

        for (i, other) in self.monitors.iter().enumerate() {
            if i == index {
                continue;
            }
            let o = rect_of(other);
            // Butt against the neighbour horizontally, or align vertical edges.
            consider(o.right(), x, &mut best_x);
            consider(o.x - w, x, &mut best_x);
            consider(o.x, x, &mut best_x);
            consider(o.right() - w, x, &mut best_x);
            // Butt vertically, or align horizontal edges.
            consider(o.bottom(), y, &mut best_y);
            consider(o.y - h, y, &mut best_y);
            consider(o.y, y, &mut best_y);
            consider(o.bottom() - h, y, &mut best_y);
        }

        (best_x.map_or(x, |(_, v)| v), best_y.map_or(y, |(_, v)| v))
    }

    /// Apply a drag, snapping first.
    pub fn move_monitor(&mut self, index: usize, x: f64, y: f64, threshold: f64) {
        let (sx, sy) = self.snap(index, x, y, threshold);
        if let Some(m) = self.monitors.get_mut(index) {
            m.logical_x = sx;
            m.logical_y = sy;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::monitor::{MonitorId, Rotation};
    use ultidesk_core::DeviceId;

    fn mon(name: &str, x: f64, y: f64, w: f64, h: f64) -> Monitor {
        Monitor {
            device_id: DeviceId::new(),
            monitor_id: MonitorId(1),
            friendly_name: name.to_string(),
            logical_x: x,
            logical_y: y,
            logical_width: w,
            logical_height: h,
            native_pixel_width: w as u32,
            native_pixel_height: h as u32,
            scale_factor: 1.0,
            rotation: Rotation::Landscape,
            refresh_rate: 60.0,
            primary: false,
        }
    }

    fn side_by_side() -> Layout {
        Layout::new(vec![
            mon("left", 0.0, 0.0, 1920.0, 1080.0),
            mon("right", 1920.0, 0.0, 1920.0, 1080.0),
        ])
    }

    #[test]
    fn touching_monitors_do_not_count_as_overlapping() {
        // Side-by-side screens share an edge exactly. If that read as an overlap the
        // editor would reject every valid arrangement.
        assert!(side_by_side().overlapping_pairs().is_empty());
    }

    #[test]
    fn genuinely_overlapping_monitors_are_reported() {
        // Ambiguous: one logical coordinate would belong to two displays.
        let l = Layout::new(vec![
            mon("a", 0.0, 0.0, 1920.0, 1080.0),
            mon("b", 1900.0, 0.0, 1920.0, 1080.0),
        ]);
        assert_eq!(l.overlapping_pairs(), vec![(0, 1)]);
    }

    #[test]
    fn bounds_span_every_monitor_including_negative_origins() {
        let l = Layout::new(vec![
            mon("primary", 0.0, 0.0, 1920.0, 1080.0),
            mon("left-of", -1280.0, -100.0, 1280.0, 720.0),
        ]);
        let b = l.bounds().expect("non-empty");
        assert_eq!(b.x, -1280.0);
        assert_eq!(b.y, -100.0);
        assert_eq!(b.right(), 1920.0);
        assert_eq!(b.bottom(), 1080.0);
    }

    #[test]
    fn an_empty_layout_has_no_bounds_rather_than_a_zero_rect() {
        // A zero-sized desktop divides by zero in every mapping downstream.
        assert!(Layout::default().bounds().is_none());
    }

    #[test]
    fn side_by_side_monitors_are_adjacent_along_their_full_height() {
        let l = side_by_side();
        let adj = l.adjacency(0, 1).expect("adjacent");
        assert_eq!(adj.side, Side::Right);
        assert_eq!(adj.span_start, 0.0);
        assert_eq!(adj.span_end, 1080.0);
        assert_eq!(adj.span(), 1080.0);
    }

    #[test]
    fn adjacency_is_reported_from_each_monitors_own_perspective() {
        let l = side_by_side();
        assert_eq!(l.adjacency(0, 1).unwrap().side, Side::Right);
        assert_eq!(l.adjacency(1, 0).unwrap().side, Side::Left);
    }

    #[test]
    fn a_vertically_offset_neighbour_shares_only_the_overlapping_span() {
        // This is what makes crossing correct: dragging off the right at y=900 must not
        // land anywhere when the neighbour only spans 0..720.
        let l = Layout::new(vec![
            mon("a", 0.0, 0.0, 1920.0, 1080.0),
            mon("b", 1920.0, 300.0, 1280.0, 720.0),
        ]);
        let adj = l.adjacency(0, 1).expect("adjacent");
        assert_eq!(adj.side, Side::Right);
        assert_eq!(adj.span_start, 300.0);
        assert_eq!(adj.span_end, 1020.0);
    }

    #[test]
    fn corner_touching_monitors_are_not_adjacent() {
        // They meet at exactly one point, so there is nowhere to cross. Treating that
        // as a border would create an edge that swallows the pointer.
        let l = Layout::new(vec![
            mon("a", 0.0, 0.0, 1920.0, 1080.0),
            mon("b", 1920.0, 1080.0, 1920.0, 1080.0),
        ]);
        assert!(l.adjacency(0, 1).is_none());
    }

    #[test]
    fn separated_monitors_are_not_adjacent() {
        let l = Layout::new(vec![
            mon("a", 0.0, 0.0, 1920.0, 1080.0),
            mon("b", 2000.0, 0.0, 1920.0, 1080.0),
        ]);
        assert!(l.adjacency(0, 1).is_none());
    }

    #[test]
    fn stacked_monitors_are_adjacent_vertically() {
        let l = Layout::new(vec![
            mon("top", 0.0, 0.0, 1920.0, 1080.0),
            mon("bottom", 0.0, 1080.0, 1920.0, 1080.0),
        ]);
        let adj = l.adjacency(0, 1).expect("adjacent");
        assert_eq!(adj.side, Side::Bottom);
        assert_eq!(adj.span(), 1920.0);
    }

    #[test]
    fn a_near_miss_drag_snaps_flush_against_the_neighbour() {
        // The point of snapping: a hand-dragged monitor 9px short would otherwise leave
        // a gap, and a gap means no adjacency and no crossing.
        let l = side_by_side();
        let (x, y) = l.snap(1, 1911.0, 4.0, DEFAULT_SNAP);
        assert_eq!(x, 1920.0, "should butt against the left monitor");
        assert_eq!(y, 0.0, "should align tops");
    }

    #[test]
    fn a_drag_beyond_the_threshold_is_left_alone() {
        // Snapping must not teleport a monitor the user deliberately placed apart.
        let l = side_by_side();
        let (x, y) = l.snap(1, 2400.0, 600.0, DEFAULT_SNAP);
        assert_eq!((x, y), (2400.0, 600.0));
    }

    #[test]
    fn snapping_picks_the_nearest_candidate_not_the_first() {
        // With several neighbours in range, the closest edge must win, or dragging
        // feels like the monitor is being yanked somewhere arbitrary.
        let l = Layout::new(vec![
            mon("a", 0.0, 0.0, 1000.0, 1000.0),
            mon("b", 2000.0, 0.0, 1000.0, 1000.0),
            mon("moving", 900.0, 0.0, 100.0, 100.0),
        ]);
        // 1010 is 10 from a.right()=1000 and 90 from b.x - w = 1900.
        let (x, _) = l.snap(2, 1010.0, 0.0, DEFAULT_SNAP);
        assert_eq!(x, 1000.0);
    }

    #[test]
    fn snapping_can_align_edges_without_butting() {
        // Two monitors stacked with their left edges flush: x should align to 0 even
        // though nothing butts horizontally.
        let l = Layout::new(vec![
            mon("a", 0.0, 0.0, 1920.0, 1080.0),
            mon("b", 0.0, 1080.0, 1920.0, 1080.0),
        ]);
        let (x, y) = l.snap(1, 12.0, 1075.0, DEFAULT_SNAP);
        assert_eq!(x, 0.0);
        assert_eq!(y, 1080.0);
    }

    #[test]
    fn moving_a_monitor_applies_the_snap() {
        let mut l = side_by_side();
        l.move_monitor(1, 1908.0, 6.0, DEFAULT_SNAP);
        assert_eq!(l.monitors[1].logical_x, 1920.0);
        assert_eq!(l.monitors[1].logical_y, 0.0);
        assert!(l.overlapping_pairs().is_empty());
        assert!(l.adjacency(0, 1).is_some());
    }

    #[test]
    fn sides_pair_with_their_opposite() {
        assert_eq!(Side::Right.opposite(), Side::Left);
        assert_eq!(Side::Top.opposite(), Side::Bottom);
        for s in [Side::Left, Side::Right, Side::Top, Side::Bottom] {
            assert_eq!(s.opposite().opposite(), s);
        }
    }
}
