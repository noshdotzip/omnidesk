//! Turning continuous scroll deltas into discrete wheel notches.
//!
//! The two sides of the KVM disagree about what a scroll event is. libei (and Wayland
//! underneath it) reports scrolling as a continuous distance in logical pixels, which a
//! touchpad emits in a long stream of small fractions. Windows `SendInput` wants whole
//! multiples of [`WHEEL_DELTA`], one per notch of a physical wheel.
//!
//! # Two ways to get this wrong, both silent
//!
//! **Truncating each delta.** A touchpad drag arrives as a run of values well under one
//! notch. Converting each one independently and rounding produces an unbroken run of
//! zeroes, so the peer never scrolls at all while the operator swipes. The remainder has
//! to be carried, which is what this type exists for.
//!
//! **The sign.** Wayland's axis convention is positive-is-down: scrolling toward the
//! user increases the value. Windows' wheel convention is the opposite — a positive
//! `mouseData` means the wheel rotated *forward*, away from the user. So the vertical
//! axis has to be negated on the way across. Getting it wrong does not fail; it scrolls
//! the wrong way, everywhere, forever.
//!
//! Horizontal is *not* negated: both call positive "to the right".
//!
//! # Status
//! The sign conventions here come from the Wayland and Win32 documentation, not from a
//! measurement — the live capture path needs a permission dialog that has not been
//! answered yet. They are pinned by tests so a future correction is a one-line change
//! with a failing test to prove it, rather than an archaeology exercise.

use serde::{Deserialize, Serialize};

/// One notch of a physical mouse wheel, as Win32 defines it.
pub const WHEEL_DELTA: i32 = 120;

/// How many logical pixels of continuous scrolling equal one notch.
///
/// libei has no notion of notches, so a threshold has to be chosen. 15 px is the value
/// GTK and Chromium both use when converting smooth scrolling to discrete steps, which
/// makes a swipe here feel like a swipe in an ordinary application rather than being
/// wildly faster or slower.
pub const PIXELS_PER_NOTCH: f64 = 15.0;

/// Accumulates fractional scroll distance and emits whole wheel deltas.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct ScrollAccumulator {
    pending_x: f64,
    pending_y: f64,
}

/// Whole wheel deltas ready to inject, in Win32 units and Win32 sign convention.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WheelDelta {
    /// Horizontal, positive to the right.
    pub x: i32,
    /// Vertical, positive away from the user (scroll up).
    pub y: i32,
}

impl WheelDelta {
    pub fn is_zero(&self) -> bool {
        self.x == 0 && self.y == 0
    }
}

impl ScrollAccumulator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed a continuous scroll delta in logical pixels, using the *source's* sign
    /// convention (positive y is toward the user), and get back whatever whole notches
    /// that completes.
    ///
    /// Returns a zero delta while the movement is still under a notch; the remainder is
    /// carried to the next call.
    pub fn push(&mut self, dx: f64, dy: f64) -> WheelDelta {
        // A NaN would poison the accumulator permanently: every later comparison
        // against the threshold would be false and scrolling would stop for good.
        if dx.is_finite() {
            self.pending_x += dx;
        }
        if dy.is_finite() {
            self.pending_y += dy;
        }

        let notches_x = (self.pending_x / PIXELS_PER_NOTCH).trunc();
        let notches_y = (self.pending_y / PIXELS_PER_NOTCH).trunc();
        self.pending_x -= notches_x * PIXELS_PER_NOTCH;
        self.pending_y -= notches_y * PIXELS_PER_NOTCH;

        WheelDelta {
            x: (notches_x as i32) * WHEEL_DELTA,
            // Negated: see the module docs on sign conventions.
            y: -(notches_y as i32) * WHEEL_DELTA,
        }
    }

    /// Discard any partial notch.
    ///
    /// Called when scrolling stops or control leaves the peer, so a half-notch of
    /// leftover motion does not surface as a stray scroll the next time round.
    pub fn reset(&mut self) {
        self.pending_x = 0.0;
        self.pending_y = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_notch_of_scrolling_produces_one_wheel_delta() {
        let mut a = ScrollAccumulator::new();
        assert_eq!(
            a.push(0.0, PIXELS_PER_NOTCH),
            WheelDelta {
                x: 0,
                y: -WHEEL_DELTA
            }
        );
    }

    #[test]
    fn vertical_scrolling_is_negated_because_the_conventions_are_opposite() {
        // Wayland positive-y is toward the user; Win32 positive is away from it.
        // Without the flip the peer scrolls the wrong way, always.
        let mut a = ScrollAccumulator::new();
        let down = a.push(0.0, PIXELS_PER_NOTCH);
        assert!(
            down.y < 0,
            "scrolling toward the user must give a negative wheel delta"
        );
        let mut b = ScrollAccumulator::new();
        let up = b.push(0.0, -PIXELS_PER_NOTCH);
        assert!(
            up.y > 0,
            "scrolling away from the user must give a positive wheel delta"
        );
    }

    #[test]
    fn horizontal_scrolling_is_not_negated() {
        // Both sides agree that positive is rightward, so flipping this too would be
        // an easy symmetry mistake.
        let mut a = ScrollAccumulator::new();
        assert!(a.push(PIXELS_PER_NOTCH, 0.0).x > 0);
    }

    #[test]
    fn sub_notch_deltas_accumulate_instead_of_vanishing() {
        // The touchpad bug: converting each delta independently rounds every one of
        // them to zero, so a long smooth swipe scrolls nothing at all.
        let mut a = ScrollAccumulator::new();
        let step = PIXELS_PER_NOTCH / 5.0;
        for _ in 0..4 {
            assert!(
                a.push(0.0, step).is_zero(),
                "should not fire before a whole notch"
            );
        }
        assert_eq!(a.push(0.0, step).y, -WHEEL_DELTA);
    }

    #[test]
    fn the_remainder_carries_rather_than_being_dropped() {
        // Dropping it makes scrolling lose ground steadily: a swipe of 10 notches
        // worth of pixels would deliver fewer than 10.
        let mut a = ScrollAccumulator::new();
        let total = PIXELS_PER_NOTCH * 10.0;
        let step = total / 100.0;
        let mut notches = 0;
        for _ in 0..100 {
            notches += -a.push(0.0, step).y / WHEEL_DELTA;
        }
        assert_eq!(
            notches, 10,
            "ten notches of travel must deliver ten notches"
        );
    }

    #[test]
    fn a_large_delta_produces_several_notches_at_once() {
        let mut a = ScrollAccumulator::new();
        assert_eq!(a.push(0.0, PIXELS_PER_NOTCH * 3.0).y, -3 * WHEEL_DELTA);
    }

    #[test]
    fn reversing_direction_cancels_the_pending_remainder() {
        // Half a notch down then half a notch up is no scroll, not two half-notches.
        let mut a = ScrollAccumulator::new();
        assert!(a.push(0.0, PIXELS_PER_NOTCH * 0.5).is_zero());
        assert!(a.push(0.0, -PIXELS_PER_NOTCH * 0.5).is_zero());
        // And the accumulator is genuinely back at rest, not merely quiet.
        assert!(a.push(0.0, PIXELS_PER_NOTCH * 0.9).is_zero());
    }

    #[test]
    fn reset_discards_a_partial_notch() {
        let mut a = ScrollAccumulator::new();
        a.push(0.0, PIXELS_PER_NOTCH * 0.9);
        a.reset();
        // Without the reset this 0.2 would complete the earlier 0.9 and fire.
        assert!(a.push(0.0, PIXELS_PER_NOTCH * 0.2).is_zero());
    }

    #[test]
    fn a_non_finite_delta_does_not_poison_the_accumulator() {
        let mut a = ScrollAccumulator::new();
        assert!(a.push(0.0, f64::NAN).is_zero());
        assert!(a.push(0.0, f64::INFINITY).is_zero());
        // Still works afterwards.
        assert_eq!(a.push(0.0, PIXELS_PER_NOTCH).y, -WHEEL_DELTA);
    }

    #[test]
    fn the_two_axes_do_not_interfere() {
        // One shared remainder would make horizontal scrolling trigger vertical steps.
        let mut a = ScrollAccumulator::new();
        assert!(a.push(PIXELS_PER_NOTCH * 0.9, 0.0).is_zero());
        let d = a.push(0.0, PIXELS_PER_NOTCH);
        assert_eq!(
            d.x, 0,
            "horizontal remainder must not fire on vertical motion"
        );
        assert_eq!(d.y, -WHEEL_DELTA);
    }
}
