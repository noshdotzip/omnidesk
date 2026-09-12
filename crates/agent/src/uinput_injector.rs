//! An [`Injector`] backed by virtual input devices instead of the portal.
//!
//! Same capability as [`crate::portal_injector`], with three differences that matter:
//!
//! * **No permission dialog.** The RemoteDesktop portal asks once per session, so a KVM
//!   left running cannot resume unattended. `/dev/uinput` is granted to the active
//!   session user by logind, so this needs neither a prompt nor sudo.
//! * **Exact positioning.** The portal only offers relative motion, so the injector has
//!   to slam the pointer into a corner to establish an origin and then dead-reckon.
//!   Worse, libinput accelerates relative motion — measured on the target machine, an
//!   injected +700,+450 landed the cursor at roughly +1350,+875. The virtual pointer is
//!   an absolute device, so a position is a position: verified landing within one pixel
//!   of the request at both 300,200 and 1500,900.
//! * **No D-Bus round trip per event.** Injection is a write to a file descriptor.
//!
//! The portal path is kept. It needs no device access at all, which matters on a
//! machine where the `input` group is not available, and it is the sanctioned API.

use ultidesk_core::keymap;
use ultidesk_platform_linux::uinput::{normalise, PointerButton, UinputDevices};
use ultidesk_platform_windows::inject::{InputError, MouseButton, VirtualScreen};

use crate::ipc::{Injector, WindowDto};

/// Injects into the local desktop through virtual input devices.
pub struct UinputInjector {
    // The devices are `&mut` to emit, but `Injector` takes `&self` because the IPC
    // server shares one injector across connections. A mutex rather than a cell: the
    // agent serves each connection on its own thread.
    devices: std::sync::Mutex<UinputDevices>,
}

impl UinputInjector {
    pub fn open() -> anyhow::Result<Self> {
        Ok(UinputInjector {
            devices: std::sync::Mutex::new(UinputDevices::open()?),
        })
    }

    /// Run `f` against the devices.
    ///
    /// A poisoned lock is recovered rather than propagated: the devices are a plain
    /// file descriptor with no invariant a panic could have broken, and refusing every
    /// later injection because one unrelated handler panicked would strand the
    /// operator's input on the wrong machine.
    fn with<T>(
        &self,
        f: impl FnOnce(&mut UinputDevices) -> Result<T, ultidesk_platform_linux::uinput::UinputError>,
    ) -> Result<T, InputError> {
        let mut guard = self
            .devices
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        f(&mut guard).map_err(|e| InputError::Os(e.to_string()))
    }
}

impl Injector for UinputInjector {
    fn mouse_move(
        &self,
        screen_x: i32,
        screen_y: i32,
        vs: VirtualScreen,
    ) -> Result<(), InputError> {
        // Relative to the virtual desktop's origin, which is not necessarily 0,0 when a
        // monitor sits above or left of the primary one.
        let x = normalise(screen_x - vs.left, vs.width);
        let y = normalise(screen_y - vs.top, vs.height);
        self.with(|d| d.pointer_position(x, y))
    }

    fn mouse_button(&self, button: MouseButton, down: bool) -> Result<(), InputError> {
        let button = match button {
            MouseButton::Left => PointerButton::Left,
            MouseButton::Right => PointerButton::Right,
            MouseButton::Middle => PointerButton::Middle,
        };
        self.with(|d| d.pointer_button(button, down))
    }

    fn key(&self, scancode: u16, down: bool) -> Result<(), InputError> {
        // The wire carries PS/2 set-1; the kernel wants evdev. An unmapped scancode is
        // dropped rather than guessed, for the reason `keymap` documents: a wrong
        // keycode types something the operator never pressed.
        let Some(evdev) = keymap::scancode_to_evdev(scancode) else {
            return Err(InputError::Os(format!(
                "scancode 0x{scancode:04X} has no evdev mapping"
            )));
        };
        self.with(|d| d.key(evdev as u16, down))
    }

    fn scroll(&self, delta_x: i32, delta_y: i32) -> Result<(), InputError> {
        // Win32 units in, whole notches out, and **no sign flip** — unlike the portal
        // path. evdev's REL_WHEEL counts positive as away from the user, the same as
        // Win32; the portal's NotifyPointerAxisDiscrete follows Wayland, which counts
        // positive toward the user. Two Linux injectors, opposite conventions, and
        // nothing catches a mistake at runtime.
        let steps_y = delta_y / ultidesk_core::scroll::WHEEL_DELTA;
        let steps_x = delta_x / ultidesk_core::scroll::WHEEL_DELTA;
        self.with(|d| d.scroll(steps_x, steps_y))
    }

    fn enumerate(&self) -> Vec<WindowDto> {
        // Window enumeration is a capture concern, not an injection one, and there is
        // no Wayland equivalent that does not go through a portal. Reported as empty
        // rather than faked.
        Vec::new()
    }
}
