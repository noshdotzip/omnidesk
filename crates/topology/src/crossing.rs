//! Deciding that the pointer has left this machine, and where it arrives.
//!
//! Every piece around this existed already — `Layout` knows which screens touch,
//! `map_edge_crossing` maps a position along a shared border, `RemotePointer` tracks the
//! pointer once it is over there, `KvmMachine` holds the state transitions. What was
//! missing is the join: *this* pointer, at *this* pixel, is now the peer's problem, and it
//! should appear at *that* point on *that* screen.
//!
//! # Only a crossing between machines counts
//! Two monitors on the same machine share a border too, and the operating system already
//! moves the pointer between them. Treating that as a handoff would grab the input every
//! time the operator dragged a window to their second screen. So a crossing is only a
//! crossing when the screen on the other side belongs to a different device.
//!
//! # Why the pointer's own position is not enough
//! The OS clamps the cursor to the desktop, so the pointer never actually reaches a
//! coordinate outside it — it stops at the edge and stays there however hard the mouse is
//! pushed. "Is it past the edge?" is therefore always no. What distinguishes a operator
//! pushing *through* an edge from one resting against it is the motion they asked for, so
//! the intended delta is an input here, and the platform layer that swallows the motion
//! is what can see it.
//!
//! # The arrangement decides where it lands, not a proportion
//! Leaving at y=720 arrives at y=720 in the shared plane, whatever the two screens'
//! heights are. It is tempting to scale instead — two-thirds down a 1080-tall screen
//! arriving two-thirds down a 1440-tall one — and `map_edge_crossing` does exactly that.
//! It is the wrong rule *here*, and the difference is the whole point of the editor: if
//! the operator lined the screens up top-aligned, the pointer must come out level with
//! where it went in. Scaling would drop it 240px below the arrangement they made, and
//! they would be looking straight at the discrepancy.
//!
//! `map_edge_crossing` is still right for the case it was written for — mirroring a whole
//! screen onto a whole screen, where there is no shared plane and nothing has been
//! arranged, so proportion is the only available meaning.

use crate::layout::{Layout, Side};
use crate::monitor::Monitor;

/// Where the pointer arrives after leaving this machine.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Crossing {
    /// Index into the layout of the screen the pointer enters.
    pub monitor: usize,
    /// The side of *that* screen the pointer arrives on. Leaving through a right edge
    /// arrives on a left one.
    pub entry: Side,
    /// How far along the entry edge, 0.0 at the top/left end and 1.0 at the
    /// bottom/right end.
    pub fraction: f64,
}

/// The edge a pointer is pushing against, if any.
///
/// `delta` is the motion the operator asked for, not the motion that happened — see the
/// module note on clamping. A pointer resting on an edge with no outward motion is not
/// crossing, which is what stops a cursor parked at the screen edge from handing control
/// over on its own.
pub fn edge_pressed(monitor: &Monitor, x: f64, y: f64, delta: (f64, f64)) -> Option<(Side, f64)> {
    let (dx, dy) = delta;
    // The last pixel of a screen is `right() - 1`, not `right()`: a 1920-wide monitor at
    // x=0 has its rightmost pixel at 1919. Comparing against `right()` means the
    // condition is never true and the edge never fires — the same off-by-one that makes
    // a portal barrier land outside its zone and silently never trigger.
    if dx > 0.0 && x >= monitor.right() - 1.0 {
        return Some((Side::Right, y));
    }
    if dx < 0.0 && x <= monitor.logical_x {
        return Some((Side::Left, y));
    }
    if dy > 0.0 && y >= monitor.bottom() - 1.0 {
        return Some((Side::Bottom, x));
    }
    if dy < 0.0 && y <= monitor.logical_y {
        return Some((Side::Top, x));
    }
    None
}

/// Which screen the pointer arrives on after leaving `from` through `side`.
///
/// `at` is the position along the shared border in layout coordinates — the y of a
/// left/right crossing, the x of a top/bottom one.
///
/// `None` when nothing belonging to another machine is there: the pointer stays put, and
/// the operating system carries on clamping it as it always did.
pub fn landing(layout: &Layout, from: usize, side: Side, at: f64) -> Option<Crossing> {
    let source = layout.monitors.get(from)?;

    for (index, candidate) in layout.monitors.iter().enumerate() {
        if index == from {
            continue;
        }
        // Same machine: the OS already moves the pointer across this border, and
        // grabbing input here would fire every time a window is dragged to the second
        // screen.
        if candidate.device_id == source.device_id {
            continue;
        }
        let Some(adjacency) = layout.adjacency(from, index) else {
            continue;
        };
        if adjacency.side != side {
            continue;
        }
        // The pointer has to leave through the part of the edge the neighbour actually
        // occupies. A screen touching only the bottom half of a taller one is reachable
        // from the bottom half and not the top, and pretending otherwise teleports the
        // pointer.
        if at < adjacency.span_start || at > adjacency.span_end {
            continue;
        }

        let entry = side.opposite();
        let (start, length) = match entry {
            Side::Left | Side::Right => (candidate.logical_y, candidate.logical_height),
            Side::Top | Side::Bottom => (candidate.logical_x, candidate.logical_width),
        };
        // Expressed as a fraction of the entry edge because that is what `RemotePointer`
        // takes, but derived from the absolute position: the pointer comes out level
        // with where it went in, as the arrangement says it should. See the module note.
        let fraction = if length > 0.0 {
            ((at - start) / length).clamp(0.0, 1.0)
        } else {
            0.0
        };

        return Some(Crossing {
            monitor: index,
            entry,
            fraction,
        });
    }
    None
}

/// The whole decision: is this pointer motion a handoff, and to where?
///
/// `from` is the screen the pointer is currently on. Kept separate from [`landing`] so
/// that the two halves — *am I leaving* and *where do I arrive* — can be tested and
/// reasoned about apart, since they fail for entirely different reasons.
pub fn crossing(
    layout: &Layout,
    from: usize,
    x: f64,
    y: f64,
    delta: (f64, f64),
) -> Option<Crossing> {
    let monitor = layout.monitors.get(from)?;
    let (side, at) = edge_pressed(monitor, x, y, delta)?;
    landing(layout, from, side, at)
}

/// Where the pointer lands, in the entered screen's own coordinates.
///
/// The peer needs an absolute position on one of *its* screens; the fraction is what
/// survives a difference in size between the two.
pub fn landing_point(layout: &Layout, crossing: &Crossing) -> Option<(f64, f64)> {
    let m = layout.monitors.get(crossing.monitor)?;
    Some(match crossing.entry {
        Side::Left => (
            m.logical_x,
            m.logical_y + crossing.fraction * m.logical_height,
        ),
        Side::Right => (
            m.right() - 1.0,
            m.logical_y + crossing.fraction * m.logical_height,
        ),
        Side::Top => (
            m.logical_x + crossing.fraction * m.logical_width,
            m.logical_y,
        ),
        Side::Bottom => (
            m.logical_x + crossing.fraction * m.logical_width,
            m.bottom() - 1.0,
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::monitor::{MonitorId, Rotation};
    use ultidesk_core::DeviceId;

    fn machine(seed: u8) -> DeviceId {
        DeviceId::from_uuid(uuid::Uuid::from_bytes([seed; 16]))
    }

    fn monitor(device: DeviceId, name: &str, x: f64, y: f64, w: f64, h: f64) -> Monitor {
        Monitor {
            device_id: device,
            monitor_id: MonitorId(1),
            friendly_name: name.into(),
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

    /// This machine on the left, the peer to its right, both 1920x1080.
    fn two_machines() -> Layout {
        let (a, b) = (machine(1), machine(2));
        Layout::new(vec![
            monitor(a, "local", 0.0, 0.0, 1920.0, 1080.0),
            monitor(b, "peer", 1920.0, 0.0, 1920.0, 1080.0),
        ])
    }

    #[test]
    fn pushing_right_at_the_right_edge_crosses() {
        let layout = two_machines();
        let c = crossing(&layout, 0, 1919.0, 540.0, (5.0, 0.0)).expect("should cross");
        assert_eq!(c.monitor, 1);
        assert_eq!(c.entry, Side::Left, "leaving right arrives left");
        assert!((c.fraction - 0.5).abs() < 1e-9, "half way down");
    }

    #[test]
    fn resting_on_the_edge_without_pushing_is_not_a_crossing() {
        // Otherwise a cursor parked at the screen edge hands control over on its own,
        // and the operator loses their machine to a mouse they are not touching.
        let layout = two_machines();
        assert!(crossing(&layout, 0, 1919.0, 540.0, (0.0, 0.0)).is_none());
        assert!(
            crossing(&layout, 0, 1919.0, 540.0, (-5.0, 0.0)).is_none(),
            "pushing away from the edge is not leaving through it"
        );
    }

    #[test]
    fn the_edge_is_the_last_pixel_not_the_bounding_coordinate() {
        // A 1920-wide screen at x=0 has its rightmost pixel at 1919. Comparing against
        // 1920 means the condition is never true and the edge never fires — the same
        // off-by-one that makes a portal barrier land outside its zone.
        let layout = two_machines();
        assert!(crossing(&layout, 0, 1919.0, 100.0, (1.0, 0.0)).is_some());
    }

    #[test]
    fn a_crossing_within_one_machine_is_left_to_the_operating_system() {
        // Grabbing input here would fire every time a window is dragged to the second
        // screen.
        let a = machine(1);
        let layout = Layout::new(vec![
            monitor(a, "left", 0.0, 0.0, 1920.0, 1080.0),
            monitor(a, "right", 1920.0, 0.0, 1920.0, 1080.0),
        ]);
        assert!(crossing(&layout, 0, 1919.0, 540.0, (5.0, 0.0)).is_none());
    }

    #[test]
    fn a_screen_that_only_touches_part_of_the_edge_is_only_reachable_there() {
        // The peer sits against the bottom half of a taller local screen. Leaving from
        // the top half has nowhere to go, and pretending otherwise teleports the pointer.
        let (a, b) = (machine(1), machine(2));
        let layout = Layout::new(vec![
            monitor(a, "tall", 0.0, 0.0, 1920.0, 2000.0),
            monitor(b, "peer", 1920.0, 1000.0, 1920.0, 1000.0),
        ]);
        assert!(
            crossing(&layout, 0, 1919.0, 200.0, (5.0, 0.0)).is_none(),
            "the top half touches nothing"
        );
        let c = crossing(&layout, 0, 1919.0, 1500.0, (5.0, 0.0)).expect("the bottom half does");
        assert_eq!(c.monitor, 1);
        assert!((c.fraction - 0.5).abs() < 1e-9);
    }

    #[test]
    fn the_pointer_comes_out_level_with_where_it_went_in() {
        // Screens of different heights, top-aligned by the operator. Leaving at y=720
        // arrives at y=720 — not at two-thirds of 1440, which would drop it 240px below
        // the arrangement they are looking at.
        let (a, b) = (machine(1), machine(2));
        let layout = Layout::new(vec![
            monitor(a, "local", 0.0, 0.0, 1920.0, 1080.0),
            monitor(b, "peer", 1920.0, 0.0, 2560.0, 1440.0),
        ]);
        let c = crossing(&layout, 0, 1919.0, 720.0, (5.0, 0.0)).unwrap();
        let (x, y) = landing_point(&layout, &c).unwrap();
        assert_eq!(x, 1920.0, "arrives on the peer's left edge");
        assert!((y - 720.0).abs() < 1e-9, "level, not scaled: {y}");
    }

    #[test]
    fn a_peer_the_operator_moved_down_is_entered_where_they_put_it() {
        // The arrangement is the truth. A peer dragged 300px down means the top 300px of
        // the local edge lead nowhere, and a crossing below that arrives level with the
        // pointer — 500 on the local screen is 500 in the shared plane either way.
        let (a, b) = (machine(1), machine(2));
        let layout = Layout::new(vec![
            monitor(a, "local", 0.0, 0.0, 1920.0, 1080.0),
            monitor(b, "peer", 1920.0, 300.0, 1920.0, 1080.0),
        ]);
        assert!(
            crossing(&layout, 0, 1919.0, 100.0, (5.0, 0.0)).is_none(),
            "above the peer entirely"
        );
        let c = crossing(&layout, 0, 1919.0, 500.0, (5.0, 0.0)).unwrap();
        let (_, y) = landing_point(&layout, &c).unwrap();
        assert!((y - 500.0).abs() < 1e-9, "level with the crossing: {y}");
    }

    #[test]
    fn crossing_works_in_every_direction() {
        let (a, b) = (machine(1), machine(2));
        // Peer above, below, left and right of a central local screen.
        for (dx, dy, px, py, side, entry) in [
            (5.0, 0.0, 1920.0, 0.0, Side::Right, Side::Left),
            (-5.0, 0.0, -1920.0, 0.0, Side::Left, Side::Right),
            (0.0, 5.0, 0.0, 1080.0, Side::Bottom, Side::Top),
            (0.0, -5.0, 0.0, -1080.0, Side::Top, Side::Bottom),
        ] {
            let layout = Layout::new(vec![
                monitor(a, "local", 0.0, 0.0, 1920.0, 1080.0),
                monitor(b, "peer", px, py, 1920.0, 1080.0),
            ]);
            let (x, y) = match side {
                Side::Right => (1919.0, 540.0),
                Side::Left => (0.0, 540.0),
                Side::Bottom => (960.0, 1079.0),
                Side::Top => (960.0, 0.0),
            };
            let c = crossing(&layout, 0, x, y, (dx, dy))
                .unwrap_or_else(|| panic!("expected a crossing through {side:?}"));
            assert_eq!(c.entry, entry, "through {side:?}");
            assert!((c.fraction - 0.5).abs() < 1e-9, "through {side:?}");
        }
    }

    #[test]
    fn a_gap_between_the_machines_means_no_crossing() {
        // The operator dragged them apart. There is no shared border, so there is
        // nowhere to hand the pointer to — and inventing one would send it to a screen
        // that is not where the arrangement says it is.
        let (a, b) = (machine(1), machine(2));
        let layout = Layout::new(vec![
            monitor(a, "local", 0.0, 0.0, 1920.0, 1080.0),
            monitor(b, "peer", 1921.0, 0.0, 1920.0, 1080.0),
        ]);
        assert!(crossing(&layout, 0, 1919.0, 540.0, (5.0, 0.0)).is_none());
    }

    #[test]
    fn landing_lands_inside_the_screen_not_one_pixel_past_it() {
        // Entering from the right means arriving on the *last* pixel, not on the
        // bounding coordinate, which is outside the screen and may not be injectable.
        let (a, b) = (machine(1), machine(2));
        let layout = Layout::new(vec![
            monitor(a, "local", 1920.0, 0.0, 1920.0, 1080.0),
            monitor(b, "peer", 0.0, 0.0, 1920.0, 1080.0),
        ]);
        let c = crossing(&layout, 0, 1920.0, 540.0, (-5.0, 0.0)).unwrap();
        assert_eq!(c.entry, Side::Right);
        let (x, _) = landing_point(&layout, &c).unwrap();
        assert_eq!(x, 1919.0);
    }

    #[test]
    fn an_index_that_is_not_in_the_layout_is_none_rather_than_a_panic() {
        let layout = two_machines();
        assert!(crossing(&layout, 9, 0.0, 0.0, (1.0, 0.0)).is_none());
        assert!(landing(&layout, 9, Side::Right, 0.0).is_none());
    }
}
