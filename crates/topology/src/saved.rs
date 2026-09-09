//! Restoring a saved monitor arrangement onto the monitors actually present.
//!
//! The editor's whole value is that the arrangement sticks. What is saved and what is
//! detected are never guaranteed to be the same set, though: monitors get unplugged,
//! added, and re-ordered by the driver between sessions. So restoring is a *match*, not
//! an assignment, and how the match is keyed decides whether the result is right or
//! quietly scrambled.
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

/// One monitor's saved position, keyed by the name the platform reports for it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SavedMonitor {
    /// The platform's own name — `\\.\DISPLAY1` on Windows, `eDP-1-0x82ED` on Wayland.
    ///
    /// Opaque on purpose. It only has to be stable across sessions and unique among
    /// attached monitors, and both platforms already provide that.
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
        let ambiguous = duplicate_names(&detected);

        let mut restored = detected;
        for monitor in &mut restored {
            // A name shared by two attached monitors cannot identify either of them.
            // Applying the saved position to both would stack them exactly on top of
            // each other, so leave both where the platform put them.
            if ambiguous.iter().any(|n| n == &monitor.friendly_name) {
                continue;
            }
            if let Some(saved) = self
                .monitors
                .iter()
                .find(|s| s.name == monitor.friendly_name)
            {
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
            .filter(|s| !detected.iter().any(|m| m.friendly_name == s.name))
            .collect()
    }
}

/// Names that appear more than once among the detected monitors.
fn duplicate_names(detected: &[Monitor]) -> Vec<String> {
    let mut seen: Vec<&str> = Vec::new();
    let mut dupes: Vec<String> = Vec::new();
    for m in detected {
        let name = m.friendly_name.as_str();
        if seen.contains(&name) {
            if !dupes.iter().any(|d| d == name) {
                dupes.push(name.to_string());
            }
        } else {
            seen.push(name);
        }
    }
    dupes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::monitor::{MonitorId, Rotation};
    use ultidesk_core::DeviceId;

    fn monitor(name: &str, x: f64, y: f64) -> Monitor {
        Monitor {
            device_id: DeviceId::new(),
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
