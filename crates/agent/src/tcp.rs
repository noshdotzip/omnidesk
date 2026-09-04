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
/// A write-mostly connection to a peer agent, for streaming input at it.
///
/// # Why this does not wait for each acknowledgement
/// [`kvm_mirror`] writes one message and then blocks reading its `Injected` response
/// before sending the next. That costs a full network round trip per event. It is fine
/// for a demo that samples the cursor 125 times a second, and completely wrong for a
/// KVM: captured input arrives in bursts, and serialising each event behind an RTT adds
/// latency exactly where it is most visible — a dragged window or a fast mouse flick
/// lands behind the operator's hand.
///
/// So writes are pipelined and the responses are drained by a background task. TCP's
/// own backpressure bounds how far ahead the sender can run.
///
/// Errors are still noticed. The drain task counts refusals and records the first one,
/// and [`PeerSink::send`] fails once the connection is gone, so a peer that stops
/// accepting input surfaces rather than being written into a void.
pub struct PeerSink {
    write: tokio::io::WriteHalf<TcpStream>,
    state: std::sync::Arc<PeerSinkState>,
    drain: tokio::task::JoinHandle<()>,
    sent: u64,
}

#[derive(Default)]
struct PeerSinkState {
    /// Set when the peer closes the connection or answers with an error.
    dead: std::sync::atomic::AtomicBool,
    refusals: std::sync::atomic::AtomicU64,
    first_error: std::sync::Mutex<Option<String>>,
}

impl PeerSink {
    /// Connect and complete the handshake.
    ///
    /// The handshake *is* synchronous: nothing may be pipelined until the peer has
    /// accepted the token, or a rejected connection would silently swallow a burst of
    /// injected input.
    pub async fn connect(addr: &str, token: &str) -> anyhow::Result<Self> {
        let stream = TcpStream::connect(addr)
            .await
            .with_context(|| format!("could not connect to peer at {addr}"))?;
        // Nagle would coalesce input events into 40ms batches, which is the opposite of
        // what this connection is for.
        stream.set_nodelay(true).ok();
        let (read_half, mut write) = tokio::io::split(stream);
        let mut reader = BufReader::new(read_half);

        let mut out = serde_json::to_string(&IpcRequest::Hello {
            token: token.to_string(),
            protocol_version: ultidesk_core::protocol::PROTOCOL_VERSION,
        })?;
        out.push('\n');
        write.write_all(out.as_bytes()).await?;
        write.flush().await?;

        let mut line = String::new();
        reader.read_line(&mut line).await?;
        if !line.contains("HelloOk") {
            anyhow::bail!("peer rejected the handshake: {}", line.trim());
        }

        let state = std::sync::Arc::new(PeerSinkState::default());
        let drain_state = state.clone();
        let drain = tokio::spawn(async move {
            use std::sync::atomic::Ordering;
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line).await {
                    Ok(0) | Err(_) => {
                        drain_state.dead.store(true, Ordering::Relaxed);
                        return;
                    }
                    Ok(_) => {
                        if line.contains("\"Error\"") {
                            drain_state.refusals.fetch_add(1, Ordering::Relaxed);
                            let mut first = drain_state.first_error.lock().unwrap();
                            if first.is_none() {
                                *first = Some(line.trim().to_string());
                            }
                        }
                    }
                }
            }
        });

        Ok(PeerSink {
            write,
            state,
            drain,
            sent: 0,
        })
    }

    /// Queue one message. Does not wait for the peer to act on it.
    pub async fn send(&mut self, req: &IpcRequest) -> anyhow::Result<()> {
        if self.state.dead.load(std::sync::atomic::Ordering::Relaxed) {
            anyhow::bail!("peer closed the connection");
        }
        let mut msg = serde_json::to_string(req)?;
        msg.push('\n');
        self.write.write_all(msg.as_bytes()).await?;
        // Flushed per message rather than batched: a buffered input event that arrives
        // late is worse than an extra syscall.
        self.write.flush().await?;
        self.sent += 1;
        Ok(())
    }

    /// Ask the peer to drop everything it is holding, then close.
    ///
    /// Sent before hanging up because a key that was down when control returned would
    /// otherwise stay down on the peer — a stuck modifier makes the other machine
    /// unusable, and the operator is no longer looking at it.
    pub async fn close(mut self) -> anyhow::Result<PeerSinkReport> {
        use std::sync::atomic::Ordering;
        let _ = self.send(&IpcRequest::ReleaseAllInput).await;
        let _ = self.write.shutdown().await;
        self.drain.abort();
        let first = self.state.first_error.lock().unwrap().clone();
        Ok(PeerSinkReport {
            sent: self.sent,
            refusals: self.state.refusals.load(Ordering::Relaxed),
            first_error: first,
        })
    }
}

/// What happened over the life of a [`PeerSink`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerSinkReport {
    pub sent: u64,
    pub refusals: u64,
    pub first_error: Option<String>,
}

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

    // Pipelined through PeerSink rather than waiting for each `Injected`. Blocking on
    // the acknowledgement put a full network round trip between reading the cursor and
    // reading it again, which both added latency to every update and capped the sample
    // rate at 1/RTT no matter what the sleep below said.
    let mut sink = PeerSink::connect(addr, token).await?;
    println!("handshake accepted; mirroring for {seconds}s — move your mouse");

    let remote_vs = VirtualScreenDto {
        left: 0,
        top: 0,
        width: remote_w as i32,
        height: remote_h as i32,
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(seconds);
    let mut last_sent: Option<(i32, i32)> = None;

    while std::time::Instant::now() < deadline {
        if let Some((lx, ly)) = cursor_position() {
            let rx = map_edge_crossing((lx - vs.left) as f64, vs.width as f64, remote_w) as i32;
            let ry = map_edge_crossing((ly - vs.top) as f64, vs.height as f64, remote_h) as i32;
            // Only send on change: a still pointer should cost nothing on the wire.
            if last_sent != Some((rx, ry)) {
                sink.send(&IpcRequest::InjectMouseMove {
                    screen_x: rx,
                    screen_y: ry,
                    virtual_screen: remote_vs,
                })
                .await?;
                last_sent = Some((rx, ry));
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(8)).await;
    }

    let report = sink.close().await?;
    println!(
        "mirrored {} pointer updates, {} refused{}",
        report.sent,
        report.refusals,
        report
            .first_error
            .map(|e| format!(" (first: {e})"))
            .unwrap_or_default()
    );
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
