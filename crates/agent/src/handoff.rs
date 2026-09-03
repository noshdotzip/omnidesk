//! KVM handoff driver — the source side, on Windows.
//!
//! Mirroring forwards a copy of the pointer. **Handoff takes it**: once the pointer
//! crosses the configured edge, local motion is swallowed and only the peer sees it.
//! This is the module that can lock an operator out of their own machine, so it is
//! built out of pieces whose failure modes are already pinned by tests:
//! [`ultidesk_core::kvm::KvmMachine`] owns every transition, and the grab flag the hook
//! reads is derived from it rather than tracked separately.
//!
//! # Three independent ways out
//! Each works even if the others are broken:
//!
//! 1. **The emergency hotkey.** Delivered by the OS as `WM_HOTKEY`, so it fires even if
//!    this loop is wedged or blocked on a dead socket.
//! 2. **Peer loss.** Any send or receive failure releases immediately; a KVM that keeps
//!    grabbing input after the far end vanishes is a trap.
//! 3. **A hard deadline.** The session ends after a fixed time no matter what. Crude,
//!    and exactly the sort of backstop worth having while the rest is unproven.
//!
//! # Why the cursor gets re-anchored
//! Swallowing `WM_MOUSEMOVE` stops the cursor moving, so its absolute position stops
//! advancing and would pin the remote pointer at the edge forever. Instead the cursor is
//! warped back to a fixed anchor after every swallowed event and motion is recovered as
//! the delta from that anchor. The warp itself comes back through the hook flagged
//! `LLMHF_INJECTED`, which is how it is told apart from real movement.

use crate::ipc::{IpcRequest, VirtualScreenDto};
use anyhow::Context;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use ultidesk_core::kvm::{KvmEvent, KvmMachine, KvmOutcome};
use ultidesk_platform_windows::cursor::{
    at_right_edge, set_cursor_position, vertical_fraction, virtual_screen,
};
use ultidesk_platform_windows::hook::spawn_mouse_hook;
use ultidesk_platform_windows::hotkey::{spawn_emergency_release, EMERGENCY_RELEASE_LABEL};

/// How close to the edge counts as touching it.
const EDGE_SLOP: i32 = 1;

pub async fn run(
    addr: &str,
    token: &str,
    remote_w: f64,
    remote_h: f64,
    seconds: u64,
) -> anyhow::Result<()> {
    let vs = virtual_screen()
        .ok_or_else(|| anyhow::anyhow!("could not read the virtual screen bounds"))?;
    let anchor = (vs.left + vs.width / 2, vs.top + vs.height / 2);

    let grab = Arc::new(AtomicBool::new(false));
    let hook_rx = spawn_mouse_hook(grab.clone()).context("installing the mouse hook")?;
    let hotkey_rx = spawn_emergency_release().context("registering the emergency hotkey")?;

    let stream = TcpStream::connect(addr)
        .await
        .with_context(|| format!("could not connect to peer at {addr}"))?;
    stream.set_nodelay(true).ok();
    let (read_half, mut write_half) = tokio::io::split(stream);
    let mut reader = BufReader::new(read_half);
    let mut line = String::new();

    let mut hello = serde_json::to_string(&IpcRequest::Hello {
        token: token.to_string(),
        protocol_version: ultidesk_core::protocol::PROTOCOL_VERSION,
    })?;
    hello.push('\n');
    write_half.write_all(hello.as_bytes()).await?;
    write_half.flush().await?;
    reader.read_line(&mut line).await?;
    if !line.contains("HelloOk") {
        anyhow::bail!("peer rejected the handshake: {}", line.trim());
    }

    println!("handoff armed for {seconds}s");
    println!("  move the pointer to the RIGHT edge to hand control to the peer");
    println!("  press {EMERGENCY_RELEASE_LABEL} to take it back at any time");

    let remote_vs = VirtualScreenDto {
        left: 0,
        top: 0,
        width: remote_w as i32,
        height: remote_h as i32,
    };
    let mut machine = KvmMachine::new();
    let mut remote = (0.0f64, 0.0f64);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(seconds);
    let mut forwarded = 0u64;

    // Any exit path must clear the grab, so the loop body only ever sets it from the
    // state machine and the cleanup below is unconditional.
    let result = async {
        while std::time::Instant::now() < deadline {
            if hotkey_rx.try_recv().is_ok() {
                if let KvmOutcome::Released(reason) = machine.on(KvmEvent::EmergencyRelease) {
                    grab.store(false, Ordering::Relaxed);
                    println!("control released ({reason:?})");
                }
            }

            while let Ok(ev) = hook_rx.try_recv() {
                // Our own re-anchoring warp: not operator motion, must not feed back.
                if ev.injected {
                    continue;
                }

                if !machine.grab_active() {
                    let event = if at_right_edge(ev.x, vs, EDGE_SLOP) {
                        KvmEvent::EdgeReached
                    } else {
                        KvmEvent::EdgeLeft
                    };
                    if machine.on(event) == KvmOutcome::Captured {
                        // Enter the peer at the matching height on its left edge.
                        remote = (0.0, vertical_fraction(ev.y, vs) * remote_h);
                        grab.store(true, Ordering::Relaxed);
                        set_cursor_position(anchor.0, anchor.1);
                        println!("control handed to peer at y={:.0}", remote.1);
                    }
                    continue;
                }

                // Grabbed: recover motion as a delta from the anchor, then re-anchor.
                let dx = (ev.x - anchor.0) as f64;
                let dy = (ev.y - anchor.1) as f64;
                set_cursor_position(anchor.0, anchor.1);
                remote.0 = (remote.0 + dx).clamp(0.0, remote_w - 1.0);
                remote.1 = (remote.1 + dy).clamp(0.0, remote_h - 1.0);

                let mut msg = serde_json::to_string(&IpcRequest::InjectMouseMove {
                    screen_x: remote.0 as i32,
                    screen_y: remote.1 as i32,
                    virtual_screen: remote_vs,
                })?;
                msg.push('\n');
                write_half.write_all(msg.as_bytes()).await?;
                write_half.flush().await?;
                line.clear();
                if reader.read_line(&mut line).await? == 0 {
                    anyhow::bail!("peer closed the connection");
                }
                forwarded += 1;

                // Walking back off the peer's left edge returns control here.
                if remote.0 <= 0.0 && dx < 0.0 {
                    if let KvmOutcome::Released(reason) = machine.on(KvmEvent::ReturnedAcrossEdge) {
                        grab.store(false, Ordering::Relaxed);
                        println!("control returned ({reason:?})");
                    }
                }
            }

            tokio::time::sleep(std::time::Duration::from_millis(4)).await;
        }
        Ok::<(), anyhow::Error>(())
    }
    .await;

    // Unconditional cleanup. If the loop exited because the peer died, the state machine
    // must still be told, and the grab must come off before this function returns.
    machine.on(KvmEvent::PeerEnded);
    grab.store(false, Ordering::Relaxed);
    let mut rel = serde_json::to_string(&IpcRequest::ReleaseAllInput)?;
    rel.push('\n');
    let _ = write_half.write_all(rel.as_bytes()).await;
    let _ = write_half.flush().await;

    println!("handoff ended; forwarded {forwarded} pointer updates; input is local again");
    result
}
