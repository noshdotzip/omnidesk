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
//!   ultidesk-agent inject-test Open a RemoteDesktop portal session and nudge the
//!                              pointer, to prove input injection works. Linux only.
//!                              DOES prompt for permission and DOES move the cursor.

mod audio;
mod endpoint;
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
        "audio-send" => audio_send(),
        "audio-recv" => audio_recv(),
        "serve" => serve(),
        other => {
            eprintln!("unknown subcommand: {other}");
            eprintln!(
                "usage: ultidesk-agent [serve|enumerate|probe|inject-test|capture-test|cast-test|serve-peer-dev|kvm-demo|kvm-mirror|kvm-handoff|audio-send|audio-recv]"
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

    let opts = CastOptions {
        cursor: CursorMode::best_available(cursor_bits).unwrap_or(CursorMode::Embedded),
        ..CastOptions::default()
    };
    let grant = CastGrant {
        restore_token: std::env::var("ULTIDESK_CAST_TOKEN").ok(),
    };
    session.select_sources(opts, &grant)?;
    eprintln!(
        "SelectSources OK (types={}, cursor={:?})",
        opts.type_bits(),
        opts.cursor
    );

    // Start opens the compositor's picker, so it only runs when explicitly asked for.
    if std::env::args().nth(2).as_deref() != Some("start") {
        session.close()?;
        println!("screencast negotiation reached Start-ready state");
        println!("run 'cast-test start' to open the picker and obtain a PipeWire node");
        return Ok(());
    }

    eprintln!("KDE will ask which window to share — pick one to continue.");
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
    // Proving we hold a live, authorised PipeWire connection is the milestone here;
    // turning it into frames needs a PipeWire client, which is the next step.
    println!("pipewire fd acquired: {}", {
        use std::os::fd::AsRawFd;
        fd.as_raw_fd()
    });

    session.close()?;
    println!("screencast session started and closed cleanly");
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
