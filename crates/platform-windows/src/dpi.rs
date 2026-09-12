//! Telling Windows this process deals in real pixels.
//!
//! # What a DPI-unaware process is shown
//! Windows lies to a process that has not declared DPI awareness, and it lies
//! *consistently*: `EnumDisplayMonitors` returns rectangles divided by the scale factor,
//! `GetDpiForMonitor` answers 96 for every monitor, and the virtual-screen metrics match.
//! Nothing looks wrong from the inside — the numbers agree with each other.
//!
//! It was found by comparing the agent with the control app on the same machine: the
//! agent reported its 150% panel as **1664x1109 at scale 1.0** while the toolkit-based
//! control app reported **2496x1664 at scale 1.5**. Both were "right" for their own
//! process, and 1664 x 1.5 = 2496 is the whole explanation.
//!
//! # Why that is not survivable here
//! Self-consistency is enough for a process that only talks to itself. This one does not:
//!
//! - a peer is told this machine's monitor geometry, and acts on it;
//! - the control app arranges those monitors and saves positions the agent later reads;
//! - injected pointer coordinates are expressed in that same space.
//!
//! Two processes on one machine disagreeing by a factor of 1.5 puts the pointer in the
//! wrong place, and it does so only on scaled displays — the ones most likely to be
//! someone's actual laptop.
//!
//! # Per-monitor v2, not system-aware
//! System awareness picks one scale for the whole desktop and virtualizes anything that
//! disagrees, which is the same bug again on a mixed-DPI desk. V2 reports every monitor
//! truthfully, which is what the topology model assumes.

/// Declare this process per-monitor DPI aware.
///
/// Idempotent from the caller's point of view and safe to call unconditionally: Windows
/// refuses a second call, and a process that already has awareness (from a manifest, say)
/// is already in the state this wants.
///
/// Returns whether the declaration was applied by this call. `false` means it was already
/// set or the OS declined — not that the process is unaware.
pub fn make_process_per_monitor_aware() -> bool {
    #[cfg(windows)]
    {
        use windows::Win32::UI::HiDpi::{
            SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        };
        // SAFETY: no arguments to get wrong; the constant is a well-known sentinel and
        // the call only affects this process.
        unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2).is_ok() }
    }
    #[cfg(not(windows))]
    {
        // Wayland has no equivalent: the compositor reports logical geometry to every
        // client and never virtualizes it per process.
        false
    }
}
