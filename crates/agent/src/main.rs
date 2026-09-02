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

mod endpoint;
mod ipc;
#[cfg(windows)]
mod pipe;

use anyhow::Result;
use ipc::Injector;

fn main() -> Result<()> {
    init_tracing();
    let mode = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "serve".to_string());
    match mode.as_str() {
        "enumerate" => enumerate(),
        "serve" => serve(),
        other => {
            eprintln!("unknown subcommand: {other}");
            eprintln!("usage: ultidesk-agent [serve|enumerate]");
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
