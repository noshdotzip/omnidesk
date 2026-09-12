//! Enumerating this machine's monitors without a window.
//!
//! # Why the agent needs its own enumerator
//! The control app reads monitors through `tao`, which needs a window to hang them off.
//! The agent has none — it runs headless, often before anyone has opened a UI — and a
//! peer asking what screens this machine has must be answered whether or not a window
//! exists. So this walks Win32 directly.
//!
//! Two enumerators are two chances to disagree, which
//! `apps/control/src/monitors.rs` warns about at length. The intended resolution is that
//! the control app eventually asks the agent rather than the toolkit, leaving one; until
//! then the two are kept honest by reporting the *same* coordinate space, which is the
//! thing that would actually hurt to get wrong.
//!
//! # The coordinate space is the virtual desktop's, unscaled
//! `EnumDisplayMonitors` reports each monitor's rectangle in virtual-desktop pixels, and
//! that is what is stored — deliberately not divided by the monitor's DPI. The virtual
//! desktop is one coordinate space: a 100% monitor and a 150% monitor sitting edge to
//! edge share an exact boundary in it, and rescaling each by its own factor turns that
//! boundary into a gap or an overlap. Adjacency then reports no shared border and the
//! pointer can never cross between them — a bug that only appears on mixed-DPI desks.
//!
//! The origin is **not** necessarily `(0, 0)`: a monitor left of or above the primary
//! has negative coordinates.

use serde::{Deserialize, Serialize};
use ultidesk_core::DeviceId;
use ultidesk_topology::{Monitor, MonitorId, Rotation};

/// One monitor as Win32 describes it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WinMonitor {
    /// The device name, e.g. `\\.\DISPLAY1`. Stable across sessions and unique among
    /// attached monitors, which is what a saved arrangement keys on.
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    /// The monitor's effective scale, as a factor: 1.5 for 150%.
    pub scale_factor: f64,
    pub primary: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum MonitorEnumError {
    #[error("this build has no Win32 support (not a Windows target)")]
    Unsupported,
    #[error("EnumDisplayMonitors reported no monitors")]
    NoMonitors,
}

/// Every attached monitor, in the order Win32 reports them.
#[cfg(windows)]
pub fn enumerate() -> Result<Vec<WinMonitor>, MonitorEnumError> {
    let found = imp::enumerate();
    if found.is_empty() {
        // A real state on a locked or session-0 context, and better named than returned
        // as an empty list that reads like "this machine has no screens".
        return Err(MonitorEnumError::NoMonitors);
    }
    Ok(found)
}

#[cfg(not(windows))]
pub fn enumerate() -> Result<Vec<WinMonitor>, MonitorEnumError> {
    Err(MonitorEnumError::Unsupported)
}

/// The same monitors as [`enumerate`], as the shared [`Monitor`] the topology model and
/// the settings IPC both speak.
///
/// The mapping lives here rather than in each caller for the same reason the audio one
/// does: it encodes which field is the identity a saved arrangement keys on, and two
/// copies are two chances to disagree about that.
pub fn enumerate_shared(device_id: DeviceId) -> Result<Vec<Monitor>, MonitorEnumError> {
    Ok(enumerate()?
        .into_iter()
        .enumerate()
        .map(|(i, m)| to_shared(device_id, i, m))
        .collect())
}

/// Pure so the mapping is testable without a desktop.
fn to_shared(device_id: DeviceId, index: usize, m: WinMonitor) -> Monitor {
    Monitor {
        device_id,
        monitor_id: MonitorId(index as u32 + 1),
        friendly_name: m.name,
        // Verbatim. See the module docs on why these are not divided by the scale.
        logical_x: m.x as f64,
        logical_y: m.y as f64,
        logical_width: m.width as f64,
        logical_height: m.height as f64,
        native_pixel_width: m.width.max(0) as u32,
        native_pixel_height: m.height.max(0) as u32,
        scale_factor: m.scale_factor,
        // Win32 reports orientation per *display device* rather than per monitor handle,
        // and nothing in the layout consults it: a rotated monitor already reports its
        // rotated width and height above, which is what the geometry uses.
        rotation: Rotation::Landscape,
        // `EnumDisplayMonitors` says nothing about refresh rate. `None` rather than a
        // plausible 60, which would put an unmeasured number on screen.
        refresh_rate: None,
        primary: m.primary,
    }
}

#[cfg(windows)]
mod imp {
    use super::WinMonitor;
    use windows::Win32::Foundation::{BOOL, LPARAM, RECT};
    use windows::Win32::Graphics::Gdi::{
        EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFOEXW,
    };
    use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};

    /// The DPI Windows reports for an unscaled monitor. A scale factor is the reported
    /// DPI over this.
    const USER_DEFAULT_SCREEN_DPI: f64 = 96.0;

    /// `MONITORINFOF_PRIMARY`. Spelled out because `windows` 0.58 does not export it;
    /// it is a stable Win32 value, not something this code invented.
    const MONITORINFOF_PRIMARY: u32 = 0x0000_0001;

    pub fn enumerate() -> Vec<WinMonitor> {
        let mut out: Vec<WinMonitor> = Vec::new();
        // SAFETY: the callback below only writes through the `LPARAM` we pass, which is
        // a pointer to `out` and outlives the call — `EnumDisplayMonitors` is
        // synchronous and does not retain it.
        unsafe {
            let _ = EnumDisplayMonitors(
                HDC::default(),
                None,
                Some(callback),
                LPARAM(&mut out as *mut Vec<WinMonitor> as isize),
            );
        }
        out
    }

    unsafe extern "system" fn callback(
        monitor: HMONITOR,
        _hdc: HDC,
        _rect: *mut RECT,
        data: LPARAM,
    ) -> BOOL {
        let out = &mut *(data.0 as *mut Vec<WinMonitor>);

        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        if GetMonitorInfoW(monitor, &mut info.monitorInfo as *mut _).as_bool() {
            let r = info.monitorInfo.rcMonitor;
            out.push(WinMonitor {
                name: device_name(&info),
                x: r.left,
                y: r.top,
                width: r.right - r.left,
                height: r.bottom - r.top,
                scale_factor: scale_of(monitor),
                primary: info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
            });
        }
        // Keep enumerating: returning FALSE here would silently truncate the list at the
        // first monitor whose info could not be read.
        BOOL(1)
    }

    /// `szDevice` is a fixed-size, NUL-padded UTF-16 array, not a string.
    unsafe fn device_name(info: &MONITORINFOEXW) -> String {
        let raw = &info.szDevice;
        let len = raw.iter().position(|&c| c == 0).unwrap_or(raw.len());
        String::from_utf16_lossy(&raw[..len])
    }

    /// The monitor's effective scale.
    ///
    /// Falls back to 1.0 rather than failing the whole enumeration: the scale is used to
    /// describe a monitor, never to place it (positions are already in virtual-desktop
    /// pixels), so a wrong scale is a cosmetic error while a missing monitor is not.
    unsafe fn scale_of(monitor: HMONITOR) -> f64 {
        let mut dpi_x = 0u32;
        let mut dpi_y = 0u32;
        match GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) {
            Ok(()) if dpi_x > 0 => dpi_x as f64 / USER_DEFAULT_SCREEN_DPI,
            _ => 1.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn win(name: &str, x: i32, y: i32, w: i32, h: i32, scale: f64) -> WinMonitor {
        WinMonitor {
            name: name.into(),
            x,
            y,
            width: w,
            height: h,
            scale_factor: scale,
            primary: false,
        }
    }

    #[test]
    fn geometry_is_carried_across_unscaled() {
        // The decision this pins: a 150% monitor keeps its virtual-desktop rectangle.
        // Dividing by the scale here is what turns a shared boundary into a gap.
        let id = DeviceId::new();
        let m = to_shared(id, 0, win(r"\\.\DISPLAY1", 0, 0, 2496, 1664, 1.5));
        assert_eq!((m.logical_width, m.logical_height), (2496.0, 1664.0));
        assert_eq!(m.scale_factor, 1.5);
        assert_eq!(m.friendly_name, r"\\.\DISPLAY1");
        assert_eq!(m.device_id, id);
    }

    #[test]
    fn a_monitor_left_of_the_primary_keeps_its_negative_origin() {
        // Code that assumes an origin of (0,0) works on one monitor and breaks on the
        // multi-monitor desks this project exists for.
        let m = to_shared(DeviceId::new(), 1, win("D2", -1920, -200, 1920, 1080, 1.0));
        assert_eq!((m.logical_x, m.logical_y), (-1920.0, -200.0));
        assert_eq!(m.right(), 0.0);
    }

    #[test]
    fn two_monitors_that_touch_still_touch_after_mapping() {
        // The property every crossing depends on, stated as a test rather than trusted.
        let id = DeviceId::new();
        let a = to_shared(id, 0, win("A", 0, 0, 2496, 1664, 1.5));
        let b = to_shared(id, 1, win("B", 2496, 0, 1920, 1080, 1.0));
        assert_eq!(a.right(), b.logical_x, "the shared boundary must survive");
    }

    #[test]
    fn monitor_ids_are_positional_and_start_at_one() {
        let id = DeviceId::new();
        assert_eq!(
            to_shared(id, 0, win("A", 0, 0, 1, 1, 1.0)).monitor_id,
            MonitorId(1)
        );
        assert_eq!(
            to_shared(id, 3, win("D", 0, 0, 1, 1, 1.0)).monitor_id,
            MonitorId(4)
        );
    }

    #[test]
    fn no_refresh_rate_is_reported_because_none_is_read() {
        assert!(to_shared(DeviceId::new(), 0, win("A", 0, 0, 1, 1, 1.0))
            .refresh_rate
            .is_none());
    }
}
