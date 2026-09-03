//! KVM handoff driver — the source side, on Windows.
//!
//! Mirroring forwards a copy of the pointer. **Handoff takes it**: once the pointer
//! crosses the configured edge, local mouse and (optionally) keyboard input are
//! swallowed and only the peer sees them. This is the module that can lock an operator
//! out of their own machine, so it is assembled from pieces whose failure modes are
//! already pinned by tests: [`ultidesk_core::kvm::KvmMachine`] owns every transition and
//! the grab flag the hooks read is derived from it, never tracked separately.
//!
//! # Ways out, in order of reliability
//! 1. **`Ctrl+Alt+Shift+U` seen by the hook itself.** When the keyboard is grabbed this
//!    is the only route that can work, because a low-level keyboard hook runs *before*
//!    the OS dispatches registered hotkeys. The hook never swallows it.
//! 2. **The registered hotkey.** Independent of this loop, so it fires even if the loop
//!    is wedged — but only while the keyboard is not being swallowed.
//! 3. **Peer loss**, and **a hard deadline**. Any send/receive failure releases, and the
//!    session ends after a fixed time regardless.
//!
//! # Why the cursor gets re-anchored
//! Swallowing `WM_MOUSEMOVE` stops the cursor moving, so its absolute position stops
//! advancing and would pin the remote pointer at the edge. Motion is instead recovered
//! as a delta from a fixed anchor, with the cursor warped back after every swallowed
//! event. The warp returns through the hook flagged `LLMHF_INJECTED`, which is how it
//! is told apart from real movement — without that check it feeds back and runs away.

use crate::ipc::{IpcRequest, MouseButtonDto, VirtualScreenDto};
use anyhow::Context;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use ultidesk_core::kvm::{KvmEvent, KvmMachine, KvmOutcome};
use ultidesk_platform_windows::cursor::{
    at_right_edge, set_cursor_position, vertical_fraction, virtual_screen,
};
use ultidesk_platform_windows::hook::{spawn_input_hooks, HookEvent};
use ultidesk_platform_windows::hotkey::{spawn_emergency_release, EMERGENCY_RELEASE_LABEL};
use ultidesk_platform_windows::inject::MouseButton;

/// How close to the edge counts as touching it.
const EDGE_SLOP: i32 = 1;

/// Set-1 scancodes that arrived with an `0xE0` prefix are tagged with this bit before
/// going on the wire, matching `ultidesk_platform_linux::keymap::EXTENDED`.
const EXTENDED_BIT: u16 = 0xE000;

fn to_dto(button: MouseButton) -> MouseButtonDto {
    match button {
        MouseButton::Left => MouseButtonDto::Left,
        MouseButton::Right => MouseButtonDto::Right,
        MouseButton::Middle => MouseButtonDto::Middle,
    }
}

pub async fn run(
    addr: &str,
    token: &str,
    remote_w: f64,
    remote_h: f64,
    seconds: u64,
    keyboard: bool,
) -> anyhow::Result<()> {
    let vs = virtual_screen()
        .ok_or_else(|| anyhow::anyhow!("could not read the virtual screen bounds"))?;
    let anchor = (vs.left + vs.width / 2, vs.top + vs.height / 2);

    let grab = Arc::new(AtomicBool::new(false));
    let hook_rx =
        spawn_input_hooks(grab.clone(), keyboard).context("installing the input hooks")?;
    // Registered as a second route. It cannot fire while the keyboard is swallowed, so
    // the hook's own detection is the primary escape in that case.
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

    println!("handoff armed for {seconds}s (keyboard grabbed: {keyboard})");
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

    let result = async {
        // One request/response exchange. Any failure propagates and releases the grab.
        macro_rules! send_req {
            ($req:expr) => {{
                let mut msg = serde_json::to_string(&$req)?;
                msg.push('\n');
                write_half.write_all(msg.as_bytes()).await?;
                write_half.flush().await?;
                line.clear();
                if reader.read_line(&mut line).await? == 0 {
                    anyhow::bail!("peer closed the connection");
                }
                forwarded += 1;
            }};
        }

        while std::time::Instant::now() < deadline {
            // Route 2: the registered hotkey (only reachable when not swallowing keys).
            if hotkey_rx.try_recv().is_ok() {
                if let KvmOutcome::Released(reason) = machine.on(KvmEvent::EmergencyRelease) {
                    grab.store(false, Ordering::Relaxed);
                    send_req!(IpcRequest::ReleaseAllInput);
                    println!("control released ({reason:?})");
                }
            }

            while let Ok(ev) = hook_rx.try_recv() {
                // Route 1: the hook saw the combination itself. Checked before anything
                // else, so a release is never queued behind forwarding work.
                if ev == HookEvent::EmergencyRelease {
                    if let KvmOutcome::Released(reason) = machine.on(KvmEvent::EmergencyRelease) {
                        grab.store(false, Ordering::Relaxed);
                        send_req!(IpcRequest::ReleaseAllInput);
                        println!("control released ({reason:?})");
                    }
                    continue;
                }

                if !machine.grab_active() {
                    // Not grabbed: the only thing that matters is edge contact.
                    if let HookEvent::Motion { x, y, injected } = ev {
                        if injected {
                            continue;
                        }
                        let edge = if at_right_edge(x, vs, EDGE_SLOP) {
                            KvmEvent::EdgeReached
                        } else {
                            KvmEvent::EdgeLeft
                        };
                        if machine.on(edge) == KvmOutcome::Captured {
                            remote = (0.0, vertical_fraction(y, vs) * remote_h);
                            grab.store(true, Ordering::Relaxed);
                            set_cursor_position(anchor.0, anchor.1);
                            println!("control handed to peer at y={:.0}", remote.1);
                        }
                    }
                    continue;
                }

                match ev {
                    HookEvent::Motion { x, y, injected } => {
                        if injected {
                            continue;
                        }
                        let dx = (x - anchor.0) as f64;
                        let dy = (y - anchor.1) as f64;
                        set_cursor_position(anchor.0, anchor.1);
                        remote.0 = (remote.0 + dx).clamp(0.0, remote_w - 1.0);
                        remote.1 = (remote.1 + dy).clamp(0.0, remote_h - 1.0);
                        send_req!(IpcRequest::InjectMouseMove {
                            screen_x: remote.0 as i32,
                            screen_y: remote.1 as i32,
                            virtual_screen: remote_vs,
                        });
                        // Walking back off the peer's left edge returns control.
                        if remote.0 <= 0.0 && dx < 0.0 {
                            if let KvmOutcome::Released(reason) =
                                machine.on(KvmEvent::ReturnedAcrossEdge)
                            {
                                grab.store(false, Ordering::Relaxed);
                                send_req!(IpcRequest::ReleaseAllInput);
                                println!("control returned ({reason:?})");
                            }
                        }
                    }
                    HookEvent::Button { button, down } => {
                        send_req!(IpcRequest::InjectMouseButton {
                            button: to_dto(button),
                            down,
                        });
                    }
                    HookEvent::Key {
                        scancode,
                        extended,
                        down,
                    } => {
                        let wire = if extended {
                            scancode | EXTENDED_BIT
                        } else {
                            scancode
                        };
                        send_req!(IpcRequest::InjectKey {
                            scancode: wire,
                            down,
                        });
                    }
                    HookEvent::EmergencyRelease => unreachable!("handled above"),
                }
            }

            tokio::time::sleep(std::time::Duration::from_millis(4)).await;
        }
        Ok::<(), anyhow::Error>(())
    }
    .await;

    // Unconditional cleanup. Whatever ended the loop — deadline, dead peer, an error
    // mid-forward — the grab comes off before this function returns, and the peer is
    // told to drop anything it is holding so no key stays stuck on the far machine.
    machine.on(KvmEvent::PeerEnded);
    grab.store(false, Ordering::Relaxed);
    if let Ok(mut rel) = serde_json::to_string(&IpcRequest::ReleaseAllInput) {
        rel.push('\n');
        let _ = write_half.write_all(rel.as_bytes()).await;
        let _ = write_half.flush().await;
    }

    println!("handoff ended; forwarded {forwarded} events; input is local again");
    result
}
