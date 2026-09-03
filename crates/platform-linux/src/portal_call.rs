//! Shared machinery for calling a portal method and awaiting its `Response`.
//!
//! Every XDG portal uses the same asynchronous shape: the method returns a `Request`
//! object path, and the answer arrives later as a signal. Both `RemoteDesktop` and
//! `InputCapture` need it, and getting it subtly wrong produces a silent permanent
//! hang rather than an error, so it lives in exactly one place.
//!
//! Linux-only: on other platforms nothing in this module is compiled.

#![cfg(target_os = "linux")]

use crate::portal::PortalError;
use crate::request::{request_path, ResponseCode};
use std::collections::HashMap;
use std::sync::mpsc;
use std::time::Duration;
use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::OwnedValue;

pub const PORTAL_SERVICE: &str = "org.freedesktop.portal.Desktop";
pub const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
pub const REQUEST_IFACE: &str = "org.freedesktop.portal.Request";
pub const SESSION_IFACE: &str = "org.freedesktop.portal.Session";

/// How long to wait for a portal Response. Generous, because a `Start`-like call waits
/// on a human reading a dialog, but finite: an unanswered prompt must not pin a caller
/// forever.
pub const RESPONSE_TIMEOUT: Duration = Duration::from_secs(120);

pub fn bus(e: impl std::fmt::Display) -> PortalError {
    PortalError::Bus(e.to_string())
}

/// Lets `call_and_await` accept differently-shaped argument tuples without each portal
/// needing its own copy of the call logic.
pub trait DynArgs {
    fn call(&self, proxy: &Proxy<'_>, method: &str) -> Result<(), PortalError>;
}

impl<T: serde::Serialize + zbus::zvariant::DynamicType> DynArgs for T {
    fn call(&self, proxy: &Proxy<'_>, method: &str) -> Result<(), PortalError> {
        proxy.call_method(method, self).map_err(bus)?;
        Ok(())
    }
}

/// Call `interface.method` on the desktop portal and block until its `Response` signal
/// arrives, or until [`RESPONSE_TIMEOUT`] elapses.
///
/// The subscription is established on a worker thread *before* the method call is
/// issued. Both halves of that matter:
///
/// - **Order**: the portal may emit `Response` before the method call returns. A caller
///   that subscribes afterwards can miss it and then wait forever with no error.
/// - **Bound**: a permission dialog nobody answers must surface as
///   [`PortalError::TimedOut`], distinct from the user actively refusing
///   ([`PortalError::Denied`]) — they call for completely different handling.
pub fn call_and_await(
    conn: &Connection,
    interface: &'static str,
    method: &str,
    token: &str,
    body: &dyn DynArgs,
) -> Result<HashMap<String, OwnedValue>, PortalError> {
    let unique = conn
        .unique_name()
        .ok_or_else(|| PortalError::Bus("connection has no unique name".into()))?
        .to_string();
    let path = request_path(&unique, token);

    let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();
    let (resp_tx, resp_rx) = mpsc::channel();
    let conn_for_thread = conn.clone();
    let signal_path = path.clone();
    std::thread::spawn(move || {
        let req = match Proxy::new(&conn_for_thread, PORTAL_SERVICE, signal_path, REQUEST_IFACE) {
            Ok(p) => p,
            Err(e) => {
                let _ = ready_tx.send(Err(e.to_string()));
                return;
            }
        };
        let mut signals = match req.receive_signal("Response") {
            Ok(s) => s,
            Err(e) => {
                let _ = ready_tx.send(Err(e.to_string()));
                return;
            }
        };
        if ready_tx.send(Ok(())).is_err() {
            return; // caller gave up before the subscription was ready
        }
        let _ = resp_tx.send(signals.next());
    });

    match ready_rx.recv() {
        Ok(Ok(())) => {}
        Ok(Err(e)) => return Err(PortalError::Bus(e)),
        Err(e) => return Err(PortalError::Bus(e.to_string())),
    }

    let portal = Proxy::new(conn, PORTAL_SERVICE, PORTAL_PATH, interface).map_err(bus)?;
    body.call(&portal, method)?;

    let msg = match resp_rx.recv_timeout(RESPONSE_TIMEOUT) {
        Ok(Some(msg)) => msg,
        Ok(None) => return Err(PortalError::Bus(format!("no Response for {method}"))),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            return Err(PortalError::TimedOut(method.to_string()))
        }
        Err(e) => return Err(PortalError::Bus(e.to_string())),
    };

    let (code, results): (u32, HashMap<String, OwnedValue>) =
        msg.body().deserialize().map_err(bus)?;

    match ResponseCode::from_raw(code) {
        ResponseCode::Success => Ok(results),
        ResponseCode::Cancelled => Err(PortalError::Denied(method.to_string())),
        other => Err(PortalError::Bus(format!("{method} ended: {other:?}"))),
    }
}

/// Pull the `session_handle` out of a portal Response.
///
/// Portals are not consistent about its D-Bus type: RemoteDesktop and ScreenCast
/// return it as a string (`s`), while InputCapture returns a real object path (`o`).
/// Assuming either one alone produces a bare "incorrect type" from zvariant with no
/// clue which portal or which field is at fault, so both shapes are accepted here in
/// one place.
pub fn session_handle(
    results: &HashMap<String, OwnedValue>,
    method: &str,
) -> Result<zbus::zvariant::OwnedObjectPath, PortalError> {
    let v = results
        .get("session_handle")
        .ok_or_else(|| PortalError::Bus(format!("{method} returned no session_handle")))?;
    if let Ok(path) = zbus::zvariant::OwnedObjectPath::try_from(v.clone()) {
        return Ok(path);
    }
    let s: String = v.clone().try_into().map_err(|_| {
        PortalError::Bus(format!(
            "{method} session_handle was neither an object path nor a string"
        ))
    })?;
    zbus::zvariant::OwnedObjectPath::try_from(s).map_err(bus)
}

/// Close a portal session object.
pub fn close_session(
    conn: &Connection,
    session: &zbus::zvariant::OwnedObjectPath,
) -> Result<(), PortalError> {
    let p = Proxy::new(conn, PORTAL_SERVICE, session.clone(), SESSION_IFACE).map_err(bus)?;
    p.call_method("Close", &()).map_err(bus)?;
    Ok(())
}
