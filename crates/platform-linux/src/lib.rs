//! `ultidesk-platform-linux` — Wayland/portal backend.
//!
//! This is the Linux counterpart to `ultidesk-platform-windows`. It is **not** a port
//! of it, because the two platforms expose fundamentally different models:
//!
//! | | Windows | Wayland |
//! |---|---|---|
//! | Discover windows | `EnumWindows` lists every top-level window | no equivalent, by design |
//! | Choose a window | Ultidesk draws the picker | the *compositor* draws the picker |
//! | Result | an `HWND` we can capture | a PipeWire node id for the chosen window |
//!
//! Wayland deliberately denies a client any view of other clients' windows, so
//! [`enumerate_top_level_windows`] cannot be implemented the way the Windows backend
//! implements it and always returns empty. That is a permanent property of the
//! platform, not a to-do: the projection flow on Linux must ask the ScreenCast portal
//! to open its own picker and hand back the user's choice.
//!
//! KDE does expose `org.kde.KWin` D-Bus methods that can reach a window list
//! (`/Scripting loadScript`, or the interactive `queryWindowInfo`), but reaching them
//! means injecting a script into the compositor or forcing a click. Both are
//! KDE-specific and neither respects the portal permission model, so neither is used
//! here — consistent with ADR-0006's refusal to work around OS security boundaries.
//!
//! # Status
//! Capability probing ([`portal::probe`]) is implemented and runs against a real
//! session bus. Capture and input injection are not implemented yet; their entry
//! points return [`InputError::Unsupported`] rather than pretending to work.

pub mod audio_devices;
pub mod caps;
pub mod input_capture;
pub mod keymap;
pub mod pipewire_capture;
pub mod pointer;
pub mod portal;
pub mod portal_call;
pub mod remote_desktop;
pub mod request;
pub mod screen_cast;

pub use caps::{DeviceTypes, SourceTypes};
pub use portal::{probe, PortalError, PortalReport};

/// Re-exported so callers can handle one error type across platform backends.
pub use ultidesk_platform_windows::inject::InputError;

/// Enumerate capturable top-level windows.
///
/// Always returns empty on Linux. See the module docs: Wayland has no API for one
/// client to list another client's windows, so a truthful implementation returns
/// nothing rather than inventing a partial list from compositor-specific side doors.
/// The picker must come from the ScreenCast portal instead.
pub fn enumerate_top_level_windows() -> Vec<ultidesk_platform_windows::WindowInfo> {
    Vec::new()
}

/// Inject a pointer motion event.
///
/// Not implemented. On Linux this must go through an approved
/// `org.freedesktop.portal.RemoteDesktop` session, which requires a user-granted
/// session handle that this crate does not yet establish.
pub fn inject_pointer_motion(_x: f64, _y: f64) -> Result<(), InputError> {
    Err(InputError::Unsupported)
}

/// Inject a key event by evdev keycode. Not implemented; see [`inject_pointer_motion`].
pub fn inject_key(_keycode: i32, _pressed: bool) -> Result<(), InputError> {
    Err(InputError::Unsupported)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enumeration_is_empty_and_honest_on_every_platform() {
        assert!(enumerate_top_level_windows().is_empty());
    }

    #[test]
    fn unimplemented_injection_reports_unsupported_not_success() {
        // The failure this guards: a stub that returns Ok(()) and makes the projection
        // layer believe input was delivered when nothing happened.
        assert_eq!(
            inject_pointer_motion(10.0, 10.0),
            Err(InputError::Unsupported)
        );
        assert_eq!(inject_key(30, true), Err(InputError::Unsupported));
    }
}
