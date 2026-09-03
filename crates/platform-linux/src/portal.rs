//! XDG desktop portal capability probe.
//!
//! Reads the advertised version and capability properties of the portals Ultidesk
//! needs. This is *read-only*: it opens no session and shows the user no dialog, so it
//! is safe to run at startup or from a headless SSH shell to decide what the local
//! compositor can actually do before offering the user a feature that cannot work.
//!
//! Creating a session (the step that raises a permission dialog and yields a PipeWire
//! node or an input handle) is deliberately not done here — see `docs/permissions.md`;
//! nothing in Ultidesk should raise a portal prompt as a side effect of a probe.

use crate::caps::{DeviceTypes, SourceTypes};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[cfg(target_os = "linux")]
const PORTAL_SERVICE: &str = "org.freedesktop.portal.Desktop";
#[cfg(target_os = "linux")]
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";

#[derive(Debug, Error)]
pub enum PortalError {
    #[error("portals are only available on Linux builds")]
    Unsupported,
    /// Could not open the session bus at all.
    #[error("could not reach the session bus: {0}")]
    Connect(String),
    /// Any other D-Bus level failure: a bad signature, an unexpected reply shape, a
    /// method error. Worded neutrally on purpose — an earlier version reported all of
    /// these as "could not reach the session bus", which pointed debugging at the
    /// connection when the real fault was in the message.
    #[error("portal call failed: {0}")]
    Bus(String),
    /// The user declined the portal permission dialog. A decision, not a fault: it must
    /// never be retried in a loop or reported as an internal error.
    #[error("the user declined the {0} permission request")]
    Denied(String),
    /// No Response signal arrived in time — usually a permission dialog nobody
    /// answered. Distinct from Denied: the user did not refuse, they were not there.
    #[error("timed out waiting for a response to {0} (was the permission dialog answered?)")]
    TimedOut(String),
}

/// What one portal interface reports about itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScreenCastInfo {
    pub version: u32,
    pub source_types: SourceTypes,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputInfo {
    pub version: u32,
    pub device_types: DeviceTypes,
}

/// The capabilities of the local desktop, as advertised by its portal backend.
///
/// Every field is optional because a portal interface that the backend does not
/// implement is simply absent from the bus. `None` means "this compositor does not
/// offer it", which is a different and more actionable answer than a zero bitmask.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PortalReport {
    pub screen_cast: Option<ScreenCastInfo>,
    /// Injecting input *into* this machine (the destination half of a KVM link).
    pub remote_desktop: Option<InputInfo>,
    /// Capturing input *from* this machine at a screen edge (the source half).
    pub input_capture: Option<InputInfo>,
    pub clipboard: Option<u32>,
}

impl PortalReport {
    /// Whether this desktop can act as a projection **source**: it must be able to
    /// hand us a single window, not just a whole monitor.
    pub fn can_project_window(&self) -> bool {
        self.screen_cast
            .as_ref()
            .is_some_and(|s| s.source_types.supports_window_capture())
    }

    /// Whether this desktop can act as a KVM **destination** (receive and inject).
    pub fn can_receive_input(&self) -> bool {
        self.remote_desktop
            .as_ref()
            .is_some_and(|i| i.device_types.supports_keyboard_and_pointer())
    }

    /// Whether this desktop can act as a KVM **source**, capturing input at an edge.
    ///
    /// `InputCapture` is the correct primitive for edge crossing. Its absence does not
    /// mean KVM is impossible, but it does mean any implementation would be a
    /// compositor-specific workaround rather than a portable one.
    pub fn can_capture_input(&self) -> bool {
        self.input_capture
            .as_ref()
            .is_some_and(|i| i.device_types.supports_keyboard_and_pointer())
    }
}

#[cfg(target_os = "linux")]
pub fn probe() -> Result<PortalReport, PortalError> {
    imp::probe()
}

#[cfg(not(target_os = "linux"))]
pub fn probe() -> Result<PortalReport, PortalError> {
    Err(PortalError::Unsupported)
}

#[cfg(target_os = "linux")]
mod imp {
    use super::*;
    use zbus::blocking::{Connection, Proxy};

    /// Read a `u32` property, treating "the backend does not implement this interface"
    /// as `None` rather than as an error — that is a normal answer, not a failure.
    fn u32_prop(conn: &Connection, interface: &str, property: &str) -> Option<u32> {
        let proxy = Proxy::new(conn, PORTAL_SERVICE, PORTAL_PATH, interface).ok()?;
        proxy.get_property::<u32>(property).ok()
    }

    pub fn probe() -> Result<PortalReport, PortalError> {
        let conn = Connection::session().map_err(|e| PortalError::Connect(e.to_string()))?;

        let screen_cast =
            u32_prop(&conn, "org.freedesktop.portal.ScreenCast", "version").map(|version| {
                let bits = u32_prop(
                    &conn,
                    "org.freedesktop.portal.ScreenCast",
                    "AvailableSourceTypes",
                )
                .unwrap_or(0);
                ScreenCastInfo {
                    version,
                    source_types: SourceTypes::from_bits(bits),
                }
            });

        let remote_desktop = u32_prop(&conn, "org.freedesktop.portal.RemoteDesktop", "version")
            .map(|version| {
                let bits = u32_prop(
                    &conn,
                    "org.freedesktop.portal.RemoteDesktop",
                    "AvailableDeviceTypes",
                )
                .unwrap_or(0);
                InputInfo {
                    version,
                    device_types: DeviceTypes::from_bits(bits),
                }
            });

        let input_capture =
            u32_prop(&conn, "org.freedesktop.portal.InputCapture", "version").map(|version| {
                let bits = u32_prop(
                    &conn,
                    "org.freedesktop.portal.InputCapture",
                    "SupportedCapabilities",
                )
                .unwrap_or(0);
                InputInfo {
                    version,
                    device_types: DeviceTypes::from_bits(bits),
                }
            });

        let clipboard = u32_prop(&conn, "org.freedesktop.portal.Clipboard", "version");

        Ok(PortalReport {
            screen_cast,
            remote_desktop,
            input_capture,
            clipboard,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(bits: u32) -> Option<InputInfo> {
        Some(InputInfo {
            version: 2,
            device_types: DeviceTypes::from_bits(bits),
        })
    }

    #[test]
    fn a_desktop_with_everything_can_do_all_three_roles() {
        let r = PortalReport {
            screen_cast: Some(ScreenCastInfo {
                version: 4,
                source_types: SourceTypes::from_bits(7),
            }),
            remote_desktop: info(7),
            input_capture: info(7),
            clipboard: Some(1),
        };
        assert!(r.can_project_window());
        assert!(r.can_receive_input());
        assert!(r.can_capture_input());
    }

    #[test]
    fn monitor_only_screencast_cannot_project_a_window() {
        let r = PortalReport {
            screen_cast: Some(ScreenCastInfo {
                version: 4,
                source_types: SourceTypes::from_bits(SourceTypes::MONITOR),
            }),
            ..Default::default()
        };
        assert!(!r.can_project_window());
    }

    #[test]
    fn absent_portal_is_reported_as_incapable_not_as_capable() {
        // The failure this guards: treating a missing interface as "probably fine".
        let empty = PortalReport::default();
        assert!(!empty.can_project_window());
        assert!(!empty.can_receive_input());
        assert!(!empty.can_capture_input());
    }

    #[test]
    fn pointer_only_remote_desktop_is_not_a_usable_kvm_destination() {
        let r = PortalReport {
            remote_desktop: info(DeviceTypes::POINTER),
            ..Default::default()
        };
        assert!(!r.can_receive_input());
    }
}
