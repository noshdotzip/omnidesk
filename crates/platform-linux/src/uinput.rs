//! Input injection through `uinput`, with no portal and no permission dialog.
//!
//! The XDG RemoteDesktop portal works, but it asks. Every session, before anything can
//! be injected, someone has to be at the machine to click a dialog — which defeats the
//! point of a KVM you leave running. This is the same capability without the
//! gatekeeper: a virtual keyboard and a virtual pointer registered with the kernel,
//! indistinguishable from real hardware to everything above it.
//!
//! # This needs no elevation on a normal desktop
//! `/dev/uinput` is root-owned, but `systemd-logind` grants the active session's user
//! an ACL on it, so an ordinary desktop login can already create virtual devices.
//! Verified on the target machine: `getfacl /dev/uinput` lists `user:nosh:rw-`. No
//! sudo, no udev rule, no group change. (Capture is the opposite case — see
//! `evdev_capture`.)
//!
//! # Two devices, not one
//! A single device advertising both keyboard keys and pointer buttons makes libinput's
//! device classification ambiguous, and how it resolves that is not something to depend
//! on. Real hardware presents a keyboard and a mouse as separate devices, so this does
//! too.
//!
//! # The pointer is absolute, because relative motion is accelerated
//! libinput applies a pointer-acceleration curve to relative motion, and it applies it
//! to injected events exactly as it does to a real mouse. Measured on the target
//! machine: injecting a total of +700,+450 in 25 steps moved the cursor roughly
//! +1350,+875 — near enough to double, and non-linear, so it cannot be corrected with
//! a constant factor. A KVM whose remote cursor does not track the local one is not a
//! KVM.
//!
//! An absolute device is not accelerated: the position it reports *is* the position.
//! So the pointer advertises `ABS_X`/`ABS_Y` over a fixed normalised range rather than
//! `REL_X`/`REL_Y`, which is the same shape as a QEMU/VirtualBox tablet and is handled
//! by libinput as an absolute pointing device rather than a touchscreen (that would
//! need `INPUT_PROP_DIRECT`, which is deliberately not set).
//!
//! It also removes the dead reckoning the portal path needs. There is no call to ask
//! the compositor where the cursor is, so the portal injector has to slam the pointer
//! into a corner once to establish a known origin and then track it by accumulating
//! deltas. With absolute coordinates there is nothing to track and nothing to drift.
//! Scrolling stays relative — a wheel notch has no absolute position.
//!
//! # A new device is not usable immediately
//! Creating a uinput device is asynchronous from the compositor's point of view: udev
//! has to notice it and libinput has to add it to the seat. Events emitted in that
//! window are accepted by the kernel and dropped on the floor, which looks exactly like
//! injection silently not working. [`UinputDevices::open`] waits for the device nodes
//! to appear and then settles briefly, so the first injected event is not the one that
//! gets lost.

use serde::{Deserialize, Serialize};

/// Which button a pointer event refers to.
///
/// Mirrors the wire protocol's three buttons rather than evdev's full set: the
/// protocol carries three, and inventing a mapping for a fourth here would let the
/// peer click something it never asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PointerButton {
    Left,
    Right,
    Middle,
}

#[derive(Debug, thiserror::Error)]
pub enum UinputError {
    #[error("this build has no uinput support (not a Linux target)")]
    Unsupported,
    #[error(
        "could not open /dev/uinput: {0}. On a normal desktop login logind grants the \
         active user an ACL on it; check `getfacl /dev/uinput`."
    )]
    Open(String),
    #[error("could not create the virtual {device}: {source}")]
    Create {
        device: &'static str,
        source: std::io::Error,
    },
    #[error("could not emit an input event: {0}")]
    Emit(String),
}

/// The absolute axis range the virtual pointer advertises.
///
/// A normalised span rather than a pixel count, so the device does not have to be
/// recreated when the desktop is resized or a monitor is added. The caller scales a
/// screen position into it; the Windows side already normalises the same way for
/// `MOUSEEVENTF_ABSOLUTE`, so both platforms speak in fractions of the desktop.
pub const ABS_MAX: i32 = 32767;

/// How long to let udev and libinput notice a newly created device.
///
/// Measured behaviour, not superstition: events emitted before the compositor has the
/// device on its seat are accepted by the kernel and go nowhere.
#[cfg(target_os = "linux")]
const SETTLE: std::time::Duration = std::time::Duration::from_millis(250);

#[cfg(target_os = "linux")]
pub use imp::UinputDevices;

#[cfg(target_os = "linux")]
mod imp {
    use super::*;
    use evdev::uinput::VirtualDevice;
    use evdev::{
        AbsInfo, AbsoluteAxisCode, AttributeSet, EventType, InputEvent, KeyCode, RelativeAxisCode,
        UinputAbsSetup,
    };

    /// A virtual keyboard and pointer, alive for as long as this value is.
    ///
    /// Dropping it removes both devices from the seat, which is the release path: a
    /// crash or a kill cannot leave a phantom keyboard behind holding a key down.
    pub struct UinputDevices {
        pointer: VirtualDevice,
        keyboard: VirtualDevice,
    }

    impl UinputDevices {
        /// Create the virtual devices and wait for them to become usable.
        pub fn open() -> Result<Self, UinputError> {
            // `fuzz` and `flat` are zero: both are noise-suppression for physical
            // hardware, and a non-zero fuzz would make the kernel discard small
            // position changes — a slowly moved remote cursor would stutter or stop.
            let axis = |code| UinputAbsSetup::new(code, AbsInfo::new(0, 0, ABS_MAX, 0, 0, 0));
            let pointer = VirtualDevice::builder()
                .map_err(|e| UinputError::Open(e.to_string()))?
                .name("Ultidesk Virtual Pointer")
                .with_absolute_axis(&axis(AbsoluteAxisCode::ABS_X))
                .map_err(|e| UinputError::Create {
                    device: "pointer ABS_X",
                    source: e,
                })?
                .with_absolute_axis(&axis(AbsoluteAxisCode::ABS_Y))
                .map_err(|e| UinputError::Create {
                    device: "pointer ABS_Y",
                    source: e,
                })?
                .with_relative_axes(&AttributeSet::from_iter([
                    // Wheels only. Position is absolute; a wheel notch has no absolute
                    // position, so scrolling stays relative.
                    //
                    // Declared up front because the kernel fixes the advertised set at
                    // creation: an axis omitted here can never be emitted later, and
                    // the events are accepted and discarded.
                    RelativeAxisCode::REL_WHEEL,
                    RelativeAxisCode::REL_HWHEEL,
                ]))
                .map_err(|e| UinputError::Create {
                    device: "pointer wheels",
                    source: e,
                })?
                .with_keys(&AttributeSet::from_iter([
                    KeyCode::BTN_LEFT,
                    KeyCode::BTN_RIGHT,
                    KeyCode::BTN_MIDDLE,
                ]))
                .map_err(|e| UinputError::Create {
                    device: "pointer buttons",
                    source: e,
                })?
                .build()
                .map_err(|e| UinputError::Create {
                    device: "pointer",
                    source: e,
                })?;

            let keyboard = VirtualDevice::builder()
                .map_err(|e| UinputError::Open(e.to_string()))?
                .name("Ultidesk Virtual Keyboard")
                .with_keys(&keyboard_keys())
                .map_err(|e| UinputError::Create {
                    device: "keyboard keys",
                    source: e,
                })?
                .build()
                .map_err(|e| UinputError::Create {
                    device: "keyboard",
                    source: e,
                })?;

            let mut devices = UinputDevices { pointer, keyboard };
            devices.wait_until_ready();
            Ok(devices)
        }

        /// Block until the kernel has published both device nodes, then settle.
        ///
        /// Enumerating the nodes is the part that actually confirms creation; the sleep
        /// afterwards covers udev and libinput, which are watching asynchronously and
        /// are not done when the node appears.
        fn wait_until_ready(&mut self) {
            for device in [&mut self.pointer, &mut self.keyboard] {
                if let Ok(nodes) = device.enumerate_dev_nodes_blocking() {
                    for node in nodes.flatten() {
                        tracing::debug!(node = %node.display(), "virtual device node");
                    }
                }
            }
            std::thread::sleep(SETTLE);
        }

        /// Place the pointer at a normalised absolute position.
        ///
        /// `x` and `y` are in `0..=`[`ABS_MAX`] across the whole desktop. Use
        /// [`normalise`] to convert from screen pixels.
        pub fn pointer_position(&mut self, x: i32, y: i32) -> Result<(), UinputError> {
            // Both axes in one batch, so the compositor sees one movement to a point
            // rather than a horizontal step followed by a vertical one. Emitted
            // separately, a diagonal drag draws an L in anything tracking motion.
            let events = [
                InputEvent::new(
                    EventType::ABSOLUTE.0,
                    AbsoluteAxisCode::ABS_X.0,
                    x.clamp(0, ABS_MAX),
                ),
                InputEvent::new(
                    EventType::ABSOLUTE.0,
                    AbsoluteAxisCode::ABS_Y.0,
                    y.clamp(0, ABS_MAX),
                ),
            ];
            self.pointer.emit(&events).map_err(emit_err)
        }

        pub fn pointer_button(
            &mut self,
            button: PointerButton,
            pressed: bool,
        ) -> Result<(), UinputError> {
            let code = match button {
                PointerButton::Left => KeyCode::BTN_LEFT,
                PointerButton::Right => KeyCode::BTN_RIGHT,
                PointerButton::Middle => KeyCode::BTN_MIDDLE,
            };
            let value = i32::from(pressed);
            self.pointer
                .emit(&[InputEvent::new(EventType::KEY.0, code.0, value)])
                .map_err(emit_err)
        }

        /// Scroll by whole wheel notches. Positive `dy` scrolls up, away from the user.
        ///
        /// evdev's `REL_WHEEL` counts positive as away from the user, matching Win32
        /// and opposite to the Wayland axis convention that `ScrollAccumulator` reads.
        /// The caller converts; this is the raw axis.
        pub fn scroll(&mut self, dx: i32, dy: i32) -> Result<(), UinputError> {
            let mut events = Vec::with_capacity(2);
            if dy != 0 {
                events.push(InputEvent::new(
                    EventType::RELATIVE.0,
                    RelativeAxisCode::REL_WHEEL.0,
                    dy,
                ));
            }
            if dx != 0 {
                events.push(InputEvent::new(
                    EventType::RELATIVE.0,
                    RelativeAxisCode::REL_HWHEEL.0,
                    dx,
                ));
            }
            if events.is_empty() {
                return Ok(());
            }
            self.pointer.emit(&events).map_err(emit_err)
        }

        /// Press or release a key by evdev keycode.
        pub fn key(&mut self, keycode: u16, pressed: bool) -> Result<(), UinputError> {
            let value = i32::from(pressed);
            self.keyboard
                .emit(&[InputEvent::new(EventType::KEY.0, keycode, value)])
                .map_err(emit_err)
        }
    }

    fn emit_err(e: std::io::Error) -> UinputError {
        UinputError::Emit(e.to_string())
    }

    /// The key codes the virtual keyboard advertises.
    ///
    /// The kernel fixes this set when the device is created, so anything absent can
    /// never be injected — the write succeeds and the event is dropped. Declaring the
    /// whole main block plus the extended keys `keymap` can translate means the
    /// advertised set is exactly the set the protocol can carry.
    fn keyboard_keys() -> AttributeSet<KeyCode> {
        let mut keys = AttributeSet::<KeyCode>::new();
        // 1..=248 covers the standard keyboard block and the extended keys. Codes are
        // advertised rather than enumerated by name because the wire protocol is
        // defined by `keymap`, which speaks in numbers.
        for code in 1u16..=248 {
            keys.insert(KeyCode::new(code));
        }
        keys
    }
}

#[cfg(not(target_os = "linux"))]
#[derive(Debug)]
pub struct UinputDevices;

#[cfg(not(target_os = "linux"))]
impl UinputDevices {
    pub fn open() -> Result<Self, UinputError> {
        Err(UinputError::Unsupported)
    }
}

/// Convert a screen position into the virtual pointer's absolute range.
///
/// Kept out of the Linux-only module so it is compiled and tested everywhere: it is
/// pure arithmetic, and it is the step that silently puts the cursor in the wrong place
/// if it is wrong.
pub fn normalise(pos: i32, span: i32) -> i32 {
    if span <= 1 {
        // A zero- or one-pixel span has no interior to map into. Returning 0 rather
        // than dividing keeps a degenerate virtual screen from panicking.
        return 0;
    }
    // Scaled by `span - 1` so the last pixel maps to ABS_MAX rather than falling one
    // short: a pointer that can never reach the final column cannot cross an edge, and
    // edge crossing is the entire feature.
    let scaled = (pos as i64 * ABS_MAX as i64) / (span as i64 - 1);
    scaled.clamp(0, ABS_MAX as i64) as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_and_last_pixel_map_to_the_ends_of_the_range() {
        // The last pixel mattering is not pedantry: the pointer has to be able to reach
        // the final column to cross an edge.
        assert_eq!(normalise(0, 1920), 0);
        assert_eq!(normalise(1919, 1920), ABS_MAX);
    }

    #[test]
    fn the_middle_of_the_screen_maps_to_the_middle_of_the_range() {
        let mid = normalise(960, 1921);
        let expected = ABS_MAX / 2;
        assert!(
            (mid - expected).abs() <= 1,
            "expected about {expected}, got {mid}"
        );
    }

    #[test]
    fn positions_beyond_the_screen_are_clamped_rather_than_wrapping() {
        // A peer that reports a stale or oversized desktop must not put the cursor at a
        // wrapped-around coordinate.
        assert_eq!(normalise(5000, 1920), ABS_MAX);
        assert_eq!(normalise(-10, 1920), 0);
    }

    #[test]
    fn a_degenerate_span_does_not_divide_by_zero() {
        assert_eq!(normalise(0, 0), 0);
        assert_eq!(normalise(5, 1), 0);
    }

    #[test]
    fn normalisation_is_monotonic_across_the_screen() {
        // Any inversion would make the remote cursor jump backwards partway across.
        let mut previous = -1;
        for x in (0..1920).step_by(37) {
            let n = normalise(x, 1920);
            assert!(n >= previous, "went backwards at x={x}");
            previous = n;
        }
    }

    #[test]
    fn pointer_buttons_round_trip_over_the_wire() {
        for b in [
            PointerButton::Left,
            PointerButton::Right,
            PointerButton::Middle,
        ] {
            let json = serde_json::to_string(&b).unwrap();
            assert_eq!(serde_json::from_str::<PointerButton>(&json).unwrap(), b);
        }
    }

    #[test]
    #[cfg(not(target_os = "linux"))]
    fn non_linux_reports_unsupported() {
        assert!(matches!(
            UinputDevices::open().unwrap_err(),
            UinputError::Unsupported
        ));
    }
}
