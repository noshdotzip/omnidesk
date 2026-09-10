//! Unix-socket transport for local IPC — the Linux counterpart to `pipe.rs`.
//!
//! Same line-delimited JSON, same [`Session`] dispatch, same release-everything-when-the
//! -connection-drops rule. Only the door is different. Until this existed the agent had
//! no local IPC on Linux at all, so the control app could not ask it anything and every
//! peer-side panel was a placeholder.
//!
//! # The directory is the access control, not the token
//! The socket lives in `$XDG_RUNTIME_DIR/ultidesk/`, which `pam_systemd` creates mode
//! `0700` and owned by the user. The kernel checks that on every `connect()`, so another
//! user on the machine cannot reach the socket at all — which is *stronger* than the
//! Windows named pipe, where the per-launch token is the only gate and ACL restriction is
//! still tracked debt. The token is kept anyway: it is defence in depth, and it is what
//! distinguishes the desktop app from any other process running as the same user.
//!
//! # Stale socket files, and why they are not simply deleted
//! `bind()` fails with `EADDRINUSE` if the path already exists, so something has to
//! remove it. Unconditionally deleting it is the obvious fix and is wrong: if an agent is
//! *running*, the unlink silently steals its socket. The old agent keeps its open file
//! descriptor and carries on believing it is reachable, while every new client connects
//! to the replacement — two agents, both convinced they own the session, and no error
//! anywhere.
//!
//! So the path is probed first. A successful connect means a live agent and this one
//! refuses to start; `ECONNREFUSED` means the file outlived its process and is safe to
//! remove.

use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use ultidesk_core::protocol::MAX_MESSAGE_BYTES;

use crate::ipc::{Injector, IpcRequest, IpcResponse, Session};

/// `sockaddr_un.sun_path` is a fixed 108-byte array on Linux, NUL terminated.
///
/// Exceeding it does not truncate helpfully — `bind` fails, or worse binds a shortened
/// path — and the resulting error says nothing about length. A deep `TMPDIR` is enough
/// to hit it, so the limit is checked here where the message can name the real problem.
const MAX_SOCKET_PATH: usize = 107;

/// A bound listener that removes its socket file when dropped.
///
/// `Debug` prints the path rather than the socket, so a test that unwraps a `bind`
/// result — or a diagnostic that logs one — says *which* socket it is talking about.
#[derive(Debug)]
pub struct Listener {
    listener: UnixListener,
    path: PathBuf,
}

impl Drop for Listener {
    fn drop(&mut self) {
        // Best effort. A socket file left behind is not fatal — the next launch detects
        // it as stale — but leaving one is how the "is the agent running?" question gets
        // the wrong answer from a directory listing.
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Claim `path` and start listening on it.
pub fn bind(path: &Path) -> anyhow::Result<Listener> {
    if path.as_os_str().len() > MAX_SOCKET_PATH {
        anyhow::bail!(
            "socket path is {} bytes, over the {MAX_SOCKET_PATH}-byte limit the kernel \
             allows for a Unix socket: {}",
            path.as_os_str().len(),
            path.display()
        );
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("could not create {}", dir.display()))?;
        // Only meaningful on the `/tmp` fallback path — `XDG_RUNTIME_DIR` is already
        // 0700 — but that is exactly the case where it matters, because /tmp is not.
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .with_context(|| format!("could not restrict {}", dir.display()))?;
    }

    clear_stale(path)?;

    let listener =
        UnixListener::bind(path).with_context(|| format!("could not bind {}", path.display()))?;
    // Belt and braces alongside the directory mode: a socket that is only reachable
    // through a 0700 directory does not strictly need this, but a future change to where
    // the socket lives should not silently widen who can open it.
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("could not restrict {}", path.display()))?;

    Ok(Listener {
        listener,
        path: path.to_path_buf(),
    })
}

/// Decide whether an existing socket file belongs to a live agent, and remove it if not.
fn clear_stale(path: &Path) -> anyhow::Result<()> {
    match std::os::unix::net::UnixStream::connect(path) {
        // Something is listening. Refuse rather than take the socket from it.
        Ok(_) => anyhow::bail!(
            "another Ultidesk agent is already listening on {}; stop it first",
            path.display()
        ),
        // The file outlived its process — a crash, a kill -9, a reboot that kept /tmp.
        Err(e) if e.kind() == io::ErrorKind::ConnectionRefused => {
            std::fs::remove_file(path)
                .with_context(|| format!("could not remove stale socket {}", path.display()))?;
            tracing::info!(path = %path.display(), "removed a stale socket file");
            Ok(())
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        // Anything else — a permission problem, a path component that is not a directory
        // — is reported rather than guessed at, because deleting the file is the one
        // action that cannot be undone if the guess is wrong.
        Err(e) => Err(anyhow::Error::from(e))
            .with_context(|| format!("could not probe {}", path.display())),
    }
}

/// Serve clients until the process stops.
pub async fn serve(
    listener: Listener,
    token: String,
    injector: Arc<dyn Injector + Send + Sync>,
) -> anyhow::Result<()> {
    tracing::info!(path = %listener.path.display(), "local IPC listening");
    loop {
        let (stream, _addr) = listener
            .listener
            .accept()
            .await
            .context("accept failed on the IPC socket")?;
        let token = token.clone();
        let injector = injector.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, &token, injector.as_ref()).await {
                tracing::warn!(error = %e, "ipc connection ended with error");
            }
        });
    }
}

async fn handle_connection(
    stream: UnixStream,
    token: &str,
    injector: &(dyn Injector + Send + Sync),
) -> anyhow::Result<()> {
    let (read_half, mut write_half) = tokio::io::split(stream);
    let mut reader = BufReader::new(read_half);
    let mut session = Session::new(token);
    let mut line = String::new();

    let result = loop {
        line.clear();
        let n = match reader.read_line(&mut line).await {
            Ok(n) => n,
            Err(e) => break Err(anyhow::Error::from(e)),
        };
        if n == 0 {
            break Ok(()); // EOF: client disconnected
        }
        // Bounded before anything is parsed, against a buggy or hostile local client.
        if line.len() > MAX_MESSAGE_BYTES {
            let _ = write_response(
                &mut write_half,
                &IpcResponse::Error {
                    code: "too_large".into(),
                    message: "message exceeds maximum size".into(),
                },
            )
            .await;
            break Ok(());
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<IpcRequest>(trimmed) {
            Ok(req) => session.handle(req, injector),
            Err(e) => IpcResponse::Error {
                code: "bad_request".into(),
                message: format!("invalid request json: {e}"),
            },
        };
        if let Err(e) = write_response(&mut write_half, &response).await {
            break Err(e);
        }
    };

    // However the connection ended, release what it was holding. A crashed control app
    // must not be able to leave a modifier down on the machine it was driving.
    let released = session.release_all(injector);
    if released > 0 {
        tracing::info!(released, "released held input on connection close");
    }
    result
}

async fn write_response<W: AsyncWriteExt + Unpin>(
    w: &mut W,
    resp: &IpcResponse,
) -> anyhow::Result<()> {
    let mut out = serde_json::to_string(resp)?;
    out.push('\n');
    w.write_all(out.as_bytes()).await?;
    w.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::WindowDto;
    use ultidesk_core::protocol::PROTOCOL_VERSION;
    use ultidesk_platform_windows::inject::{InputError, MouseButton, VirtualScreen};

    struct NoopInjector;
    impl Injector for NoopInjector {
        fn mouse_move(&self, _: i32, _: i32, _: VirtualScreen) -> Result<(), InputError> {
            Ok(())
        }
        fn mouse_button(&self, _: MouseButton, _: bool) -> Result<(), InputError> {
            Ok(())
        }
        fn key(&self, _: u16, _: bool) -> Result<(), InputError> {
            Ok(())
        }
        fn scroll(&self, _: i32, _: i32) -> Result<(), InputError> {
            Ok(())
        }
        fn enumerate(&self) -> Vec<WindowDto> {
            vec![]
        }
    }

    /// A scratch directory named per test — `cargo test` runs threads in parallel, and
    /// two tests sharing a socket path would each see the other as a live agent.
    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("ultidesk-uds-test-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            TempDir(dir)
        }
        fn socket(&self) -> PathBuf {
            self.0.join("agent.sock")
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[tokio::test]
    async fn loopback_hello_ping_and_auth_enforced() {
        let d = TempDir::new("loopback");
        let path = d.socket();
        let token = "test-token".to_string();

        let listener = bind(&path).unwrap();
        let handle = tokio::spawn(serve(listener, token.clone(), Arc::new(NoopInjector)));

        let client = UnixStream::connect(&path).await.unwrap();
        let (r, mut w) = tokio::io::split(client);
        let mut reader = BufReader::new(r);

        // A command before Hello must be rejected.
        w.write_all(b"{\"type\":\"Ping\"}\n").await.unwrap();
        w.flush().await.unwrap();
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        assert!(line.contains("Error"), "expected auth error, got {line}");

        // The wrong token is refused too, so the directory mode is not doing all the work.
        line.clear();
        let bad = format!(
            "{{\"type\":\"Hello\",\"token\":\"nope\",\"protocol_version\":{PROTOCOL_VERSION}}}\n"
        );
        w.write_all(bad.as_bytes()).await.unwrap();
        w.flush().await.unwrap();
        reader.read_line(&mut line).await.unwrap();
        assert!(line.contains("Error"), "expected token refusal, got {line}");

        line.clear();
        let hello = format!(
            "{{\"type\":\"Hello\",\"token\":\"{token}\",\"protocol_version\":{PROTOCOL_VERSION}}}\n"
        );
        w.write_all(hello.as_bytes()).await.unwrap();
        w.flush().await.unwrap();
        reader.read_line(&mut line).await.unwrap();
        assert!(line.contains("HelloOk"), "expected HelloOk, got {line}");

        line.clear();
        w.write_all(b"{\"type\":\"Ping\"}\n").await.unwrap();
        w.flush().await.unwrap();
        reader.read_line(&mut line).await.unwrap();
        assert!(line.contains("Pong"), "expected Pong, got {line}");

        handle.abort();
    }

    #[tokio::test]
    async fn the_socket_is_reachable_only_by_this_user() {
        let d = TempDir::new("mode");
        let path = d.socket();
        let listener = bind(&path).unwrap();

        let dir_mode = std::fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        let sock_mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(dir_mode, 0o700, "directory was {dir_mode:o}");
        assert_eq!(sock_mode, 0o600, "socket was {sock_mode:o}");
        drop(listener);
    }

    #[tokio::test]
    async fn a_stale_socket_file_is_replaced() {
        // What a crash or a `kill -9` leaves behind: closing a listener releases the
        // socket but not the directory entry, so the file survives with nothing on the
        // other end of it. The next launch must not be blocked by that.
        let d = TempDir::new("stale");
        let path = d.socket();
        drop(std::os::unix::net::UnixListener::bind(&path).unwrap());
        assert!(path.exists(), "the file outlives the listener");

        let listener = bind(&path).expect("a stale file must not block a new listener");
        assert!(path.exists());

        // And the replacement is actually usable, which a bind that merely succeeded
        // would not prove.
        UnixStream::connect(&path)
            .await
            .expect("the replacement socket accepts connections");
        drop(listener);
    }

    #[tokio::test]
    async fn a_live_socket_is_not_stolen_from_the_agent_holding_it() {
        // The failure this prevents: two agents both believing they own the session.
        let d = TempDir::new("live");
        let path = d.socket();
        let first = bind(&path).unwrap();

        let err = bind(&path).expect_err("a second agent must refuse to start");
        let message = err.to_string();
        assert!(message.contains("already listening"), "{message}");
        drop(first);
    }

    #[tokio::test]
    async fn the_socket_file_is_removed_when_the_listener_goes_away() {
        let d = TempDir::new("cleanup");
        let path = d.socket();
        let listener = bind(&path).unwrap();
        assert!(path.exists());
        drop(listener);
        assert!(
            !path.exists(),
            "a socket file left behind lies about the agent"
        );
    }

    #[tokio::test]
    async fn an_over_long_path_is_refused_with_a_message_that_says_why() {
        // The kernel's error for this says nothing about length, and a deep TMPDIR is
        // enough to reach it.
        let d = TempDir::new("long");
        let path = d.0.join("x".repeat(200));
        let err = bind(&path).expect_err("an over-long path must be refused");
        assert!(err.to_string().contains("byte limit"), "{err}");
    }
}
