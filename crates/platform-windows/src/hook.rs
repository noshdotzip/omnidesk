//! Low-level mouse hook, for KVM handoff.
//!
//! Mirroring a pointer only needs to *read* it. Handoff needs to **take** it: while
//! control is on the peer, local motion must stop reaching this machine, or the two
//! cursors move together and the local desktop reacts to input meant for the remote
//! one. `WH_MOUSE_LL` is the documented way to do that.
//!
//! # Safety rules this module follows
//!
//! - **Swallowing is opt-in and externally owned.** The hook consults an
//!   [`AtomicBool`] the caller controls, so the authority to grab lives with the KVM
//!   state machine (`ultidesk_core::kvm`) and cannot drift out of sync with it.
//! - **Default is observe-only.** A freshly installed hook swallows nothing.
//! - **The callback must be fast.** Windows silently unhooks a low-level hook that
//!   exceeds `LowLevelHooksTimeout`, so the callback only does a non-blocking send and
//!   never waits on a socket, a lock held elsewhere, or an allocation-heavy path.
//! - **Keyboard is not hooked here.** Swallowing the keyboard also swallows the
//!   operator's way out of a wedged state; the mouse alone is the safer first step, and
//!   the emergency hotkey (see [`crate::hotkey`]) is delivered by the OS regardless.

use std::sync::atomic::AtomicBool;
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum HookError {
    #[error("low-level input hooks are only available on Windows builds")]
    Unsupported,
    #[error("could not install the low-level mouse hook: {0}")]
    Install(String),
}

/// A pointer event observed by the hook, in virtual-desktop coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HookedMouse {
    pub x: i32,
    pub y: i32,
    /// Whether this event was swallowed rather than passed to the rest of the system.
    pub swallowed: bool,
    /// Whether the OS marked this event as synthesized (`LLMHF_INJECTED`).
    ///
    /// Handoff re-anchors the cursor with `SetCursorPos` after every swallowed move, and
    /// that warp comes straight back through this hook. Without this flag the warp would
    /// be read as real motion and fed back into the delta, producing a runaway loop.
    pub injected: bool,
}

/// Install a low-level mouse hook on a dedicated thread.
///
/// `swallow` is read on every event: while it is `true` the event is consumed and never
/// reaches the local desktop. The caller owns that flag and is responsible for clearing
/// it — see `ultidesk_core::kvm::KvmMachine::grab_active`.
pub fn spawn_mouse_hook(swallow: Arc<AtomicBool>) -> Result<Receiver<HookedMouse>, HookError> {
    #[cfg(windows)]
    {
        imp::spawn_mouse_hook(swallow)
    }
    #[cfg(not(windows))]
    {
        let _ = swallow;
        Err(HookError::Unsupported)
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use std::sync::atomic::Ordering;
    use std::sync::mpsc::{self, Sender};
    use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, GetMessageW, SetWindowsHookExW, UnhookWindowsHookEx, HHOOK, MSG,
        MSLLHOOKSTRUCT, WH_MOUSE_LL, WM_MOUSEMOVE,
    };

    // The hook callback is a plain `extern "system" fn` with no user pointer, so its
    // context has to be thread-local. It lives on the hook thread only, which is also
    // the only thread the callback ever runs on.
    thread_local! {
        static CTX: std::cell::RefCell<Option<(Sender<HookedMouse>, Arc<AtomicBool>)>> =
            const { std::cell::RefCell::new(None) };
    }

    unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        // Negative code means "pass it on without inspecting", per the Win32 contract.
        if code < 0 {
            return CallNextHookEx(None, code, wparam, lparam);
        }

        let mut swallow_this = false;
        if wparam.0 as u32 == WM_MOUSEMOVE {
            let info = &*(lparam.0 as *const MSLLHOOKSTRUCT);
            let x = info.pt.x;
            let y = info.pt.y;
            const LLMHF_INJECTED: u32 = 0x0000_0001;
            let injected = info.flags & LLMHF_INJECTED != 0;
            CTX.with(|c| {
                if let Some((tx, swallow)) = c.borrow().as_ref() {
                    // Never swallow our own re-anchoring warp: consuming it would leave
                    // the cursor wherever the warp was heading and break the anchor.
                    swallow_this = !injected && swallow.load(Ordering::Relaxed);
                    // Non-blocking: a wedged consumer must never stall the hook, or
                    // Windows unhooks us for exceeding LowLevelHooksTimeout.
                    let _ = tx.send(HookedMouse {
                        x,
                        y,
                        swallowed: swallow_this,
                        injected,
                    });
                }
            });
        }

        if swallow_this {
            // Non-zero consumes the event: it never reaches the local desktop.
            return LRESULT(1);
        }
        CallNextHookEx(None, code, wparam, lparam)
    }

    pub fn spawn_mouse_hook(swallow: Arc<AtomicBool>) -> Result<Receiver<HookedMouse>, HookError> {
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();
        let (event_tx, event_rx) = mpsc::channel::<HookedMouse>();

        std::thread::Builder::new()
            .name("ultidesk-mouse-hook".into())
            .spawn(move || {
                CTX.with(|c| *c.borrow_mut() = Some((event_tx, swallow)));

                // SAFETY: a valid callback, no module handle needed for a low-level
                // hook, and thread id 0 installs it globally for this thread's queue.
                let hook: HHOOK =
                    match unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), None, 0) } {
                        Ok(h) => h,
                        Err(e) => {
                            let _ = ready_tx.send(Err(e.to_string()));
                            return;
                        }
                    };
                if ready_tx.send(Ok(())).is_err() {
                    // SAFETY: `hook` came from a successful SetWindowsHookExW.
                    unsafe {
                        let _ = UnhookWindowsHookEx(hook);
                    };
                    return;
                }

                // A low-level hook only fires while its thread pumps messages.
                let mut msg = MSG::default();
                while unsafe { GetMessageW(&mut msg, None, 0, 0) }.0 > 0 {}

                // SAFETY: `hook` is still the handle we installed.
                unsafe {
                    let _ = UnhookWindowsHookEx(hook);
                };
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
        // The grab authority must live with the KVM state machine, not inside the hook,
        // so the two can never disagree about whether input is being taken.
        let flag = Arc::new(AtomicBool::new(false));
        assert!(!flag.load(Ordering::Relaxed));
        flag.store(true, Ordering::Relaxed);
        assert!(flag.load(Ordering::Relaxed));
    }

    #[cfg(not(windows))]
    #[test]
    fn non_windows_reports_unsupported_rather_than_silently_observing_nothing() {
        let flag = Arc::new(AtomicBool::new(false));
        assert!(matches!(
            spawn_mouse_hook(flag),
            Err(HookError::Unsupported)
        ));
    }
}
