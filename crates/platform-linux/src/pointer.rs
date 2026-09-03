//! Absolute pointer positions over a relative-only transport.
//!
//! Ultidesk's wire protocol carries **absolute** screen coordinates, because that is
//! what window projection and edge crossing both need. The XDG RemoteDesktop portal
//! offers `NotifyPointerMotionAbsolute` only for a stream obtained from an associated
//! ScreenCast session; a pure-input KVM has no such stream and can use only the
//! **relative** `NotifyPointerMotion`.
//!
//! Bridging the two means tracking where we believe the pointer is and emitting the
//! delta that gets it where it should be. This is the same technique Synergy-style
//! tools use, and it is pure arithmetic, so it is tested on every platform.
//!
//! # Establishing a known origin
//! Dead reckoning needs a starting point, and there is no portal call to ask where the
//! pointer currently is. [`VirtualCursor::home_delta`] exploits the fact that the
//! compositor clamps the pointer at the screen edge: one enormous negative delta parks
//! it in the top-left corner no matter where it started, which *is* a known position.
//! After that, every absolute move is exact.

/// A delta large enough to drive the pointer into the top-left corner from anywhere on
/// any plausible desktop. The compositor clamps at the edge, so overshooting is safe
/// and is the whole point.
pub const HOME_DELTA: f64 = -100_000.0;

/// Tracks the believed pointer position so absolute targets can be expressed as
/// relative deltas.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct VirtualCursor {
    position: Option<(i32, i32)>,
}

impl VirtualCursor {
    pub fn new() -> Self {
        VirtualCursor { position: None }
    }

    /// Where we believe the pointer is, or `None` before homing.
    pub fn position(&self) -> Option<(i32, i32)> {
        self.position
    }

    /// The delta that parks the pointer in the top-left corner.
    ///
    /// Call this once before the first absolute move, then [`VirtualCursor::homed`] to
    /// record the resulting known position.
    pub fn home_delta(&self) -> (f64, f64) {
        (HOME_DELTA, HOME_DELTA)
    }

    /// Record that the pointer has been driven to the desktop origin.
    ///
    /// `origin` is the top-left of the virtual desktop, which is not always `(0, 0)`:
    /// a monitor placed above or left of the primary gives it negative coordinates.
    pub fn homed(&mut self, origin: (i32, i32)) {
        self.position = Some(origin);
    }

    /// Forget the tracked position, forcing a re-home before the next absolute move.
    ///
    /// Anything that lets something *else* move the pointer — the capture being
    /// released, the session being suspended — invalidates dead reckoning, and
    /// continuing from a stale belief silently offsets every later move.
    pub fn invalidate(&mut self) {
        self.position = None;
    }

    /// The relative delta that moves the pointer to an absolute target.
    ///
    /// Returns `None` when the position is not yet known, which the caller must handle
    /// by homing first rather than by guessing a delta.
    pub fn delta_to(&mut self, x: i32, y: i32) -> Option<(f64, f64)> {
        let (cx, cy) = self.position?;
        // i64 first: two i32 extremes differ by more than i32::MAX.
        let dx = x as i64 - cx as i64;
        let dy = y as i64 - cy as i64;
        self.position = Some((x, y));
        Some((dx as f64, dy as f64))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_delta_before_homing() {
        // The caller must home first; guessing a delta from an unknown origin offsets
        // every subsequent move by the same unknown amount.
        let mut c = VirtualCursor::new();
        assert_eq!(c.position(), None);
        assert_eq!(c.delta_to(100, 100), None);
    }

    #[test]
    fn home_delta_overshoots_hard_enough_to_clamp_from_any_screen() {
        let c = VirtualCursor::new();
        let (dx, dy) = c.home_delta();
        assert!(dx <= -100_000.0 && dy <= -100_000.0);
    }

    #[test]
    fn after_homing_deltas_are_exact() {
        let mut c = VirtualCursor::new();
        c.homed((0, 0));
        assert_eq!(c.delta_to(100, 50), Some((100.0, 50.0)));
        // Position advances, so the next delta is relative to the new spot, not the origin.
        assert_eq!(c.delta_to(120, 50), Some((20.0, 0.0)));
        assert_eq!(c.position(), Some((120, 50)));
    }

    #[test]
    fn deltas_can_be_negative() {
        let mut c = VirtualCursor::new();
        c.homed((0, 0));
        c.delta_to(500, 500);
        assert_eq!(c.delta_to(100, 200), Some((-400.0, -300.0)));
    }

    #[test]
    fn a_negative_origin_is_honoured() {
        // A monitor above/left of the primary puts the virtual desktop origin negative;
        // homing to (0,0) there would offset every move by the panel size.
        let mut c = VirtualCursor::new();
        c.homed((-1920, -1080));
        assert_eq!(c.delta_to(0, 0), Some((1920.0, 1080.0)));
    }

    #[test]
    fn moving_to_the_current_position_is_a_zero_delta() {
        let mut c = VirtualCursor::new();
        c.homed((10, 10));
        assert_eq!(c.delta_to(10, 10), Some((0.0, 0.0)));
    }

    #[test]
    fn invalidate_forces_a_rehome() {
        let mut c = VirtualCursor::new();
        c.homed((0, 0));
        c.delta_to(50, 50);
        c.invalidate();
        assert_eq!(c.position(), None);
        assert_eq!(c.delta_to(60, 60), None);
    }

    #[test]
    fn extreme_coordinates_do_not_overflow() {
        // i32::MIN to i32::MAX exceeds i32 range; computing the delta in i32 would wrap
        // and send the pointer the wrong way.
        let mut c = VirtualCursor::new();
        c.homed((i32::MIN, i32::MIN));
        let (dx, dy) = c.delta_to(i32::MAX, i32::MAX).expect("homed");
        assert_eq!(dx, (i32::MAX as i64 - i32::MIN as i64) as f64);
        assert!(dx > 0.0 && dy > 0.0);
    }
}
