//! Tracking the pointer on a peer's screen while this machine is driving it.
//!
//! libei reports pointer motion as **relative** deltas, and the injection side of the
//! wire protocol takes an **absolute** screen position. Something has to hold the
//! position in between, and that something has to decide when the operator has driven
//! back off the peer and wants their own machine again.
//!
//! # The entry edge is not like the other three
//! Push past the top of the peer's screen and the pointer should stop at the top — the
//! peer has no pixel above it, and handing control somewhere would be a surprise. Push
//! back out through the edge you *came in on* and you are going home. So three edges
//! clamp and one releases, and which one releases depends on where the crossing
//! happened.
//!
//! # Deltas are fractional and must accumulate
//! libei reports `f32` deltas, and a slow drag produces a long run of values well under
//! one pixel. Rounding each delta on arrival makes every one of them zero, so a slowly
//! moved mouse does not move the remote pointer at all. The position is therefore kept
//! in `f64` and only rounded when it is handed to the injector.

use crate::layout::{Rect, Side};
use serde::{Deserialize, Serialize};

/// What happened after applying a motion delta.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PointerUpdate {
    /// The pointer is still on the peer. Send the new position.
    Moved,
    /// The pointer left through the edge it entered by. Control returns to this
    /// machine; stop forwarding and release the grab.
    ReturnedHome,
}

/// The pointer's position on a peer's screen, driven by relative deltas.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RemotePointer {
    bounds: Rect,
    entry: Side,
    x: f64,
    y: f64,
}

impl RemotePointer {
    /// Place the pointer where it arrives after crossing onto the peer.
    ///
    /// `entry` is the side of the *peer's* screen the pointer arrives on — crossing off
    /// this machine's right edge arrives on the peer's `Left`. `fraction` is how far
    /// along that edge the crossing happened, 0.0 at the top/left end and 1.0 at the
    /// bottom/right end, so the pointer appears at the height it left at instead of
    /// jumping to a corner.
    ///
    /// The pointer starts exactly on the entry edge. That means a delta back the way it
    /// came returns home immediately, which is the behaviour an operator expects from
    /// nudging the mouse back.
    pub fn enter(bounds: Rect, entry: Side, fraction: f64) -> Self {
        let f = fraction.clamp(0.0, 1.0);
        let (x, y) = match entry {
            Side::Left => (bounds.x, bounds.y + bounds.height * f),
            Side::Right => (bounds.right(), bounds.y + bounds.height * f),
            Side::Top => (bounds.x + bounds.width * f, bounds.y),
            Side::Bottom => (bounds.x + bounds.width * f, bounds.bottom()),
        };
        RemotePointer {
            bounds,
            entry,
            x,
            y,
        }
    }

    /// The current position, rounded for injection.
    ///
    /// Rounding happens here rather than on each delta so sub-pixel motion still
    /// accumulates; see the module docs.
    pub fn position(&self) -> (i32, i32) {
        (self.x.round() as i32, self.y.round() as i32)
    }

    /// Exact position, for tests and for callers doing their own arithmetic.
    pub fn position_f64(&self) -> (f64, f64) {
        (self.x, self.y)
    }

    pub fn entry_side(&self) -> Side {
        self.entry
    }

    /// Apply a relative motion delta.
    ///
    /// Non-finite deltas are ignored rather than propagated: a single NaN would poison
    /// the accumulated position permanently, and every later comparison against the
    /// bounds would be false, so the pointer would neither move nor ever return home.
    pub fn apply(&mut self, dx: f64, dy: f64) -> PointerUpdate {
        if !dx.is_finite() || !dy.is_finite() {
            return PointerUpdate::Moved;
        }
        let nx = self.x + dx;
        let ny = self.y + dy;

        if self.crossed_back(nx, ny) {
            return PointerUpdate::ReturnedHome;
        }

        // Clamp to the peer's screen. Done after the return check so that leaving by
        // the entry edge is not clamped away before it can be noticed.
        self.x = nx.clamp(self.bounds.x, self.bounds.right());
        self.y = ny.clamp(self.bounds.y, self.bounds.bottom());
        PointerUpdate::Moved
    }

    /// Whether the new position has left through the edge the pointer entered by.
    fn crossed_back(&self, nx: f64, ny: f64) -> bool {
        match self.entry {
            Side::Left => nx < self.bounds.x,
            Side::Right => nx > self.bounds.right(),
            Side::Top => ny < self.bounds.y,
            Side::Bottom => ny > self.bounds.bottom(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer() -> Rect {
        Rect {
            x: 0.0,
            y: 0.0,
            width: 1920.0,
            height: 1080.0,
        }
    }

    #[test]
    fn entering_from_the_left_lands_on_the_left_edge_at_the_crossing_height() {
        // Arriving at a corner instead of the height you left at is the single most
        // noticeable KVM bug.
        let p = RemotePointer::enter(peer(), Side::Left, 0.25);
        assert_eq!(p.position(), (0, 270));
    }

    #[test]
    fn entering_from_the_right_lands_on_the_right_edge() {
        let p = RemotePointer::enter(peer(), Side::Right, 0.5);
        assert_eq!(p.position(), (1920, 540));
    }

    #[test]
    fn entering_from_the_top_uses_the_horizontal_fraction() {
        let p = RemotePointer::enter(peer(), Side::Top, 0.75);
        assert_eq!(p.position(), (1440, 0));
    }

    #[test]
    fn a_fraction_outside_the_edge_is_clamped_rather_than_extrapolated() {
        // A crossing fraction is derived from measured geometry, so a value slightly
        // out of range is plausible; landing off-screen because of it is not.
        let p = RemotePointer::enter(peer(), Side::Left, 1.4);
        assert_eq!(p.position(), (0, 1080));
        let p = RemotePointer::enter(peer(), Side::Left, -0.2);
        assert_eq!(p.position(), (0, 0));
    }

    #[test]
    fn moving_into_the_peer_just_moves() {
        let mut p = RemotePointer::enter(peer(), Side::Left, 0.5);
        assert_eq!(p.apply(100.0, 0.0), PointerUpdate::Moved);
        assert_eq!(p.position(), (100, 540));
    }

    #[test]
    fn sub_pixel_deltas_accumulate_instead_of_rounding_to_nothing() {
        // The bug this pins: rounding each delta on arrival makes a slow drag produce
        // an unbroken run of zeroes, so the remote pointer never moves at all.
        let mut p = RemotePointer::enter(peer(), Side::Left, 0.5);
        for _ in 0..10 {
            assert_eq!(p.apply(0.3, 0.0), PointerUpdate::Moved);
        }
        assert_eq!(p.position(), (3, 540));
    }

    #[test]
    fn leaving_by_the_entry_edge_returns_home() {
        let mut p = RemotePointer::enter(peer(), Side::Left, 0.5);
        p.apply(50.0, 0.0);
        assert_eq!(p.apply(-60.0, 0.0), PointerUpdate::ReturnedHome);
    }

    #[test]
    fn a_nudge_back_at_the_entry_edge_returns_immediately() {
        // The pointer starts on the edge, so pushing back the way it came should hand
        // control straight back rather than requiring a full screen of travel.
        let mut p = RemotePointer::enter(peer(), Side::Left, 0.5);
        assert_eq!(p.apply(-0.5, 0.0), PointerUpdate::ReturnedHome);
    }

    #[test]
    fn the_far_edge_clamps_rather_than_returning() {
        // Entered from the left, so the right edge is not a way home. Treating any
        // out-of-bounds motion as a return would hand control back from the wrong side.
        let mut p = RemotePointer::enter(peer(), Side::Left, 0.5);
        assert_eq!(p.apply(5000.0, 0.0), PointerUpdate::Moved);
        assert_eq!(p.position(), (1920, 540));
    }

    #[test]
    fn the_perpendicular_edges_clamp_rather_than_returning() {
        let mut p = RemotePointer::enter(peer(), Side::Left, 0.5);
        assert_eq!(p.apply(10.0, -5000.0), PointerUpdate::Moved);
        assert_eq!(p.position(), (10, 0));
        assert_eq!(p.apply(0.0, 5000.0), PointerUpdate::Moved);
        assert_eq!(p.position(), (10, 1080));
    }

    #[test]
    fn returning_is_judged_before_clamping() {
        // If the clamp ran first the position would be pinned to the edge and the
        // return would never be seen, so the operator could not get back.
        let mut p = RemotePointer::enter(peer(), Side::Top, 0.5);
        assert_eq!(p.apply(0.0, -1.0), PointerUpdate::ReturnedHome);
    }

    #[test]
    fn each_entry_side_returns_only_through_its_own_edge() {
        for (side, home, away) in [
            (Side::Left, (-1.0, 0.0), (1.0, 0.0)),
            (Side::Right, (1.0, 0.0), (-1.0, 0.0)),
            (Side::Top, (0.0, -1.0), (0.0, 1.0)),
            (Side::Bottom, (0.0, 1.0), (0.0, -1.0)),
        ] {
            let mut p = RemotePointer::enter(peer(), side, 0.5);
            assert_eq!(
                p.apply(home.0, home.1),
                PointerUpdate::ReturnedHome,
                "{side:?} should return through its own edge"
            );
            let mut p = RemotePointer::enter(peer(), side, 0.5);
            assert_eq!(
                p.apply(away.0, away.1),
                PointerUpdate::Moved,
                "{side:?} should not return by moving inward"
            );
        }
    }

    #[test]
    fn a_peer_at_a_non_zero_origin_is_handled_in_its_own_coordinates() {
        // A second monitor on the peer does not start at 0,0. Assuming it does puts
        // every click a screen-width away from where it belongs.
        let bounds = Rect {
            x: 1920.0,
            y: -200.0,
            width: 1280.0,
            height: 720.0,
        };
        let mut p = RemotePointer::enter(bounds, Side::Left, 0.0);
        assert_eq!(p.position(), (1920, -200));
        assert_eq!(p.apply(10.0, 10.0), PointerUpdate::Moved);
        assert_eq!(p.position(), (1930, -190));
        assert_eq!(p.apply(-20.0, 0.0), PointerUpdate::ReturnedHome);
    }

    #[test]
    fn a_non_finite_delta_is_ignored_rather_than_poisoning_the_position() {
        // One NaN would make every later bounds comparison false: the pointer would
        // stop moving and could never return home.
        let mut p = RemotePointer::enter(peer(), Side::Left, 0.5);
        p.apply(100.0, 0.0);
        assert_eq!(p.apply(f64::NAN, 0.0), PointerUpdate::Moved);
        assert_eq!(p.position(), (100, 540));
        assert_eq!(p.apply(f64::INFINITY, 0.0), PointerUpdate::Moved);
        assert_eq!(p.position(), (100, 540));
        // Still responsive afterwards.
        assert_eq!(p.apply(5.0, 0.0), PointerUpdate::Moved);
        assert_eq!(p.position(), (105, 540));
    }
}
