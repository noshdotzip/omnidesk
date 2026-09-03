//! Local IPC message types and the platform-independent request dispatcher.
//!
//! Transport (Windows named pipe) lives in `pipe.rs`. Keeping dispatch here — behind
//! an [`Injector`] trait — means the safety-critical logic (auth gating, and releasing
//! all held keys/buttons when a session ends) is unit-tested with a mock injector,
//! without needing a pipe, a GUI, or real input.

// Transport-gated, not dead: the only IPC transport that exists today is the Windows
// named pipe (`#[cfg(windows)] mod pipe`), so on other platforms nothing constructs
// this module's types and every item reads as dead code under `-D warnings`. The
// logic is deliberately platform-independent and stays compiled and unit-tested
// everywhere, ready for the Linux transport (Milestone 9, docs/status.md).
#![cfg_attr(not(windows), allow(dead_code))]

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use ultidesk_core::protocol::PROTOCOL_VERSION;
use ultidesk_platform_windows::inject::{InputError, MouseButton, VirtualScreen};

/// Requests the desktop app sends to the agent over local IPC.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum IpcRequest {
    /// Must be the first message; presents the per-launch auth token.
    Hello {
        token: String,
        protocol_version: u32,
    },
    Ping,
    EnumerateWindows,
    InjectMouseMove {
        screen_x: i32,
        screen_y: i32,
        virtual_screen: VirtualScreenDto,
    },
    InjectMouseButton {
        button: MouseButtonDto,
        down: bool,
    },
    InjectKey {
        scancode: u16,
        down: bool,
    },
    /// Release every key/button this session is currently holding. Idempotent.
    ReleaseAllInput,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum IpcResponse {
    HelloOk {
        agent_version: String,
        protocol_version: u32,
    },
    Pong,
    Windows {
        windows: Vec<WindowDto>,
    },
    Injected,
    Released {
        count: usize,
    },
    Error {
        code: String,
        message: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct VirtualScreenDto {
    pub left: i32,
    pub top: i32,
    pub width: i32,
    pub height: i32,
}

impl From<VirtualScreenDto> for VirtualScreen {
    fn from(d: VirtualScreenDto) -> Self {
        VirtualScreen {
            left: d.left,
            top: d.top,
            width: d.width,
            height: d.height,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MouseButtonDto {
    Left,
    Right,
    Middle,
}

impl From<MouseButtonDto> for MouseButton {
    fn from(b: MouseButtonDto) -> Self {
        match b {
            MouseButtonDto::Left => MouseButton::Left,
            MouseButtonDto::Right => MouseButton::Right,
            MouseButtonDto::Middle => MouseButton::Middle,
        }
    }
}

/// Window info as sent to the desktop app. Titles are display-only, never logged.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WindowDto {
    pub hwnd: i64,
    pub title: String,
    pub process_id: u32,
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

/// Abstraction over input injection so the dispatcher is testable off-platform.
pub trait Injector {
    fn mouse_move(&self, screen_x: i32, screen_y: i32, vs: VirtualScreen)
        -> Result<(), InputError>;
    fn mouse_button(&self, button: MouseButton, down: bool) -> Result<(), InputError>;
    fn key(&self, scancode: u16, down: bool) -> Result<(), InputError>;
    fn enumerate(&self) -> Vec<WindowDto>;
}

/// Real injector backed by `ultidesk-platform-windows`.
pub struct RealInjector;

impl Injector for RealInjector {
    fn mouse_move(&self, sx: i32, sy: i32, vs: VirtualScreen) -> Result<(), InputError> {
        let (nx, ny) = ultidesk_platform_windows::inject::to_absolute_virtual(sx, sy, vs);
        ultidesk_platform_windows::inject::move_cursor_absolute(nx, ny)
    }
    fn mouse_button(&self, button: MouseButton, down: bool) -> Result<(), InputError> {
        ultidesk_platform_windows::inject::mouse_button(button, down)
    }
    fn key(&self, scancode: u16, down: bool) -> Result<(), InputError> {
        ultidesk_platform_windows::inject::key_scancode(scancode, down)
    }
    fn enumerate(&self) -> Vec<WindowDto> {
        ultidesk_platform_windows::enumerate_top_level_windows()
            .into_iter()
            .map(|w| WindowDto {
                hwnd: w.hwnd,
                title: w.title,
                process_id: w.process_id,
                left: w.rect.left,
                top: w.rect.top,
                right: w.rect.right,
                bottom: w.rect.bottom,
            })
            .collect()
    }
}

/// Per-connection session state. Tracks authentication and, critically, every key and
/// button currently held *by this session*, so a dropped connection can release them
/// and never leave the source machine with a stuck modifier (brief §10, acceptance
/// criteria).
#[derive(Debug, Default)]
pub struct Session {
    authenticated: bool,
    held_buttons: HashSet<MouseButton>,
    held_scancodes: HashSet<u16>,
}

impl Session {
    pub fn new() -> Self {
        Self::default()
    }

    /// Introspection helpers used by tests (and by future diagnostics). Kept test-only
    /// for now so they are not flagged as dead code in the shipping binary.
    #[cfg(test)]
    pub fn is_authenticated(&self) -> bool {
        self.authenticated
    }

    #[cfg(test)]
    pub fn held_count(&self) -> usize {
        self.held_buttons.len() + self.held_scancodes.len()
    }

    /// Handle one request. `expected_token` is the per-launch secret; any command other
    /// than `Hello` before successful authentication is rejected.
    pub fn handle<I: Injector>(
        &mut self,
        req: IpcRequest,
        expected_token: &str,
        injector: &I,
    ) -> IpcResponse {
        if !self.authenticated {
            match req {
                IpcRequest::Hello {
                    token,
                    protocol_version,
                } => {
                    if !constant_time_eq(token.as_bytes(), expected_token.as_bytes()) {
                        return err("unauthorized", "invalid auth token");
                    }
                    if protocol_version != PROTOCOL_VERSION {
                        return err(
                            "protocol_mismatch",
                            &format!(
                                "agent speaks v{PROTOCOL_VERSION}, client sent v{protocol_version}"
                            ),
                        );
                    }
                    self.authenticated = true;
                    return IpcResponse::HelloOk {
                        agent_version: env!("CARGO_PKG_VERSION").to_string(),
                        protocol_version: PROTOCOL_VERSION,
                    };
                }
                _ => return err("unauthenticated", "must send Hello first"),
            }
        }

        match req {
            IpcRequest::Hello { .. } => err("already_authenticated", "duplicate Hello"),
            IpcRequest::Ping => IpcResponse::Pong,
            IpcRequest::EnumerateWindows => IpcResponse::Windows {
                windows: injector.enumerate(),
            },
            IpcRequest::InjectMouseMove {
                screen_x,
                screen_y,
                virtual_screen,
            } => match injector.mouse_move(screen_x, screen_y, virtual_screen.into()) {
                Ok(()) => IpcResponse::Injected,
                Err(e) => input_err(e),
            },
            IpcRequest::InjectMouseButton { button, down } => {
                let b: MouseButton = button.into();
                match injector.mouse_button(b, down) {
                    Ok(()) => {
                        if down {
                            self.held_buttons.insert(b);
                        } else {
                            self.held_buttons.remove(&b);
                        }
                        IpcResponse::Injected
                    }
                    Err(e) => input_err(e),
                }
            }
            IpcRequest::InjectKey { scancode, down } => match injector.key(scancode, down) {
                Ok(()) => {
                    if down {
                        self.held_scancodes.insert(scancode);
                    } else {
                        self.held_scancodes.remove(&scancode);
                    }
                    IpcResponse::Injected
                }
                Err(e) => input_err(e),
            },
            IpcRequest::ReleaseAllInput => {
                let count = self.release_all(injector);
                IpcResponse::Released { count }
            }
        }
    }

    /// Release everything this session holds. Called on `ReleaseAllInput` and, by the
    /// transport, whenever a connection drops. Best-effort: injection errors during
    /// release are ignored so one stuck key cannot block releasing the rest.
    pub fn release_all<I: Injector>(&mut self, injector: &I) -> usize {
        let mut count = 0;
        for b in self.held_buttons.drain().collect::<Vec<_>>() {
            let _ = injector.mouse_button(b, false);
            count += 1;
        }
        for s in self.held_scancodes.drain().collect::<Vec<_>>() {
            let _ = injector.key(s, false);
            count += 1;
        }
        count
    }
}

fn err(code: &str, message: &str) -> IpcResponse {
    IpcResponse::Error {
        code: code.to_string(),
        message: message.to_string(),
    }
}

fn input_err(e: InputError) -> IpcResponse {
    let code = match e {
        InputError::Unsupported => "input_unsupported",
        InputError::Blocked => "input_blocked",
        InputError::Os(_) => "input_os_error",
    };
    err(code, &e.to_string())
}

/// Length-checked, branch-constant comparison to avoid leaking token length/prefix
/// via timing. Tokens are short and local, but there is no reason to be sloppy.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[derive(Default)]
    struct MockInjector {
        events: RefCell<Vec<String>>,
        fail_blocked: bool,
    }
    impl Injector for MockInjector {
        fn mouse_move(&self, sx: i32, sy: i32, _vs: VirtualScreen) -> Result<(), InputError> {
            self.events.borrow_mut().push(format!("move {sx},{sy}"));
            Ok(())
        }
        fn mouse_button(&self, button: MouseButton, down: bool) -> Result<(), InputError> {
            if self.fail_blocked {
                return Err(InputError::Blocked);
            }
            self.events
                .borrow_mut()
                .push(format!("btn {button:?} {down}"));
            Ok(())
        }
        fn key(&self, scancode: u16, down: bool) -> Result<(), InputError> {
            self.events
                .borrow_mut()
                .push(format!("key {scancode} {down}"));
            Ok(())
        }
        fn enumerate(&self) -> Vec<WindowDto> {
            vec![]
        }
    }

    const TOKEN: &str = "s3cret-token";

    fn authed() -> (Session, MockInjector) {
        let mut s = Session::new();
        let inj = MockInjector::default();
        let r = s.handle(
            IpcRequest::Hello {
                token: TOKEN.into(),
                protocol_version: PROTOCOL_VERSION,
            },
            TOKEN,
            &inj,
        );
        assert!(matches!(r, IpcResponse::HelloOk { .. }));
        (s, inj)
    }

    #[test]
    fn commands_before_hello_are_rejected() {
        let mut s = Session::new();
        let inj = MockInjector::default();
        let r = s.handle(IpcRequest::Ping, TOKEN, &inj);
        assert!(matches!(r, IpcResponse::Error { .. }));
        assert!(!s.is_authenticated());
    }

    #[test]
    fn wrong_token_rejected() {
        let mut s = Session::new();
        let inj = MockInjector::default();
        let r = s.handle(
            IpcRequest::Hello {
                token: "nope".into(),
                protocol_version: PROTOCOL_VERSION,
            },
            TOKEN,
            &inj,
        );
        assert!(matches!(r, IpcResponse::Error { .. }));
        assert!(!s.is_authenticated());
    }

    #[test]
    fn protocol_mismatch_rejected() {
        let mut s = Session::new();
        let inj = MockInjector::default();
        let r = s.handle(
            IpcRequest::Hello {
                token: TOKEN.into(),
                protocol_version: PROTOCOL_VERSION + 100,
            },
            TOKEN,
            &inj,
        );
        assert!(matches!(r, IpcResponse::Error { code, .. } if code == "protocol_mismatch"));
    }

    #[test]
    fn held_input_is_tracked_and_released() {
        let (mut s, inj) = authed();
        s.handle(
            IpcRequest::InjectMouseButton {
                button: MouseButtonDto::Left,
                down: true,
            },
            TOKEN,
            &inj,
        );
        s.handle(
            IpcRequest::InjectKey {
                scancode: 0x1D,
                down: true,
            },
            TOKEN,
            &inj,
        ); // Ctrl
        assert_eq!(s.held_count(), 2);

        let r = s.handle(IpcRequest::ReleaseAllInput, TOKEN, &inj);
        assert!(matches!(r, IpcResponse::Released { count: 2 }));
        assert_eq!(s.held_count(), 0);
        // The mock recorded the key-up / button-up during release.
        let ev = inj.events.borrow();
        assert!(ev.iter().any(|e| e == "btn Left false"));
        assert!(ev.iter().any(|e| e == "key 29 false"));
    }

    #[test]
    fn matched_up_event_clears_held_without_release_all() {
        let (mut s, inj) = authed();
        s.handle(
            IpcRequest::InjectKey {
                scancode: 0x1D,
                down: true,
            },
            TOKEN,
            &inj,
        );
        assert_eq!(s.held_count(), 1);
        s.handle(
            IpcRequest::InjectKey {
                scancode: 0x1D,
                down: false,
            },
            TOKEN,
            &inj,
        );
        assert_eq!(s.held_count(), 0);
    }

    #[test]
    fn blocked_injection_surfaces_error_and_is_not_marked_held() {
        let mut s = Session::new();
        let inj = MockInjector {
            fail_blocked: true,
            ..Default::default()
        };
        // authenticate
        s.handle(
            IpcRequest::Hello {
                token: TOKEN.into(),
                protocol_version: PROTOCOL_VERSION,
            },
            TOKEN,
            &inj,
        );
        let r = s.handle(
            IpcRequest::InjectMouseButton {
                button: MouseButtonDto::Left,
                down: true,
            },
            TOKEN,
            &inj,
        );
        assert!(matches!(r, IpcResponse::Error { code, .. } if code == "input_blocked"));
        assert_eq!(
            s.held_count(),
            0,
            "a blocked press must not be tracked as held"
        );
    }

    #[test]
    fn constant_time_eq_basic() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
    }
}
