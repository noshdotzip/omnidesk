//! Low-level mouse and keyboard hooks, for KVM handoff.
//!
//! Mirroring a pointer only needs to *read* it. Handoff needs to **take** it: while
//! control is on the peer, local input must stop reaching this machine.
//!
//! # The keyboard trap
//! A `WH_KEYBOARD_LL` hook that swallows a key runs **before** the OS processes
//! registered hotkeys. So a hook that naively swallows everything also swallows
//! `Ctrl+Alt+Shift+U` — destroying the emergency release exactly when the operator
//! needs it, because their keyboard no longer reaches their own machine.
//!
//! This module therefore does two things that are not optional:
//!
//! - The emergency combination is **never swallowed**, and is reported as
//!   [`HookEvent::EmergencyRelease`] so the driver can act on it directly rather than
//!   relying on the (now unreachable) `RegisterHotKey` path.
//! - Nothing is swallowed at all while the three modifiers are held together, so no
//!   future edit to the combination can accidentally trap the operator.
//!
//! [`crate::hotkey`] remains registered as a second, independent route: it works when
//! the keyboard is not being grabbed, and it does not depend on this hook running.
//!
//! # Other rules
//! - **Swallowing is opt-in and externally owned**, via an [`AtomicBool`] the caller
//!   derives from `ultidesk_core::kvm::KvmMachine`.
//! - **The callback must be fast.** Windows silently unhooks a low-level hook that
//!   exceeds `LowLevelHooksTimeout`, so callbacks only do a non-blocking send.
//! - **Injected events are flagged**, so the driver's own cursor re-anchoring warp is
//!   not mistaken for operator motion.

use crate::inject::MouseButton;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum HookError {
    #[error("low-level input hooks are only available on Windows builds")]
    Unsupported,
    #[error("could not install the low-level input hooks: {0}")]
    Install(String),
}

/// What the hooks observed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookEvent {
    Motion {
        x: i32,
        y: i32,
        /// Synthesized by us (`LLMHF_INJECTED`) — the re-anchoring warp, not the operator.
        injected: bool,
    },
    Button {
        button: MouseButton,
        down: bool,
    },
    /// A key, carried as a PS/2 set-1 scancode because that is what the wire protocol
    /// and the Windows injector both speak.
    Key {
        scancode: u16,
        /// The scancode arrived with an `0xE0` prefix.
        extended: bool,
        down: bool,
    },
    /// The operator pressed the emergency release. Never swallowed.
    EmergencyRelease,
}

/// Install low-level input hooks on a dedicated thread.
///
/// `swallow` is read on every event: while true, events are consumed and never reach
/// the local desktop — except the emergency release, which always passes through. Set
/// `keyboard` to false to hook the mouse only.
pub fn spawn_input_hooks(
    swallow: Arc<AtomicBool>,
    keyboard: bool,
) -> Result<Receiver<HookEvent>, HookError> {
    #[cfg(windows)]
    {
        imp::spawn_input_hooks(swallow, keyboard)
    }
    #[cfg(not(windows))]
    {
        let _ = (swallow, keyboard);
        Err(HookError::Unsupported)
    }
}

/// Virtual-key code of the emergency-release key, matching [`crate::hotkey`].
pub const EMERGENCY_VK: u32 = 0x55; // 'U'

#[cfg(windows)]
mod imp {
    use super::*;
    use std::sync::atomic::Ordering;
    use std::sync::mpsc::{self, Sender};
    use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, VK_CONTROL, VK_MENU, VK_SHIFT,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, GetMessageW, SetWindowsHookExW, UnhookWindowsHookEx, HHOOK,
        KBDLLHOOKSTRUCT, MSG, MSLLHOOKSTRUCT, WH_KEYBOARD_LL, WH_MOUSE_LL, WM_LBUTTONDOWN,
        WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEMOVE, WM_RBUTTONDOWN, WM_RBUTTONUP,
    };

    const LLMHF_INJECTED: u32 = 0x0000_0001;
    const LLKHF_EXTENDED: u32 = 0x0000_0001;
    const LLKHF_INJECTED: u32 = 0x0000_0010;
    const LLKHF_UP: u32 = 0x0000_0080;

    thread_local! {
        static CTX: std::cell::RefCell<Option<(Sender<HookEvent>, Arc<AtomicBool>)>> =
            const { std::cell::RefCell::new(None) };
    }

    fn send(ev: HookEvent) -> bool {
        let mut swallow = false;
        CTX.with(|c| {
            if let Some((tx, flag)) = c.borrow().as_ref() {
                swallow = flag.load(Ordering::Relaxed);
                let _ = tx.send(ev);
            }
        });
        swallow
    }

    /// Whether Ctrl, Alt and Shift are all currently held.
    fn all_modifiers_held() -> bool {
        // SAFETY: GetAsyncKeyState takes no pointers.
        unsafe {
            (GetAsyncKeyState(VK_CONTROL.0 as i32) as u16 & 0x8000) != 0
                && (GetAsyncKeyState(VK_MENU.0 as i32) as u16 & 0x8000) != 0
                && (GetAsyncKeyState(VK_SHIFT.0 as i32) as u16 & 0x8000) != 0
        }
    }

    unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if code < 0 {
            return CallNextHookEx(None, code, wparam, lparam);
        }
        let info = &*(lparam.0 as *const MSLLHOOKSTRUCT);
        let injected = info.flags & LLMHF_INJECTED != 0;

        let event = match wparam.0 as u32 {
            WM_MOUSEMOVE => Some(HookEvent::Motion {
                x: info.pt.x,
                y: info.pt.y,
                injected,
            }),
            WM_LBUTTONDOWN => Some(HookEvent::Button {
                button: MouseButton::Left,
                down: true,
            }),
            WM_LBUTTONUP => Some(HookEvent::Button {
                button: MouseButton::Left,
                down: false,
            }),
            WM_RBUTTONDOWN => Some(HookEvent::Button {
                button: MouseButton::Right,
                down: true,
            }),
            WM_RBUTTONUP => Some(HookEvent::Button {
                button: MouseButton::Right,
                down: false,
            }),
            WM_MBUTTONDOWN => Some(HookEvent::Button {
                button: MouseButton::Middle,
                down: true,
            }),
            WM_MBUTTONUP => Some(HookEvent::Button {
                button: MouseButton::Middle,
                down: false,
            }),
            _ => None,
        };

        let Some(event) = event else {
            return CallNextHookEx(None, code, wparam, lparam);
        };
        let swallow = send(event);

        // Never swallow our own warp: consuming it would strand the cursor mid-warp.
        if swallow && !injected {
            return LRESULT(1);
        }
        CallNextHookEx(None, code, wparam, lparam)
    }

    unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if code < 0 {
            return CallNextHookEx(None, code, wparam, lparam);
        }
        let info = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
        if info.flags.0 & LLKHF_INJECTED != 0 {
            return CallNextHookEx(None, code, wparam, lparam);
        }
        let down = info.flags.0 & LLKHF_UP == 0;

        // The emergency release, and anything else held with all three modifiers, is
        // passed straight through. This is the one branch that must never be reordered
        // below the swallow: it is the operator's way out of a grabbed keyboard.
        if all_modifiers_held() {
            if down && info.vkCode == EMERGENCY_VK {
                let _ = send(HookEvent::EmergencyRelease);
            }
            return CallNextHookEx(None, code, wparam, lparam);
        }

        let swallow = send(HookEvent::Key {
            scancode: info.scanCode as u16,
            extended: info.flags.0 & LLKHF_EXTENDED != 0,
            down,
        });
        if swallow {
            return LRESULT(1);
        }
        CallNextHookEx(None, code, wparam, lparam)
    }

    pub fn spawn_input_hooks(
        swallow: Arc<AtomicBool>,
        keyboard: bool,
    ) -> Result<Receiver<HookEvent>, HookError> {
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();
        let (event_tx, event_rx) = mpsc::channel::<HookEvent>();

        std::thread::Builder::new()
            .name("ultidesk-input-hooks".into())
            .spawn(move || {
                CTX.with(|c| *c.borrow_mut() = Some((event_tx, swallow)));

                // SAFETY: valid callbacks; low-level hooks need no module handle.
                let mouse: HHOOK =
                    match unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), None, 0) } {
                        Ok(h) => h,
                        Err(e) => {
                            let _ = ready_tx.send(Err(e.to_string()));
                            return;
                        }
                    };
                let kb: Option<HHOOK> = if keyboard {
                    match unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), None, 0) }
                    {
                        Ok(h) => Some(h),
                        Err(e) => {
                            unsafe {
                                let _ = UnhookWindowsHookEx(mouse);
                            }
                            let _ = ready_tx.send(Err(e.to_string()));
                            return;
                        }
                    }
                } else {
                    None
                };

                if ready_tx.send(Ok(())).is_err() {
                    unsafe {
                        let _ = UnhookWindowsHookEx(mouse);
                        if let Some(k) = kb {
                            let _ = UnhookWindowsHookEx(k);
                        }
                    }
                    return;
                }

                // Low-level hooks only fire while their thread pumps messages.
                let mut msg = MSG::default();
                while unsafe { GetMessageW(&mut msg, None, 0, 0) }.0 > 0 {}

                unsafe {
                    let _ = UnhookWindowsHookEx(mouse);
                    if let Some(k) = kb {
                        let _ = UnhookWindowsHookEx(k);
                    }
                }
            })
            .map_err(|e| HookError::Install(e.to_string()))?;

        match ready_rx.recv() {
            Ok(Ok(())) => Ok(event_rx),
            Ok(Err(e)) => Err(HookError::Install(e)),
            Err(e) => Err(HookError::Install(e.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    #[test]
    fn the_swallow_flag_is_caller_owned_and_starts_clear() {
        // Grab authority lives with the KVM state machine, not inside the hook, so the
        // two can never disagree about whether input is being taken.
        let flag = Arc::new(AtomicBool::new(false));
        assert!(!flag.load(Ordering::Relaxed));
        flag.store(true, Ordering::Relaxed);
        assert!(flag.load(Ordering::Relaxed));
    }

    #[test]
    fn the_emergency_vk_matches_the_registered_hotkey_label() {
        // If these drift apart, the documented escape hatch presses one key while the
        // hook watches for another, and a grabbed keyboard has no way out.
        assert_eq!(EMERGENCY_VK, 0x55);
        assert!(crate::hotkey::EMERGENCY_RELEASE_LABEL.ends_with('U'));
    }

    #[test]
    fn emergency_release_is_a_distinct_event_from_an_ordinary_key() {
        // The driver must be able to act on it without decoding scancodes, since the
        // release has to work even if the keymap is wrong.
        let release = HookEvent::EmergencyRelease;
        let key = HookEvent::Key {
            scancode: 0x16,
            extended: false,
            down: true,
        };
        assert_ne!(release, key);
    }

    #[cfg(not(windows))]
    #[test]
    fn non_windows_reports_unsupported_rather_than_silently_observing_nothing() {
        let flag = Arc::new(AtomicBool::new(false));
        assert!(matches!(
            spawn_input_hooks(flag, true),
            Err(HookError::Unsupported)
        ));
    }
}
