//! The IPC endpoint descriptor and its on-disk handshake file.
//!
//! At startup the agent picks a listening address and generates a per-launch auth token,
//! then writes both to a handshake file that only the launching desktop app is expected
//! to read. The app reads the file, connects, and presents the token.
//!
//! # One field, two transports
//! The address is a named-pipe path on Windows and a Unix-socket path on Linux. It is
//! **one** field rather than two because a client opens both the same way — `net.connect(path)`
//! in Node, `UnixStream`/`NamedPipeClient` in Rust — so a second field would only be a
//! discriminant that nothing needs to branch on. The field was called `pipe_name` while
//! the pipe was the only transport; renaming it is a breaking change to this file's
//! shape, and the desktop client and the bench script were updated with it.
//!
//! # The file holds a secret
//! The token is in it, so on Unix it is written `0600`, in a directory the platform
//! already restricts (`XDG_RUNTIME_DIR`, mode `0700`). On Windows the equivalent ACL
//! hardening is still outstanding and tracked in docs/threat-model.md — the token is the
//! enforced control there, and this file being under `LOCALAPPDATA` is not the same
//! protection.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use ultidesk_identity::file::{write_atomic, Visibility};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Endpoint {
    /// Where to connect: a named-pipe path on Windows, a Unix-socket path on Linux.
    pub endpoint_path: String,
    pub token: String,
    pub protocol_version: u32,
    pub pid: u32,
}

impl Endpoint {
    /// The descriptor for this launch.
    ///
    /// On Windows the pipe name carries a random suffix, because the pipe namespace is
    /// machine-global and a fixed name can be squatted by another process. On Unix the
    /// socket lives in a per-user directory the kernel already guards, so a fixed name
    /// is what lets a client find the agent without being told where it is.
    pub fn generate() -> Self {
        Endpoint {
            endpoint_path: default_endpoint_path(),
            token: Uuid::new_v4().simple().to_string(),
            protocol_version: ultidesk_core::protocol::PROTOCOL_VERSION,
            pid: std::process::id(),
        }
    }
}

#[cfg(windows)]
fn default_endpoint_path() -> String {
    let id = Uuid::new_v4().simple().to_string();
    format!(r"\\.\pipe\ultidesk-agent-{id}")
}

#[cfg(unix)]
fn default_endpoint_path() -> String {
    socket_path().to_string_lossy().into_owned()
}

#[cfg(not(any(windows, unix)))]
fn default_endpoint_path() -> String {
    String::new()
}

/// Per-user, per-session directory for runtime state. See `ultidesk_core::paths`.
pub fn runtime_dir() -> PathBuf {
    ultidesk_core::paths::runtime_dir()
}

/// The Unix socket the agent listens on.
#[cfg(unix)]
pub fn socket_path() -> PathBuf {
    runtime_dir().join("agent.sock")
}

pub fn handshake_path() -> PathBuf {
    runtime_dir().join("agent-endpoint.json")
}

/// Read the descriptor a running agent left behind.
///
/// A `NotFound` from here is the ordinary "no agent is running" case, and the caller is
/// expected to say so rather than treat it as a fault.
pub fn read_handshake(path: &std::path::Path) -> std::io::Result<Endpoint> {
    let raw = std::fs::read_to_string(path)?;
    serde_json::from_str(&raw).map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("{} is not a handshake file: {e}", path.display()),
        )
    })
}

pub fn write_handshake(ep: &Endpoint) -> std::io::Result<PathBuf> {
    let path = handshake_path();
    let json = serde_json::to_string_pretty(ep)?;
    // Private, because the token is in it.
    write_atomic(&path, json.as_bytes(), Visibility::Private)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_launch_gets_a_distinct_token() {
        // The token is the local IPC's only authentication. Two launches sharing one
        // would mean a client that captured it once could talk to a later agent.
        let a = Endpoint::generate();
        let b = Endpoint::generate();
        assert_ne!(a.token, b.token);
        assert_eq!(a.token.len(), 32, "a uuid's simple form");
    }

    #[test]
    fn the_endpoint_path_is_not_empty_on_a_supported_platform() {
        let ep = Endpoint::generate();
        assert!(!ep.endpoint_path.is_empty());
        assert_eq!(
            ep.protocol_version,
            ultidesk_core::protocol::PROTOCOL_VERSION
        );
        assert_eq!(ep.pid, std::process::id());
    }

    #[cfg(windows)]
    #[test]
    fn the_pipe_name_is_unique_per_launch() {
        // The pipe namespace is machine-global, so a fixed name can be squatted.
        assert_ne!(
            Endpoint::generate().endpoint_path,
            Endpoint::generate().endpoint_path
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_socket_path_is_stable_so_a_client_can_find_it() {
        // The opposite choice from Windows, and deliberate: the directory is already
        // per-user and mode 0700, so the path does not need to be unguessable — it needs
        // to be findable without being told.
        assert_eq!(
            Endpoint::generate().endpoint_path,
            Endpoint::generate().endpoint_path
        );
        assert!(Endpoint::generate().endpoint_path.ends_with("agent.sock"));
    }

    #[test]
    fn the_handshake_lives_beside_the_socket() {
        assert_eq!(handshake_path().parent(), Some(runtime_dir().as_path()));
    }
}
