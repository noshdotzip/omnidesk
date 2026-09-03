//! **Dev-only** TCP peer transport.
//!
//! Carries the same line-delimited JSON protocol as the local named-pipe IPC, but
//! between two machines, so a peer can drive this agent's [`Injector`]. That is the
//! whole KVM path: capture on one machine, inject on the other.
//!
//! # This is not the Milestone-1 secure channel
//! ADR-0002 specifies an authenticated QUIC/TLS control channel with device identity,
//! pairing and per-peer permissions. **None of that is here.** This transport is
//! plaintext TCP gated only by the per-launch token, which means:
//!
//! - anyone who can read the token can drive the pointer and keyboard;
//! - anyone on the path can read every keystroke it carries.
//!
//! It exists to prove the end-to-end path on a trusted LAN and is therefore behind an
//! explicit subcommand that prints a warning, never the default. It must be deleted or
//! replaced when the real control channel lands — not quietly promoted.

use crate::ipc::{Injector, IpcRequest, IpcResponse, Session};
use anyhow::Context;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use ultidesk_core::protocol::MAX_MESSAGE_BYTES;

/// Listen for peers and serve each one until it disconnects.
pub async fn serve<I>(bind: String, token: String, injector: Arc<I>) -> anyhow::Result<()>
where
    I: Injector + Send + Sync + 'static,
{
    let listener = TcpListener::bind(&bind)
        .await
        .with_context(|| format!("failed to bind {bind}"))?;
    tracing::info!(bind = %bind, "dev peer transport listening (PLAINTEXT, token-gated)");

    loop {
        let (stream, peer) = listener.accept().await.context("accept failed")?;
        // The KVM path is latency-sensitive and its messages are tiny, which is exactly
        // the case Nagle's algorithm penalises.
        if let Err(e) = stream.set_nodelay(true) {
            tracing::warn!(error = %e, "could not disable Nagle on peer socket");
        }
        let token = token.clone();
        let injector = injector.clone();
        tokio::spawn(async move {
            tracing::info!(peer = %peer, "peer connected");
            if let Err(e) = handle_connection(stream, &token, injector.as_ref()).await {
                tracing::warn!(error = %e, "peer connection ended with error");
            }
            tracing::info!(peer = %peer, "peer disconnected");
        });
    }
}

async fn handle_connection<I: Injector>(
    stream: TcpStream,
    token: &str,
    injector: &I,
) -> anyhow::Result<()> {
    let (read_half, mut write_half) = tokio::io::split(stream);
    let mut reader = BufReader::new(read_half);
    let mut session = Session::new();
    let mut line = String::new();

    let result = loop {
        line.clear();
        let n = match reader.read_line(&mut line).await {
            Ok(n) => n,
            Err(e) => break Err(anyhow::Error::from(e)),
        };
        if n == 0 {
            break Ok(()); // EOF
        }
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
            Ok(req) => session.handle(req, token, injector),
            Err(e) => IpcResponse::Error {
                code: "bad_request".into(),
                message: format!("invalid request json: {e}"),
            },
        };
        if let Err(e) = write_response(&mut write_half, &response).await {
            break Err(e);
        }
    };

    // However the peer went away — clean disconnect, crash, or cable pull — anything it
    // was holding must be released, or a modifier stays stuck on this machine.
    let released = session.release_all(injector);
    if released > 0 {
        tracing::info!(released, "released held input after peer disconnect");
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

// ---- client side -----------------------------------------------------------------

/// Mirror this machine's real pointer onto a peer.
///
/// Reads the local cursor and forwards its position, mapped proportionally onto the
/// peer's screen. Unlike a true KVM handoff this **does not take over the local
/// pointer**: nothing is swallowed and nothing is warped, so the operator keeps full
/// control of this machine while the remote cursor follows along. That makes it safe
/// to run on a machine someone is using.
///
/// Handoff — where local input is grabbed and the local cursor is hidden — needs
/// low-level hooks and an emergency-release hotkey, neither of which exists yet.
pub async fn kvm_mirror(
    addr: &str,
    token: &str,
    remote_w: f64,
    remote_h: f64,
    seconds: u64,
) -> anyhow::Result<()> {
    use crate::ipc::VirtualScreenDto;
    use ultidesk_platform_windows::cursor::{cursor_position, virtual_screen};
    use ultidesk_topology::mapping::map_edge_crossing;

    let vs = virtual_screen()
        .ok_or_else(|| anyhow::anyhow!("could not read the virtual screen bounds"))?;
    println!(
        "local virtual screen: {}x{} at ({},{})",
        vs.width, vs.height, vs.left, vs.top
    );
    println!("remote screen: {remote_w}x{remote_h}");

    let stream = TcpStream::connect(addr)
        .await
        .with_context(|| format!("could not connect to peer at {addr}"))?;
    stream.set_nodelay(true).ok();
    let (read_half, mut write_half) = tokio::io::split(stream);
    let mut reader = BufReader::new(read_half);
    let mut line = String::new();

    let mut out = serde_json::to_string(&IpcRequest::Hello {
        token: token.to_string(),
        protocol_version: ultidesk_core::protocol::PROTOCOL_VERSION,
    })?;
    out.push('\n');
    write_half.write_all(out.as_bytes()).await?;
    write_half.flush().await?;
    reader.read_line(&mut line).await?;
    if !line.contains("HelloOk") {
        anyhow::bail!("peer rejected the handshake: {}", line.trim());
    }
    println!("handshake accepted; mirroring for {seconds}s — move your mouse");

    let remote_vs = VirtualScreenDto {
        left: 0,
        top: 0,
        width: remote_w as i32,
        height: remote_h as i32,
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(seconds);
    let mut last_sent: Option<(i32, i32)> = None;
    let mut sent = 0u64;

    while std::time::Instant::now() < deadline {
        if let Some((lx, ly)) = cursor_position() {
            let rx = map_edge_crossing((lx - vs.left) as f64, vs.width as f64, remote_w) as i32;
            let ry = map_edge_crossing((ly - vs.top) as f64, vs.height as f64, remote_h) as i32;
            // Only send on change: a still pointer should cost nothing on the wire.
            if last_sent != Some((rx, ry)) {
                let mut msg = serde_json::to_string(&IpcRequest::InjectMouseMove {
                    screen_x: rx,
                    screen_y: ry,
                    virtual_screen: remote_vs,
                })?;
                msg.push('\n');
                write_half.write_all(msg.as_bytes()).await?;
                write_half.flush().await?;
                line.clear();
                if reader.read_line(&mut line).await? == 0 {
                    anyhow::bail!("peer closed the connection");
                }
                if !line.contains("Injected") {
                    anyhow::bail!("peer refused a move: {}", line.trim());
                }
                last_sent = Some((rx, ry));
                sent += 1;
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(8)).await;
    }

    println!("mirrored {sent} pointer updates");
    Ok(())
}

/// A minimal peer client that drives the remote pointer through a visible square.
///
/// Deliberately motion-only: no clicks and no keystrokes, because those would land in
/// whatever window has focus on the remote machine.
pub async fn kvm_demo(addr: &str, token: &str, size: i32) -> anyhow::Result<()> {
    use crate::ipc::VirtualScreenDto;

    let stream = TcpStream::connect(addr)
        .await
        .with_context(|| format!("could not connect to peer at {addr}"))?;
    stream.set_nodelay(true).ok();
    let (read_half, mut write_half) = tokio::io::split(stream);
    let mut reader = BufReader::new(read_half);
    let mut line = String::new();

    async fn round_trip<R, W>(
        reader: &mut BufReader<R>,
        writer: &mut W,
        line: &mut String,
        req: &IpcRequest,
    ) -> anyhow::Result<String>
    where
        R: tokio::io::AsyncRead + Unpin,
        W: AsyncWriteExt + Unpin,
    {
        let mut out = serde_json::to_string(req)?;
        out.push('\n');
        writer.write_all(out.as_bytes()).await?;
        writer.flush().await?;
        line.clear();
        let n = reader.read_line(line).await?;
        if n == 0 {
            anyhow::bail!("peer closed the connection");
        }
        Ok(line.trim().to_string())
    }

    let hello = round_trip(
        &mut reader,
        &mut write_half,
        &mut line,
        &IpcRequest::Hello {
            token: token.to_string(),
            protocol_version: ultidesk_core::protocol::PROTOCOL_VERSION,
        },
    )
    .await?;
    if !hello.contains("HelloOk") {
        anyhow::bail!("peer rejected the handshake: {hello}");
    }
    tracing::info!("peer handshake accepted");

    let vs = VirtualScreenDto {
        left: 0,
        top: 0,
        width: 1920,
        height: 1080,
    };
    // A closed loop, so the remote pointer ends where this walk started.
    let origin = (400, 300);
    let corners = [
        (origin.0, origin.1),
        (origin.0 + size, origin.1),
        (origin.0 + size, origin.1 + size),
        (origin.0, origin.1 + size),
        (origin.0, origin.1),
    ];

    for (x, y) in corners {
        let resp = round_trip(
            &mut reader,
            &mut write_half,
            &mut line,
            &IpcRequest::InjectMouseMove {
                screen_x: x,
                screen_y: y,
                virtual_screen: vs,
            },
        )
        .await?;
        if !resp.contains("Injected") {
            anyhow::bail!("peer refused the move: {resp}");
        }
        println!("  moved remote pointer to ({x}, {y})");
        tokio::time::sleep(std::time::Duration::from_millis(180)).await;
    }

    let released = round_trip(
        &mut reader,
        &mut write_half,
        &mut line,
        &IpcRequest::ReleaseAllInput,
    )
    .await?;
    tracing::info!(response = %released, "released remote input");
    Ok(())
}
