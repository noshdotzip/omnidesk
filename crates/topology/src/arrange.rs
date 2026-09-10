//! Where a machine's screens sit when nobody has arranged them yet.
//!
//! # The default is a strip, left to right, in the order machines connected
//! The first machine keeps the desktop its platform reports. Each machine after it is
//! translated so its left edge meets the previous machine's right edge — the whole
//! desktop moved as one piece, never monitor by monitor.
//!
//! Moving each machine as a single block is the part that matters. Within one machine the
//! virtual desktop is one coordinate space, and two monitors that sit edge to edge share
//! an exact boundary in it; nudging them independently turns that boundary into a gap or
//! an overlap, `adjacency` then reports no shared border, and the pointer can never cross
//! between them. Translating the block preserves every internal relationship exactly,
//! because a translation preserves differences.
//!
//! # Why this sidesteps the cross-machine coordinate-space problem
//! Monitor geometry is stored in whatever space the platform reports — physical pixels on
//! Windows, and on Wayland a position that may not be in the same space as the size.
//! Placing two machines by their *reported absolute positions* would mix those spaces and
//! put crossings in the wrong place. Nothing here reads an absolute position across a
//! machine boundary: the offset is computed from the previous block's measured width, so
//! only distances within one machine are ever compared, and those are self-consistent
//! whatever the platform means by them.
//!
//! Sizes are still compared across machines when the editor draws them, so a 150%-scaled
//! desktop looks larger than an unscaled one of the same physical size. That is a display
//! problem, not a correctness one, and it is left alone rather than papered over with a
//! rescale that would break the internal adjacency above.
//!
//! # This is a starting point, not the arrangement
//! It exists so the editor has something coherent to show the first time two machines
//! meet. Once the operator drags anything, the saved layout is the truth and this is not
//! consulted again — see [`crate::saved`].

use crate::layout::Layout;
use crate::monitor::Monitor;
use ultidesk_core::DeviceId;

/// One machine's screens, in the order the machine connected.
pub struct MachineMonitors {
    pub device_id: DeviceId,
    pub monitors: Vec<Monitor>,
}

/// Lay machines out side by side, left to right, in the order given.
///
/// A machine that reports no monitors contributes nothing and takes no space, rather than
/// leaving a gap the operator would have to explain to themselves.
pub fn left_to_right(machines: Vec<MachineMonitors>) -> Layout {
    let mut placed: Vec<Monitor> = Vec::new();
    let mut next_x = 0.0_f64;
    let mut first = true;

    for machine in machines {
        let Some(bounds) = block_bounds(&machine.monitors) else {
            continue;
        };
        // The first machine is left exactly where its platform put it, so a single-machine
        // desk is untouched by this function and matches what the OS shows.
        let offset = if first {
            next_x = bounds.1;
            first = false;
            0.0
        } else {
            let offset = next_x - bounds.0;
            next_x += bounds.1 - bounds.0;
            offset
        };

        for mut monitor in machine.monitors {
            monitor.logical_x += offset;
            placed.push(monitor);
        }
    }

    Layout::new(placed)
}

/// The left and right extent of one machine's monitors, or `None` if it has none.
fn block_bounds(monitors: &[Monitor]) -> Option<(f64, f64)> {
    let first = monitors.first()?;
    let mut min_x = first.logical_x;
    let mut max_x = first.right();
    for m in monitors.iter().skip(1) {
        min_x = min_x.min(m.logical_x);
        max_x = max_x.max(m.right());
    }
    Some((min_x, max_x))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::Side;
    use crate::monitor::{MonitorId, Rotation};

    fn monitor(device_id: DeviceId, name: &str, x: f64, y: f64, w: f64, h: f64) -> Monitor {
        Monitor {
            device_id,
            monitor_id: MonitorId(1),
            friendly_name: name.to_string(),
            logical_x: x,
            logical_y: y,
            logical_width: w,
            logical_height: h,
            native_pixel_width: w as u32,
            native_pixel_height: h as u32,
            scale_factor: 1.0,
            rotation: Rotation::Landscape,
            refresh_rate: None,
            primary: false,
        }
    }

    #[test]
    fn one_machine_is_left_exactly_where_its_platform_put_it() {
        // A single-machine desk must be untouched: the editor should match what the OS
        // shows, not a normalised version of it.
        let a = DeviceId::new();
        let monitors = vec![
            monitor(a, "left", -1920.0, 0.0, 1920.0, 1080.0),
            monitor(a, "right", 0.0, 0.0, 2560.0, 1440.0),
        ];
        let layout = left_to_right(vec![MachineMonitors {
            device_id: a,
            monitors: monitors.clone(),
        }]);
        assert_eq!(layout.monitors, monitors);
    }

    #[test]
    fn a_second_machine_is_placed_to_the_right_of_the_first() {
        let a = DeviceId::new();
        let b = DeviceId::new();
        let layout = left_to_right(vec![
            MachineMonitors {
                device_id: a,
                monitors: vec![monitor(a, "a1", 0.0, 0.0, 1920.0, 1080.0)],
            },
            MachineMonitors {
                device_id: b,
                monitors: vec![monitor(b, "b1", 0.0, 0.0, 2496.0, 1664.0)],
            },
        ]);
        assert_eq!(layout.monitors[0].logical_x, 0.0);
        assert_eq!(
            layout.monitors[1].logical_x, 1920.0,
            "the second machine starts where the first ends"
        );
    }

    #[test]
    fn the_two_machines_end_up_sharing_a_border() {
        // The point of the default: the pointer can cross between machines without the
        // operator arranging anything first.
        let a = DeviceId::new();
        let b = DeviceId::new();
        let layout = left_to_right(vec![
            MachineMonitors {
                device_id: a,
                monitors: vec![monitor(a, "a1", 0.0, 0.0, 1920.0, 1080.0)],
            },
            MachineMonitors {
                device_id: b,
                monitors: vec![monitor(b, "b1", 0.0, 0.0, 1920.0, 1080.0)],
            },
        ]);
        let adjacency = layout.adjacency(0, 1);
        assert!(
            matches!(&adjacency, Some(a) if a.side == Side::Right && a.span() == 1080.0),
            "expected a full-height shared right border, got {adjacency:?}"
        );
    }

    #[test]
    fn a_machines_own_screens_keep_their_exact_relationship() {
        // The failure this prevents: nudging monitors individually turns a shared
        // boundary into a gap, `adjacency` reports no border, and the pointer can never
        // cross *within* that machine.
        let a = DeviceId::new();
        let b = DeviceId::new();
        let layout = left_to_right(vec![
            MachineMonitors {
                device_id: a,
                monitors: vec![monitor(a, "a1", 0.0, 0.0, 1920.0, 1080.0)],
            },
            MachineMonitors {
                device_id: b,
                // Two screens meeting exactly at x = 1280, and one of them above.
                monitors: vec![
                    monitor(b, "b1", 0.0, 0.0, 1280.0, 1024.0),
                    monitor(b, "b2", 1280.0, -200.0, 1920.0, 1080.0),
                ],
            },
        ]);
        let b1 = &layout.monitors[1];
        let b2 = &layout.monitors[2];
        assert_eq!(
            b1.right(),
            b2.logical_x,
            "the shared boundary must survive the move"
        );
        assert_eq!(b2.logical_y, -200.0, "vertical offsets are not touched");
        assert_eq!(b2.logical_x - b1.logical_x, 1280.0);
    }

    #[test]
    fn a_machine_placed_at_negative_coordinates_still_lands_flush() {
        // Windows puts a secondary monitor at a negative x when it is to the left of the
        // primary. Offsetting by the block's *minimum* rather than by zero is what keeps
        // that machine from overlapping its neighbour.
        let a = DeviceId::new();
        let b = DeviceId::new();
        let layout = left_to_right(vec![
            MachineMonitors {
                device_id: a,
                monitors: vec![monitor(a, "a1", 0.0, 0.0, 1000.0, 1000.0)],
            },
            MachineMonitors {
                device_id: b,
                monitors: vec![
                    monitor(b, "b1", -1920.0, 0.0, 1920.0, 1080.0),
                    monitor(b, "b2", 0.0, 0.0, 1920.0, 1080.0),
                ],
            },
        ]);
        assert_eq!(
            layout.monitors[1].logical_x, 1000.0,
            "flush, not overlapping"
        );
        assert_eq!(layout.monitors[2].logical_x, 2920.0);
    }

    #[test]
    fn a_machine_with_no_screens_takes_no_space() {
        // A headless peer should not leave a gap the operator has to explain.
        let a = DeviceId::new();
        let b = DeviceId::new();
        let c = DeviceId::new();
        let layout = left_to_right(vec![
            MachineMonitors {
                device_id: a,
                monitors: vec![monitor(a, "a1", 0.0, 0.0, 1920.0, 1080.0)],
            },
            MachineMonitors {
                device_id: b,
                monitors: Vec::new(),
            },
            MachineMonitors {
                device_id: c,
                monitors: vec![monitor(c, "c1", 0.0, 0.0, 1920.0, 1080.0)],
            },
        ]);
        assert_eq!(layout.monitors.len(), 2);
        assert_eq!(layout.monitors[1].logical_x, 1920.0);
    }

    #[test]
    fn the_order_given_is_the_order_shown() {
        // "In the order devices connected" is the caller's to decide; this must not
        // reorder them by name, size, or anything else.
        let a = DeviceId::new();
        let b = DeviceId::new();
        let forward = left_to_right(vec![
            MachineMonitors {
                device_id: a,
                monitors: vec![monitor(a, "a1", 0.0, 0.0, 100.0, 100.0)],
            },
            MachineMonitors {
                device_id: b,
                monitors: vec![monitor(b, "b1", 0.0, 0.0, 200.0, 200.0)],
            },
        ]);
        assert_eq!(forward.monitors[0].friendly_name, "a1");
        assert_eq!(forward.monitors[1].logical_x, 100.0);

        let reversed = left_to_right(vec![
            MachineMonitors {
                device_id: b,
                monitors: vec![monitor(b, "b1", 0.0, 0.0, 200.0, 200.0)],
            },
            MachineMonitors {
                device_id: a,
                monitors: vec![monitor(a, "a1", 0.0, 0.0, 100.0, 100.0)],
            },
        ]);
        assert_eq!(reversed.monitors[0].friendly_name, "b1");
        assert_eq!(reversed.monitors[1].logical_x, 200.0);
    }

    #[test]
    fn no_machines_is_an_empty_layout_not_a_panic() {
        assert!(left_to_right(Vec::new()).monitors.is_empty());
    }
}
