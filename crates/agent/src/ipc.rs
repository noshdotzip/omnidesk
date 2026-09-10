//! Local IPC message types and the platform-independent request dispatcher.
//!
//! Transport (Windows named pipe) lives in `pipe.rs`. Keeping dispatch here — behind
//! an [`Injector`] trait — means the safety-critical logic (auth gating, and releasing
//! all held keys/buttons when a session ends) is unit-tested with a mock injector,
//! without needing a pipe, a GUI, or real input.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use ultidesk_core::protocol::PROTOCOL_VERSION;
use ultidesk_identity::{PeerKey, Permissions};
use ultidesk_platform_windows::inject::{InputError, MouseButton, VirtualScreen};
use ultidesk_topology::{AudioDevice, Monitor};

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
    /// This machine's monitors, in its own virtual-desktop coordinates.
    ///
    /// Positions are *not* comparable with another machine's — see
    /// `ultidesk_topology::arrange`, which places each machine's screens as one block
    /// precisely so no absolute position ever crosses a machine boundary.
    ListMonitors,
    /// Put a question to a paired peer on the caller's behalf, and hand back the answer.
    ///
    /// This is how the control app learns anything about the other machine: it speaks
    /// only to its own agent, and the agent holds the peer connection.
    ///
    /// **Local callers only.** A peer must never be able to use this machine as a hop to
    /// reach a third one — see [`Gate::LocalOnly`].
    AskPeer {
        peer: PeerKey,
        query: PeerQuery,
    },
}

/// What may be asked of a peer through the relay.
///
/// A closed list rather than a nested [`IpcRequest`]: nesting would let a caller relay a
/// relay, and every hop would need its own loop check. These are the two read-only
/// queries that exist, and each new one is a deliberate addition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PeerQuery {
    AudioDevices,
    Monitors,
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
    Monitors {
        monitors: Vec<Monitor>,
    },
    /// A peer's audio endpoints, relayed.
    ///
    /// The peer's key is echoed so the answer says whose it is. The relaying agent has
    /// already checked every entry against the key that completed *its* handshake — it
    /// is the only party that can, having been the one to authenticate — so this field
    /// makes the answer self-describing rather than re-deriving that trust.
    PeerAudioDevices {
        peer: PeerKey,
        devices: Vec<AudioDevice>,
    },
    /// A peer's monitors, relayed. Positions are in the *peer's* coordinate space.
    PeerMonitors {
        peer: PeerKey,
        monitors: Vec<Monitor>,
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

/// What this machine can say about its own screens.
///
/// Alongside [`AudioInventory`] rather than merged with it: they fail independently — a
/// machine can have a working compositor and a broken audio daemon — and a caller that
/// asked for one should not be told about the other's problem.
pub trait MonitorInventory {
    /// The monitors, or an operator-facing reason there are none.
    fn monitors(&self) -> Result<Vec<Monitor>, String>;
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
    pub monitors: &'a dyn MonitorInventory,
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

/// Real monitor enumeration, backed by whichever platform this build targets.
///
/// Carries the device id for the same reason [`RealAudioInventory`] does: every monitor
/// it returns is labelled with this machine's identity, and that label is what lets a
/// receiver check the answer against the key it authenticated.
pub struct RealMonitorInventory {
    device_id: ultidesk_core::DeviceId,
}

impl RealMonitorInventory {
    pub fn new(device_id: ultidesk_core::DeviceId) -> Self {
        RealMonitorInventory { device_id }
    }
}

impl MonitorInventory for RealMonitorInventory {
    fn monitors(&self) -> Result<Vec<Monitor>, String> {
        #[cfg(target_os = "linux")]
        {
            ultidesk_platform_linux::monitors::enumerate_shared(self.device_id)
                .map_err(|e| e.to_string())
        }
        #[cfg(windows)]
        {
            ultidesk_platform_windows::monitors::enumerate_shared(self.device_id)
                .map_err(|e| e.to_string())
        }
        #[cfg(not(any(target_os = "linux", windows)))]
        {
            Err("monitor enumeration is not implemented for this platform".into())
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
    pub monitors: std::sync::Arc<dyn MonitorInventory + Send + Sync>,
    /// What this machine needs to reach a peer on a caller's behalf, or `None` for a
    /// transport that must never relay.
    ///
    /// The gate already refuses [`IpcRequest::AskPeer`] from a peer session, so this is
    /// belt and braces: on the peer-facing transports there is nothing to relay *with*,
    /// so a mistake in the gate still cannot turn this machine into a hop.
    pub relay: Option<std::sync::Arc<crate::relay::RelayContext>>,
}

impl LocalBackends {
    pub fn new(
        injector: std::sync::Arc<dyn Injector + Send + Sync>,
        device_id: ultidesk_core::DeviceId,
    ) -> Self {
        LocalBackends {
            injector,
            audio: std::sync::Arc::new(RealAudioInventory::new(device_id)),
            monitors: std::sync::Arc::new(RealMonitorInventory::new(device_id)),
            relay: None,
        }
    }

    /// The same backends, able to reach peers on a local caller's behalf.
    pub fn with_relay(mut self, relay: std::sync::Arc<crate::relay::RelayContext>) -> Self {
        self.relay = Some(relay);
        self
    }

    pub fn borrow(&self) -> Backends<'_> {
        Backends {
            injector: self.injector.as_ref(),
            audio: self.audio.as_ref(),
            monitors: self.monitors.as_ref(),
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

/// Run one request to completion, doing the async work a relay needs.
///
/// Shared by every transport so that the decision of what a relay costs, and what to say
/// when there is nothing to relay with, is made once.
pub async fn complete(
    session: &mut Session,
    req: IpcRequest,
    backends: &LocalBackends,
) -> IpcResponse {
    // Bound to a local first so the `Backends` borrow is dropped at the end of *this*
    // statement. Left inline it would live across the await below, and since those
    // borrows deliberately carry no `Send`/`Sync` bound — see `Backends` — the whole
    // future would stop being spawnable.
    let decision = session.dispatch(req, &backends.borrow());
    match decision {
        Dispatch::Reply(response) => response,
        Dispatch::AskPeer { peer, query } => match &backends.relay {
            Some(relay) => relay.ask(peer, query).await,
            // Unreachable through the gate, which refuses a relay from a peer session.
            // Reported rather than unwrapped, because "this transport cannot relay" is a
            // true and useful thing to say if it ever is reached.
            None => err(
                "relay_unavailable",
                "this transport cannot reach peers on a caller's behalf",
            ),
        },
    }
}

/// What the dispatcher decided to do with a request.
///
/// Exists because one request — [`IpcRequest::AskPeer`] — cannot be answered without
/// network I/O, and the dispatcher is deliberately synchronous so that the auth and
/// permission logic can be unit-tested without a runtime. So it authorises, then hands
/// the work back to the transport that has one.
pub enum Dispatch {
    Reply(IpcResponse),
    /// Authorised. The transport must ask the peer and answer on this session's behalf.
    AskPeer {
        peer: PeerKey,
        query: PeerQuery,
    },
}

/// Where a session's far end is.
///
/// Not derivable from the permissions: a peer granted everything is still not local, and
/// the difference decides whether this machine will act as a hop to a third one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// A process on this machine, holding the per-launch token.
    Local,
    /// Another device, authenticated by its pinned key.
    Peer,
}

/// What a request costs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Gate {
    /// Available to anyone the session let in.
    Open,
    /// Needs a permission the operator granted this peer.
    Permission(Needed),
    /// Never available to a peer, whatever it has been granted.
    ///
    /// A separate axis from permissions rather than another one of them, because it is
    /// not the operator's to grant: a peer that could use this machine as a hop could
    /// reach machines it was never paired with, borrowing this machine's trust to do it.
    /// No permission should be able to express that.
    LocalOnly,
}

/// Which permission a request needs, or `None` when it needs none.
///
/// A single `match` with no wildcard arm on purpose: adding a request to [`IpcRequest`]
/// then fails to compile until someone has decided what it costs. A `_ => None` default
/// would let the next message ship ungated, and it would ship silently.
fn gate_for(req: &IpcRequest) -> Gate {
    match req {
        // Liveness. Refusing it would make "the peer is gone" and "the peer denies me"
        // look identical, and it discloses nothing that completing the handshake did not.
        IpcRequest::Ping => Gate::Open,
        // Handled by the authentication gate above, not by a permission.
        IpcRequest::Hello { .. } => Gate::Open,

        IpcRequest::InjectMouseMove { .. }
        | IpcRequest::InjectMouseButton { .. }
        | IpcRequest::InjectKey { .. }
        | IpcRequest::InjectScroll { .. } => Gate::Permission(Needed::ControlInput),

        // Releasing is *not* gated, deliberately. It only ever undoes what this session
        // already did, and a peer whose input permission is revoked mid-connection must
        // still be able to let go of a held key — refusing would leave a modifier stuck
        // down on this machine, which is exactly the failure the release path exists to
        // prevent.
        IpcRequest::ReleaseAllInput => Gate::Open,

        IpcRequest::EnumerateWindows => Gate::Permission(Needed::ListWindows),
        // Both describe what this machine *has*, which is one decision an operator makes
        // once — unlike window titles, which say what they are doing.
        IpcRequest::ListAudioDevices | IpcRequest::ListMonitors => {
            Gate::Permission(Needed::ReadDevices)
        }

        IpcRequest::AskPeer { .. } => Gate::LocalOnly,
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
    origin: Origin,
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
            origin: Origin::Local,
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
            origin: Origin::Peer,
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
    #[cfg(test)]
    pub fn handle(&mut self, req: IpcRequest, backends: &Backends<'_>) -> IpcResponse {
        match self.dispatch(req, backends) {
            Dispatch::Reply(response) => response,
            // Only a transport that can do async work may accept a relay, and it calls
            // `dispatch` directly. Reaching here means a transport used the synchronous
            // entry point for a request it cannot complete, which is a wiring mistake
            // rather than something a caller did.
            Dispatch::AskPeer { .. } => err(
                "relay_unsupported",
                "this transport cannot relay a request to a peer",
            ),
        }
    }

    /// Handle one request, authorising it and then either answering or handing back work
    /// the transport must do asynchronously.
    ///
    /// Split this way so authentication and the permission gate stay in **one** place. A
    /// relay handled by intercepting the message in each transport would need the gate
    /// repeated there, and the copy that was forgotten would be the way in.
    pub fn dispatch(&mut self, req: IpcRequest, backends: &Backends<'_>) -> Dispatch {
        if !self.authenticated {
            return Dispatch::Reply(self.authenticate(req));
        }
        if let IpcRequest::Hello { .. } = req {
            return Dispatch::Reply(err("already_authenticated", "duplicate Hello"));
        }
        if let Err(refusal) = self.check_gate(&req) {
            return Dispatch::Reply(refusal);
        }
        // Authorised, and the only request this dispatcher cannot finish itself.
        if let IpcRequest::AskPeer { peer, query } = req {
            return Dispatch::AskPeer { peer, query };
        }
        Dispatch::Reply(self.act(req, backends))
    }

    /// The pre-authentication state: nothing but a valid `Hello` gets through.
    fn authenticate(&mut self, req: IpcRequest) -> IpcResponse {
        let IpcRequest::Hello {
            token,
            protocol_version,
        } = req
        else {
            return err("unauthenticated", "must send Hello first");
        };
        // No token means no way in: a session with nothing to compare against refuses
        // rather than accepting anything.
        let Some(expected) = self.expected_token.as_deref() else {
            return err("unauthorized", "this transport does not accept tokens");
        };
        if !constant_time_eq(token.as_bytes(), expected.as_bytes()) {
            return err("unauthorized", "invalid auth token");
        }
        if protocol_version != PROTOCOL_VERSION {
            return err(
                "protocol_mismatch",
                &format!("agent speaks v{PROTOCOL_VERSION}, client sent v{protocol_version}"),
            );
        }
        self.authenticated = true;
        IpcResponse::HelloOk {
            agent_version: env!("CARGO_PKG_VERSION").to_string(),
            protocol_version: PROTOCOL_VERSION,
        }
    }

    /// Whether this session may make this request at all.
    ///
    /// The single place both gates live. A relay handled by intercepting the message in
    /// each transport would need this repeated per transport, and the copy that was
    /// forgotten would be the way in.
    fn check_gate(&self, req: &IpcRequest) -> Result<(), IpcResponse> {
        match gate_for(req) {
            Gate::Open => Ok(()),
            Gate::Permission(needed) if needed.granted_by(&self.permissions) => Ok(()),
            Gate::Permission(needed) => Err(err(
                // Named, so an operator reading the peer's log knows which grant to
                // change rather than only that something was refused.
                "not_permitted",
                &format!(
                    "this device does not allow {:?} for the peer on this session",
                    needed.name()
                ),
            )),
            Gate::LocalOnly if self.origin == Origin::Local => Ok(()),
            Gate::LocalOnly => Err(err(
                "local_only",
                "this request may only be made by a process on the machine itself",
            )),
        }
    }

    /// Carry out a request that has already been authenticated and authorised.
    fn act(&mut self, req: IpcRequest, backends: &Backends<'_>) -> IpcResponse {
        let injector = backends.injector;
        match req {
            // Both handled before `act` is reached.
            IpcRequest::Hello { .. } | IpcRequest::AskPeer { .. } => err(
                "unreachable",
                "this request is handled before dispatch reaches the backends",
            ),
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
            IpcRequest::ListMonitors => match backends.monitors.monitors() {
                Ok(monitors) => IpcResponse::Monitors { monitors },
                Err(message) => err("monitors_unavailable", &message),
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

    struct MockMonitors(Result<Vec<Monitor>, String>);
    impl MonitorInventory for MockMonitors {
        fn monitors(&self) -> Result<Vec<Monitor>, String> {
            self.0.clone()
        }
    }

    /// The monitor backend most tests do not care about.
    fn no_monitors() -> MockMonitors {
        MockMonitors(Ok(Vec::new()))
    }

    fn backends<'a>(injector: &'a dyn Injector, audio: &'a dyn AudioInventory) -> Backends<'a> {
        Backends {
            injector,
            audio,
            // Leaked so the borrow can outlive this call without every test having to
            // declare a monitor backend it does not use. Test-only, and one small
            // allocation per dispatch.
            monitors: Box::leak(Box::new(no_monitors())),
        }
    }

    fn backends_with<'a>(
        injector: &'a dyn Injector,
        audio: &'a dyn AudioInventory,
        monitors: &'a dyn MonitorInventory,
    ) -> Backends<'a> {
        Backends {
            injector,
            audio,
            monitors,
        }
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
        // `gate_for` has no wildcard arm, so this is really a reminder of why: adding a
        // request must not silently inherit "costs nothing".
        let ungated = [
            IpcRequest::Ping,
            IpcRequest::ReleaseAllInput,
            IpcRequest::Hello {
                token: String::new(),
                protocol_version: PROTOCOL_VERSION,
            },
        ];
        for req in ungated {
            assert_eq!(gate_for(&req), Gate::Open, "{req:?} should be open");
        }
        assert_eq!(
            gate_for(&IpcRequest::ListAudioDevices),
            Gate::Permission(Needed::ReadDevices)
        );
        assert_eq!(
            gate_for(&IpcRequest::ListMonitors),
            Gate::Permission(Needed::ReadDevices)
        );
        assert_eq!(
            gate_for(&IpcRequest::EnumerateWindows),
            Gate::Permission(Needed::ListWindows)
        );
        assert_eq!(
            gate_for(&IpcRequest::InjectScroll {
                delta_x: 0,
                delta_y: 120
            }),
            Gate::Permission(Needed::ControlInput)
        );
        assert_eq!(
            gate_for(&IpcRequest::AskPeer {
                peer: ultidesk_identity::Identity::generate().public(),
                query: PeerQuery::Monitors,
            }),
            Gate::LocalOnly,
            "relaying is not a permission an operator can grant a peer"
        );
    }

    fn a_monitor(device_id: ultidesk_core::DeviceId, name: &str, x: f64) -> Monitor {
        Monitor {
            device_id,
            monitor_id: ultidesk_topology::MonitorId(1),
            friendly_name: name.to_string(),
            logical_x: x,
            logical_y: 0.0,
            logical_width: 1920.0,
            logical_height: 1080.0,
            native_pixel_width: 1920,
            native_pixel_height: 1080,
            scale_factor: 1.0,
            rotation: ultidesk_topology::Rotation::Landscape,
            refresh_rate: None,
            primary: true,
        }
    }

    #[test]
    fn an_authenticated_session_gets_the_monitors() {
        let (mut s, inj, audio) = authed();
        let id = ultidesk_core::DeviceId::new();
        let monitors = MockMonitors(Ok(vec![
            a_monitor(id, "eDP-1", 0.0),
            a_monitor(id, "HDMI-A-1", 1920.0),
        ]));

        match s.handle(
            IpcRequest::ListMonitors,
            &backends_with(&inj, &audio, &monitors),
        ) {
            IpcResponse::Monitors { monitors } => {
                assert_eq!(monitors.len(), 2);
                assert_eq!(monitors[1].logical_x, 1920.0);
                assert!(monitors.iter().all(|m| m.device_id == id));
            }
            other => panic!("expected Monitors, got {other:?}"),
        }
    }

    #[test]
    fn a_machine_that_cannot_read_its_monitors_says_so() {
        // A headless session is a real state, and it is not the same answer as "no
        // screens" — an operator debugging an empty editor needs to tell them apart.
        let (mut s, inj, audio) = authed();
        let monitors = MockMonitors(Err("could not connect to the Wayland compositor".into()));

        match s.handle(
            IpcRequest::ListMonitors,
            &backends_with(&inj, &audio, &monitors),
        ) {
            IpcResponse::Error { code, message } => {
                assert_eq!(code, "monitors_unavailable");
                assert!(message.contains("Wayland"), "{message}");
            }
            other => panic!("expected an error, got {other:?}"),
        }
    }

    #[test]
    fn a_peer_without_read_devices_cannot_list_monitors_either() {
        // Screens and speakers are one decision: both describe what this machine has.
        let mut s = peer_with(Permissions {
            control_input: true,
            read_devices: false,
            list_windows: true,
        });
        let inj = MockInjector::default();
        let audio = MockAudio(Ok(Vec::new()));
        let monitors = MockMonitors(Ok(vec![a_monitor(
            ultidesk_core::DeviceId::new(),
            "eDP-1",
            0.0,
        )]));

        let r = s.handle(
            IpcRequest::ListMonitors,
            &backends_with(&inj, &audio, &monitors),
        );
        assert!(
            matches!(r, IpcResponse::Error { ref code, .. } if code == "not_permitted"),
            "{r:?}"
        );
    }

    #[test]
    fn a_peer_cannot_use_this_machine_as_a_hop_to_a_third() {
        // The reason relaying is not a permission: a peer that could relay would reach
        // machines it was never paired with, wearing this machine's trust to do it. No
        // grant should be able to express that, so a fully trusted peer is still refused.
        let mut s = peer_with(Permissions::all());
        let inj = MockInjector::default();
        let audio = MockAudio(Ok(Vec::new()));
        let third = ultidesk_identity::Identity::generate().public();

        let r = s.handle(
            IpcRequest::AskPeer {
                peer: third,
                query: PeerQuery::Monitors,
            },
            &backends(&inj, &audio),
        );
        match r {
            IpcResponse::Error { code, .. } => assert_eq!(code, "local_only"),
            other => panic!("a peer must not be able to relay: {other:?}"),
        }
    }

    #[test]
    fn a_local_caller_may_relay_and_the_work_is_handed_back() {
        // The dispatcher authorises but cannot perform it: answering needs a network
        // round trip, and the gate is deliberately synchronous so it can be tested
        // without a runtime.
        let (mut s, inj, audio) = authed();
        let peer = ultidesk_identity::Identity::generate().public();

        match s.dispatch(
            IpcRequest::AskPeer {
                peer,
                query: PeerQuery::AudioDevices,
            },
            &backends(&inj, &audio),
        ) {
            Dispatch::AskPeer { peer: asked, query } => {
                assert_eq!(asked, peer);
                assert_eq!(query, PeerQuery::AudioDevices);
            }
            Dispatch::Reply(other) => panic!("expected a relay work order, got {other:?}"),
        }
    }

    #[test]
    fn a_transport_that_cannot_relay_says_so_rather_than_pretending() {
        // `handle` is the synchronous entry point; a relay reaching it means a transport
        // was wired up wrong, and that should be visible rather than silently answered.
        let (mut s, inj, audio) = authed();
        let r = s.handle(
            IpcRequest::AskPeer {
                peer: ultidesk_identity::Identity::generate().public(),
                query: PeerQuery::Monitors,
            },
            &backends(&inj, &audio),
        );
        assert!(
            matches!(r, IpcResponse::Error { ref code, .. } if code == "relay_unsupported"),
            "{r:?}"
        );
    }

    #[test]
    fn constant_time_eq_basic() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
    }
}
