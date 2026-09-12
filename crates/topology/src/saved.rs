//! Restoring a saved monitor arrangement onto the monitors actually present.
//!
//! The editor's whole value is that the arrangement sticks. What is saved and what is
//! detected are never guaranteed to be the same set, though: monitors get unplugged,
//! added, and re-ordered by the driver between sessions. So restoring is a *match*, not
//! an assignment, and how the match is keyed decides whether the result is right or
//! quietly scrambled.
//!
//! # Keyed by device *and* name, never by index
//! A monitor name is unique among the screens attached to **one** machine, which was
//! enough while a layout only ever held one. It stops being enough the moment a peer's
//! screens are in the same layout: `eDP-1` is a name two laptops can both report, and
//! matching on it alone would apply the local machine's saved position to the peer's
//! screen and vice versa — silently, and only on the desks where both machines happen to
//! use the same panel naming.
//!
//! # Keyed by name, never by index
//! The obvious implementation walks both lists in parallel. It is wrong the first time
//! a monitor is unplugged: every monitor after it shifts up one, and each inherits the
//! position of the one before. Nothing errors — the arrangement is simply wrong, and
//! the pointer starts crossing at edges that are not where the operator put them.
//!
//! # Saved entries are hints, never a source of monitors
//! `apply` returns exactly the monitors it was given. A saved entry naming a monitor
//! that is no longer attached is dropped, not resurrected: a ghost screen in the layout
//! would claim an edge, and the pointer would cross onto a machine that cannot show it.

use crate::layout::Layout;
use crate::monitor::Monitor;
use serde::{Deserialize, Serialize};
use ultidesk_core::DeviceId;

/// One monitor's saved position, keyed by the machine it is attached to and the name
/// that machine reports for it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SavedMonitor {
    /// Which machine the monitor belongs to.
    ///
    /// `None` in a file written before layouts could hold more than one machine. Such an
    /// entry matches by name on any device, which is what it meant when it was written —
    /// see [`SavedLayout::apply`].
    #[serde(default)]
    pub device_id: Option<DeviceId>,
    /// The platform's own name — `\\.\DISPLAY1` on Windows, `eDP-1-0x82ED` on Wayland.
    ///
    /// Opaque on purpose. It only has to be stable across sessions and unique among the
    /// monitors attached to one machine, and both platforms already provide that.
    pub name: String,
    pub x: f64,
    pub y: f64,
}

/// A saved arrangement.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SavedLayout {
    pub monitors: Vec<SavedMonitor>,
}

impl SavedLayout {
    /// Capture the current arrangement.
    pub fn from_layout(layout: &Layout) -> Self {
        SavedLayout {
            monitors: layout
                .monitors
                .iter()
                .map(|m| SavedMonitor {
                    device_id: Some(m.device_id),
                    name: m.friendly_name.clone(),
                    x: m.logical_x,
                    y: m.logical_y,
                })
                .collect(),
        }
    }

    /// Restore saved positions onto the monitors that are actually attached.
    ///
    /// A detected monitor with no saved entry keeps the position the platform reported,
    /// rather than being moved somewhere invented. That can leave it overlapping a
    /// restored neighbour, which the editor already flags — showing the operator a real
    /// conflict beats silently shuffling their arrangement.
    pub fn apply(&self, detected: Vec<Monitor>) -> Layout {
        let ambiguous = ambiguous_keys(&detected);

        let mut restored = detected;
        for monitor in &mut restored {
            // A key shared by two attached monitors cannot identify either of them.
            // Applying the saved position to both would stack them exactly on top of
            // each other, so leave both where the platform put them.
            if ambiguous
                .iter()
                .any(|(d, n)| *d == monitor.device_id && n == &monitor.friendly_name)
            {
                continue;
            }
            if let Some(saved) = self.monitors.iter().find(|s| s.matches(monitor)) {
                monitor.logical_x = saved.x;
                monitor.logical_y = saved.y;
            }
        }
        Layout::new(restored)
    }

    /// Saved entries naming a monitor that is not attached.
    ///
    /// Returned rather than silently discarded so the caller can say "2 saved displays
    /// are not connected" instead of leaving the operator wondering where their
    /// arrangement went.
    pub fn missing_from(&self, detected: &[Monitor]) -> Vec<&SavedMonitor> {
        self.monitors
            .iter()
            .filter(|s| !detected.iter().any(|m| s.matches(m)))
            .collect()
    }
}

impl SavedMonitor {
    /// Whether this entry describes `monitor`.
    ///
    /// An entry that names a device must match that device *and* the name. An entry with
    /// no device — written before layouts could hold more than one machine — matches on
    /// name alone, which is exactly what it meant when it was written. Re-saving upgrades
    /// it, so the looser match applies once and then stops.
    pub fn matches(&self, monitor: &Monitor) -> bool {
        if self.name != monitor.friendly_name {
            return false;
        }
        match self.device_id {
            Some(id) => id == monitor.device_id,
            None => true,
        }
    }
}

/// Device-and-name pairs that appear more than once among the detected monitors.
///
/// Two machines both reporting `eDP-1` is *not* ambiguous — the device tells them apart.
/// One machine reporting `eDP-1` twice is, and that is the case this finds.
fn ambiguous_keys(detected: &[Monitor]) -> Vec<(DeviceId, String)> {
    let mut seen: Vec<(DeviceId, &str)> = Vec::new();
    let mut dupes: Vec<(DeviceId, String)> = Vec::new();
    for m in detected {
        let key = (m.device_id, m.friendly_name.as_str());
        if seen.contains(&key) {
            if !dupes
                .iter()
                .any(|(d, n)| *d == m.device_id && n == &m.friendly_name)
            {
                dupes.push((m.device_id, m.friendly_name.clone()));
            }
        } else {
            seen.push(key);
        }
    }
    dupes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::monitor::{MonitorId, Rotation};
    use ultidesk_core::DeviceId;

    /// A fixed id, so monitors built by `monitor` are all on the *same* machine —
    /// which is what every test written before layouts could hold two of them assumed.
    fn this_machine() -> DeviceId {
        DeviceId::from_uuid(uuid::Uuid::from_bytes([7u8; 16]))
    }

    fn monitor(name: &str, x: f64, y: f64) -> Monitor {
        monitor_on(this_machine(), name, x, y)
    }

    fn monitor_on(device_id: DeviceId, name: &str, x: f64, y: f64) -> Monitor {
        Monitor {
            device_id,
            monitor_id: MonitorId(1),
            friendly_name: name.into(),
            logical_x: x,
            logical_y: y,
            logical_width: 1920.0,
            logical_height: 1080.0,
            native_pixel_width: 1920,
            native_pixel_height: 1080,
            scale_factor: 1.0,
            rotation: Rotation::Landscape,
            refresh_rate: None,
            primary: false,
        }
    }

    fn saved(entries: &[(&str, f64, f64)]) -> SavedLayout {
        SavedLayout {
            monitors: entries
                .iter()
                .map(|(n, x, y)| SavedMonitor {
                    device_id: Some(this_machine()),
                    name: (*n).into(),
                    x: *x,
                    y: *y,
                })
                .collect(),
        }
    }

    #[test]
    fn a_saved_position_is_restored_onto_its_monitor() {
        let s = saved(&[("DISPLAY1", 100.0, 200.0)]);
        let out = s.apply(vec![monitor("DISPLAY1", 0.0, 0.0)]);
        assert_eq!(
            (out.monitors[0].logical_x, out.monitors[0].logical_y),
            (100.0, 200.0)
        );
    }

    #[test]
    fn matching_is_by_name_so_reordering_does_not_scramble_positions() {
        // The bug this exists to prevent: matched by index, unplugging or reordering
        // gives each monitor its neighbour's position, silently.
        let s = saved(&[("A", 0.0, 0.0), ("B", 1920.0, 0.0)]);
        // Detected in the opposite order from the save.
        let out = s.apply(vec![monitor("B", 0.0, 0.0), monitor("A", 0.0, 0.0)]);
        let b = out
            .monitors
            .iter()
            .find(|m| m.friendly_name == "B")
            .unwrap();
        let a = out
            .monitors
            .iter()
            .find(|m| m.friendly_name == "A")
            .unwrap();
        assert_eq!(b.logical_x, 1920.0, "B kept its own saved position");
        assert_eq!(a.logical_x, 0.0, "A kept its own saved position");
    }

    #[test]
    fn unplugging_a_monitor_does_not_shift_the_others() {
        // The concrete version of the index bug: A is gone, so a positional match would
        // hand A's position to B.
        let s = saved(&[("A", 0.0, 0.0), ("B", 1920.0, 0.0)]);
        let out = s.apply(vec![monitor("B", 0.0, 0.0)]);
        assert_eq!(out.monitors.len(), 1);
        assert_eq!(out.monitors[0].logical_x, 1920.0);
    }

    #[test]
    fn a_saved_monitor_that_is_gone_is_not_resurrected() {
        // A ghost screen would claim an edge, and the pointer would cross onto a
        // display that does not exist.
        let s = saved(&[("A", 0.0, 0.0), ("GONE", 5000.0, 0.0)]);
        let out = s.apply(vec![monitor("A", 0.0, 0.0)]);
        assert_eq!(out.monitors.len(), 1);
        assert!(out.monitors.iter().all(|m| m.friendly_name != "GONE"));
    }

    #[test]
    fn a_missing_saved_monitor_is_reported_rather_than_silently_dropped() {
        let s = saved(&[("A", 0.0, 0.0), ("GONE", 5000.0, 0.0)]);
        let detected = vec![monitor("A", 0.0, 0.0)];
        let missing = s.missing_from(&detected);
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].name, "GONE");
    }

    #[test]
    fn a_newly_attached_monitor_keeps_the_position_the_platform_reported() {
        // Inventing a position for it would move a screen the operator never touched.
        let s = saved(&[("A", 0.0, 0.0)]);
        let out = s.apply(vec![monitor("A", 0.0, 0.0), monitor("NEW", 3000.0, 40.0)]);
        let new = out
            .monitors
            .iter()
            .find(|m| m.friendly_name == "NEW")
            .unwrap();
        assert_eq!((new.logical_x, new.logical_y), (3000.0, 40.0));
    }

    #[test]
    fn two_monitors_sharing_a_name_are_both_left_where_the_platform_put_them() {
        // A name that identifies two monitors identifies neither. Applying the saved
        // position to both would stack them exactly on top of each other.
        let s = saved(&[("SAME", 500.0, 500.0)]);
        let out = s.apply(vec![
            monitor("SAME", 0.0, 0.0),
            monitor("SAME", 1920.0, 0.0),
        ]);
        assert_eq!(out.monitors[0].logical_x, 0.0);
        assert_eq!(out.monitors[1].logical_x, 1920.0);
        assert!(
            out.overlapping_pairs().is_empty(),
            "the two must not be stacked on each other"
        );
    }

    #[test]
    fn two_machines_may_use_the_same_monitor_name_without_colliding() {
        // `eDP-1` is a name two laptops both report. Keyed by name alone, the local
        // machine's saved position would be applied to the peer's screen and vice versa —
        // silently, and only on desks where both happen to name their panels the same.
        let a = this_machine();
        let b = DeviceId::from_uuid(uuid::Uuid::from_bytes([9u8; 16]));
        let s = SavedLayout {
            monitors: vec![
                SavedMonitor {
                    device_id: Some(a),
                    name: "eDP-1".into(),
                    x: 0.0,
                    y: 0.0,
                },
                SavedMonitor {
                    device_id: Some(b),
                    name: "eDP-1".into(),
                    x: 1920.0,
                    y: 300.0,
                },
            ],
        };

        let out = s.apply(vec![
            monitor_on(a, "eDP-1", 555.0, 555.0),
            monitor_on(b, "eDP-1", 555.0, 555.0),
        ]);
        assert_eq!(
            (out.monitors[0].logical_x, out.monitors[0].logical_y),
            (0.0, 0.0)
        );
        assert_eq!(
            (out.monitors[1].logical_x, out.monitors[1].logical_y),
            (1920.0, 300.0)
        );
    }

    #[test]
    fn a_saved_entry_is_not_applied_to_a_different_machines_monitor() {
        let a = this_machine();
        let b = DeviceId::from_uuid(uuid::Uuid::from_bytes([9u8; 16]));
        let s = SavedLayout {
            monitors: vec![SavedMonitor {
                device_id: Some(a),
                name: "eDP-1".into(),
                x: 100.0,
                y: 100.0,
            }],
        };
        let out = s.apply(vec![monitor_on(b, "eDP-1", 42.0, 42.0)]);
        assert_eq!(
            (out.monitors[0].logical_x, out.monitors[0].logical_y),
            (42.0, 42.0),
            "the peer's screen keeps what the platform reported"
        );
        assert_eq!(s.missing_from(&out.monitors).len(), 1);
    }

    #[test]
    fn an_entry_from_before_devices_were_recorded_still_matches_by_name() {
        // What it meant when it was written, and the only reading that keeps a
        // single-machine arrangement working across the upgrade.
        let s = SavedLayout {
            monitors: vec![SavedMonitor {
                device_id: None,
                name: "DISPLAY1".into(),
                x: 300.0,
                y: 400.0,
            }],
        };
        let out = s.apply(vec![monitor("DISPLAY1", 0.0, 0.0)]);
        assert_eq!(
            (out.monitors[0].logical_x, out.monitors[0].logical_y),
            (300.0, 400.0)
        );

        // And re-saving upgrades it, so the looser match applies once and then stops.
        let upgraded = SavedLayout::from_layout(&out);
        assert_eq!(upgraded.monitors[0].device_id, Some(this_machine()));
    }

    #[test]
    fn two_monitors_sharing_a_name_on_two_machines_are_not_ambiguous() {
        // The device tells them apart, so unlike the same-machine case both are restored.
        let a = this_machine();
        let b = DeviceId::from_uuid(uuid::Uuid::from_bytes([9u8; 16]));
        assert!(ambiguous_keys(&[
            monitor_on(a, "SAME", 0.0, 0.0),
            monitor_on(b, "SAME", 0.0, 0.0),
        ])
        .is_empty());
        assert_eq!(
            ambiguous_keys(&[
                monitor_on(a, "SAME", 0.0, 0.0),
                monitor_on(a, "SAME", 0.0, 0.0)
            ])
            .len(),
            1,
            "one machine reporting a name twice is still ambiguous"
        );
    }

    #[test]
    fn an_empty_save_leaves_every_detected_position_untouched() {
        // First run, before anything has been saved.
        let s = SavedLayout::default();
        let out = s.apply(vec![monitor("A", 10.0, 20.0)]);
        assert_eq!(
            (out.monitors[0].logical_x, out.monitors[0].logical_y),
            (10.0, 20.0)
        );
    }

    #[test]
    fn a_round_trip_through_save_and_restore_is_the_identity() {
        let detected = vec![monitor("A", 0.0, 0.0), monitor("B", 1920.0, 120.0)];
        let layout = Layout::new(detected.clone());
        let restored = SavedLayout::from_layout(&layout).apply(detected);
        for (a, b) in layout.monitors.iter().zip(restored.monitors.iter()) {
            assert_eq!((a.logical_x, a.logical_y), (b.logical_x, b.logical_y));
        }
    }

    #[test]
    fn only_position_is_restored_not_size_or_identity() {
        // A saved file from when a monitor ran at a different resolution must not
        // impose that stale size on the panel as it is now.
        let s = saved(&[("A", 100.0, 100.0)]);
        let mut detected = monitor("A", 0.0, 0.0);
        detected.logical_width = 2560.0;
        detected.native_pixel_width = 2560;
        let out = s.apply(vec![detected]);
        assert_eq!(out.monitors[0].logical_width, 2560.0);
        assert_eq!(out.monitors[0].native_pixel_width, 2560);
    }
}
