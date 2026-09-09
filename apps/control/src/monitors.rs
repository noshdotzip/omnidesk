//! Reading this machine's real monitors into the topology model.
//!
//! Enumerated through `tao`, which the window toolkit already provides on both
//! platforms, rather than through `EnumDisplayMonitors` and a Wayland output listener.
//! Two backends would be two chances to disagree about the coordinate space, and this
//! editor's whole job is to agree with it.
//!
//! # The coordinate space is the platform's, and it must not be rescaled per monitor
//! `tao` reports each monitor's position and size in the virtual desktop's own units.
//! It is tempting to divide both by that monitor's `scale_factor` to get "logical"
//! values, and on a single-monitor machine it even looks right.
//!
//! It is wrong as soon as two monitors have different scale factors. The virtual
//! desktop is *one* coordinate space: a 100%% monitor and a 150%% monitor that sit edge
//! to edge share an exact boundary in it. Divide each by its own scale and that shared
//! boundary becomes a gap or an overlap — so `adjacency` reports no shared border, and
//! the pointer can never cross between them. The bug would only appear on mixed-DPI
//! desks, which is exactly where nobody tests.
//!
//! So positions and sizes are stored exactly as reported. `scale_factor` and
//! `native_pixel_*` are kept alongside for the pointer mapping that does need them.

use dioxus::desktop::tao::window::Window;
use ultidesk_core::DeviceId;
use ultidesk_topology::{Monitor, MonitorId, Rotation};

/// Read the monitors attached to this machine.
///
/// Returns an empty vector if the toolkit reports none, which happens on a headless
/// session. The caller shows that as an empty editor rather than inventing a display.
pub fn local_monitors(window: &Window, device_id: DeviceId) -> Vec<Monitor> {
    let primary = window.primary_monitor().and_then(|m| m.name());

    window
        .available_monitors()
        .enumerate()
        .map(|(i, handle)| {
            let pos = handle.position();
            let size = handle.size();
            let name = handle.name();

            Monitor {
                device_id,
                monitor_id: MonitorId(i as u32 + 1),
                friendly_name: name
                    .clone()
                    // A monitor with no reported name still has to be selectable, and
                    // an empty label in the editor is unusable.
                    .filter(|n| !n.trim().is_empty())
                    .unwrap_or_else(|| format!("Display {}", i + 1)),
                // Verbatim, not rescaled. See the module docs.
                logical_x: pos.x as f64,
                logical_y: pos.y as f64,
                logical_width: size.width as f64,
                logical_height: size.height as f64,
                native_pixel_width: size.width,
                native_pixel_height: size.height,
                scale_factor: handle.scale_factor(),
                // tao exposes no rotation. Reporting Landscape is a placeholder, not a
                // reading: the editor does not act on it yet, and a rotated monitor
                // still reports its rotated width and height above, which is what the
                // layout actually uses.
                rotation: Rotation::Landscape,
                refresh_rate: refresh_rate(&handle),
                // Matched by name because `MonitorHandle` has no identity comparison.
                // Both `None` would match every monitor, so that case is excluded.
                primary: primary.is_some() && name == primary,
            }
        })
        .collect()
}

/// The monitor's refresh rate, when the platform will say.
///
/// `None` on Linux: `tao`'s `video_modes()` is documented as unsupported there and
/// always yields an empty iterator. Reporting a plausible 60 Hz instead would put a
/// number on screen that was never measured.
///
/// Where modes *are* reported (Windows), this takes the highest advertised rate. That
/// is the panel's capability rather than necessarily the mode currently active — tao
/// exposes no "current mode" — so it is a ceiling, not a reading.
fn refresh_rate(handle: &dioxus::desktop::tao::monitor::MonitorHandle) -> Option<f32> {
    handle
        .video_modes()
        .map(|m| m.refresh_rate())
        .max()
        .map(|hz| hz as f32)
}
