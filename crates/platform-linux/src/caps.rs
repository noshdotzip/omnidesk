//! Portal capability bitmasks.
//!
//! The XDG desktop portals advertise what they can do as plain `u32` bitmasks
//! (`AvailableSourceTypes`, `AvailableDeviceTypes`, `SupportedCapabilities`). Decoding
//! them is pure arithmetic with no D-Bus involved, so it lives here and is unit-tested
//! on every platform — including Windows, where the rest of this crate is inert.

use serde::{Deserialize, Serialize};

/// What a ScreenCast portal is willing to capture.
///
/// Bit values are fixed by the portal specification, not by us.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SourceTypes {
    pub monitor: bool,
    pub window: bool,
    pub virtual_display: bool,
}

impl SourceTypes {
    pub const MONITOR: u32 = 1;
    pub const WINDOW: u32 = 2;
    pub const VIRTUAL: u32 = 4;

    pub fn from_bits(bits: u32) -> Self {
        Self {
            monitor: bits & Self::MONITOR != 0,
            window: bits & Self::WINDOW != 0,
            virtual_display: bits & Self::VIRTUAL != 0,
        }
    }

    /// Whether this portal can capture a single window rather than a whole monitor.
    ///
    /// This is the capability Ultidesk's whole projection model depends on: without it
    /// a "projected window" would really be a projected screen, which is the thing the
    /// project explicitly is not (see README).
    pub fn supports_window_capture(&self) -> bool {
        self.window
    }
}

/// Input device classes a RemoteDesktop or InputCapture portal handles.
///
/// The two portals use the same bit layout, so one type serves both. They are not the
/// same capability though: RemoteDesktop *injects* events into this machine,
/// InputCapture *steals* them from it at a screen edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct DeviceTypes {
    pub keyboard: bool,
    pub pointer: bool,
    pub touchscreen: bool,
}

impl DeviceTypes {
    pub const KEYBOARD: u32 = 1;
    pub const POINTER: u32 = 2;
    pub const TOUCHSCREEN: u32 = 4;

    pub fn from_bits(bits: u32) -> Self {
        Self {
            keyboard: bits & Self::KEYBOARD != 0,
            pointer: bits & Self::POINTER != 0,
            touchscreen: bits & Self::TOUCHSCREEN != 0,
        }
    }

    /// A KVM link is only useful if both halves of a keyboard+mouse are available.
    pub fn supports_keyboard_and_pointer(&self) -> bool {
        self.keyboard && self.pointer
    }

    pub fn to_bits(self) -> u32 {
        let mut bits = 0;
        if self.keyboard {
            bits |= Self::KEYBOARD;
        }
        if self.pointer {
            bits |= Self::POINTER;
        }
        if self.touchscreen {
            bits |= Self::TOUCHSCREEN;
        }
        bits
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_types_decode_each_bit() {
        assert_eq!(
            SourceTypes::from_bits(1),
            SourceTypes {
                monitor: true,
                window: false,
                virtual_display: false
            }
        );
        assert_eq!(
            SourceTypes::from_bits(2),
            SourceTypes {
                monitor: false,
                window: true,
                virtual_display: false
            }
        );
        assert_eq!(
            SourceTypes::from_bits(4),
            SourceTypes {
                monitor: false,
                window: false,
                virtual_display: true
            }
        );
    }

    #[test]
    fn source_types_decode_combined_mask() {
        // 7 is what KDE Plasma 6.7 advertises: monitor + window + virtual.
        let all = SourceTypes::from_bits(7);
        assert!(all.monitor && all.window && all.virtual_display);
        assert!(all.supports_window_capture());
    }

    #[test]
    fn monitor_only_portal_cannot_do_window_projection() {
        // A compositor offering monitor capture alone cannot support the projection
        // model; callers must degrade rather than silently share a whole screen.
        let monitor_only = SourceTypes::from_bits(SourceTypes::MONITOR);
        assert!(!monitor_only.supports_window_capture());
    }

    #[test]
    fn unknown_high_bits_are_ignored_not_misread() {
        // Future portal versions may add bits; they must not corrupt known ones.
        let future = SourceTypes::from_bits(0xFFFF_FFF8 | SourceTypes::WINDOW);
        assert!(future.window);
        assert!(!future.monitor);
    }

    #[test]
    fn device_types_decode_and_roundtrip() {
        let all = DeviceTypes::from_bits(7);
        assert!(all.keyboard && all.pointer && all.touchscreen);
        assert!(all.supports_keyboard_and_pointer());
        assert_eq!(all.to_bits(), 7);
    }

    #[test]
    fn pointer_without_keyboard_is_not_a_kvm() {
        let pointer_only = DeviceTypes::from_bits(DeviceTypes::POINTER);
        assert!(!pointer_only.supports_keyboard_and_pointer());
        assert_eq!(pointer_only.to_bits(), DeviceTypes::POINTER);
    }

    #[test]
    fn empty_mask_supports_nothing() {
        assert_eq!(DeviceTypes::from_bits(0), DeviceTypes::default());
        assert_eq!(SourceTypes::from_bits(0), SourceTypes::default());
    }
}
