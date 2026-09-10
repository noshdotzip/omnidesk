//! Windows named-pipe transport for local IPC.
//!
//! One connection is handled at a time end-to-end (read request → dispatch → write
//! response) which is sufficient for a single desktop-app client and keeps the session
//! state simple. When a connection drops, held input is released so a crash of the
//! desktop app can never leave a modifier stuck on the source machine.
//!
//! (Included only on Windows via `#[cfg(windows)] mod pipe;` in main.rs.)

use crate::ipc::{IpcRequest, IpcResponse, LocalBackends, Session};
use anyhow::Context;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use ultidesk_core::protocol::MAX_MESSAGE_BYTES;

/// Serve the named pipe forever, accepting one client at a time.
pub async fn serve(
    pipe_name: String,
    token: String,
    backends: Arc<LocalBackends>,
) -> anyhow::Result<()> {
    // Claim the pipe name with the first instance, then keep one instance pre-created
    // and waiting so there is never a window where a client gets ERROR_FILE_NOT_FOUND.
    let mut server = ServerOptions::new()
        .first_pipe_instance(true)
        .create(&pipe_name)
        .with_context(|| format!("failed to create pipe {pipe_name}"))?;

    loop {
        server.connect().await.context("pipe connect failed")?;
        let connected = server;
        // Pre-create the next instance before handling this one.
        server = ServerOptions::new()
            .create(&pipe_name)
            .context("failed to create next pipe instance")?;

        let token = token.clone();
        let backends = backends.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_connection(connected, &token, backends.as_ref()).await {
                tracing::warn!(error = %e, "ipc connection ended with error");
            }
        });
    }
}

async fn handle_connection(
    server: NamedPipeServer,
    token: &str,
    backends: &LocalBackends,
) -> anyhow::Result<()> {
    let (read_half, mut write_half) = tokio::io::split(server);
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
        // Bound message size against a buggy/hostile local client.
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
            Ok(req) => session.handle(req, &backends.borrow()),
            Err(e) => IpcResponse::Error {
                code: "bad_request".into(),
                message: format!("invalid request json: {e}"),
            },
        };
        if let Err(e) = write_response(&mut write_half, &response).await {
            break Err(e);
        }
    };

    // Always release held input when the connection ends, however it ended.
    let released = session.release_all(backends.injector.as_ref());
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
    use crate::ipc::{AudioInventory, Injector, MonitorInventory, WindowDto};
    use tokio::net::windows::named_pipe::ClientOptions;
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

    struct NoAudio;
    impl AudioInventory for NoAudio {
        fn devices(&self) -> Result<Vec<ultidesk_topology::AudioDevice>, String> {
            Ok(Vec::new())
        }
    }

    struct NoMonitors;
    impl MonitorInventory for NoMonitors {
        fn monitors(&self) -> Result<Vec<ultidesk_topology::Monitor>, String> {
            Ok(Vec::new())
        }
    }

    async fn open_with_retry(name: &str) -> tokio::net::windows::named_pipe::NamedPipeClient {
        for _ in 0..50 {
            match ClientOptions::new().open(name) {
                Ok(c) => return c,
                Err(_) => tokio::time::sleep(std::time::Duration::from_millis(20)).await,
            }
        }
        panic!("could not connect to pipe");
    }

    #[tokio::test]
    async fn loopback_hello_ping_and_auth_enforced() {
        let name = format!(r"\\.\pipe\ultidesk-test-{}", uuid::Uuid::new_v4().simple());
        let token = "test-token".to_string();
        let backends = Arc::new(LocalBackends {
            injector: Arc::new(NoopInjector),
            audio: Arc::new(NoAudio),
            monitors: Arc::new(NoMonitors),
        });

        let srv_name = name.clone();
        let srv_token = token.clone();
        let handle = tokio::spawn(async move { serve(srv_name, srv_token, backends).await });

        let client = open_with_retry(&name).await;
        let (r, mut w) = tokio::io::split(client);
        let mut reader = BufReader::new(r);

        // A command before Hello must be rejected.
        w.write_all(b"{\"type\":\"Ping\"}\n").await.unwrap();
        w.flush().await.unwrap();
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        assert!(line.contains("Error"), "expected auth error, got {line}");

        // Hello with the right token succeeds.
        line.clear();
        let hello = format!(
            "{{\"type\":\"Hello\",\"token\":\"{token}\",\"protocol_version\":{PROTOCOL_VERSION}}}\n"
        );
        w.write_all(hello.as_bytes()).await.unwrap();
        w.flush().await.unwrap();
        reader.read_line(&mut line).await.unwrap();
        assert!(line.contains("HelloOk"), "expected HelloOk, got {line}");

        // Now Ping works.
        line.clear();
        w.write_all(b"{\"type\":\"Ping\"}\n").await.unwrap();
        w.flush().await.unwrap();
        reader.read_line(&mut line).await.unwrap();
        assert!(line.contains("Pong"), "expected Pong, got {line}");

        handle.abort();
    }
}
