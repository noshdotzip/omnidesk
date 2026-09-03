//! Reading the local pointer position and the virtual desktop bounds.
//!
//! This is the *source* side of a KVM link on Windows: to forward the pointer to
//! another machine you first have to know where it is and what space it lives in.
//! Injection (the destination side) lives in [`crate::inject`].
//!
//! The virtual screen is the bounding box of every monitor, and its origin is **not**
//! necessarily `(0, 0)`: a monitor placed left of or above the primary gives it
//! negative coordinates. Code that assumes an origin of zero works on a single-monitor
//! desk and breaks on the multi-monitor setups this project exists to serve.

use crate::inject::VirtualScreen;

/// Where the pointer is, in virtual-desktop coordinates.
///
/// Returns `None` if the OS refuses, which happens on a locked workstation or secure
/// desktop. That is a legitimate answer, not an error to paper over: forwarding a stale
/// position would be worse than forwarding none.
pub fn cursor_position() -> Option<(i32, i32)> {
    #[cfg(windows)]
    {
        imp::cursor_position()
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// The bounding box of all monitors.
pub fn virtual_screen() -> Option<VirtualScreen> {
    #[cfg(windows)]
    {
        imp::virtual_screen()
    }
    #[cfg(not(windows))]
    {
        None
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::WindowsAndMessaging::{
        GetCursorPos, GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
        SM_YVIRTUALSCREEN,
    };

    pub fn cursor_position() -> Option<(i32, i32)> {
        let mut p = POINT::default();
        // SAFETY: `p` is a valid, writable POINT for the duration of the call.
        unsafe { GetCursorPos(&mut p) }.ok()?;
        Some((p.x, p.y))
    }

    pub fn virtual_screen() -> Option<VirtualScreen> {
        // SAFETY: GetSystemMetrics takes no pointers and cannot fail unsafely.
        let (left, top, width, height) = unsafe {
            (
                GetSystemMetrics(SM_XVIRTUALSCREEN),
                GetSystemMetrics(SM_YVIRTUALSCREEN),
                GetSystemMetrics(SM_CXVIRTUALSCREEN),
                GetSystemMetrics(SM_CYVIRTUALSCREEN),
            )
        };
        // A zero-sized virtual screen means the metrics are unavailable (no session).
        // Returning it would make every later mapping divide by zero.
        if width <= 0 || height <= 0 {
            return None;
        }
        Some(VirtualScreen {
            left,
            top,
            width,
            height,
        })
    }
}

/// Whether a point sits on the given edge of a virtual screen, within `slop` pixels.
///
/// Pure, so it is tested on every platform. `slop` exists because the pointer does not
/// reliably land exactly on the last pixel column: the OS clamps it there, but polling
/// can observe it a pixel or two short depending on acceleration and timing.
pub fn at_right_edge(x: i32, vs: VirtualScreen, slop: i32) -> bool {
    x >= vs.left + vs.width - 1 - slop.max(0)
}

/// Position along the vertical axis as a fraction of the screen height, clamped.
///
/// This is what gets handed to the peer so it can place the pointer at the matching
/// height on a screen of a different size.
pub fn vertical_fraction(y: i32, vs: VirtualScreen) -> f64 {
    if vs.height <= 1 {
        return 0.0;
    }
    let f = (y - vs.top) as f64 / (vs.height - 1) as f64;
    f.clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const VS: VirtualScreen = VirtualScreen {
        left: 0,
        top: 0,
        width: 1920,
        height: 1080,
    };

    #[test]
    fn right_edge_is_the_last_pixel_column() {
        assert!(at_right_edge(1919, VS, 0));
        assert!(!at_right_edge(1918, VS, 0));
    }

    #[test]
    fn slop_widens_the_edge_band() {
        // Polling can observe the pointer a pixel short of the clamp.
        assert!(at_right_edge(1917, VS, 2));
        assert!(!at_right_edge(1916, VS, 2));
    }

    #[test]
    fn negative_slop_is_treated_as_zero_not_as_a_narrower_edge() {
        assert!(at_right_edge(1919, VS, -5));
        assert!(!at_right_edge(1918, VS, -5));
    }

    #[test]
    fn a_left_of_primary_monitor_shifts_the_edge() {
        // Origin is not always (0,0); assuming so breaks exactly the multi-monitor
        // setups this project targets.
        let vs = VirtualScreen {
            left: -1920,
            top: 0,
            width: 3840,
            height: 1080,
        };
        assert!(at_right_edge(1919, vs, 0));
        assert!(!at_right_edge(0, vs, 0));
    }

    #[test]
    fn vertical_fraction_spans_zero_to_one_inclusive() {
        assert_eq!(vertical_fraction(0, VS), 0.0);
        assert_eq!(vertical_fraction(1079, VS), 1.0);
        assert!((vertical_fraction(540, VS) - 0.5).abs() < 0.01);
    }

    #[test]
    fn vertical_fraction_clamps_outside_the_screen() {
        // A pointer on another monitor can be outside this screen's band entirely.
        assert_eq!(vertical_fraction(-500, VS), 0.0);
        assert_eq!(vertical_fraction(99_999, VS), 1.0);
    }

    #[test]
    fn vertical_fraction_honours_a_negative_origin() {
        let vs = VirtualScreen {
            left: 0,
            top: -1080,
            width: 1920,
            height: 1080,
        };
        assert_eq!(vertical_fraction(-1080, vs), 0.0);
        assert_eq!(vertical_fraction(-1, vs), 1.0);
    }

    #[test]
    fn degenerate_height_does_not_divide_by_zero() {
        let vs = VirtualScreen {
            left: 0,
            top: 0,
            width: 100,
            height: 1,
        };
        assert_eq!(vertical_fraction(0, vs), 0.0);
    }
}
