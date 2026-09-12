//! Reading the real input devices directly, with no portal and no permission dialog.
//!
//! The counterpart to [`crate::uinput`]: that injects without asking, this captures
//! without asking. Together they replace both halves of the portal path, which is what
//! a KVM left running at login actually needs.
//!
//! # This one does need a permission change, once
//! Unlike `/dev/uinput`, logind grants no ACL on `/dev/input/event*` — they are
//! `root:input` mode 0660, and a desktop user is not in `input` by default. One
//! `usermod -aG input <user>` and a re-login is enough; running the agent as root is
//! not necessary and is a much larger blast radius for the same capability.
//!
//! # Never capture our own virtual devices
//! The single most dangerous mistake available here. If the same machine is both a KVM
//! source and a target, its virtual pointer and keyboard are real evdev devices sitting
//! in `/dev/input` alongside the physical ones. Grabbing those feeds every injected
//! event straight back into the capture, which forwards it to the peer, which injects
//! it again. That is not a slow leak — it is an unbounded loop at input event rate,
//! and the machine becomes unusable immediately.
//!
//! `input_guard` catches echoed events at the protocol layer, but this has to not
//! create them in the first place: excluding them by name at enumeration is the cheap,
//! total fix.
//!
//! # Grabbing is what makes it a KVM rather than a logger
//! `EVIOCGRAB` takes the device exclusively, so events stop reaching the local desktop
//! while control is on the peer. Without it, moving the mouse would drive both machines
//! at once. It is also why the release path matters: a grabbed keyboard that is never
//! released leaves the operator with no local input at all.

use serde::{Deserialize, Serialize};

/// The prefix every virtual device this project creates is named with.
///
/// Enumeration excludes anything starting with it. Kept as one constant because the
/// producer ([`crate::uinput`]) and the consumer (here) agreeing is what prevents the
/// feedback loop described above.
pub const VIRTUAL_DEVICE_PREFIX: &str = "Ultidesk Virtual";

/// What a device is useful for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeviceRole {
    /// Reports keys in the main keyboard block.
    Keyboard,
    /// Reports pointer motion and buttons.
    Pointer,
}

/// A capturable input device.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CapturableDevice {
    pub path: String,
    pub name: String,
    pub role: DeviceRole,
}

#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    #[error("this build has no evdev support (not a Linux target)")]
    Unsupported,
    #[error(
        "no permission to read input devices. Add the user to the `input` group \
         (`sudo usermod -aG input $USER`) and log out and back in."
    )]
    Denied,
    #[error("could not enumerate input devices: {0}")]
    Enumerate(String),
    #[error("could not grab {device}: {source}. Another process may already hold it.")]
    Grab {
        device: String,
        source: std::io::Error,
    },
    #[error("no {0:?} device found to capture")]
    NoDevice(DeviceRole),
}

/// Whether a device name belongs to one of our own virtual devices.
///
/// Split out and tested because getting it wrong does not fail — it produces the
/// feedback loop in the module docs, at full input event rate.
pub fn is_our_virtual_device(name: &str) -> bool {
    name.starts_with(VIRTUAL_DEVICE_PREFIX)
}

/// Classify a device from the event types it advertises.
///
/// Takes plain booleans rather than an evdev handle so the rules are testable on any
/// platform. A device can satisfy both (some keyboards carry a trackpoint); keyboard
/// wins, because the keys are the part that cannot be recovered from elsewhere.
pub fn classify(has_keys: bool, has_rel_xy: bool, has_mouse_buttons: bool) -> Option<DeviceRole> {
    if has_keys {
        return Some(DeviceRole::Keyboard);
    }
    // Motion alone is not enough: a great many devices report a relative axis without
    // being a pointer. Buttons alongside it is what distinguishes a mouse.
    if has_rel_xy && has_mouse_buttons {
        return Some(DeviceRole::Pointer);
    }
    None
}

#[cfg(target_os = "linux")]
pub use imp::{enumerate, GrabbedDevices};

#[cfg(target_os = "linux")]
mod imp {
    use super::*;
    use evdev::{Device, EventType, KeyCode, RelativeAxisCode};

    /// List the devices that could be captured.
    ///
    /// Devices the user cannot open are skipped rather than failing the whole call: a
    /// machine always has some nodes an unprivileged user cannot read, and refusing to
    /// list anything because of one of them would be wrong. If *nothing* opened, that
    /// is reported as [`CaptureError::Denied`], which is the case worth acting on.
    pub fn enumerate() -> Result<Vec<CapturableDevice>, CaptureError> {
        let mut found = Vec::new();
        let mut opened_any = false;

        for (path, device) in evdev::enumerate() {
            opened_any = true;
            let name = device.name().unwrap_or("").to_string();
            if is_our_virtual_device(&name) {
                // See the module docs: capturing these is the feedback loop.
                tracing::debug!(%name, "skipping our own virtual device");
                continue;
            }

            let keys = device.supported_keys();
            let has_keys = keys.is_some_and(|k| {
                // A representative sample of the main block rather than every code: a
                // device with A, Enter and Space is a keyboard, and one with only
                // BTN_LEFT is not.
                k.contains(KeyCode::KEY_A)
                    && k.contains(KeyCode::KEY_ENTER)
                    && k.contains(KeyCode::KEY_SPACE)
            });
            let has_mouse_buttons = keys.is_some_and(|k| k.contains(KeyCode::BTN_LEFT));
            let has_rel_xy = device.supported_relative_axes().is_some_and(|a| {
                a.contains(RelativeAxisCode::REL_X) && a.contains(RelativeAxisCode::REL_Y)
            });

            if let Some(role) = classify(has_keys, has_rel_xy, has_mouse_buttons) {
                found.push(CapturableDevice {
                    path: path.to_string_lossy().into_owned(),
                    name,
                    role,
                });
            }
        }

        if !opened_any {
            return Err(CaptureError::Denied);
        }
        Ok(found)
    }

    /// Devices held exclusively for the duration of a capture.
    ///
    /// Dropping this releases every grab. That is the release path, and it is why the
    /// grabs are owned by one value rather than left to individual call sites: a panic
    /// or an early return must not leave the operator without a keyboard.
    pub struct GrabbedDevices {
        devices: Vec<(CapturableDevice, Device)>,
    }

    impl GrabbedDevices {
        /// Grab the given devices exclusively.
        ///
        /// All-or-nothing: if any grab fails, the ones already taken are released
        /// before returning. A half-grabbed state would leave the keyboard captured and
        /// the mouse local, which is worse than either.
        pub fn grab(list: &[CapturableDevice]) -> Result<Self, CaptureError> {
            let mut held: Vec<(CapturableDevice, Device)> = Vec::new();
            for entry in list {
                let mut device = Device::open(&entry.path).map_err(|e| CaptureError::Grab {
                    device: entry.name.clone(),
                    source: e,
                })?;
                if let Err(source) = device.grab() {
                    // `held` drops here, releasing everything already taken.
                    return Err(CaptureError::Grab {
                        device: entry.name.clone(),
                        source,
                    });
                }
                held.push((entry.clone(), device));
            }
            Ok(GrabbedDevices { devices: held })
        }

        pub fn devices(&self) -> impl Iterator<Item = &CapturableDevice> {
            self.devices.iter().map(|(d, _)| d)
        }

        /// Read whatever events are pending, translating them for the wire.
        ///
        /// Returns the events from every grabbed device in one batch. `SYN_REPORT` and
        /// key auto-repeat are dropped: the former is a frame marker with no content,
        /// and the latter is generated locally by the kernel — forwarding it would make
        /// the peer repeat a second time on top of its own repeat.
        pub fn poll(&mut self) -> Result<Vec<super::CapturedEvent>, CaptureError> {
            let mut out = Vec::new();
            for (entry, device) in &mut self.devices {
                let events = match device.fetch_events() {
                    Ok(e) => e,
                    // No events pending on a non-blocking read is not an error.
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
                    Err(e) => {
                        return Err(CaptureError::Grab {
                            device: entry.name.clone(),
                            source: e,
                        })
                    }
                };
                for ev in events {
                    if let Some(translated) = translate(&ev) {
                        out.push(translated);
                    }
                }
            }
            Ok(out)
        }
    }

    fn translate(ev: &evdev::InputEvent) -> Option<super::CapturedEvent> {
        use super::CapturedEvent;
        match ev.event_type() {
            EventType::RELATIVE => {
                let code = RelativeAxisCode(ev.code());
                match code {
                    RelativeAxisCode::REL_X => Some(CapturedEvent::MotionX(ev.value())),
                    RelativeAxisCode::REL_Y => Some(CapturedEvent::MotionY(ev.value())),
                    RelativeAxisCode::REL_WHEEL => Some(CapturedEvent::WheelY(ev.value())),
                    RelativeAxisCode::REL_HWHEEL => Some(CapturedEvent::WheelX(ev.value())),
                    _ => None,
                }
            }
            EventType::KEY => {
                // 0 = release, 1 = press, 2 = auto-repeat. Auto-repeat is dropped: the
                // peer's own kernel repeats a held key, so forwarding ours doubles it.
                let pressed = match ev.value() {
                    0 => false,
                    1 => true,
                    _ => return None,
                };
                Some(CapturedEvent::Key {
                    code: ev.code(),
                    pressed,
                })
            }
            _ => None,
        }
    }
}

/// One event read from a real device, before it is assembled into a wire message.
///
/// Deliberately per-axis rather than a combined motion event: evdev reports X and Y as
/// separate events within one frame, and pretending otherwise would either drop half of
/// a diagonal or invent a pairing that was not in the stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CapturedEvent {
    MotionX(i32),
    MotionY(i32),
    WheelX(i32),
    WheelY(i32),
    Key { code: u16, pressed: bool },
}

#[cfg(not(target_os = "linux"))]
pub fn enumerate() -> Result<Vec<CapturableDevice>, CaptureError> {
    Err(CaptureError::Unsupported)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn our_own_virtual_devices_are_recognised() {
        // If this ever returns false for a device `uinput` creates, the same machine
        // acting as both source and target loops injected events back into capture at
        // full event rate.
        assert!(is_our_virtual_device("Ultidesk Virtual Pointer"));
        assert!(is_our_virtual_device("Ultidesk Virtual Keyboard"));
    }

    #[test]
    fn real_hardware_is_not_mistaken_for_ours() {
        for name in [
            "AT Translated Set 2 keyboard",
            "SYNA32A4:00 06CB:CE17 Touchpad",
            "Logitech USB Receiver",
            "",
        ] {
            assert!(!is_our_virtual_device(name), "{name} was treated as ours");
        }
    }

    #[test]
    fn a_keyboard_is_classified_as_a_keyboard() {
        assert_eq!(classify(true, false, false), Some(DeviceRole::Keyboard));
    }

    #[test]
    fn a_mouse_needs_both_motion_and_buttons() {
        assert_eq!(classify(false, true, true), Some(DeviceRole::Pointer));
        // Motion without buttons is not a pointer: plenty of devices report a relative
        // axis (volume wheels, dials) and grabbing one would take it from the desktop
        // for nothing.
        assert_eq!(classify(false, true, false), None);
        // Buttons without motion is not a pointer either.
        assert_eq!(classify(false, false, true), None);
    }

    #[test]
    fn a_device_with_neither_is_ignored() {
        // Lid switches, power buttons and the PC speaker all appear in /dev/input.
        assert_eq!(classify(false, false, false), None);
    }

    #[test]
    fn a_keyboard_with_a_trackpoint_is_treated_as_a_keyboard() {
        // Both roles at once is real hardware, not a hypothetical. Keys win because
        // they are the part no other device can supply.
        assert_eq!(classify(true, true, true), Some(DeviceRole::Keyboard));
    }

    #[test]
    fn captured_events_round_trip_over_the_wire() {
        let events = [
            CapturedEvent::MotionX(-3),
            CapturedEvent::MotionY(12),
            CapturedEvent::WheelY(-1),
            CapturedEvent::WheelX(1),
            CapturedEvent::Key {
                code: 30,
                pressed: true,
            },
        ];
        for e in events {
            let json = serde_json::to_string(&e).unwrap();
            assert_eq!(serde_json::from_str::<CapturedEvent>(&json).unwrap(), e);
        }
    }
}
