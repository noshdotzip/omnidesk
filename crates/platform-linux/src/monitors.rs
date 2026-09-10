//! Enumerating this machine's monitors without a window, by talking to the compositor.
//!
//! # Why not the toolkit, and why not a portal
//! The control app reads monitors through `tao`, which needs a window; the agent has
//! none and must still answer a peer asking what screens this machine has. The portals
//! that *would* know — ScreenCast, InputCapture — raise a permission dialog or open a
//! session with real side effects, which is far too much to pay for a geometry question.
//!
//! `wl_output` is the compositor's own answer, needs no window and no permission: any
//! Wayland client may list outputs, because that is how every application decides where
//! to put itself.
//!
//! # `xdg_output` is what makes this comparable to Windows
//! `wl_output` reports two different things and it is easy to mix them up: `geometry`
//! carries a position in the compositor's *global* space, while `mode` carries a size in
//! **physical** pixels. On a scaled output those are not the same space, so combining
//! them produces a rectangle that exists nowhere — and the mistake is invisible on an
//! unscaled monitor, which is exactly what a single 1.0-scale test machine has.
//!
//! `xdg-output-unstable-v1` exists to resolve that: `logical_position` and
//! `logical_size` are both in the compositor's global space, the same space positions
//! come in. Those are what this stores, so a monitor's rectangle is internally
//! consistent and two monitors that sit edge to edge share an exact boundary — the
//! property every crossing depends on.
//!
//! The physical mode size is kept alongside as `native_pixel_*`, which is what it
//! actually is.
//!
//! # A compositor without `xdg_output`
//! Reported as an error rather than guessed at by combining the two mismatched spaces.
//! Every compositor this targets has implemented it for years; one that has not would
//! produce silently wrong geometry, and a clear refusal is better than that.

use serde::{Deserialize, Serialize};
use ultidesk_core::DeviceId;
use ultidesk_topology::{Monitor, MonitorId, Rotation};

/// One output as the compositor describes it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WlMonitor {
    /// `xdg_output.name` — `eDP-1`, `HDMI-A-1`. Stable across sessions and unique among
    /// attached outputs, which is what a saved arrangement keys on.
    pub name: String,
    /// A human label such as the panel's make and model, when the compositor sets one.
    pub description: String,
    /// Logical position and size, both in the compositor's global space.
    pub logical_x: i32,
    pub logical_y: i32,
    pub logical_width: i32,
    pub logical_height: i32,
    /// The current mode, in physical pixels.
    pub mode_width: i32,
    pub mode_height: i32,
    /// The output's integer buffer scale, as a factor.
    pub scale_factor: f64,
    /// Refresh rate in Hz, from the current mode. `None` if the compositor sent none.
    pub refresh_hz: Option<f32>,
}

#[derive(Debug, thiserror::Error)]
pub enum MonitorEnumError {
    #[error("this build has no Wayland support (not a Linux target)")]
    Unsupported,
    #[error("could not connect to the Wayland compositor: {0}")]
    Connect(String),
    #[error("the compositor does not implement xdg-output-unstable-v1, so logical monitor geometry cannot be read reliably")]
    NoXdgOutput,
    #[error("the compositor reported no outputs")]
    NoOutputs,
    #[error("wayland protocol error: {0}")]
    Protocol(String),
}

/// Every output the compositor reports.
pub fn enumerate() -> Result<Vec<WlMonitor>, MonitorEnumError> {
    #[cfg(target_os = "linux")]
    {
        imp::enumerate()
    }
    #[cfg(not(target_os = "linux"))]
    {
        Err(MonitorEnumError::Unsupported)
    }
}

/// The same outputs as [`enumerate`], as the shared [`Monitor`] the topology model and
/// the settings IPC both speak.
pub fn enumerate_shared(device_id: DeviceId) -> Result<Vec<Monitor>, MonitorEnumError> {
    Ok(enumerate()?
        .into_iter()
        .enumerate()
        .map(|(i, m)| to_shared(device_id, i, m))
        .collect())
}

/// Pure so the mapping is testable without a compositor.
fn to_shared(device_id: DeviceId, index: usize, m: WlMonitor) -> Monitor {
    Monitor {
        device_id,
        monitor_id: MonitorId(index as u32 + 1),
        // The name, not the description: the description is a label a compositor may
        // change or leave blank, while the name is what a saved arrangement matches on.
        friendly_name: m.name,
        logical_x: m.logical_x as f64,
        logical_y: m.logical_y as f64,
        logical_width: m.logical_width as f64,
        logical_height: m.logical_height as f64,
        // The mode, which really is physical pixels — unlike the logical size above.
        native_pixel_width: m.mode_width.max(0) as u32,
        native_pixel_height: m.mode_height.max(0) as u32,
        scale_factor: m.scale_factor,
        // The compositor reports a transform, but nothing in the layout consults it: a
        // rotated output already reports its rotated logical size above, which is what
        // the geometry uses.
        rotation: Rotation::Landscape,
        refresh_rate: m.refresh_hz,
        // Wayland has no notion of a primary output. Reporting `false` for every one is
        // honest; inventing a primary would put a label on screen the compositor never
        // said.
        primary: false,
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use super::{MonitorEnumError, WlMonitor};
    use std::collections::HashMap;
    use wayland_client::protocol::{wl_output, wl_registry};
    use wayland_client::{Connection, Dispatch, QueueHandle};
    use wayland_protocols::xdg::xdg_output::zv1::client::{
        zxdg_output_manager_v1::ZxdgOutputManagerV1,
        zxdg_output_v1::{self, ZxdgOutputV1},
    };

    /// What has been learned about one output so far.
    #[derive(Default)]
    struct Pending {
        name: Option<String>,
        description: Option<String>,
        logical_position: Option<(i32, i32)>,
        logical_size: Option<(i32, i32)>,
        mode: Option<(i32, i32)>,
        refresh_mhz: Option<i32>,
        scale: Option<i32>,
    }

    #[derive(Default)]
    struct State {
        manager: Option<ZxdgOutputManagerV1>,
        outputs: Vec<(u32, wl_output::WlOutput)>,
        /// Keyed by the `wl_output`'s protocol id, because events arrive interleaved
        /// across outputs and each has to be attributed to the right one.
        pending: HashMap<u32, Pending>,
        error: Option<String>,
    }

    pub fn enumerate() -> Result<Vec<WlMonitor>, MonitorEnumError> {
        let conn =
            Connection::connect_to_env().map_err(|e| MonitorEnumError::Connect(e.to_string()))?;
        let display = conn.display();
        let mut queue = conn.new_event_queue();
        let qh = queue.handle();
        display.get_registry(&qh, ());

        let mut state = State::default();
        // First round trip: the registry advertises every global, so after this the
        // outputs and the manager are known.
        queue
            .roundtrip(&mut state)
            .map_err(|e| MonitorEnumError::Protocol(e.to_string()))?;

        if state.outputs.is_empty() {
            return Err(MonitorEnumError::NoOutputs);
        }
        let Some(manager) = state.manager.clone() else {
            return Err(MonitorEnumError::NoXdgOutput);
        };

        // Ask for the logical geometry of each output. These are separate objects, so
        // their events cannot arrive until the requests have been flushed — which is why
        // a second round trip is needed rather than reading after the first.
        for (id, output) in state.outputs.clone() {
            manager.get_xdg_output(&output, &qh, id);
        }
        queue
            .roundtrip(&mut state)
            .map_err(|e| MonitorEnumError::Protocol(e.to_string()))?;

        if let Some(e) = state.error.take() {
            return Err(MonitorEnumError::Protocol(e));
        }

        let mut out = Vec::new();
        for (id, _) in &state.outputs {
            let Some(p) = state.pending.get(id) else {
                continue;
            };
            // An output missing its logical geometry is skipped rather than filled in
            // from the physical mode: that would be the exact space mix-up this module
            // exists to avoid.
            let (Some((lx, ly)), Some((lw, lh))) = (p.logical_position, p.logical_size) else {
                continue;
            };
            let (mw, mh) = p.mode.unwrap_or((lw, lh));
            out.push(WlMonitor {
                name: p.name.clone().unwrap_or_else(|| format!("output-{id}")),
                description: p.description.clone().unwrap_or_default(),
                logical_x: lx,
                logical_y: ly,
                logical_width: lw,
                logical_height: lh,
                mode_width: mw,
                mode_height: mh,
                scale_factor: p.scale.unwrap_or(1).max(1) as f64,
                // Wayland reports refresh in millihertz.
                refresh_hz: p.refresh_mhz.filter(|r| *r > 0).map(|r| r as f32 / 1000.0),
            });
        }

        if out.is_empty() {
            return Err(MonitorEnumError::NoXdgOutput);
        }
        Ok(out)
    }

    impl Dispatch<wl_registry::WlRegistry, ()> for State {
        fn event(
            state: &mut Self,
            registry: &wl_registry::WlRegistry,
            event: wl_registry::Event,
            _: &(),
            _: &Connection,
            qh: &QueueHandle<Self>,
        ) {
            let wl_registry::Event::Global {
                name,
                interface,
                version,
            } = event
            else {
                return;
            };
            match interface.as_str() {
                "wl_output" => {
                    // Version 2 is enough for `scale`; asking for more than the
                    // compositor offers is a protocol error, so take the lower.
                    let output =
                        registry.bind::<wl_output::WlOutput, _, _>(name, version.min(4), qh, name);
                    state.outputs.push((name, output));
                    state.pending.entry(name).or_default();
                }
                "zxdg_output_manager_v1" => {
                    state.manager = Some(registry.bind::<ZxdgOutputManagerV1, _, _>(
                        name,
                        version.min(3),
                        qh,
                        (),
                    ));
                }
                _ => {}
            }
        }
    }

    impl Dispatch<wl_output::WlOutput, u32> for State {
        fn event(
            state: &mut Self,
            _: &wl_output::WlOutput,
            event: wl_output::Event,
            id: &u32,
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            let entry = state.pending.entry(*id).or_default();
            match event {
                // Only the *current* mode matters; a compositor lists every supported one.
                wl_output::Event::Mode {
                    flags: wayland_client::WEnum::Value(flags),
                    width,
                    height,
                    refresh,
                } if flags.contains(wl_output::Mode::Current) => {
                    entry.mode = Some((width, height));
                    entry.refresh_mhz = Some(refresh);
                }
                wl_output::Event::Scale { factor } => entry.scale = Some(factor),
                _ => {}
            }
        }
    }

    impl Dispatch<ZxdgOutputManagerV1, ()> for State {
        fn event(
            _: &mut Self,
            _: &ZxdgOutputManagerV1,
            _: <ZxdgOutputManagerV1 as wayland_client::Proxy>::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            // The manager has no events; it exists only to hand out xdg_outputs.
        }
    }

    impl Dispatch<ZxdgOutputV1, u32> for State {
        fn event(
            state: &mut Self,
            _: &ZxdgOutputV1,
            event: zxdg_output_v1::Event,
            id: &u32,
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            let entry = state.pending.entry(*id).or_default();
            match event {
                zxdg_output_v1::Event::LogicalPosition { x, y } => {
                    entry.logical_position = Some((x, y))
                }
                zxdg_output_v1::Event::LogicalSize { width, height } => {
                    entry.logical_size = Some((width, height))
                }
                zxdg_output_v1::Event::Name { name } => entry.name = Some(name),
                zxdg_output_v1::Event::Description { description } => {
                    entry.description = Some(description)
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wl(
        name: &str,
        lx: i32,
        ly: i32,
        lw: i32,
        lh: i32,
        mode: (i32, i32),
        scale: f64,
    ) -> WlMonitor {
        WlMonitor {
            name: name.into(),
            description: format!("{name} panel"),
            logical_x: lx,
            logical_y: ly,
            logical_width: lw,
            logical_height: lh,
            mode_width: mode.0,
            mode_height: mode.1,
            scale_factor: scale,
            refresh_hz: Some(60.0),
        }
    }

    #[test]
    fn the_logical_rectangle_is_used_not_the_physical_mode() {
        // The mistake this module exists to avoid: a 2x output's mode is 3840x2160 while
        // its logical size is 1920x1080, and mixing them produces a rectangle that
        // exists in neither space. It is invisible on a 1.0-scale monitor, which is all
        // the test hardware here has.
        let m = to_shared(
            DeviceId::new(),
            0,
            wl("eDP-1", 0, 0, 1920, 1080, (3840, 2160), 2.0),
        );
        assert_eq!((m.logical_width, m.logical_height), (1920.0, 1080.0));
        assert_eq!((m.native_pixel_width, m.native_pixel_height), (3840, 2160));
        assert_eq!(m.scale_factor, 2.0);
    }

    #[test]
    fn two_outputs_that_touch_still_touch_after_mapping() {
        let id = DeviceId::new();
        let a = to_shared(id, 0, wl("eDP-1", 0, 0, 1920, 1080, (3840, 2160), 2.0));
        let b = to_shared(
            id,
            1,
            wl("HDMI-A-1", 1920, 0, 2560, 1440, (2560, 1440), 1.0),
        );
        assert_eq!(
            a.right(),
            b.logical_x,
            "a scaled and an unscaled output must still share their boundary"
        );
    }

    #[test]
    fn the_name_is_the_identity_and_the_description_is_not() {
        // A saved arrangement matches on the name; the description is a label the
        // compositor may change or leave blank.
        let m = to_shared(DeviceId::new(), 0, wl("HDMI-A-1", 0, 0, 1, 1, (1, 1), 1.0));
        assert_eq!(m.friendly_name, "HDMI-A-1");
    }

    #[test]
    fn no_output_is_reported_as_primary_because_wayland_has_no_such_idea() {
        let m = to_shared(DeviceId::new(), 0, wl("eDP-1", 0, 0, 1, 1, (1, 1), 1.0));
        assert!(!m.primary);
    }

    #[test]
    fn an_output_left_of_the_origin_keeps_its_negative_position() {
        let m = to_shared(
            DeviceId::new(),
            0,
            wl("DP-2", -2560, -300, 2560, 1440, (2560, 1440), 1.0),
        );
        assert_eq!((m.logical_x, m.logical_y), (-2560.0, -300.0));
    }
}
