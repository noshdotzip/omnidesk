//! The IPC endpoint descriptor and its on-disk handshake file.
//!
//! At startup the agent generates a random pipe name and a per-launch auth token, then
//! writes them to a handshake file that only the launching desktop app is expected to
//! read. The desktop app reads the file, connects to the pipe, and presents the token.
//!
//! Security note (tracked in docs/threat-model.md): the token gates the pipe, but the
//! handshake file and pipe should *also* be ACL-restricted to the current user. That
//! OS-level hardening is a follow-up; today the token is the enforced control and the
//! handshake file is written under a per-user directory.

// Transport-gated, not dead: the only IPC transport that exists today is the Windows
// named pipe (`#[cfg(windows)] mod pipe`), so on other platforms nothing constructs
// this module's types and every item reads as dead code under `-D warnings`. The
// logic is deliberately platform-independent and stays compiled and unit-tested
// everywhere, ready for the Linux transport (Milestone 9, docs/status.md).
#![cfg_attr(not(windows), allow(dead_code))]

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Endpoint {
    pub pipe_name: String,
    pub token: String,
    pub protocol_version: u32,
    pub pid: u32,
}

impl Endpoint {
    pub fn generate() -> Self {
        let id = Uuid::new_v4().simple().to_string();
        Endpoint {
            // Local RPC pipe namespace. The random suffix avoids collisions/squatting.
            pipe_name: format!(r"\\.\pipe\ultidesk-agent-{id}"),
            token: Uuid::new_v4().simple().to_string(),
            protocol_version: ultidesk_core::protocol::PROTOCOL_VERSION,
            pid: std::process::id(),
        }
    }
}

/// Per-user directory for Ultidesk runtime state. Uses `LOCALAPPDATA` on Windows and
/// falls back to a dev directory so `cargo run` works from a checkout.
pub fn runtime_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("ULTIDESK_DEV_DIR") {
        return PathBuf::from(dir);
    }
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        return PathBuf::from(local).join("Ultidesk");
    }
    std::env::temp_dir().join("Ultidesk")
}

pub fn handshake_path() -> PathBuf {
    runtime_dir().join("agent-endpoint.json")
}

pub fn write_handshake(ep: &Endpoint) -> std::io::Result<PathBuf> {
    let dir = runtime_dir();
    std::fs::create_dir_all(&dir)?;
    let path = handshake_path();
    let json = serde_json::to_string_pretty(ep)?;
    std::fs::write(&path, json)?;
    Ok(path)
}
