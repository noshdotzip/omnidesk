//! `org.freedesktop.portal.ScreenCast` — capturing a window or monitor.
//!
//! This is the source side of window projection. Per ADR-0009 the compositor owns the
//! picker: we declare *what kind* of source we will accept, and `Start` makes the
//! compositor show its own chooser. We never learn about windows the user did not pick.
//!
//! # Lifecycle
//! `CreateSession` -> `SelectSources` -> `Start` -> `OpenPipeWireRemote`. Only `Start`
//! shows the picker; the first two steps are silent, which is what makes a capability
//! check possible without interrupting the user.
//!
//! # What this module does not do yet
//! `OpenPipeWireRemote` returns a file descriptor for a PipeWire connection, and
//! turning that into frames needs a PipeWire client. Until that exists this module can
//! negotiate a session but produces no video.

use crate::caps::SourceTypes;

/// How the cursor should appear in the captured stream. Bit values are fixed by the
/// portal specification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorMode {
    /// Not captured at all.
    Hidden,
    /// Composited into the video frames.
    Embedded,
    /// Sent out of band as position metadata, leaving frames clean.
    Metadata,
}

impl CursorMode {
    pub const HIDDEN: u32 = 1;
    pub const EMBEDDED: u32 = 2;
    pub const METADATA: u32 = 4;

    pub fn to_bits(self) -> u32 {
        match self {
            CursorMode::Hidden => Self::HIDDEN,
            CursorMode::Embedded => Self::EMBEDDED,
            CursorMode::Metadata => Self::METADATA,
        }
    }

    /// Whether a portal advertising `available` supports this mode.
    pub fn is_available_in(self, available: u32) -> bool {
        available & self.to_bits() != 0
    }

    /// Pick the best mode a portal actually offers.
    ///
    /// `Metadata` is preferred: it keeps the cursor out of the encoded frames, so the
    /// receiver can draw it locally at its own refresh rate instead of inheriting the
    /// sender's latency on the one element the eye tracks most closely. `Embedded` is
    /// the fallback, and `Hidden` the last resort.
    pub fn best_available(available: u32) -> Option<CursorMode> {
        [
            CursorMode::Metadata,
            CursorMode::Embedded,
            CursorMode::Hidden,
        ]
        .into_iter()
        .find(|mode| mode.is_available_in(available))
    }
}

/// What to ask `SelectSources` for.
#[derive(Debug, Clone, Copy)]
pub struct CastOptions {
    /// Which source kinds are acceptable. Defaults to windows only.
    pub types: SourceTypes,
    /// Allow the user to pick more than one source.
    pub multiple: bool,
    pub cursor: CursorMode,
}

/// A grant returned by a previous `Start`, replayed to skip the picker.
///
/// Without this the compositor shows its chooser on every launch, which for a window
/// that is projected all day reads as broken rather than secure.
#[derive(Debug, Clone, Default)]
pub struct CastGrant {
    pub restore_token: Option<String>,
}

impl Default for CastOptions {
    fn default() -> Self {
        CastOptions {
            // Window-only by default: Ultidesk projects windows, and offering a whole
            // monitor here would quietly turn it into screen sharing (see README).
            types: SourceTypes {
                monitor: false,
                window: true,
                virtual_display: false,
            },
            multiple: false,
            cursor: CursorMode::Metadata,
        }
    }
}

impl CastOptions {
    pub fn type_bits(&self) -> u32 {
        let mut bits = 0;
        if self.types.monitor {
            bits |= SourceTypes::MONITOR;
        }
        if self.types.window {
            bits |= SourceTypes::WINDOW;
        }
        if self.types.virtual_display {
            bits |= SourceTypes::VIRTUAL;
        }
        bits
    }
}

/// What a granted `Start` yields.
#[derive(Debug, Clone, Default)]
pub struct StartedCast {
    /// PipeWire node ids, one per source the user granted.
    pub nodes: Vec<u32>,
    /// Grant to replay next time so the picker does not reappear.
    pub restore_token: Option<String>,
}

#[cfg(target_os = "linux")]
pub use imp::ScreenCastSession;

#[cfg(not(target_os = "linux"))]
/// Stub so the type name resolves off Linux; every constructor fails.
pub struct ScreenCastSession;

#[cfg(not(target_os = "linux"))]
impl ScreenCastSession {
    pub fn open() -> Result<Self, crate::portal::PortalError> {
        Err(crate::portal::PortalError::Unsupported)
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use super::*;
    use crate::portal::PortalError;
    use crate::portal_call::{
        bus, call_and_await, close_session, session_handle, PORTAL_PATH, PORTAL_SERVICE,
    };
    use crate::request::sanitize_token;
    use std::collections::HashMap;
    use std::os::fd::OwnedFd;
    use zbus::blocking::{Connection, Proxy};
    use zbus::zvariant::{OwnedObjectPath, Value};

    const SCREEN_CAST: &str = "org.freedesktop.portal.ScreenCast";

    /// A ScreenCast session that has been created but not necessarily started.
    pub struct ScreenCastSession {
        conn: Connection,
        session: OwnedObjectPath,
        counter: std::cell::Cell<u64>,
    }

    impl ScreenCastSession {
        fn next_token(&self, prefix: &str) -> String {
            let n = self.counter.get();
            self.counter.set(n + 1);
            sanitize_token(&format!("ultidesk_sc_{prefix}_{n}"))
        }

        /// Create the session. Silent: no dialog is raised by this step.
        pub fn open() -> Result<Self, PortalError> {
            let conn = Connection::session().map_err(|e| PortalError::Connect(e.to_string()))?;
            let handle_token = sanitize_token("ultidesk_sc_create_0");
            let session_token = sanitize_token("ultidesk_sc_session_0");
            let mut opts: HashMap<&str, Value> = HashMap::new();
            opts.insert("handle_token", Value::from(handle_token.as_str()));
            opts.insert("session_handle_token", Value::from(session_token.as_str()));
            tracing::info!("ScreenCast step 1/3: CreateSession (no dialog expected)");
            let results =
                call_and_await(&conn, SCREEN_CAST, "CreateSession", &handle_token, &(opts,))?;

            let session = session_handle(&results, "CreateSession")?;

            Ok(ScreenCastSession {
                conn,
                session,
                counter: std::cell::Cell::new(1),
            })
        }

        /// Declare what kinds of source we will accept. Also silent.
        pub fn select_sources(
            &self,
            options: CastOptions,
            grant: &CastGrant,
        ) -> Result<(), PortalError> {
            let tok = self.next_token("select");
            let mut opts: HashMap<&str, Value> = HashMap::new();
            opts.insert("handle_token", Value::from(tok.as_str()));
            opts.insert("types", Value::U32(options.type_bits()));
            opts.insert("multiple", Value::Bool(options.multiple));
            opts.insert("cursor_mode", Value::U32(options.cursor.to_bits()));
            // 2 = persistent, so the compositor can hand back a restore token.
            opts.insert("persist_mode", Value::U32(2));
            if let Some(rt) = grant.restore_token.as_deref() {
                opts.insert("restore_token", Value::from(rt));
            }
            tracing::info!(
                types = options.type_bits(),
                cursor = options.cursor.to_bits(),
                "ScreenCast step 2/3: SelectSources (no dialog expected)"
            );
            call_and_await(
                &self.conn,
                SCREEN_CAST,
                "SelectSources",
                &tok,
                &(self.session.clone(), opts),
            )?;
            Ok(())
        }

        /// Start the cast. **This raises the compositor's own window picker** and
        /// blocks until the user chooses or cancels (ADR-0009).
        ///
        /// Returns the PipeWire node ids of the streams the user granted.
        pub fn start(&self) -> Result<StartedCast, PortalError> {
            let tok = self.next_token("start");
            let mut opts: HashMap<&str, Value> = HashMap::new();
            opts.insert("handle_token", Value::from(tok.as_str()));
            tracing::info!("ScreenCast step 3/3: Start — RAISES THE COMPOSITOR'S PICKER");
            let results = call_and_await(
                &self.conn,
                SCREEN_CAST,
                "Start",
                &tok,
                &(self.session.clone(), "", opts),
            )?;

            let restore_token = results
                .get("restore_token")
                .and_then(|v| String::try_from(v.clone()).ok());
            let mut nodes = Vec::new();
            if let Some(v) = results.get("streams") {
                let arr = zbus::zvariant::Array::try_from(v.clone()).map_err(bus)?;
                for item in arr.iter() {
                    let st = zbus::zvariant::Structure::try_from(item.try_clone().map_err(bus)?)
                        .map_err(bus)?;
                    let f = st.fields();
                    if let Some(first) = f.first() {
                        nodes.push(u32::try_from(first.try_clone().map_err(bus)?).map_err(bus)?);
                    }
                }
            }
            Ok(StartedCast {
                nodes,
                restore_token,
            })
        }

        /// Open the PipeWire connection carrying the granted streams.
        ///
        /// Returns a file descriptor, not a socket path: the portal hands over an
        /// already-authorised connection so the client never needs access to the
        /// PipeWire socket itself. It is a direct method with no Request/Response, so
        /// there is no signal to await here.
        pub fn open_pipewire_remote(&self) -> Result<OwnedFd, PortalError> {
            let opts: HashMap<&str, Value> = HashMap::new();
            let reply = Proxy::new(&self.conn, PORTAL_SERVICE, PORTAL_PATH, SCREEN_CAST)
                .map_err(bus)?
                .call_method("OpenPipeWireRemote", &(self.session.clone(), opts))
                .map_err(bus)?;
            let fd: zbus::zvariant::OwnedFd = reply.body().deserialize().map_err(bus)?;
            Ok(OwnedFd::from(fd))
        }

        pub fn close(&self) -> Result<(), PortalError> {
            close_session(&self.conn, &self.session)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_modes_map_to_spec_bits() {
        assert_eq!(CursorMode::Hidden.to_bits(), 1);
        assert_eq!(CursorMode::Embedded.to_bits(), 2);
        assert_eq!(CursorMode::Metadata.to_bits(), 4);
    }

    #[test]
    fn best_available_prefers_metadata_over_embedded() {
        // Metadata keeps the cursor out of the encoded frames so the receiver can draw
        // it locally; embedding it inherits sender latency on the one element the eye
        // tracks most closely.
        assert_eq!(CursorMode::best_available(7), Some(CursorMode::Metadata));
        assert_eq!(CursorMode::best_available(3), Some(CursorMode::Embedded));
        assert_eq!(CursorMode::best_available(1), Some(CursorMode::Hidden));
    }

    #[test]
    fn best_available_is_none_when_nothing_is_offered() {
        // Must not fall back to a mode the portal never advertised.
        assert_eq!(CursorMode::best_available(0), None);
    }

    #[test]
    fn availability_check_ignores_unrelated_bits() {
        assert!(CursorMode::Metadata.is_available_in(CursorMode::METADATA));
        assert!(!CursorMode::Metadata.is_available_in(CursorMode::EMBEDDED));
        assert!(!CursorMode::Hidden.is_available_in(0));
    }

    #[test]
    fn default_options_request_windows_only_not_monitors() {
        // Ultidesk projects windows. Asking for MONITOR here would quietly turn the
        // product into screen sharing, which the README explicitly says it is not.
        let o = CastOptions::default();
        assert_eq!(o.type_bits(), SourceTypes::WINDOW);
        assert!(!o.multiple);
        assert_eq!(o.cursor, CursorMode::Metadata);
    }

    #[test]
    fn type_bits_combine_when_several_kinds_are_allowed() {
        let o = CastOptions {
            types: SourceTypes {
                monitor: true,
                window: true,
                virtual_display: false,
            },
            ..CastOptions::default()
        };
        assert_eq!(o.type_bits(), SourceTypes::MONITOR | SourceTypes::WINDOW);
    }
}
