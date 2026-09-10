//! Local IPC message types and the platform-independent request dispatcher.
//!
//! Transport (Windows named pipe) lives in `pipe.rs`. Keeping dispatch here — behind
//! an [`Injector`] trait — means the safety-critical logic (auth gating, and releasing
//! all held keys/buttons when a session ends) is unit-tested with a mock injector,
//! without needing a pipe, a GUI, or real input.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use ultidesk_core::protocol::PROTOCOL_VERSION;
use ultidesk_identity::Permissions;
use ultidesk_platform_windows::inject::{InputError, MouseButton, VirtualScreen};
use ultidesk_topology::AudioDevice;

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
    /// Wheel movement in Win32 units (multiples of `WHEEL_DELTA`), already whole
    /// notches and already in the receiver's sign convention. The sender owns the
    /// accumulation and the sign flip (`ultidesk_core::scroll`) so every receiver does
    /// not have to re-derive them.
    InjectScroll {
        delta_x: i32,
        delta_y: i32,
    },
    /// Release every key/button this session is currently holding. Idempotent.
    ReleaseAllInput,
    /// This machine's audio endpoints. Read-only, and the first message of the settings
    /// surface: the control app cannot show a peer's devices without asking for them.
    ListAudioDevices,
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
    AudioDevices {
        devices: Vec<AudioDevice>,
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
    fn scroll(&self, delta_x: i32, delta_y: i32) -> Result<(), InputError>;
    fn enumerate(&self) -> Vec<WindowDto>;
}

/// What this machine can say about its own audio endpoints.
///
/// Separate from [`Injector`] rather than another method on it: injection is something a
/// peer *does to* this machine and is gated accordingly, while this is something the
/// machine *reports about itself*. Folding them together would mean a future permission
/// check could not tell the two apart.
pub trait AudioInventory {
    /// The endpoints, or an operator-facing reason there are none.
    ///
    /// A `String` because every caller does the same thing with it — puts it in front of
    /// a person — and the platform errors underneath are already unrelated types.
    fn devices(&self) -> Result<Vec<AudioDevice>, String>;
}

/// The local capabilities a session may be asked about.
///
/// Bundled rather than passed one argument at a time because the settings surface is
/// going to grow — monitors and topology next — and a dispatcher whose signature changes
/// with every capability drags four transports and every test along with it.
///
/// The borrows carry no `Send`/`Sync` bound. They do not need one: a `Backends` is built
/// inside the statement that dispatches and dropped at the end of it, so it never
/// crosses an await and never leaves the task. [`LocalBackends`] carries the bounds
/// instead, because that one really is shared between tasks. Requiring them here as well
/// would only forbid the obvious `RefCell`-recording test double.
pub struct Backends<'a> {
    pub injector: &'a dyn Injector,
    pub audio: &'a dyn AudioInventory,
}

/// Real audio enumeration, backed by whichever platform this build targets.
///
/// Carries the device id rather than reading it per call: every endpoint it returns is
/// labelled with *this* machine's identity, and that label is what lets a receiver check
/// the answer against the key it authenticated. Deriving it once at construction means
/// a single session cannot report two different machines.
pub struct RealAudioInventory {
    device_id: ultidesk_core::DeviceId,
}

impl RealAudioInventory {
    pub fn new(device_id: ultidesk_core::DeviceId) -> Self {
        RealAudioInventory { device_id }
    }
}

impl AudioInventory for RealAudioInventory {
    fn devices(&self) -> Result<Vec<AudioDevice>, String> {
        // Only the per-platform selection is here; the field mapping lives in the
        // platform crate that owns the endpoint type, so the editor and the agent cannot
        // disagree about what a saved route is keyed on.
        #[cfg(target_os = "linux")]
        {
            ultidesk_platform_linux::audio_devices::enumerate_shared(self.device_id)
                .map_err(|e| e.to_string())
        }
        #[cfg(windows)]
        {
            ultidesk_platform_windows::audio_devices::enumerate_shared(self.device_id)
                .map_err(|e| e.to_string())
        }
        #[cfg(not(any(target_os = "linux", windows)))]
        {
            Err("audio device enumeration is not implemented for this platform".into())
        }
    }
}

/// The backends a transport owns and lends to each connection.
///
/// `Backends` borrows; this owns. A transport serves many connections from one set of
/// capabilities, and each connection is a spawned task, so the shared thing has to be
/// `Arc`-held while the thing the dispatcher takes stays a cheap borrow.
pub struct LocalBackends {
    pub injector: std::sync::Arc<dyn Injector + Send + Sync>,
    pub audio: std::sync::Arc<dyn AudioInventory + Send + Sync>,
}

impl LocalBackends {
    pub fn new(
        injector: std::sync::Arc<dyn Injector + Send + Sync>,
        device_id: ultidesk_core::DeviceId,
    ) -> Self {
        LocalBackends {
            injector,
            audio: std::sync::Arc::new(RealAudioInventory::new(device_id)),
        }
    }

    pub fn borrow(&self) -> Backends<'_> {
        Backends {
            injector: self.injector.as_ref(),
            audio: self.audio.as_ref(),
        }
    }
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
    fn scroll(&self, delta_x: i32, delta_y: i32) -> Result<(), InputError> {
        ultidesk_platform_windows::inject::mouse_scroll(delta_x, delta_y)
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

/// Which permission a request needs, or `None` when it needs none.
///
/// A single `match` with no wildcard arm on purpose: adding a request to [`IpcRequest`]
/// then fails to compile until someone has decided what it costs. A `_ => None` default
/// would let the next message ship ungated, and it would ship silently.
fn required_permission(req: &IpcRequest) -> Option<Needed> {
    match req {
        // Liveness. Refusing it would make "the peer is gone" and "the peer denies me"
        // look identical, and it discloses nothing that completing the handshake did not.
        IpcRequest::Ping => None,
        // Handled by the authentication gate above, not by a permission.
        IpcRequest::Hello { .. } => None,

        IpcRequest::InjectMouseMove { .. }
        | IpcRequest::InjectMouseButton { .. }
        | IpcRequest::InjectKey { .. }
        | IpcRequest::InjectScroll { .. } => Some(Needed::ControlInput),

        // Releasing is *not* gated, deliberately. It only ever undoes what this session
        // already did, and a peer whose input permission is revoked mid-connection must
        // still be able to let go of a held key — refusing would leave a modifier stuck
        // down on this machine, which is exactly the failure the release path exists to
        // prevent.
        IpcRequest::ReleaseAllInput => None,

        IpcRequest::EnumerateWindows => Some(Needed::ListWindows),
        IpcRequest::ListAudioDevices => Some(Needed::ReadDevices),
    }
}

/// One permission, in the form the gate needs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Needed {
    ControlInput,
    ReadDevices,
    ListWindows,
}

impl Needed {
    fn granted_by(self, p: &Permissions) -> bool {
        match self {
            Needed::ControlInput => p.control_input,
            Needed::ReadDevices => p.read_devices,
            Needed::ListWindows => p.list_windows,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Needed::ControlInput => "control-input",
            Needed::ReadDevices => "read-devices",
            Needed::ListWindows => "list-windows",
        }
    }
}

/// Per-connection session state. Tracks authentication and, critically, every key and
/// button currently held *by this session*, so a dropped connection can release them
/// and never leave the source machine with a stuck modifier (brief §10, acceptance
/// criteria).
#[derive(Debug)]
pub struct Session {
    /// The secret this session will accept in `Hello`, or `None` when the transport
    /// already authenticated the peer.
    ///
    /// Held here rather than passed to every [`Session::handle`] call so that an
    /// already-authenticated session has no token to compare against at all. Passing
    /// one in per call meant the transport that does not use tokens had to invent a
    /// value to pass — and an empty string would have matched an empty `Hello`.
    expected_token: Option<String>,
    /// What the far end is allowed to ask for, once authenticated.
    ///
    /// Held per session rather than looked up per request: the peer is decided by the
    /// handshake and cannot change mid-connection, and re-reading the store on every
    /// pointer move would put a file read on the input hot path.
    permissions: Permissions,
    authenticated: bool,
    held_buttons: HashSet<MouseButton>,
    held_scancodes: HashSet<u16>,
}

impl Session {
    /// A session gated by a shared secret: the local named pipe, and the dev TCP peer
    /// transport.
    /// A session gated by a shared secret, and allowed everything.
    ///
    /// Not an oversight: the only holder of the token is a process running as this user
    /// on this machine, which can already do anything the agent can — it could drive the
    /// same APIs directly, or read the agent's own key file. A permission check there
    /// would be theatre, and pretending otherwise would make the *peer* checks look like
    /// the same kind of gesture. Permissions exist for the far end of a network.
    pub fn new(expected_token: &str) -> Self {
        Session {
            expected_token: Some(expected_token.to_string()),
            permissions: Permissions::all(),
            authenticated: false,
            held_buttons: HashSet::new(),
            held_scancodes: HashSet::new(),
        }
    }

    /// A session whose peer was authenticated by the transport itself.
    ///
    /// The QUIC channel (`ultidesk-transport`) proves the peer holds the private key for
    /// a pinned identity before a single byte of this protocol is exchanged, and
    /// negotiates the protocol version through ALPN. There is nothing left for a `Hello`
    /// to establish, so one is refused as a duplicate rather than being a second, weaker
    /// way in.
    pub fn for_authenticated_peer(permissions: Permissions) -> Self {
        Session {
            expected_token: None,
            permissions,
            authenticated: true,
            held_buttons: HashSet::new(),
            held_scancodes: HashSet::new(),
        }
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

    /// Handle one request. Any command other than `Hello` before successful
    /// authentication is rejected.
    pub fn handle(&mut self, req: IpcRequest, backends: &Backends<'_>) -> IpcResponse {
        let injector = backends.injector;
        if !self.authenticated {
            match req {
                IpcRequest::Hello {
                    token,
                    protocol_version,
                } => {
                    // No token means no way in: a session with nothing to compare
                    // against refuses rather than accepting anything.
                    let Some(expected) = self.expected_token.as_deref() else {
                        return err("unauthorized", "this transport does not accept tokens");
                    };
                    if !constant_time_eq(token.as_bytes(), expected.as_bytes()) {
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

        if let Some(needed) = required_permission(&req) {
            if !needed.granted_by(&self.permissions) {
                // Named, so an operator reading the peer's log knows which grant to
                // change rather than only that something was refused.
                return err(
                    "not_permitted",
                    &format!(
                        "this device does not allow {:?} for the peer on this session",
                        needed.name()
                    ),
                );
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
            // Scroll holds no state, so unlike keys and buttons there is nothing to
            // track here and nothing for ReleaseAllInput to undo: a wheel notch is
            // instantaneous and cannot be left stuck down.
            IpcRequest::InjectScroll { delta_x, delta_y } => {
                match injector.scroll(delta_x, delta_y) {
                    Ok(()) => IpcResponse::Injected,
                    Err(e) => input_err(e),
                }
            }
            IpcRequest::ReleaseAllInput => {
                let count = self.release_all(injector);
                IpcResponse::Released { count }
            }
            // Deliberately answers about *this* machine only. A request to relay a
            // peer's devices would be a different message, because the answer would
            // then carry a device id this machine cannot vouch for.
            IpcRequest::ListAudioDevices => match backends.audio.devices() {
                Ok(devices) => IpcResponse::AudioDevices { devices },
                Err(message) => err("audio_unavailable", &message),
            },
        }
    }

    /// Release everything this session holds. Called on `ReleaseAllInput` and, by the
    /// transport, whenever a connection drops. Best-effort: injection errors during
    /// release are ignored so one stuck key cannot block releasing the rest.
    pub fn release_all<I: Injector + ?Sized>(&mut self, injector: &I) -> usize {
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

    /// Answers whatever the test told it to, including a failure — the error path is
    /// the one a machine with no working audio daemon actually takes.
    struct MockAudio(Result<Vec<AudioDevice>, String>);
    impl AudioInventory for MockAudio {
        fn devices(&self) -> Result<Vec<AudioDevice>, String> {
            self.0.clone()
        }
    }

    fn backends<'a>(injector: &'a dyn Injector, audio: &'a dyn AudioInventory) -> Backends<'a> {
        Backends { injector, audio }
    }

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
        fn scroll(&self, dx: i32, dy: i32) -> Result<(), InputError> {
            self.events.borrow_mut().push(format!("scroll {dx},{dy}"));
            Ok(())
        }
        fn enumerate(&self) -> Vec<WindowDto> {
            vec![]
        }
    }

    const TOKEN: &str = "s3cret-token";

    fn authed() -> (Session, MockInjector, MockAudio) {
        let mut s = Session::new(TOKEN);
        let inj = MockInjector::default();
        let audio = MockAudio(Ok(Vec::new()));
        let r = s.handle(
            IpcRequest::Hello {
                token: TOKEN.into(),
                protocol_version: PROTOCOL_VERSION,
            },
            &backends(&inj, &audio),
        );
        assert!(matches!(r, IpcResponse::HelloOk { .. }));
        (s, inj, audio)
    }

    #[test]
    fn commands_before_hello_are_rejected() {
        let mut s = Session::new(TOKEN);
        let inj = MockInjector::default();
        let audio = MockAudio(Ok(Vec::new()));
        let r = s.handle(IpcRequest::Ping, &backends(&inj, &audio));
        assert!(matches!(r, IpcResponse::Error { .. }));
        assert!(!s.is_authenticated());
    }

    #[test]
    fn a_transport_authenticated_peer_needs_no_hello() {
        // The QUIC path: the handshake already proved which device this is, so the
        // first message may be a command.
        let mut s = Session::for_authenticated_peer(Permissions::all());
        let inj = MockInjector::default();
        let audio = MockAudio(Ok(Vec::new()));
        assert!(s.is_authenticated());
        assert!(matches!(
            s.handle(IpcRequest::Ping, &backends(&inj, &audio)),
            IpcResponse::Pong
        ));
    }

    #[test]
    fn a_transport_authenticated_peer_cannot_offer_a_token() {
        // There is nothing for a Hello to establish, and accepting one would be a
        // second, weaker way in beside the handshake.
        let mut s = Session::for_authenticated_peer(Permissions::all());
        let inj = MockInjector::default();
        let audio = MockAudio(Ok(Vec::new()));
        let r = s.handle(
            IpcRequest::Hello {
                token: String::new(),
                protocol_version: PROTOCOL_VERSION,
            },
            &backends(&inj, &audio),
        );
        assert!(matches!(r, IpcResponse::Error { .. }), "{r:?}");
    }

    #[test]
    fn wrong_token_rejected() {
        let mut s = Session::new(TOKEN);
        let inj = MockInjector::default();
        let audio = MockAudio(Ok(Vec::new()));
        let r = s.handle(
            IpcRequest::Hello {
                token: "nope".into(),
                protocol_version: PROTOCOL_VERSION,
            },
            &backends(&inj, &audio),
        );
        assert!(matches!(r, IpcResponse::Error { .. }));
        assert!(!s.is_authenticated());
    }

    #[test]
    fn protocol_mismatch_rejected() {
        let mut s = Session::new(TOKEN);
        let inj = MockInjector::default();
        let audio = MockAudio(Ok(Vec::new()));
        let r = s.handle(
            IpcRequest::Hello {
                token: TOKEN.into(),
                protocol_version: PROTOCOL_VERSION + 100,
            },
            &backends(&inj, &audio),
        );
        assert!(matches!(r, IpcResponse::Error { code, .. } if code == "protocol_mismatch"));
    }

    #[test]
    fn held_input_is_tracked_and_released() {
        let (mut s, inj, audio) = authed();
        s.handle(
            IpcRequest::InjectMouseButton {
                button: MouseButtonDto::Left,
                down: true,
            },
            &backends(&inj, &audio),
        );
        s.handle(
            IpcRequest::InjectKey {
                scancode: 0x1D,
                down: true,
            },
            &backends(&inj, &audio),
        ); // Ctrl
        assert_eq!(s.held_count(), 2);

        let r = s.handle(IpcRequest::ReleaseAllInput, &backends(&inj, &audio));
        assert!(matches!(r, IpcResponse::Released { count: 2 }));
        assert_eq!(s.held_count(), 0);
        // The mock recorded the key-up / button-up during release.
        let ev = inj.events.borrow();
        assert!(ev.iter().any(|e| e == "btn Left false"));
        assert!(ev.iter().any(|e| e == "key 29 false"));
    }

    #[test]
    fn matched_up_event_clears_held_without_release_all() {
        let (mut s, inj, audio) = authed();
        s.handle(
            IpcRequest::InjectKey {
                scancode: 0x1D,
                down: true,
            },
            &backends(&inj, &audio),
        );
        assert_eq!(s.held_count(), 1);
        s.handle(
            IpcRequest::InjectKey {
                scancode: 0x1D,
                down: false,
            },
            &backends(&inj, &audio),
        );
        assert_eq!(s.held_count(), 0);
    }

    #[test]
    fn blocked_injection_surfaces_error_and_is_not_marked_held() {
        let mut s = Session::new(TOKEN);
        let inj = MockInjector {
            fail_blocked: true,
            ..Default::default()
        };
        let audio = MockAudio(Ok(Vec::new()));
        // authenticate
        s.handle(
            IpcRequest::Hello {
                token: TOKEN.into(),
                protocol_version: PROTOCOL_VERSION,
            },
            &backends(&inj, &audio),
        );
        let r = s.handle(
            IpcRequest::InjectMouseButton {
                button: MouseButtonDto::Left,
                down: true,
            },
            &backends(&inj, &audio),
        );
        assert!(matches!(r, IpcResponse::Error { code, .. } if code == "input_blocked"));
        assert_eq!(
            s.held_count(),
            0,
            "a blocked press must not be tracked as held"
        );
    }

    fn a_device(device_id: ultidesk_core::DeviceId, node: &str) -> AudioDevice {
        AudioDevice {
            device_id,
            node: node.to_string(),
            name: format!("{node} (display name)"),
            kind: ultidesk_topology::DeviceKind::Output,
            is_default: false,
        }
    }

    #[test]
    fn listing_audio_devices_needs_authentication_like_everything_else() {
        // The settings surface is read-only, which is not the same as public: what
        // endpoints a machine has, and what they are called, is information about it.
        let mut s = Session::new(TOKEN);
        let inj = MockInjector::default();
        let id = ultidesk_core::DeviceId::new();
        let audio = MockAudio(Ok(vec![a_device(id, "speakers")]));

        let r = s.handle(IpcRequest::ListAudioDevices, &backends(&inj, &audio));
        assert!(matches!(r, IpcResponse::Error { code, .. } if code == "unauthenticated"));
    }

    #[test]
    fn an_authenticated_session_gets_the_devices() {
        let (mut s, inj, _) = authed();
        let id = ultidesk_core::DeviceId::new();
        let audio = MockAudio(Ok(vec![a_device(id, "speakers"), a_device(id, "headset")]));

        match s.handle(IpcRequest::ListAudioDevices, &backends(&inj, &audio)) {
            IpcResponse::AudioDevices { devices } => {
                assert_eq!(devices.len(), 2);
                assert_eq!(devices[0].node, "speakers");
                assert!(
                    devices.iter().all(|d| d.device_id == id),
                    "every endpoint must be labelled with the machine that owns it"
                );
            }
            other => panic!("expected AudioDevices, got {other:?}"),
        }
    }

    #[test]
    fn a_machine_that_cannot_read_its_audio_says_so_rather_than_reporting_none() {
        // An empty list and a broken audio daemon are different answers, and an operator
        // debugging silence needs to be able to tell them apart.
        let (mut s, inj, _) = authed();
        let audio = MockAudio(Err("could not reach the PipeWire daemon".into()));

        match s.handle(IpcRequest::ListAudioDevices, &backends(&inj, &audio)) {
            IpcResponse::Error { code, message } => {
                assert_eq!(code, "audio_unavailable");
                assert!(message.contains("PipeWire"), "{message}");
            }
            other => panic!("expected an error, got {other:?}"),
        }
    }

    #[test]
    fn a_machine_with_no_endpoints_is_not_an_error() {
        let (mut s, inj, _) = authed();
        let audio = MockAudio(Ok(Vec::new()));
        assert!(matches!(
            s.handle(IpcRequest::ListAudioDevices, &backends(&inj, &audio)),
            IpcResponse::AudioDevices { devices } if devices.is_empty()
        ));
    }

    #[test]
    fn listing_devices_holds_no_input_and_releases_nothing() {
        // A read-only request must not disturb the held-input bookkeeping that a
        // disconnect depends on.
        let (mut s, inj, _) = authed();
        let audio = MockAudio(Ok(Vec::new()));
        s.handle(
            IpcRequest::InjectKey {
                scancode: 0x1D,
                down: true,
            },
            &backends(&inj, &audio),
        );
        s.handle(IpcRequest::ListAudioDevices, &backends(&inj, &audio));
        assert_eq!(s.held_count(), 1, "the held key must survive a query");
    }

    /// A peer session allowed exactly `permissions`, already authenticated.
    fn peer_with(permissions: Permissions) -> Session {
        Session::for_authenticated_peer(permissions)
    }

    #[test]
    fn a_peer_without_control_input_cannot_move_the_pointer() {
        let mut s = peer_with(Permissions {
            control_input: false,
            read_devices: true,
            list_windows: true,
        });
        let inj = MockInjector::default();
        let audio = MockAudio(Ok(Vec::new()));

        let r = s.handle(
            IpcRequest::InjectMouseMove {
                screen_x: 10,
                screen_y: 10,
                virtual_screen: VirtualScreenDto {
                    left: 0,
                    top: 0,
                    width: 1920,
                    height: 1080,
                },
            },
            &backends(&inj, &audio),
        );
        match r {
            IpcResponse::Error { code, message } => {
                assert_eq!(code, "not_permitted");
                assert!(message.contains("control-input"), "{message}");
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
        assert!(
            inj.events.borrow().is_empty(),
            "a refused request must not reach the injector at all"
        );
    }

    #[test]
    fn a_peer_without_read_devices_cannot_list_them() {
        let mut s = peer_with(Permissions {
            control_input: true,
            read_devices: false,
            list_windows: true,
        });
        let inj = MockInjector::default();
        let audio = MockAudio(Ok(vec![a_device(ultidesk_core::DeviceId::new(), "spk")]));

        let r = s.handle(IpcRequest::ListAudioDevices, &backends(&inj, &audio));
        assert!(
            matches!(r, IpcResponse::Error { ref code, .. } if code == "not_permitted"),
            "{r:?}"
        );
    }

    #[test]
    fn a_peer_without_list_windows_cannot_read_titles() {
        // The one that leaks what the operator is doing, and the reason it is a separate
        // permission from reading devices.
        let mut s = peer_with(Permissions::on_pairing());
        let inj = MockInjector::default();
        let audio = MockAudio(Ok(Vec::new()));

        let r = s.handle(IpcRequest::EnumerateWindows, &backends(&inj, &audio));
        assert!(
            matches!(r, IpcResponse::Error { ref code, .. } if code == "not_permitted"),
            "a fresh pairing must not hand over window titles: {r:?}"
        );
    }

    #[test]
    fn a_peer_allowed_nothing_can_still_be_pinged() {
        // Liveness is not a capability. Refusing it would make "the peer is gone" and
        // "the peer denies me" indistinguishable.
        let mut s = peer_with(Permissions::default());
        let inj = MockInjector::default();
        let audio = MockAudio(Ok(Vec::new()));
        assert!(matches!(
            s.handle(IpcRequest::Ping, &backends(&inj, &audio)),
            IpcResponse::Pong
        ));
    }

    #[test]
    fn a_peer_can_always_let_go_of_a_key_it_is_holding() {
        // Release is deliberately ungated. It only undoes what this session already did,
        // and a peer whose input permission is revoked mid-connection must still be able
        // to drop a held modifier — otherwise revoking a permission is what leaves the
        // machine with a stuck key.
        let mut s = peer_with(Permissions::all());
        let inj = MockInjector::default();
        let audio = MockAudio(Ok(Vec::new()));
        s.handle(
            IpcRequest::InjectKey {
                scancode: 0x1D,
                down: true,
            },
            &backends(&inj, &audio),
        );
        assert_eq!(s.held_count(), 1);

        // Revoke on the live session, holding the key it already pressed.
        s.permissions = Permissions::default();

        let r = s.handle(IpcRequest::ReleaseAllInput, &backends(&inj, &audio));
        assert!(matches!(r, IpcResponse::Released { count: 1 }), "{r:?}");
        assert_eq!(s.held_count(), 0);
    }

    #[test]
    fn the_local_ipc_is_allowed_everything() {
        // The token holder is a process running as this user on this machine; it could
        // drive the same APIs directly. Gating it would be theatre.
        let (mut s, inj, _) = authed();
        let audio = MockAudio(Ok(Vec::new()));
        assert!(!matches!(
            s.handle(IpcRequest::EnumerateWindows, &backends(&inj, &audio)),
            IpcResponse::Error { .. }
        ));
    }

    #[test]
    fn every_request_has_a_decided_permission() {
        // `required_permission` has no wildcard arm, so this is really a reminder of why:
        // adding a request must not silently inherit "needs nothing".
        let ungated = [
            IpcRequest::Ping,
            IpcRequest::ReleaseAllInput,
            IpcRequest::Hello {
                token: String::new(),
                protocol_version: PROTOCOL_VERSION,
            },
        ];
        for req in ungated {
            assert!(
                required_permission(&req).is_none(),
                "{req:?} should need no permission"
            );
        }
        assert_eq!(
            required_permission(&IpcRequest::ListAudioDevices),
            Some(Needed::ReadDevices)
        );
        assert_eq!(
            required_permission(&IpcRequest::EnumerateWindows),
            Some(Needed::ListWindows)
        );
        assert_eq!(
            required_permission(&IpcRequest::InjectScroll {
                delta_x: 0,
                delta_y: 120
            }),
            Some(Needed::ControlInput)
        );
    }

    #[test]
    fn constant_time_eq_basic() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
    }
}
