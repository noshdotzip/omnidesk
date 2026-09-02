//! Input injection via `SendInput`, plus the pure coordinate conversion it needs.
//!
//! The absolute-coordinate math is platform-independent and unit-tested; the actual
//! `SendInput` calls are Windows-only. Every injected event carries
//! [`ULTIDESK_INJECT_MARKER`] in `dwExtraInfo` — the OS-level companion to the core
//! `LoopGuard` injection marker, so our own low-level hook can recognise and drop
//! events we synthesized (one of several layered loop defenses, never the only one).

use thiserror::Error;

/// `dwExtraInfo` tag stamped on every event we synthesize. ASCII "ULTD".
pub const ULTIDESK_INJECT_MARKER: usize = 0x554C_5444;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum InputError {
    #[error("input injection is not supported on this platform build")]
    Unsupported,
    /// The OS refused injection — typically UIPI blocking a higher-integrity
    /// (elevated) target window. We surface this instead of silently dropping input.
    #[error("input blocked by the OS (target may be elevated / higher integrity)")]
    Blocked,
    #[error("input injection failed: {0}")]
    Os(String),
}

pub type Result<T> = std::result::Result<T, InputError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

/// The virtual desktop bounding box in screen pixels (spans all monitors).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtualScreen {
    pub left: i32,
    pub top: i32,
    pub width: i32,
    pub height: i32,
}

/// Convert an absolute screen pixel to the `0..=65535` normalized space `SendInput`
/// expects with `MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK`. Pure and clamped.
pub fn to_absolute_virtual(screen_x: i32, screen_y: i32, v: VirtualScreen) -> (i32, i32) {
    let denom_x = (v.width - 1).max(1);
    let denom_y = (v.height - 1).max(1);
    let nx = ((screen_x - v.left) as i64 * 65535 / denom_x as i64).clamp(0, 65535) as i32;
    let ny = ((screen_y - v.top) as i64 * 65535 / denom_y as i64).clamp(0, 65535) as i32;
    (nx, ny)
}

// ---- Windows implementation -------------------------------------------------

#[cfg(windows)]
mod imp {
    use super::*;
    use windows::Win32::Foundation::{GetLastError, ERROR_ACCESS_DENIED};
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYBD_EVENT_FLAGS,
        KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_LEFTDOWN,
        MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE,
        MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK, MOUSEINPUT,
        MOUSE_EVENT_FLAGS, VIRTUAL_KEY,
    };

    fn send(inputs: &[INPUT]) -> Result<()> {
        let sent = unsafe { SendInput(inputs, std::mem::size_of::<INPUT>() as i32) };
        if sent as usize == inputs.len() {
            return Ok(());
        }
        // Partial/zero insertion — inspect the OS error to distinguish UIPI blocking.
        let code = unsafe { GetLastError() };
        if code == ERROR_ACCESS_DENIED {
            Err(InputError::Blocked)
        } else {
            Err(InputError::Os(format!(
                "SendInput inserted {sent}/{} events (win32 error {:?})",
                inputs.len(),
                code
            )))
        }
    }

    pub fn move_cursor_absolute(nx: i32, ny: i32) -> Result<()> {
        let mi = MOUSEINPUT {
            dx: nx,
            dy: ny,
            mouseData: 0,
            dwFlags: MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
            time: 0,
            dwExtraInfo: ULTIDESK_INJECT_MARKER,
        };
        send(&[INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 { mi },
        }])
    }

    pub fn mouse_button(button: MouseButton, down: bool) -> Result<()> {
        let flag: MOUSE_EVENT_FLAGS = match (button, down) {
            (MouseButton::Left, true) => MOUSEEVENTF_LEFTDOWN,
            (MouseButton::Left, false) => MOUSEEVENTF_LEFTUP,
            (MouseButton::Right, true) => MOUSEEVENTF_RIGHTDOWN,
            (MouseButton::Right, false) => MOUSEEVENTF_RIGHTUP,
            (MouseButton::Middle, true) => MOUSEEVENTF_MIDDLEDOWN,
            (MouseButton::Middle, false) => MOUSEEVENTF_MIDDLEUP,
        };
        let mi = MOUSEINPUT {
            dx: 0,
            dy: 0,
            mouseData: 0,
            dwFlags: flag,
            time: 0,
            dwExtraInfo: ULTIDESK_INJECT_MARKER,
        };
        send(&[INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 { mi },
        }])
    }

    /// Inject a key by hardware scan code (layout-independent identity of the key), so
    /// the protocol is not tied to ASCII. `down = false` sends the key-up.
    pub fn key_scancode(scan: u16, down: bool) -> Result<()> {
        let mut flags: KEYBD_EVENT_FLAGS = KEYEVENTF_SCANCODE;
        if !down {
            flags |= KEYEVENTF_KEYUP;
        }
        let ki = KEYBDINPUT {
            wVk: VIRTUAL_KEY(0),
            wScan: scan,
            dwFlags: flags,
            time: 0,
            dwExtraInfo: ULTIDESK_INJECT_MARKER,
        };
        send(&[INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 { ki },
        }])
    }
}

#[cfg(windows)]
pub use imp::{key_scancode, mouse_button, move_cursor_absolute};

// ---- Non-Windows fallbacks --------------------------------------------------

#[cfg(not(windows))]
pub fn move_cursor_absolute(_nx: i32, _ny: i32) -> Result<()> {
    Err(InputError::Unsupported)
}
#[cfg(not(windows))]
pub fn mouse_button(_button: MouseButton, _down: bool) -> Result<()> {
    Err(InputError::Unsupported)
}
#[cfg(not(windows))]
pub fn key_scancode(_scan: u16, _down: bool) -> Result<()> {
    Err(InputError::Unsupported)
}

#[cfg(test)]
mod tests {
    use super::*;

    const VS: VirtualScreen = VirtualScreen {
        left: 0,
        top: 0,
        width: 1920,
        height: 1080,
    };

    #[test]
    fn origin_maps_to_zero() {
        assert_eq!(to_absolute_virtual(0, 0, VS), (0, 0));
    }

    #[test]
    fn far_corner_maps_to_max() {
        let (x, y) = to_absolute_virtual(1919, 1079, VS);
        assert_eq!((x, y), (65535, 65535));
    }

    #[test]
    fn center_maps_to_about_half() {
        let (x, y) = to_absolute_virtual(960, 540, VS);
        assert!((32000..=33500).contains(&x), "x was {x}");
        assert!((32000..=33500).contains(&y), "y was {y}");
    }

    #[test]
    fn offset_virtual_origin_is_subtracted() {
        let vs = VirtualScreen {
            left: -1920,
            top: 0,
            width: 1920,
            height: 1080,
        };
        // A point at the very left of a secondary monitor placed to the left.
        assert_eq!(to_absolute_virtual(-1920, 0, vs), (0, 0));
    }

    #[test]
    fn out_of_range_is_clamped() {
        assert_eq!(to_absolute_virtual(-5000, -5000, VS), (0, 0));
        assert_eq!(to_absolute_virtual(999999, 999999, VS), (65535, 65535));
    }
}
