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
//!   ultidesk-agent identity    Print this machine's Ed25519 device identity, creating
//!                              it on first run. Never prints the private key.
//!   ultidesk-agent pair        Pair with another machine: compare a six-digit code on
//!                              both screens, then pin its key.
//!                              pair            wait for a peer to dial in
//!                              pair host:port  dial a peer that is waiting
//!   ultidesk-agent peers       List trusted devices and what each may do.
//!                              peers forget <key>        revoke trust entirely
//!                              peers allow  <key> <perm> grant one permission
//!                              peers deny   <key> <perm> revoke one permission
//!   ultidesk-agent serve-peer  Serve paired peers over the authenticated QUIC channel
//!                              (ADR-0002). No token: a peer is admitted by its key.
//!   ultidesk-agent peer-ping   Round-trip a Ping against a paired peer and report the
//!                              latency of the real path. Injects nothing.
//!   ultidesk-agent peer-devices Print a paired peer's audio endpoints as JSON, refusing
//!                              any the peer labels as another machine's.
//!   ultidesk-agent monitors    Print this machine's monitors as JSON. Needs no window
//!                              and raises no permission dialog.
//!   ultidesk-agent peer-monitors Print a paired peer's monitors, with the same check.
//!   ultidesk-agent topology    Print the layout both machines share and where they
//!                              touch — the first thing to check if the pointer will
//!                              not cross. topology <peer host:port>
//!   ultidesk-agent ask         Ask this machine's *running* agent over the local IPC,
//!                              exactly as the control app does.
//!                              ask <monitors|devices>
//!   ultidesk-agent ask-peer    The same, but relayed to a paired peer.
//!                              ask-peer <monitors|devices> [peer key]
//!   ultidesk-agent uinput-test Move the pointer through a square using virtual
//!                              input devices. Linux only. Raises NO permission
//!                              dialog and DOES move the real cursor.
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
mod forward;
#[cfg(windows)]
mod handoff;
mod ipc;
#[cfg(windows)]
mod pipe;
#[cfg(target_os = "linux")]
mod portal_injector;
mod quic;
mod relay;
mod tcp;
mod topology;
#[cfg(target_os = "linux")]
mod uinput_injector;
#[cfg(unix)]
mod unix_socket;

use anyhow::{Context, Result};
use ipc::Injector;

fn main() -> Result<()> {
    // Before anything reads a coordinate. A DPI-unaware process is shown monitor
    // rectangles divided by the scale factor and a scale of 1.0 for every monitor — a
    // self-consistent lie that only breaks once those numbers reach another process.
    // See `ultidesk_platform_windows::dpi`.
    ultidesk_platform_windows::dpi::make_process_per_monitor_aware();
    init_tracing();
    let mode = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "serve".to_string());
    match mode.as_str() {
        "enumerate" => enumerate(),
        "probe" => probe(),
        "identity" => identity(),
        "inject-test" => inject_test(),
        "capture-test" => capture_test(),
        "cast-test" => cast_test(),
        "serve-peer-dev" => serve_peer_dev(),
        "serve-peer" => serve_peer(),
        "peer-ping" => peer_ping(),
        "peer-devices" => peer_devices(),
        "monitors" => monitors(),
        "peer-monitors" => peer_monitors(),
        "topology" => topology_report(),
        "ask" => ask(false),
        "ask-peer" => ask(true),
        "pair" => pair(),
        "peers" => peers(),
        "kvm-demo" => kvm_demo(),
        "kvm-mirror" => kvm_mirror(),
        "kvm-handoff" => kvm_handoff(),
        "input-devices" => input_devices(),
        "uinput-test" => uinput_test(),
        "kvm-source" => kvm_source(),
        "audio-devices" => audio_devices(),
        "audio-send" => audio_send(),
        "audio-recv" => audio_recv(),
        "serve" => serve(),
        other => {
            eprintln!("unknown subcommand: {other}");
            eprintln!(
                "usage: ultidesk-agent [serve|enumerate|probe|identity|pair|peers|serve-peer|peer-ping|peer-devices|monitors|peer-monitors|ask|ask-peer|topology|inject-test|capture-test|cast-test [start|pick]|serve-peer-dev|kvm-demo|kvm-mirror|kvm-handoff|kvm-source|uinput-test|input-devices|audio-devices|audio-send|audio-recv]"
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

/// Print this machine's device identity, creating it on first run.
///
/// The fingerprint is what an operator compares against the other machine's before
/// trusting it, so it is printed alongside the ids rather than only the ids: a uuid is
/// unreadable at a glance and two of them differing in the middle look identical.
///
/// Read-only apart from the first run, and raises no permission dialog on either
/// platform.
fn identity() -> Result<()> {
    let dir = ultidesk_core::paths::config_dir().ok_or_else(|| {
        anyhow::anyhow!(
            "no configuration directory (no APPDATA on Windows, no HOME or XDG_CONFIG_HOME \
             on Linux); set ULTIDESK_CONFIG_DIR to choose one"
        )
    })?;
    let loaded = ultidesk_identity::load_or_create(&dir)?;
    if let Some(note) = &loaded.note {
        eprintln!("WARNING: {note}");
    }

    let report = serde_json::json!({
        "device_id": loaded.identity.device_id().to_string(),
        "fingerprint": loaded.identity.fingerprint(),
        "public_key": loaded.identity.public().to_string(),
        "path": ultidesk_identity::store::identity_path(&dir),
        "created": loaded.created,
    });
    println!("{}", serde_json::to_string_pretty(&report)?);

    // The private key is never logged, printed, or included above — only the public
    // half and the digests derived from it.
    tracing::info!(
        created = loaded.created,
        fingerprint = %loaded.identity.fingerprint(),
        "device identity ready"
    );
    Ok(())
}

/// Choose the input backend this machine will let peers drive.
///
/// Shared by both peer transports rather than decided per transport: which injector is
/// in use decides whether the agent can run unattended, and choosing it in two places is
/// two chances to choose differently.
fn choose_injector() -> Result<std::sync::Arc<dyn Injector + Send + Sync>> {
    #[cfg(target_os = "linux")]
    {
        // uinput first: it needs no permission dialog, so an agent started at login can
        // accept input without anyone being at the machine, and its pointer is absolute
        // rather than dead-reckoned through libinput's acceleration curve. The portal
        // remains the fallback for a machine where /dev/uinput is not writable.
        //
        // `ULTIDESK_FORCE_PORTAL=1` selects the portal explicitly, which is how the two
        // paths get compared without rebuilding.
        let force_portal = std::env::var("ULTIDESK_FORCE_PORTAL").is_ok();
        if !force_portal {
            match uinput_injector::UinputInjector::open() {
                Ok(injector) => {
                    println!("injector=uinput (no permission dialog)");
                    tracing::info!("uinput devices ready; peers may now inject input");
                    return Ok(std::sync::Arc::new(injector));
                }
                Err(e) => {
                    // Reported rather than silently downgraded: falling back to a path
                    // that raises a dialog changes whether the agent can run unattended,
                    // which the operator needs to know about.
                    eprintln!("uinput unavailable ({e}); falling back to the portal");
                }
            }
        }
        let restore = std::env::var("ULTIDESK_RESTORE_TOKEN").ok();
        let injector = portal_injector::PortalInjector::open(restore)?;
        if let Some(t) = injector.restore_token() {
            println!("restore_token={t}");
        }
        println!("injector=portal");
        tracing::info!("portal session ready; peers may now inject input");
        Ok(std::sync::Arc::new(injector))
    }
    #[cfg(not(target_os = "linux"))]
    {
        Ok(std::sync::Arc::new(ipc::RealInjector))
    }
}

/// Serve the **dev** peer transport so another machine can drive this one's input.
///
/// Plaintext and token-gated only — see the warning in `tcp.rs`. Superseded by
/// `serve-peer`, and kept only so the two can be compared on a bench.
fn serve_peer_dev() -> Result<()> {
    let bind = std::env::args()
        .nth(2)
        .unwrap_or_else(|| "0.0.0.0:45872".to_string());
    let token = std::env::var("ULTIDESK_PEER_TOKEN")
        .unwrap_or_else(|_| uuid::Uuid::new_v4().simple().to_string());

    eprintln!("WARNING: dev peer transport is PLAINTEXT TCP, gated only by a token.");
    eprintln!("WARNING: it is not the ADR-0002 secure channel. Use `serve-peer` instead.");
    println!("bind={bind}");
    println!("token={token}");

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let (_dir, identity) = local_identity()?;
    let backends = std::sync::Arc::new(ipc::LocalBackends::new(
        choose_injector()?,
        identity.device_id(),
    ));
    rt.block_on(tcp::serve(bind, token, backends))
}

/// Serve paired peers over the authenticated QUIC channel (ADR-0002).
///
/// Unlike `serve-peer-dev` there is no token to print and none to copy: a peer is
/// admitted because it holds the private key for an identity this machine has pinned,
/// and it proves that during the handshake.
fn serve_peer() -> Result<()> {
    let bind: std::net::SocketAddr = std::env::args()
        .nth(2)
        .unwrap_or_else(|| "0.0.0.0:45872".to_string())
        .parse()
        .context("the bind address must be host:port, e.g. 0.0.0.0:45872")?;

    let (dir, identity) = local_identity()?;
    let store = load_peers(&dir, identity.public())?;

    println!("this machine: {}", identity.fingerprint());
    for peer in store.peers() {
        println!(
            "paired with: {} ({}) [{}]",
            peer.name,
            peer.key.fingerprint(),
            describe(&peer.permissions)
        );
    }

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let backends = std::sync::Arc::new(ipc::LocalBackends::new(
        choose_injector()?,
        identity.device_id(),
    ));
    rt.block_on(quic::serve(&identity, bind, &store, backends))
}

/// Round-trip a Ping against a paired peer, and report the latency of the real path.
fn peer_ping() -> Result<()> {
    let addr: std::net::SocketAddr = std::env::args()
        .nth(2)
        .ok_or_else(|| anyhow::anyhow!("usage: ultidesk-agent peer-ping <host:port> [count]"))?
        .parse()
        .context("the peer address must be host:port")?;
    let count: u32 = std::env::args()
        .nth(3)
        .map(|n| n.parse())
        .transpose()
        .context("count must be a number")?
        .unwrap_or(5);

    let (dir, identity) = local_identity()?;
    let store = load_peers(&dir, identity.public())?;

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(quic::peer_ping(&identity, &store, addr, count))
}

/// Print this machine's monitors as JSON, read without a window.
///
/// Read-only, and raises no permission dialog on either platform: on Windows it walks
/// Win32, and on Linux it asks the compositor for its outputs, which any Wayland client
/// may do.
fn monitors() -> Result<()> {
    use ipc::MonitorInventory;
    let (_dir, identity) = local_identity()?;
    let monitors = ipc::RealMonitorInventory::new(identity.device_id())
        .monitors()
        .map_err(|e| anyhow::anyhow!(e))?;
    tracing::info!(count = monitors.len(), "enumerated monitors");
    println!("{}", serde_json::to_string_pretty(&monitors)?);
    Ok(())
}

/// Ask a paired peer for its monitors.
fn peer_monitors() -> Result<()> {
    let (addr, dir, identity) = peer_query_args("peer-monitors")?;
    let store = load_peers(&dir, identity.public())?;
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let monitors = rt.block_on(quic::peer_monitors(&identity, &store, addr))?;
    remember_where(&dir, store, &monitors[..], addr)?;
    println!("{}", serde_json::to_string_pretty(&monitors)?);
    tracing::info!(count = monitors.len(), "read a peer's monitors");
    Ok(())
}

/// The address, config directory and identity every peer query needs.
fn peer_query_args(
    command: &str,
) -> Result<(
    std::net::SocketAddr,
    std::path::PathBuf,
    ultidesk_identity::Identity,
)> {
    let addr: std::net::SocketAddr = std::env::args()
        .nth(2)
        .ok_or_else(|| anyhow::anyhow!("usage: ultidesk-agent {command} <host:port>"))?
        .parse()
        .context("the peer address must be host:port")?;
    let (dir, identity) = local_identity()?;
    Ok((addr, dir, identity))
}

/// Ask a paired peer for its audio endpoints — the first settings-IPC message.
fn peer_devices() -> Result<()> {
    let (addr, dir, identity) = peer_query_args("peer-devices")?;
    let store = load_peers(&dir, identity.public())?;

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let devices = rt.block_on(quic::peer_audio_devices(&identity, &store, addr))?;
    remember_where(&dir, store, &devices[..], addr)?;
    println!("{}", serde_json::to_string_pretty(&devices)?);
    tracing::info!(count = devices.len(), "read a peer's audio endpoints");
    Ok(())
}

/// Note where a peer was reached, so the relay can find it again without being told.
///
/// The answer's own labels identify the peer: every item carries the device id of the
/// machine that sent it, and that id was already checked against the key the handshake
/// proved. So this cannot attach an address to the wrong entry — it looks up the peer by
/// the id in the data it just verified.
fn remember_where<T>(
    dir: &std::path::Path,
    mut store: ultidesk_identity::PeerStore,
    answered: &[T],
    addr: std::net::SocketAddr,
) -> Result<()>
where
    T: OwnedItem,
{
    let Some(owner) = answered.first().map(OwnedItem::owner) else {
        // Nothing came back, so nothing identifies the peer. Not an error: a machine
        // really can have no endpoints.
        return Ok(());
    };
    let Some(key) = store
        .peers()
        .iter()
        .find(|p| p.key.device_id() == owner)
        .map(|p| p.key)
    else {
        return Ok(());
    };
    if store.remember_address(&key, &addr.to_string()) {
        store.save(dir)?;
    }
    Ok(())
}

/// Something a peer answered with, carrying the device that owns it.
trait OwnedItem {
    fn owner(&self) -> ultidesk_core::DeviceId;
}
impl OwnedItem for ultidesk_topology::AudioDevice {
    fn owner(&self) -> ultidesk_core::DeviceId {
        self.device_id
    }
}
impl OwnedItem for ultidesk_topology::Monitor {
    fn owner(&self) -> ultidesk_core::DeviceId {
        self.device_id
    }
}

/// Where the pointer is, if this platform will say.
///
/// Windows will. Wayland deliberately will not: a client is told the pointer's position
/// only while it is over that client's own surface, so a headless agent cannot ask. That
/// is not a gap to work around here — it is why the Linux side dead-reckons instead.
fn pointer_now() -> Option<(f64, f64)> {
    #[cfg(windows)]
    {
        ultidesk_platform_windows::cursor::cursor_position().map(|(x, y)| (x as f64, y as f64))
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// Print the layout both machines' pointers share, and where they touch.
///
/// The diagnostic for the KVM: if the pointer will not cross, this says whether the two
/// machines share a border at all — which is the first thing to know and, until now,
/// nothing could answer.
fn topology_report() -> Result<()> {
    let addr: std::net::SocketAddr = match std::env::args().nth(2) {
        Some(text) => text.parse().context("the peer address must be host:port")?,
        None => anyhow::bail!("usage: ultidesk-agent topology <peer host:port>"),
    };
    let (dir, identity) = local_identity()?;
    let store = load_peers(&dir, identity.public())?;

    use ipc::MonitorInventory;
    let local = ipc::RealMonitorInventory::new(identity.device_id())
        .monitors()
        .map_err(|e| anyhow::anyhow!(e))?;

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let assembled = rt.block_on(topology::assemble(&identity, &store, local, addr))?;

    for (i, m) in assembled.layout.monitors.iter().enumerate() {
        let whose = if assembled.local.contains(&i) {
            "this machine"
        } else {
            "peer"
        };
        println!(
            "{i}: {:<24} {whose:<12} {}x{} at ({},{})",
            m.friendly_name, m.logical_width, m.logical_height, m.logical_x, m.logical_y
        );
    }

    // Where the pointer is, when the platform will say. It decides which screen a
    // crossing would start from, so "the pointer is not where you think" is a real
    // answer to "why will it not cross".
    match pointer_now() {
        Some((x, y)) => match assembled.local_monitor_at(x, y) {
            Some(i) => println!(
                "
pointer at ({x},{y}) — on {}",
                assembled.layout.monitors[i].friendly_name
            ),
            None => println!(
                "
pointer at ({x},{y}) — not on any of this machine's screens, which                  happens in the gap between two that are not flush"
            ),
        },
        None => println!(
            "
pointer position: not available on this platform — Wayland does not tell a              client where the pointer is, which is why the KVM tracks it by dead reckoning              (see `ultidesk_platform_linux::pointer`)"
        ),
    }

    let borders = assembled.shared_borders();
    if borders.is_empty() {
        println!();
        println!("NO SHARED BORDER: the pointer cannot cross between these machines.");
    }
    for (from, to, adjacency) in borders {
        println!();
        println!(
            "{} -> {} on its {:?}, {}px of shared edge",
            assembled.layout.monitors[from].friendly_name,
            assembled.layout.monitors[to].friendly_name,
            adjacency.side,
            adjacency.span()
        );
    }
    Ok(())
}

/// Ask this machine's running agent something, exactly as the control app does.
///
/// Goes through the local IPC rather than doing the work in this process. That is the
/// point: it exercises the client, the transport, the gate and — for a peer question —
/// the relay, which is the whole path the UI depends on. Doing it in-process would test
/// none of them.
///
///     ask monitors | devices            about this machine
///     ask-peer monitors | devices [key] about a paired peer
fn ask(peer: bool) -> Result<()> {
    let what = std::env::args().nth(2);
    let query = match what.as_deref() {
        Some("monitors") => ipc::PeerQuery::Monitors,
        Some("devices") => ipc::PeerQuery::AudioDevices,
        _ => anyhow::bail!(
            "usage: ultidesk-agent {} <monitors|devices>{}",
            if peer { "ask-peer" } else { "ask" },
            if peer { " [peer key]" } else { "" }
        ),
    };

    let request = if peer {
        ipc::IpcRequest::AskPeer {
            peer: which_peer()?,
            query,
        }
    } else {
        match query {
            ipc::PeerQuery::Monitors => ipc::IpcRequest::ListMonitors,
            ipc::PeerQuery::AudioDevices => ipc::IpcRequest::ListAudioDevices,
        }
    };

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let response = rt.block_on(async {
        let mut client = ultidesk_ipc::Client::connect().await?;
        client.request(request).await
    })?;

    match response {
        ipc::IpcResponse::Monitors { monitors } => {
            println!("{}", serde_json::to_string_pretty(&monitors)?)
        }
        ipc::IpcResponse::AudioDevices { devices } => {
            println!("{}", serde_json::to_string_pretty(&devices)?)
        }
        ipc::IpcResponse::PeerMonitors { peer, monitors } => {
            eprintln!("from {}", peer.fingerprint());
            println!("{}", serde_json::to_string_pretty(&monitors)?);
        }
        ipc::IpcResponse::PeerAudioDevices { peer, devices } => {
            eprintln!("from {}", peer.fingerprint());
            println!("{}", serde_json::to_string_pretty(&devices)?);
        }
        other => anyhow::bail!("unexpected answer: {other:?}"),
    }
    Ok(())
}

/// The peer named on the command line, or the only one paired.
fn which_peer() -> Result<ultidesk_identity::PeerKey> {
    if let Some(text) = std::env::args().nth(3) {
        return ultidesk_identity::PeerKey::parse(&text)
            .ok_or_else(|| anyhow::anyhow!("not a public key; pass the value `peers` prints"));
    }
    let (dir, identity) = local_identity()?;
    let store = load_peers(&dir, identity.public())?;
    // Convenient for the two-machine desk, and refused rather than guessed at when there
    // is more than one: sending a question to the wrong machine is hard to notice.
    Ok(store
        .only_peer()
        .ok_or_else(|| {
            anyhow::anyhow!("several peers are paired, so name which one: ask-peer <query> <key>")
        })?
        .key)
}

/// Pair with another machine: establish a channel, compare a code, pin the key.
///
/// `pair` with no address listens; `pair <host:port>` dials. One machine does each, and
/// both operators must see the same six digits before answering yes.
fn pair() -> Result<()> {
    let target = std::env::args().nth(2);
    let (dir, identity) = local_identity()?;
    let mut store = load_peers(&dir, identity.public())?;

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    println!("this machine: {}", identity.fingerprint());
    let paired = match &target {
        Some(addr) => {
            let addr: std::net::SocketAddr = addr
                .parse()
                .context("the peer address must be host:port, e.g. 192.168.137.9:45872")?;
            rt.block_on(quic::pair_connect(&identity, addr))?
        }
        None => {
            let bind: std::net::SocketAddr = "0.0.0.0:45872".parse()?;
            rt.block_on(quic::pair_listen(&identity, bind))?
        }
    };

    println!();
    println!("  peer fingerprint: {}", paired.key.fingerprint());
    println!("  pairing code:     {}", paired.code);
    println!();
    println!("The other machine must be showing the SAME code. If it is not, answer no:");
    println!("two different codes is what a machine in the middle looks like.");

    if !confirm("Does the code match? [y/N] ")? {
        // Nothing is written. Refusing has to leave the machine exactly as it was.
        println!("not paired");
        return Ok(());
    }

    let name = prompt("Name for this peer: ")?;
    let outcome = store
        .pin(paired.key, &name, ultidesk_identity::peers::now_unix())
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    // Only the dialling side learns an address here; the listening side saw a source
    // port that will not be there next time, and recording it would be worse than
    // recording nothing.
    if let Some(addr) = &target {
        store.remember_address(&paired.key, addr);
    }
    store.save(&dir)?;

    match outcome {
        ultidesk_identity::PinOutcome::Added => println!("paired with {name}"),
        ultidesk_identity::PinOutcome::Unchanged => println!("{name} was already paired"),
        ultidesk_identity::PinOutcome::Renamed { previous } => {
            println!("already paired; renamed from {previous} to {name}")
        }
    }
    println!("now run `ultidesk-agent serve-peer` on both machines");
    Ok(())
}

/// List trusted devices, or change what one is allowed to do.
///
///     peers                        list
///     peers forget <key>           revoke trust entirely
///     peers allow  <key> <perm>    grant one permission
///     peers deny   <key> <perm>    revoke one permission
fn peers() -> Result<()> {
    let (dir, identity) = local_identity()?;
    let mut store = load_peers(&dir, identity.public())?;

    let verb = std::env::args().nth(2);
    match verb.as_deref() {
        Some("forget") => {
            let key = peer_key_arg(3)?;
            // Reported honestly rather than always claiming success: "forgotten" and
            // "was never there" are different answers to the same command.
            if store.forget(&key) {
                store.save(&dir)?;
                println!("forgot {}", key.fingerprint());
            } else {
                println!("{} was not paired", key.fingerprint());
            }
            return Ok(());
        }
        Some(v @ ("allow" | "deny")) => {
            let key = peer_key_arg(3)?;
            let name = std::env::args().nth(4).ok_or_else(|| {
                anyhow::anyhow!(
                    "usage: ultidesk-agent peers {v} <key> <{}>",
                    ultidesk_identity::Permissions::names().join("|")
                )
            })?;
            let allowed = v == "allow";
            if !store.set_permission(&key, &name, allowed)? {
                anyhow::bail!(
                    "{} is not paired, so there is nothing to {v}",
                    key.fingerprint()
                );
            }
            store.save(&dir)?;
            println!(
                "{} may now: {}",
                key.fingerprint(),
                describe(&store.permissions(&key))
            );
            return Ok(());
        }
        Some(other) => {
            anyhow::bail!("unknown peers command {other:?}; expected forget, allow or deny")
        }
        None => {}
    }

    println!(
        "this machine: {} ({})",
        identity.fingerprint(),
        identity.public()
    );
    if store.peers().is_empty() {
        println!("no peers are paired; run `ultidesk-agent pair`");
    }
    for peer in store.peers() {
        println!(
            "{}  {}  [{}]  last seen at {}  {}",
            peer.key.fingerprint(),
            peer.name,
            describe(&peer.permissions),
            // Shown because it is what the relay will try, and "never reached" is the
            // answer to a question an operator would otherwise have to guess at.
            peer.address.as_deref().unwrap_or("(never reached)"),
            peer.key
        );
    }
    Ok(())
}

/// What a peer may do, or a plain statement that it may do nothing.
///
/// Spelled out rather than shown as an empty list: "nothing" is a state an operator
/// should be able to read at a glance, not infer from blank space.
fn describe(permissions: &ultidesk_identity::Permissions) -> String {
    let granted = permissions.granted();
    if granted.is_empty() {
        "nothing".to_string()
    } else {
        granted.join(", ")
    }
}

/// Read a peer's public key from argument `n`.
fn peer_key_arg(n: usize) -> Result<ultidesk_identity::PeerKey> {
    let text = std::env::args()
        .nth(n)
        .ok_or_else(|| anyhow::anyhow!("expected a peer public key"))?;
    ultidesk_identity::PeerKey::parse(&text).ok_or_else(|| {
        anyhow::anyhow!("not a public key; pass the 64-character value `peers` prints")
    })
}

/// The peers this machine trusts, upgrading the store's schema once if it is old.
///
/// The write happens here rather than inside `load` so that reading stays reading — but
/// it does have to happen somewhere, or the "these peers kept their old access" warning
/// is printed on every launch for ever and stops being read.
fn load_peers(
    dir: &std::path::Path,
    local: ultidesk_identity::PeerKey,
) -> Result<ultidesk_identity::PeerStore> {
    let loaded = ultidesk_identity::peers::load(dir, local);
    if let Some(note) = &loaded.note {
        eprintln!("WARNING: {note}");
    }
    if loaded.migrated {
        loaded.store.save(dir)?;
    }
    Ok(loaded.store)
}

/// This machine's configuration directory and identity, or a message saying why not.
fn local_identity() -> Result<(std::path::PathBuf, ultidesk_identity::Identity)> {
    let dir = ultidesk_core::paths::config_dir().ok_or_else(|| {
        anyhow::anyhow!(
            "no configuration directory (no APPDATA on Windows, no HOME or XDG_CONFIG_HOME \
             on Linux); set ULTIDESK_CONFIG_DIR to choose one"
        )
    })?;
    let loaded = ultidesk_identity::load_or_create(&dir)?;
    if let Some(note) = &loaded.note {
        eprintln!("WARNING: {note}");
    }
    Ok((dir, loaded.identity))
}

/// Read a line, treating anything but an explicit yes as no.
///
/// Default-no on purpose: an operator who presses enter without reading has not
/// confirmed that two codes match, and pairing is the one moment where the security of
/// everything afterwards depends on them having actually looked.
fn confirm(question: &str) -> Result<bool> {
    let answer = prompt(question)?;
    Ok(matches!(answer.to_lowercase().as_str(), "y" | "yes"))
}

fn prompt(question: &str) -> Result<String> {
    use std::io::Write;
    print!("{question}");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(line.trim().to_string())
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

/// List the input devices this machine could capture, as JSON.
///
/// Read-only and raises no dialog. Its other job is to report the permission state
/// plainly: reading `/dev/input/event*` needs the `input` group, and the error says so
/// rather than leaving an empty list to be misread as "no devices".
#[cfg(target_os = "linux")]
fn input_devices() -> Result<()> {
    let devices = ultidesk_platform_linux::evdev_capture::enumerate()?;
    println!("{}", serde_json::to_string_pretty(&devices)?);
    tracing::info!(count = devices.len(), "enumerated capturable input devices");
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn input_devices() -> Result<()> {
    anyhow::bail!("input-devices reads /dev/input and is Linux-only")
}

/// Drive the real pointer with virtual input devices, with no portal involved.
///
/// The point of this subcommand is what it *does not* do: no session, no D-Bus, no
/// permission dialog. If the cursor moves, injection works unattended, which is the
/// requirement a KVM that is left running actually has.
///
/// Deliberately motion-only. Clicks and keystrokes would land in whatever window has
/// focus, and a probe should not be able to type into someone's editor.
#[cfg(target_os = "linux")]
fn uinput_test() -> Result<()> {
    use ultidesk_platform_linux::uinput::{normalise, UinputDevices};

    println!("Creating virtual input devices (no permission dialog should appear)...");
    let mut devices = UinputDevices::open()?;
    println!("Devices created and settled.");

    // With an explicit delta, make that one move and stop. Used to drive the pointer to
    // a known place so a screenshot can confirm it actually arrived — "the write
    // succeeded" and "the compositor moved the cursor" are different claims.
    let px: i32 = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(-1);
    let py: i32 = std::env::args()
        .nth(3)
        .and_then(|s| s.parse().ok())
        .unwrap_or(-1);
    let w: i32 = std::env::args()
        .nth(4)
        .and_then(|s| s.parse().ok())
        .unwrap_or(1920);
    let h: i32 = std::env::args()
        .nth(5)
        .and_then(|s| s.parse().ok())
        .unwrap_or(1080);
    if px >= 0 && py >= 0 {
        devices.pointer_position(normalise(px, w), normalise(py, h))?;
        println!("placed at {px},{py} of {w}x{h}");
        return Ok(());
    }

    // A square traced in absolute coordinates across the middle of the desktop.
    let (w, h) = (1920, 1080);
    let corners = [(600, 300), (1300, 300), (1300, 800), (600, 800), (600, 300)];
    for i in 0..corners.len() - 1 {
        let (x0, y0) = corners[i];
        let (x1, y1) = corners[i + 1];
        // Stepped rather than jumped: a teleport does not show that continuous motion
        // works, and continuous motion is what a KVM produces.
        for s in 0..=20 {
            let x = x0 + (x1 - x0) * s / 20;
            let y = y0 + (y1 - y0) * s / 20;
            devices.pointer_position(normalise(x, w), normalise(y, h))?;
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        println!("to {x1},{y1}");
    }

    println!("Done. If the cursor traced a square, uinput injection works unattended.");
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn uinput_test() -> Result<()> {
    anyhow::bail!("uinput-test drives /dev/uinput and is Linux-only")
}

/// Print the machine's audio endpoints as JSON.
///
/// The routing UI needs a device list from each machine, and this is the shape it reads.
/// The two platforms produce the same fields from completely different sources — the
/// PipeWire registry and the WASAPI endpoint enumeration — so this is also where a
/// divergence between them would show up first.
fn audio_devices() -> Result<()> {
    // The same call the IPC answers with, so the subcommand and the message can never
    // disagree about what this machine has.
    use ipc::AudioInventory;
    let (_dir, identity) = local_identity()?;
    let devices = ipc::RealAudioInventory::new(identity.device_id())
        .devices()
        .map_err(|e| anyhow::anyhow!(e))?;
    tracing::info!(count = devices.len(), "enumerated audio endpoints");
    println!("{}", serde_json::to_string_pretty(&devices)?);
    Ok(())
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
    let ep = ultidesk_ipc::Endpoint::generate();
    let (dir, identity) = local_identity()?;
    // Every endpoint this agent reports is labelled with the machine's derived id, so a
    // client can check the answer against the identity it authenticated.
    // The local IPC may reach peers on a caller's behalf; the peer-facing transports
    // below are built without a relay, so they have nothing to hop with.
    let device_id = identity.device_id();
    let store = load_peers(&dir, identity.public())?;
    let backends = Arc::new(
        ipc::LocalBackends::new(Arc::new(ipc::RealInjector), device_id).with_relay(Arc::new(
            relay::RelayContext::new(Arc::new(identity), store),
        )),
    );
    let path = ultidesk_ipc::write_handshake(&ep)?;
    // The pipe name is fine to log; the token is NOT logged.
    tracing::info!(pipe = %ep.endpoint_path, handshake = %path.display(), "agent IPC listening");
    // Also print the handshake path to stdout so a launching parent can find it.
    println!("{}", path.display());

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(
        async move { pipe::serve(ep.endpoint_path.clone(), ep.token.clone(), backends).await },
    )
}

/// Run the local IPC server on Unix, over a socket in the session's runtime directory.
///
/// The injector is `RealInjector`, which on Linux reports every injection as
/// unsupported. That is deliberate rather than an oversight: this surface exists for the
/// control app to *ask* the agent things — its monitors, its audio devices, its topology
/// — and opening `/dev/uinput` to create a pair of virtual input devices just to answer
/// a question would be a side effect nobody asked for. Injection on Linux is the peer
/// path's job (`serve-peer`), which picks uinput because it is about to be driven.
#[cfg(unix)]
fn serve() -> Result<()> {
    use std::sync::Arc;
    let ep = ultidesk_ipc::Endpoint::generate();
    let (dir, identity) = local_identity()?;
    // Every endpoint this agent reports is labelled with the machine's derived id, so a
    // client can check the answer against the identity it authenticated.
    // The local IPC may reach peers on a caller's behalf; the peer-facing transports
    // below are built without a relay, so they have nothing to hop with.
    let device_id = identity.device_id();
    let store = load_peers(&dir, identity.public())?;
    let backends = Arc::new(
        ipc::LocalBackends::new(Arc::new(ipc::RealInjector), device_id).with_relay(Arc::new(
            relay::RelayContext::new(Arc::new(identity), store),
        )),
    );

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(async move {
        let listener = unix_socket::bind(std::path::Path::new(&ep.endpoint_path)).await?;

        // Written only once the socket is bound. A handshake file naming a socket that
        // does not exist sends the client to a dead path, and the failure then looks
        // like the agent crashed rather than like it never started.
        let path = ultidesk_ipc::write_handshake(&ep)?;
        // The socket path is fine to log; the token is NOT logged.
        tracing::info!(
            socket = %ep.endpoint_path,
            handshake = %path.display(),
            "agent IPC listening"
        );
        println!("{}", path.display());

        unix_socket::serve(listener, ep.token.clone(), backends).await
    })
}

#[cfg(not(any(windows, unix)))]
fn serve() -> Result<()> {
    anyhow::bail!("no local IPC transport exists for this platform")
}
