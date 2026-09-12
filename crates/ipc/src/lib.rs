//! `ultidesk-ipc` — the local IPC contract, and a client that speaks it.
//!
//! The agent listens; the control app (and anything else running as the same user) asks.
//! Both halves need the same message shapes and the same idea of where to connect, so
//! they live here rather than inside the agent binary — which is why the control app's
//! peer panels were placeholders: not for want of an answer, but for want of a message it
//! could name.
//!
//! What is deliberately *not* here is the decision of whether a request is allowed. That
//! is enforced on the machine providing the capability (docs/permissions.md), so it lives
//! in the agent with the thing it protects. A client knowing the shape of a request has
//! never been what authorises it.

pub mod client;
pub mod endpoint;
pub mod message;

pub use client::{Client, ClientError};
pub use endpoint::{handshake_path, read_handshake, runtime_dir, write_handshake, Endpoint};
pub use message::{
    IpcRequest, IpcResponse, MouseButtonDto, PeerQuery, VirtualScreenDto, WindowDto,
};
