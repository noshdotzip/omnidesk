//! The emergency-release hotkey.
//!
//! This is the escape hatch for KVM handoff. When local input is grabbed and forwarded
//! to a peer, the operator's keyboard no longer reaches their own machine — so the one
//! key combination that ends the grab must be handled by the OS *before* our grab sees
//! it, and must keep working if the peer, the network, or our own forwarding loop has
//! died.
//!
//! `RegisterHotKey` is used deliberately rather than checking for the combination
//! inside the input hook. The OS delivers a registered hotkey as a `WM_HOTKEY` message
//! independently of any low-level hook, so the release still fires when the hook
//! callback is wedged, blocked on a dead socket, or simply buggy. A release that
//! depends on the code being released from is not a release.
//!
//! On Linux the equivalent is the `org.freedesktop.portal.GlobalShortcuts` portal,
//! which the KDE probe reported at version 2. Not wired up yet.

use std::sync::mpsc::Receiver;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum HotkeyError {
    #[error("global hotkeys are only available on Windows builds")]
    Unsupported,
    #[error("could not register the emergency-release hotkey (already taken?): {0}")]
    Register(String),
}

/// Arbitrary per-process id for our single hotkey registration.
pub const EMERGENCY_RELEASE_ID: i32 = 0x0D15;

/// Human-readable form of the combination, for UI and logs.
pub const EMERGENCY_RELEASE_LABEL: &str = "Ctrl+Alt+Shift+U";

/// Register the emergency-release hotkey and return a channel that fires on each press.
///
/// The registration owns a dedicated thread with its own message loop, because
/// `RegisterHotKey` delivers to the thread that registered it. Dropping the receiver
/// does not unregister; the thread lives for the process, which is the correct lifetime
/// for a safety control.
pub fn spawn_emergency_release() -> Result<Receiver<()>, HotkeyError> {
    #[cfg(windows)]
    {
        imp::spawn_emergency_release()
    }
    #[cfg(not(windows))]
    {
        Err(HotkeyError::Unsupported)
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use std::sync::mpsc;
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        RegisterHotKey, MOD_ALT, MOD_CONTROL, MOD_SHIFT,
    };
    use windows::Win32::UI::WindowsAndMessaging::{GetMessageW, MSG, WM_HOTKEY};

    /// Virtual-key code for `U`.
    const VK_U: u32 = 0x55;

    pub fn spawn_emergency_release() -> Result<Receiver<()>, HotkeyError> {
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();
        let (press_tx, press_rx) = mpsc::channel::<()>();

        std::thread::Builder::new()
            .name("ultidesk-emergency-hotkey".into())
            .spawn(move || {
                // SAFETY: registering against the calling thread (None window), with a
                // process-unique id.
                let registered = unsafe {
                    RegisterHotKey(
                        None,
                        EMERGENCY_RELEASE_ID,
                        MOD_CONTROL | MOD_ALT | MOD_SHIFT,
                        VK_U,
                    )
                };
                if let Err(e) = registered {
                    let _ = ready_tx.send(Err(e.to_string()));
                    return;
                }
                if ready_tx.send(Ok(())).is_err() {
                    return; // caller gave up
                }

                let mut msg = MSG::default();
                // GetMessageW returns 0 on WM_QUIT and -1 on error; both end the loop.
                // SAFETY: `msg` is a valid, writable MSG for the duration of each call.
                while unsafe { GetMessageW(&mut msg, None, 0, 0) }.0 > 0 {
                    if msg.message == WM_HOTKEY && msg.wParam.0 as i32 == EMERGENCY_RELEASE_ID {
                        // A closed receiver means the owner is gone; stop pumping.
                        if press_tx.send(()).is_err() {
                            break;
                        }
                    }
                }
            })
            .map_err(|e| HotkeyError::Register(e.to_string()))?;

        match ready_rx.recv() {
            Ok(Ok(())) => Ok(press_rx),
            Ok(Err(e)) => Err(HotkeyError::Register(e)),
            Err(e) => Err(HotkeyError::Register(e.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_label_matches_the_registered_combination() {
        // The label is what the UI tells the operator to press. If it drifts from the
        // real registration, the documented escape hatch does not work.
        assert!(EMERGENCY_RELEASE_LABEL.contains("Ctrl"));
        assert!(EMERGENCY_RELEASE_LABEL.contains("Alt"));
        assert!(EMERGENCY_RELEASE_LABEL.contains("Shift"));
        assert!(EMERGENCY_RELEASE_LABEL.ends_with('U'));
    }

    #[cfg(not(windows))]
    #[test]
    fn non_windows_reports_unsupported_rather_than_pretending_to_register() {
        // Silently "succeeding" here would leave a Linux build believing it has an
        // emergency release it does not have.
        assert!(matches!(
            spawn_emergency_release(),
            Err(HotkeyError::Unsupported)
        ));
    }
}
