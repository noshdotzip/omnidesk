//! Ultidesk user-session agent.
//!
//! Runs in the logged-in user's interactive session (NOT as a privileged service —
//! desktop capture, clipboard, global input and interactive windows are all
//! session-specific). See docs/architecture.md.
//!
//! Subcommands:
//!   ultidesk-agent serve       Run the local IPC server (default).
//!   ultidesk-agent enumerate   Print capturable top-level windows as JSON and exit.
//!                              (No IPC, no elevation — a quick feasibility probe.)
//!   ultidesk-agent probe       Print what the local desktop can actually do, as JSON.
//!                              Linux only; read-only, raises no permission dialog.
//!   ultidesk-agent kvm-source  Capture this desktop's input at a screen edge and
//!                              drive a peer with it. Linux only. GRABS INPUT — press
//!                              Esc to release.
//!                              kvm-source [peer:port token [w h]]
//!   ultidesk-agent audio-devices Print this machine's audio endpoints as JSON.
//!                              Read-only; raises no permission dialog on either
//!                              platform.
//!   ultidesk-agent inject-test Open a RemoteDesktop portal session and nudge the
//!                              pointer, to prove input injection works. Linux only.
//!                              DOES prompt for permission and DOES move the cursor.

mod audio;
mod endpoint;
mod forward;
#[cfg(windows)]
mod handoff;
mod ipc;
#[cfg(windows)]
mod pipe;
#[cfg(target_os = "linux")]
mod portal_injector;
mod tcp;

use anyhow::Result;
use ipc::Injector;

fn main() -> Result<()> {
    init_tracing();
    let mode = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "serve".to_string());
    match mode.as_str() {
        "enumerate" => enumerate(),
        "probe" => probe(),
        "inject-test" => inject_test(),
        "capture-test" => capture_test(),
        "cast-test" => cast_test(),
        "serve-peer-dev" => serve_peer_dev(),
        "kvm-demo" => kvm_demo(),
        "kvm-mirror" => kvm_mirror(),
        "kvm-handoff" => kvm_handoff(),
        "kvm-source" => kvm_source(),
        "audio-devices" => audio_devices(),
        "audio-send" => audio_send(),
        "audio-recv" => audio_recv(),
        "serve" => serve(),
        other => {
            eprintln!("unknown subcommand: {other}");
            eprintln!(
                "usage: ultidesk-agent [serve|enumerate|probe|inject-test|capture-test|cast-test [start|pick]|serve-peer-dev|kvm-demo|kvm-mirror|kvm-handoff|kvm-source|audio-devices|audio-send|audio-recv]"
            );
            std::process::exit(2);
        }
    }
}

/// Structured logs to stderr. Deliberately no event carries window titles, key codes,
/// clipboard content, file names, or the auth token (brief: no sensitive logging).
fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_env("ULTIDESK_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}

fn enumerate() -> Result<()> {
    let windows = ipc::RealInjector.enumerate();
    // Print to stdout as JSON for the caller / for manual verification.
    println!("{}", serde_json::to_string_pretty(&windows)?);
    tracing::info!(count = windows.len(), "enumerated top-level windows");
    Ok(())
}

/// Report the local desktop's real capabilities.
///
/// On Linux this reads the XDG portal properties: read-only, opens no session, and so
/// raises no permission dialog (see docs/permissions.md). On Windows the capabilities
/// are not negotiated at runtime, so there is nothing to report.
#[cfg(target_os = "linux")]
fn probe() -> Result<()> {
    let report = ultidesk_platform_linux::probe()?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    tracing::info!(
        window_capture = report.can_project_window(),
        input_inject = report.can_receive_input(),
        input_capture = report.can_capture_input(),
        "probed desktop portals"
    );
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn probe() -> Result<()> {
    anyhow::bail!("portal probing is Linux-only; on Windows the Win32 APIs are used directly")
}

/// Prove that input injection actually reaches the compositor.
///
/// Moves the pointer in a small square and returns it to where it started. It does not
/// press buttons and does not type: a test that synthesizes clicks or keystrokes into
/// whatever window happens to have focus is not a test, it is a hazard.
///
/// This raises a real permission dialog and blocks until the user answers it.
#[cfg(target_os = "linux")]
fn inject_test() -> Result<()> {
    use ultidesk_platform_linux::remote_desktop::{RemoteDesktopSession, SessionOptions};

    eprintln!("Requesting a RemoteDesktop portal session.");
    eprintln!("KDE will ask you to allow remote control — approve it to continue.");
    eprintln!("(This call blocks until you answer the dialog.)");

    let session = RemoteDesktopSession::open(SessionOptions::default())?;
    eprintln!("session granted; moving the pointer in a 40px square");

    // A closed loop, so the pointer ends where it began.
    for (dx, dy) in [(40.0, 0.0), (0.0, 40.0), (-40.0, 0.0), (0.0, -40.0)] {
        session.pointer_motion(dx, dy)?;
        std::thread::sleep(std::time::Duration::from_millis(150));
    }

    match session.restore_token() {
        Some(token) => {
            tracing::info!("received a restore token; future sessions can skip the prompt");
            // The token is a capability: it is printed for this manual test only and
            // must be stored in OS secret storage, never logged, once pairing exists.
            println!("restore_token={token}");
        }
        None => tracing::warn!("no restore token issued; every launch will prompt"),
    }

    session.close()?;
    eprintln!("session closed cleanly");
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn inject_test() -> Result<()> {
    anyhow::bail!("inject-test drives the XDG RemoteDesktop portal and is Linux-only")
}

/// Exercise the InputCapture portal: create a session, read the compositor's zones,
/// and declare a barrier on the right edge of each.
///
/// Stops short of `Enable` on purpose. Arming a barrier with no libei client reading
/// the event stream would divert the pointer to a consumer that does not exist, which
/// strands it at the screen edge.
#[cfg(target_os = "linux")]
fn capture_test() -> Result<()> {
    use ultidesk_platform_linux::caps::DeviceTypes;
    use ultidesk_platform_linux::input_capture::{Edge, InputCaptureSession};

    eprintln!("Opening an InputCapture session (KDE may prompt for permission).");
    let session = InputCaptureSession::open(DeviceTypes {
        keyboard: true,
        pointer: true,
        touchscreen: false,
    })?;

    println!("zone_set={}", session.zone_set());
    for (i, z) in session.zones().iter().enumerate() {
        println!("zone[{i}] {}x{} at ({},{})", z.width, z.height, z.x, z.y);
    }

    // Place a barrier on the right edge of every zone: the natural default for a
    // KVM whose peer sits to the right. Zone::barrier encodes the coordinate
    // convention the compositor actually accepts (see input_capture docs).
    let barriers: Vec<_> = session
        .zones()
        .iter()
        .enumerate()
        .map(|(i, z)| z.barrier(Edge::Right, i as u32 + 1))
        .collect();
    if barriers.is_empty() {
        anyhow::bail!("compositor offered no zones; cannot place a barrier");
    }
    for b in &barriers {
        println!("barrier {} -> {:?}", b.id, b.position());
    }
    // A rejected barrier is not a D-Bus error: the call succeeds and the edge simply
    // never fires, so this must be inspected rather than assumed.
    let failed = session.set_barriers(&barriers)?;
    if failed.is_empty() {
        println!("all {} barrier(s) accepted", barriers.len());
    } else {
        println!("REJECTED barrier ids: {failed:?}");
    }

    session.close()?;
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn capture_test() -> Result<()> {
    anyhow::bail!("capture-test drives the XDG InputCapture portal and is Linux-only")
}

/// Negotiate a ScreenCast session as far as it can go without interrupting anyone.
///
/// Runs CreateSession and SelectSources, both of which are silent, then stops. Start
/// is what raises the compositor's window picker (ADR-0009), and is left to the real
/// projection flow rather than fired from a probe.
///
/// This doubles as a diagnostic: it shares one code path with the RemoteDesktop and
/// InputCapture clients, so if it completes while InputCapture times out, the fault is
/// in that portal's backend rather than in our Request/Response handling.
#[cfg(target_os = "linux")]
fn cast_test() -> Result<()> {
    use ultidesk_platform_linux::screen_cast::{
        CastGrant, CastOptions, CursorMode, ScreenCastSession,
    };

    let report = ultidesk_platform_linux::probe()?;
    let cursor_bits = 7; // KDE advertises hidden|embedded|metadata
    eprintln!(
        "portal says window capture available: {}",
        report.can_project_window()
    );

    let session = ScreenCastSession::open()?;
    eprintln!("CreateSession OK");

    // multiple: true asks the compositor for a multi-select picker, so one dialog can
    // authorise a whole working set of windows. Each still arrives as its own node and
    // is composited separately, so occlusion never matters.
    let opts = CastOptions {
        cursor: CursorMode::best_available(cursor_bits).unwrap_or(CursorMode::Embedded),
        multiple: true,
        ..CastOptions::default()
    };
    // 'cast-test pick' deliberately ignores any stored grant so the picker reappears.
    // Without this the token pins the selection and there is no way to choose a
    // different window, which is a dead end rather than a security property.
    let force_pick = std::env::args().nth(2).as_deref() == Some("pick");
    let grant = CastGrant {
        restore_token: if force_pick {
            None
        } else {
            std::env::var("ULTIDESK_CAST_TOKEN").ok()
        },
    };
    session.select_sources(opts, &grant)?;
    eprintln!(
        "SelectSources OK (types={}, cursor={:?})",
        opts.type_bits(),
        opts.cursor
    );

    // Start opens the compositor's picker, so it only runs when explicitly asked for.
    let go = matches!(
        std::env::args().nth(2).as_deref(),
        Some("start") | Some("pick")
    );
    if !go {
        session.close()?;
        println!("screencast negotiation reached Start-ready state");
        println!("run 'cast-test start' to capture, or 'cast-test pick' to choose windows afresh");
        return Ok(());
    }

    if force_pick {
        eprintln!("KDE will ask which windows to share — select as many as you want.");
    } else {
        eprintln!("KDE will ask which windows to share unless a stored grant applies.");
    }
    let started = session.start()?;
    if started.nodes.is_empty() {
        anyhow::bail!("the compositor granted no streams");
    }
    for node in &started.nodes {
        println!("pipewire_node={node}");
    }
    match started.restore_token.as_deref() {
        // A capability: printed for this manual test only. It belongs in OS secret
        // storage once pairing exists, and must never be logged.
        Some(t) => println!("restore_token={t}"),
        None => tracing::warn!("no restore token issued; the picker will reappear next run"),
    }

    let fd = session.open_pipewire_remote()?;
    println!(
        "capturing {} window(s) concurrently, one stream each ...",
        started.nodes.len()
    );

    let reports = ultidesk_platform_linux::pipewire_capture::capture_nodes(
        fd,
        &started.nodes,
        120,
        std::time::Duration::from_secs(10),
    )?;

    let mut any_frames = false;
    let mut all_zero_copy = true;
    for r in &reports {
        println!(
            "  node {}: frames={} size={}x{} max_fps={} dma-buf={} mapped={}",
            r.node_id,
            r.frames,
            r.width,
            r.height,
            r.max_framerate,
            r.dma_buf_frames,
            r.mem_ptr_frames
        );
        // Reported independently of frame count: buffers are allocated before any
        // frame arrives, so this answers "did zero copy negotiate?" even for a window
        // that never changes.
        match r.allocated {
            Some(k) if r.negotiated_dma_buf() => {
                println!("    allocated {k:?} — zero-copy capable")
            }
            Some(k) => println!("    allocated {k:?} — NOT zero-copy capable"),
            None => println!("    no buffers were allocated"),
        }
        if r.saw_frames() {
            any_frames = true;
            if !r.used_zero_copy() {
                all_zero_copy = false;
            }
        } else {
            // Not a failure: compositors send frames on damage, so a window nobody is
            // touching legitimately produces none.
            println!("    (no frames — that window did not change during the run)");
        }
    }
    if !any_frames {
        println!("  NO FRAMES from any node: move or resize a captured window and retry");
    } else if all_zero_copy {
        println!("  zero-copy (DMA-BUF) — importable into a hardware encoder");
    } else {
        println!("  NOT zero-copy: mapped memory, costing a GPU readback per frame");
    }

    session.close()?;
    println!("screencast session closed cleanly");
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn cast_test() -> Result<()> {
    anyhow::bail!("cast-test drives the XDG ScreenCast portal and is Linux-only")
}

/// Serve the **dev** peer transport so another machine can drive this one's input.
///
/// On Linux this opens a RemoteDesktop portal session first, so the (single)
/// permission prompt happens at startup rather than on the first injected event.
/// Pass a previous grant in `ULTIDESK_RESTORE_TOKEN` to skip the prompt entirely.
///
/// Plaintext and token-gated only — see the warning in `tcp.rs`. Trusted LAN only.
fn serve_peer_dev() -> Result<()> {
    let bind = std::env::args()
        .nth(2)
        .unwrap_or_else(|| "0.0.0.0:45872".to_string());
    let token = std::env::var("ULTIDESK_PEER_TOKEN")
        .unwrap_or_else(|_| uuid::Uuid::new_v4().simple().to_string());

    eprintln!("WARNING: dev peer transport is PLAINTEXT TCP, gated only by a token.");
    eprintln!("WARNING: it is not the ADR-0002 secure channel. Trusted LAN only.");
    println!("bind={bind}");
    println!("token={token}");

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    #[cfg(target_os = "linux")]
    {
        let restore = std::env::var("ULTIDESK_RESTORE_TOKEN").ok();
        let injector = portal_injector::PortalInjector::open(restore)?;
        if let Some(t) = injector.restore_token() {
            println!("restore_token={t}");
        }
        tracing::info!("portal session ready; peers may now inject input");
        rt.block_on(tcp::serve(bind, token, std::sync::Arc::new(injector)))
    }
    #[cfg(not(target_os = "linux"))]
    {
        rt.block_on(tcp::serve(
            bind,
            token,
            std::sync::Arc::new(ipc::RealInjector),
        ))
    }
}

/// Drive a remote peer's pointer through a square, to prove the whole path works.
fn kvm_demo() -> Result<()> {
    let addr = std::env::args()
        .nth(2)
        .ok_or_else(|| anyhow::anyhow!("usage: kvm-demo <host:port> <token> [size]"))?;
    let token = std::env::args()
        .nth(3)
        .ok_or_else(|| anyhow::anyhow!("usage: kvm-demo <host:port> <token> [size]"))?;
    let size: i32 = std::env::args()
        .nth(4)
        .and_then(|s| s.parse().ok())
        .unwrap_or(200);
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(tcp::kvm_demo(&addr, &token, size))
}

/// Mirror the local pointer onto a peer for a bounded time.
fn kvm_mirror() -> Result<()> {
    let addr = std::env::args().nth(2).ok_or_else(|| {
        anyhow::anyhow!("usage: kvm-mirror <host:port> <token> [remote_w] [remote_h] [seconds]")
    })?;
    let token = std::env::args().nth(3).ok_or_else(|| {
        anyhow::anyhow!("usage: kvm-mirror <host:port> <token> [remote_w] [remote_h] [seconds]")
    })?;
    let rw: f64 = std::env::args()
        .nth(4)
        .and_then(|s| s.parse().ok())
        .unwrap_or(1920.0);
    let rh: f64 = std::env::args()
        .nth(5)
        .and_then(|s| s.parse().ok())
        .unwrap_or(1080.0);
    let secs: u64 = std::env::args()
        .nth(6)
        .and_then(|s| s.parse().ok())
        .unwrap_or(15);
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(tcp::kvm_mirror(&addr, &token, rw, rh, secs))
}

/// Hand control of the local pointer to a peer when it crosses the right edge.
///
/// Unlike `kvm-mirror` this **grabs** local input: while control is on the peer the
/// pointer stops moving here. Three independent releases exist — the emergency
/// hotkey, peer loss, and a hard deadline — see the module docs.
#[cfg(windows)]
fn kvm_handoff() -> Result<()> {
    let addr = std::env::args().nth(2).ok_or_else(|| {
        anyhow::anyhow!("usage: kvm-handoff <host:port> <token> [remote_w] [remote_h] [seconds]")
    })?;
    let token = std::env::args().nth(3).ok_or_else(|| {
        anyhow::anyhow!("usage: kvm-handoff <host:port> <token> [remote_w] [remote_h] [seconds]")
    })?;
    let rw: f64 = std::env::args()
        .nth(4)
        .and_then(|s| s.parse().ok())
        .unwrap_or(1920.0);
    let rh: f64 = std::env::args()
        .nth(5)
        .and_then(|s| s.parse().ok())
        .unwrap_or(1080.0);
    // Deliberately short by default: a bounded session is the backstop while the
    // grab path is still unproven on real hardware.
    let secs: u64 = std::env::args()
        .nth(6)
        .and_then(|s| s.parse().ok())
        .unwrap_or(20);
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let keyboard: bool = std::env::args()
        .nth(7)
        .map(|s| s != "false" && s != "0" && s != "no")
        .unwrap_or(true);
    rt.block_on(handoff::run(&addr, &token, rw, rh, secs, keyboard))
}

#[cfg(not(windows))]
fn kvm_handoff() -> Result<()> {
    anyhow::bail!(
        "kvm-handoff needs Windows low-level input hooks; the Linux source side needs libei"
    )
}

/// Capture this desktop's input at a screen edge and drive a peer with it.
///
/// This is the Linux half of the KVM: the machine becomes a *source*, so its pointer
/// and keyboard drive a Windows peer. `capture-test` stops after placing barriers
/// because that much is silent; this one goes all the way — Enable, then ConnectToEIS,
/// then a libei client on the returned socket, then translation onto the wire.
///
/// With no peer address it prints the events instead of sending them, which is how to
/// check that capture works before involving a second machine.
///
/// # This grabs real input
/// Once capture engages, the compositor routes the pointer and keyboard here instead of
/// to the desktop. Esc always releases it: that check runs before anything else in the
/// event loop, so a bug further down cannot strand the operator. Killing the process
/// also releases capture, because the compositor drops the session with the connection.
#[cfg(target_os = "linux")]
fn kvm_source() -> Result<()> {
    use crate::forward::{Forwarded, Forwarder};
    use ultidesk_platform_linux::caps::DeviceTypes;
    use ultidesk_platform_linux::ei_client::{capture_events, CapturedInput, EiSession};
    use ultidesk_platform_linux::input_capture::{Edge, InputCaptureSession};
    use ultidesk_topology::{Rect, Side};

    /// evdev keycode for Esc. Not a keysym: libei reports evdev codes.
    const KEY_ESC: u32 = 1;

    let peer = std::env::args().nth(2);
    let token = std::env::args().nth(3);
    let remote_w: f64 = std::env::args()
        .nth(4)
        .and_then(|s| s.parse().ok())
        .unwrap_or(1920.0);
    let remote_h: f64 = std::env::args()
        .nth(5)
        .and_then(|s| s.parse().ok())
        .unwrap_or(1080.0);

    eprintln!("Opening an InputCapture session (KDE may prompt for permission).");
    let session = InputCaptureSession::open(DeviceTypes {
        keyboard: true,
        pointer: true,
        touchscreen: false,
    })?;

    let barriers: Vec<_> = session
        .zones()
        .iter()
        .enumerate()
        .map(|(i, z)| z.barrier(Edge::Right, i as u32 + 1))
        .collect();
    if barriers.is_empty() {
        anyhow::bail!("compositor offered no zones; cannot place a barrier");
    }
    // A rejected barrier is not a D-Bus error: the call succeeds and the edge simply
    // never fires, so this has to be inspected rather than assumed.
    let failed = session.set_barriers(&barriers)?;
    if !failed.is_empty() {
        anyhow::bail!("compositor rejected barrier ids {failed:?}; capture would never fire");
    }
    println!("{} barrier(s) accepted on the right edge", barriers.len());

    session.enable()?;
    let fd = session.connect_to_eis()?;
    let ei = EiSession::from_fd(fd)?;

    println!();
    match &peer {
        Some(addr) => println!("Forwarding to peer {addr} ({remote_w}x{remote_h})."),
        None => println!("No peer given — printing events only."),
    }
    println!("Capture is ARMED. Push the pointer off the RIGHT edge to engage it.");
    println!("Press Esc at any time to release input and exit.");
    println!();

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    rt.block_on(async move {
        // Crossing off this machine's right edge arrives on the peer's left. Entering
        // at mid-height is a placeholder: the real fraction comes from where the
        // pointer actually hit the barrier, which needs the Zone geometry the portal
        // reports at crossing time.
        let mut sink = match (&peer, &token) {
            (Some(addr), Some(tok)) => Some(tcp::PeerSink::connect(addr, tok).await?),
            (Some(_), None) => anyhow::bail!("a peer address also needs its auth token"),
            (None, _) => None,
        };
        let mut fwd = Forwarder::new(
            Rect {
                x: 0.0,
                y: 0.0,
                width: remote_w,
                height: remote_h,
            },
            crate::ipc::VirtualScreenDto {
                left: 0,
                top: 0,
                width: remote_w as i32,
                height: remote_h as i32,
            },
            Side::Left,
            0.5,
        );
        if let Some(sink) = sink.as_mut() {
            sink.send(&fwd.initial_move()).await?;
        }

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<CapturedInput>();
        // The libei callback is synchronous and the peer write is async, so events are
        // handed across rather than blocking the capture loop on the network. An
        // unbounded channel is right here: dropping input to apply backpressure would
        // lose keystrokes, and TCP already bounds how far the writer can run ahead.
        //
        // `spawn_local`, not `spawn`: reis holds its protocol objects in `Rc`, so the
        // capture future is `!Send` and cannot move to another worker thread. That is
        // why the runtime above is single-threaded.
        let local = tokio::task::LocalSet::new();
        let pump = local.spawn_local(async move {
            capture_events(ei, move |event| {
                if let CapturedInput::Key {
                    keycode: KEY_ESC,
                    pressed: false,
                } = event
                {
                    return false;
                }
                tx.send(event).is_ok()
            })
            .await
        });

        let mut count: u64 = 0;
        local
            .run_until(async {
                while let Some(event) = rx.recv().await {
                    count += 1;
                    let wire = to_wire(event);
                    match fwd.translate(wire) {
                        Forwarded::Send(req) => match sink.as_mut() {
                            Some(sink) => sink.send(&req).await?,
                            None => {
                                if count % 50 == 0 {
                                    println!("[{count}] {req:?}");
                                }
                            }
                        },
                        Forwarded::Pending => {}
                        Forwarded::Dropped(reason) => {
                            tracing::debug!(?reason, "event could not be forwarded");
                        }
                        Forwarded::ReturnHome => {
                            println!("pointer returned home");
                            break;
                        }
                    }
                }
                Ok::<(), anyhow::Error>(())
            })
            .await?;

        println!("captured {count} event(s), {} dropped", fwd.dropped());
        if let Some(sink) = sink {
            let report = sink.close().await?;
            println!(
                "sent {} message(s), {} refused{}",
                report.sent,
                report.refusals,
                report
                    .first_error
                    .map(|e| format!(" (first: {e})"))
                    .unwrap_or_default()
            );
        }
        pump.abort();
        Ok::<(), anyhow::Error>(())
    })?;

    session.disable()?;
    session.close()?;
    Ok(())
}

/// Bridge the platform crate's event type to the agent's wire type.
///
/// Two declarations of the same shape exist on purpose: `forward` must compile on
/// Windows, where the Linux crate is not a dependency. This is the one place they meet,
/// so a field added to one and not the other fails to compile here rather than silently
/// dropping input.
#[cfg(target_os = "linux")]
fn to_wire(e: ultidesk_platform_linux::ei_client::CapturedInput) -> crate::forward::CapturedInput {
    use crate::forward::CapturedInput as W;
    use ultidesk_platform_linux::ei_client::CapturedInput as L;
    match e {
        L::PointerMotion { dx, dy } => W::PointerMotion { dx, dy },
        L::PointerMotionAbsolute { x, y } => W::PointerMotionAbsolute { x, y },
        L::Button { button, pressed } => W::Button { button, pressed },
        L::Scroll { dx, dy } => W::Scroll { dx, dy },
        L::Key { keycode, pressed } => W::Key { keycode, pressed },
    }
}

#[cfg(not(target_os = "linux"))]
fn kvm_source() -> Result<()> {
    anyhow::bail!("kvm-source drives the XDG InputCapture portal and libei; it is Linux-only")
}

/// Print the machine's audio endpoints as JSON.
///
/// The routing UI needs a device list from each machine, and this is the shape it reads.
/// The two platforms produce the same fields from completely different sources — the
/// PipeWire registry and the WASAPI endpoint enumeration — so this is also where a
/// divergence between them would show up first.
fn audio_devices() -> Result<()> {
    println!("{}", local_audio_devices_json()?);
    Ok(())
}

// One function per platform rather than `cfg` blocks inside one body: the blocks each
// need their own tail, which reads as an unconditional early return on whichever
// platform is being compiled.
#[cfg(target_os = "linux")]
fn local_audio_devices_json() -> Result<String> {
    let devices = ultidesk_platform_linux::audio_devices::enumerate()?;
    tracing::info!(count = devices.len(), "enumerated PipeWire audio endpoints");
    Ok(serde_json::to_string_pretty(&devices)?)
}

#[cfg(windows)]
fn local_audio_devices_json() -> Result<String> {
    let devices = ultidesk_platform_windows::audio_devices::enumerate()?;
    tracing::info!(count = devices.len(), "enumerated WASAPI audio endpoints");
    Ok(serde_json::to_string_pretty(&devices)?)
}

#[cfg(not(any(target_os = "linux", windows)))]
fn local_audio_devices_json() -> Result<String> {
    anyhow::bail!("audio device enumeration is not implemented for this platform")
}

/// Stream this machine's audio output to a peer (Linux/PipeWire source side).
fn audio_send() -> Result<()> {
    let addr = std::env::args().nth(2).ok_or_else(|| {
        anyhow::anyhow!("usage: audio-send <host:port> <pipewire-target> [rate] [channels]")
    })?;
    let target = std::env::args().nth(3).ok_or_else(|| {
        anyhow::anyhow!("usage: audio-send <host:port> <pipewire-target> [rate] [channels]")
    })?;
    let rate: u32 = std::env::args()
        .nth(4)
        .and_then(|s| s.parse().ok())
        .unwrap_or(48_000);
    let channels: u16 = std::env::args()
        .nth(5)
        .and_then(|s| s.parse().ok())
        .unwrap_or(2);
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(audio::send(
        &addr,
        &target,
        audio::AudioFormat { rate, channels },
    ))
}

/// Play a peer's audio on this machine (Windows/WASAPI receiving side).
fn audio_recv() -> Result<()> {
    let bind = std::env::args()
        .nth(2)
        .unwrap_or_else(|| "0.0.0.0:45873".to_string());
    // 120ms of slack by default: enough to ride out WiFi jitter (measured 15ms mean
    // absolute deviation on this link) without a audible lag.
    let latency: f64 = std::env::args()
        .nth(3)
        .and_then(|s| s.parse().ok())
        .unwrap_or(120.0);
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(audio::recv(&bind, latency))
}

#[cfg(windows)]
fn serve() -> Result<()> {
    use std::sync::Arc;
    let ep = endpoint::Endpoint::generate();
    let path = endpoint::write_handshake(&ep)?;
    // The pipe name is fine to log; the token is NOT logged.
    tracing::info!(pipe = %ep.pipe_name, handshake = %path.display(), "agent IPC listening");
    // Also print the handshake path to stdout so a launching parent can find it.
    println!("{}", path.display());

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(async move {
        pipe::serve(
            ep.pipe_name.clone(),
            ep.token.clone(),
            Arc::new(ipc::RealInjector),
        )
        .await
    })
}

#[cfg(not(windows))]
fn serve() -> Result<()> {
    anyhow::bail!(
        "the IPC server transport is currently Windows-only; use `enumerate` on this platform"
    )
}
