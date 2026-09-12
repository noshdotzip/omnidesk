//! Linux [`Injector`] backed by the XDG RemoteDesktop portal.
//!
//! This is what makes the Arch box a usable KVM *destination*: input events arriving
//! from a peer over the wire are replayed into the local Wayland session here. It is
//! the Linux counterpart to `ipc::RealInjector`, which uses Win32 `SendInput`.
//!
//! Two impedance mismatches are resolved here, both with logic that is unit-tested in
//! `ultidesk-platform-linux`:
//!
//! - **Absolute vs relative.** The wire protocol carries absolute screen coordinates;
//!   the portal offers only relative motion without a ScreenCast stream. A
//!   [`VirtualCursor`] tracks the believed position and emits deltas.
//! - **Scancodes vs evdev.** The wire carries PS/2 set-1 scancodes (what Windows sends
//!   natively); the portal wants evdev keycodes.

use crate::ipc::{Injector, WindowDto};
use std::sync::Mutex;
use ultidesk_platform_linux::keymap::scancode_to_evdev;
use ultidesk_platform_linux::pointer::VirtualCursor;
use ultidesk_platform_linux::portal::PortalError;
use ultidesk_platform_linux::remote_desktop::{
    MouseButton as PortalButton, RemoteDesktopSession, SessionOptions,
};
use ultidesk_platform_windows::inject::{InputError, MouseButton, VirtualScreen};

/// Translate a portal failure into the platform-neutral input error.
///
/// `Denied` becomes `Blocked` rather than a generic error: it is the same class of
/// answer as Windows UIPI refusing injection into an elevated window — the OS said no,
/// and the projection layer should report that, not retry.
fn map_err(e: PortalError) -> InputError {
    match e {
        PortalError::Denied(_) => InputError::Blocked,
        PortalError::Unsupported => InputError::Unsupported,
        other => InputError::Os(other.to_string()),
    }
}

pub struct PortalInjector {
    session: RemoteDesktopSession,
    cursor: Mutex<VirtualCursor>,
}

impl PortalInjector {
    /// Open a portal session. Prompts the user unless `restore_token` is a valid grant
    /// from a previous run.
    pub fn open(restore_token: Option<String>) -> Result<Self, PortalError> {
        let session = RemoteDesktopSession::open(SessionOptions {
            devices: None,
            restore_token,
        })?;
        Ok(PortalInjector {
            session,
            cursor: Mutex::new(VirtualCursor::new()),
        })
    }

    /// The grant to persist so the next launch does not prompt.
    pub fn restore_token(&self) -> Option<&str> {
        self.session.restore_token()
    }

    pub fn close(&self) -> Result<(), PortalError> {
        self.session.close()
    }
}

impl Drop for PortalInjector {
    /// Release the portal session when the injector goes away.
    ///
    /// Leaving it open holds an input-injection grant the user can see in their
    /// session; tidying up is the difference between a tool that borrows a permission
    /// and one that quietly keeps it. Failure is logged, never panicked — unwinding out
    /// of a Drop would abort the process.
    fn drop(&mut self) {
        if let Err(e) = self.close() {
            tracing::warn!(error = %e, "could not close the RemoteDesktop session cleanly");
        }
    }
}

impl PortalInjector {
    /// Portal axis numbering: 0 vertical, 1 horizontal.
    const AXIS_VERTICAL: u32 = 0;
    const AXIS_HORIZONTAL: u32 = 1;
}

impl Injector for PortalInjector {
    fn mouse_move(
        &self,
        screen_x: i32,
        screen_y: i32,
        vs: VirtualScreen,
    ) -> Result<(), InputError> {
        let mut cursor = self
            .cursor
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        // Dead reckoning needs a known starting point and there is no portal call that
        // reports the pointer position. Slam it into the corner once; the compositor
        // clamps, so the result is a position we know.
        if cursor.position().is_none() {
            let (hx, hy) = cursor.home_delta();
            self.session.pointer_motion(hx, hy).map_err(map_err)?;
            cursor.homed((vs.left, vs.top));
        }

        let (dx, dy) = match cursor.delta_to(screen_x, screen_y) {
            Some(d) => d,
            // Unreachable given the homing above, but returning an error beats an
            // unwrap that would take the agent down mid-session.
            None => return Err(InputError::Os("pointer position unknown".into())),
        };
        self.session.pointer_motion(dx, dy).map_err(map_err)
    }

    fn mouse_button(&self, button: MouseButton, down: bool) -> Result<(), InputError> {
        let portal_button = match button {
            MouseButton::Left => PortalButton::Left,
            MouseButton::Right => PortalButton::Right,
            MouseButton::Middle => PortalButton::Middle,
        };
        self.session
            .pointer_button(portal_button, down)
            .map_err(map_err)
    }

    /// Scroll, converting from the wire's Win32 units back to portal steps.
    ///
    /// The wire carries multiples of `WHEEL_DELTA` in Win32's sign convention
    /// (positive is away from the user); the portal wants whole steps in Wayland's
    /// (positive is toward the user). So the vertical axis is divided *and* negated.
    /// Horizontal agrees on sign and is only divided.
    fn scroll(&self, delta_x: i32, delta_y: i32) -> Result<(), InputError> {
        let steps_y = -delta_y / ultidesk_core::scroll::WHEEL_DELTA;
        let steps_x = delta_x / ultidesk_core::scroll::WHEEL_DELTA;
        if steps_y != 0 {
            self.session
                .pointer_axis_discrete(Self::AXIS_VERTICAL, steps_y)
                .map_err(map_err)?;
        }
        if steps_x != 0 {
            self.session
                .pointer_axis_discrete(Self::AXIS_HORIZONTAL, steps_x)
                .map_err(map_err)?;
        }
        Ok(())
    }

    fn key(&self, scancode: u16, down: bool) -> Result<(), InputError> {
        // An unmapped scancode is dropped, never guessed: injecting the wrong keycode
        // types something the user did not press, which is worse than a missing key.
        let Some(evdev) = scancode_to_evdev(scancode) else {
            tracing::warn!(scancode, "no evdev mapping for scancode; key dropped");
            return Ok(());
        };
        self.session.key(evdev, down).map_err(map_err)
    }

    fn enumerate(&self) -> Vec<WindowDto> {
        // Wayland has no window enumeration; see ADR-0009.
        Vec::new()
    }
}
