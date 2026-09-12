//! The local IPC wire protocol: what a client may ask the agent, and what it answers.
//!
//! These types were inside the agent binary, which meant nothing else could speak them —
//! including the control app, whose peer panels stayed placeholders for want of a message
//! it could name. They are pure data with no I/O, so they belong in a library both ends
//! depend on.
//!
//! Deciding *whether* a request is allowed, and carrying it out, stays in the agent
//! (`ipc.rs` there). A client that could see the gate would still not be the one running
//! it — the enforcement is source-side, and moving the types does not move that.

use serde::{Deserialize, Serialize};
use ultidesk_identity::PeerKey;
use ultidesk_platform_windows::inject::{MouseButton, VirtualScreen};
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
