//! `org.freedesktop.portal.InputCapture` — grabbing input at a screen edge.
//!
//! This is the **source** half of a KVM link, and the counterpart to
//! [`crate::remote_desktop`]. The compositor hands us pointer and keyboard events once
//! the pointer crosses a barrier we declared, and stops delivering them to local
//! windows until the capture is released. That is the Synergy/Barrier model, except
//! sanctioned by the compositor instead of fought with.
//!
//! # Lifecycle
//! `CreateSession` -> `GetZones` -> `SetPointerBarriers` -> `Enable`. Then the
//! `Activated` signal fires each time the pointer hits a barrier.
//!
//! # What this module does not do yet
//! The portal only *arbitrates* capture. The actual event stream arrives over
//! **libei**, obtained by calling `ConnectToEIS` and reading the returned file
//! descriptor with an EI client (the `reis` crate). Until that is wired, this module
//! can declare barriers, but no key or pointer event is delivered. That boundary is
//! deliberate and is not papered over.

use serde::{Deserialize, Serialize};

/// A rectangular region the compositor is willing to place barriers on, as returned by
/// `GetZones`. Coordinates are in the compositor's logical space.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Zone {
    pub width: u32,
    pub height: u32,
    pub x: i32,
    pub y: i32,
}

/// Which side of a zone to place a barrier on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Edge {
    Left,
    Right,
    Top,
    Bottom,
}

impl Edge {
    /// The edge a pointer arrives on when it crosses *into* a machine from this edge.
    ///
    /// Leaving the right edge of one screen means entering at the left edge of the
    /// next, so a KVM pairing always uses opposite edges on the two machines.
    pub fn opposite(self) -> Edge {
        match self {
            Edge::Left => Edge::Right,
            Edge::Right => Edge::Left,
            Edge::Top => Edge::Bottom,
            Edge::Bottom => Edge::Top,
        }
    }
}

/// A pointer barrier: a straight line on the boundary of a zone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Barrier {
    pub id: u32,
    pub x1: i32,
    pub y1: i32,
    pub x2: i32,
    pub y2: i32,
}

impl Barrier {
    pub fn position(&self) -> (i32, i32, i32, i32) {
        (self.x1, self.y1, self.x2, self.y2)
    }
    pub fn is_vertical(&self) -> bool {
        self.x1 == self.x2
    }
    pub fn is_horizontal(&self) -> bool {
        self.y1 == self.y2
    }
    /// The portal rejects any barrier that is not axis-aligned.
    pub fn is_axis_aligned(&self) -> bool {
        self.is_vertical() || self.is_horizontal()
    }
}

impl Zone {
    /// Rightmost pixel column *inside* the zone (`x + width - 1`).
    pub fn right(&self) -> i32 {
        self.x + self.width as i32 - 1
    }

    /// Bottommost pixel row inside the zone (`y + height - 1`).
    pub fn bottom(&self) -> i32 {
        self.y + self.height as i32 - 1
    }

    /// The boundary *line* down the right of the zone (`x + width`).
    pub fn right_edge(&self) -> i32 {
        self.x + self.width as i32
    }

    /// The boundary *line* across the bottom of the zone (`y + height`).
    pub fn bottom_edge(&self) -> i32 {
        self.y + self.height as i32
    }

    /// Build the barrier lying along one edge of this zone.
    ///
    /// The portal uses a **mixed** convention, established by submitting candidate
    /// encodings to KDE Plasma 6.7.2 and seeing which it accepted:
    ///
    /// - the coordinate *perpendicular* to the barrier is the boundary line, so the
    ///   right edge is `x + width`, not `x + width - 1`;
    /// - the extent *along* the barrier is an inclusive pixel range, so it ends at
    ///   `y + height - 1`, not `y + height`.
    ///
    /// Getting either half wrong is invisible at runtime: `SetPointerBarriers`
    /// succeeds and returns the id in `failed_barriers`, so the KVM edge simply never
    /// fires. All four edges were verified accepted in one call.
    pub fn barrier(&self, edge: Edge, id: u32) -> Barrier {
        let (x1, y1, x2, y2) = match edge {
            Edge::Left => (self.x, self.y, self.x, self.bottom()),
            Edge::Right => (self.right_edge(), self.y, self.right_edge(), self.bottom()),
            Edge::Top => (self.x, self.y, self.right(), self.y),
            Edge::Bottom => (self.x, self.bottom_edge(), self.right(), self.bottom_edge()),
        };
        Barrier { id, x1, y1, x2, y2 }
    }
}

#[cfg(target_os = "linux")]
pub use imp::InputCaptureSession;

#[cfg(not(target_os = "linux"))]
/// Stub so the type name resolves off Linux; every constructor fails.
pub struct InputCaptureSession;

#[cfg(not(target_os = "linux"))]
impl InputCaptureSession {
    pub fn open(_c: crate::caps::DeviceTypes) -> Result<Self, crate::portal::PortalError> {
        Err(crate::portal::PortalError::Unsupported)
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use super::*;
    use crate::caps::DeviceTypes;
    use crate::portal::PortalError;
    use crate::portal_call::{
        bus, call_and_await, close_session, session_handle, PORTAL_PATH, PORTAL_SERVICE,
    };
    use crate::request::sanitize_token;
    use std::collections::HashMap;
    use std::os::fd::OwnedFd;
    use zbus::blocking::{Connection, Proxy};
    use zbus::zvariant::{Array, OwnedObjectPath, Structure, Value};

    const INPUT_CAPTURE: &str = "org.freedesktop.portal.InputCapture";

    /// A capture session: barriers can be declared, and once enabled the compositor
    /// diverts input to us when the pointer crosses one.
    pub struct InputCaptureSession {
        conn: Connection,
        session: OwnedObjectPath,
        zone_set: u32,
        zones: Vec<Zone>,
        counter: std::cell::Cell<u64>,
    }

    impl InputCaptureSession {
        fn next_token(&self, prefix: &str) -> String {
            let n = self.counter.get();
            self.counter.set(n + 1);
            sanitize_token(&format!("ultidesk_ic_{prefix}_{n}"))
        }

        /// Create the session and immediately fetch the compositor's zones.
        pub fn open(capabilities: DeviceTypes) -> Result<Self, PortalError> {
            let conn = Connection::session().map_err(|e| PortalError::Connect(e.to_string()))?;

            let handle_token = sanitize_token("ultidesk_ic_create_0");
            let session_token = sanitize_token("ultidesk_ic_session_0");
            let mut opts: HashMap<&str, Value> = HashMap::new();
            opts.insert("handle_token", Value::from(handle_token.as_str()));
            opts.insert("session_handle_token", Value::from(session_token.as_str()));
            opts.insert("capabilities", Value::U32(capabilities.to_bits()));
            tracing::info!(
                capabilities = capabilities.to_bits(),
                "InputCapture step 1/3: CreateSession"
            );
            let results = call_and_await(
                &conn,
                INPUT_CAPTURE,
                "CreateSession",
                &handle_token,
                &("", opts),
            )?;

            let session = session_handle(&results, "CreateSession")?;

            let mut me = InputCaptureSession {
                conn,
                session,
                zone_set: 0,
                zones: Vec::new(),
                counter: std::cell::Cell::new(1),
            };
            me.refresh_zones()?;
            Ok(me)
        }

        /// Ask the compositor which regions may carry barriers.
        ///
        /// The `zone_set` is a generation counter: barriers must be submitted against
        /// the set they were computed from, or the compositor rejects them. Plugging in
        /// a monitor invalidates it, which is why this is re-runnable.
        pub fn refresh_zones(&mut self) -> Result<(), PortalError> {
            let tok = self.next_token("zones");
            let mut opts: HashMap<&str, Value> = HashMap::new();
            opts.insert("handle_token", Value::from(tok.as_str()));
            tracing::info!("InputCapture step 2/3: GetZones");
            let results = call_and_await(
                &self.conn,
                INPUT_CAPTURE,
                "GetZones",
                &tok,
                &(self.session.clone(), opts),
            )?;

            let zone_set: u32 = results
                .get("zone_set")
                .ok_or_else(|| PortalError::Bus("GetZones returned no zone_set".into()))
                .and_then(|v| u32::try_from(v.clone()).map_err(bus))?;

            let raw = results
                .get("zones")
                .ok_or_else(|| PortalError::Bus("GetZones returned no zones".into()))?;
            let arr = Array::try_from(raw.clone()).map_err(bus)?;

            let mut zones = Vec::new();
            for item in arr.iter() {
                let st = Structure::try_from(item.try_clone().map_err(bus)?).map_err(bus)?;
                let f = st.fields();
                if f.len() < 4 {
                    return Err(PortalError::Bus(format!(
                        "a zone had {} fields, expected 4 (uuii)",
                        f.len()
                    )));
                }
                zones.push(Zone {
                    width: u32::try_from(f[0].try_clone().map_err(bus)?).map_err(bus)?,
                    height: u32::try_from(f[1].try_clone().map_err(bus)?).map_err(bus)?,
                    x: i32::try_from(f[2].try_clone().map_err(bus)?).map_err(bus)?,
                    y: i32::try_from(f[3].try_clone().map_err(bus)?).map_err(bus)?,
                });
            }

            self.zone_set = zone_set;
            self.zones = zones;
            Ok(())
        }

        pub fn zones(&self) -> &[Zone] {
            &self.zones
        }

        pub fn zone_set(&self) -> u32 {
            self.zone_set
        }

        /// Declare pointer barriers.
        ///
        /// Returns the ids the compositor **rejected**. A rejected barrier is not an
        /// error at the D-Bus level: the call succeeds and the barrier simply never
        /// fires, so callers must inspect this rather than assume success.
        pub fn set_barriers(&self, barriers: &[Barrier]) -> Result<Vec<u32>, PortalError> {
            let tok = self.next_token("barriers");
            let mut opts: HashMap<&str, Value> = HashMap::new();
            opts.insert("handle_token", Value::from(tok.as_str()));

            // Wire shape is `aa{sv}` — an array of plain dicts — with the id carried
            // *inside* each dict as "barrier_id". It is not `a(ua{sv})`; sending the id
            // as a struct field is rejected outright:
            //   Type of message, "(oa{sv}a(ua{sv})u)", does not match expected type
            //   "(oa{sv}aa{sv}u)"
            let mut wire: Vec<HashMap<&str, Value>> = Vec::new();
            for b in barriers {
                let mut m: HashMap<&str, Value> = HashMap::new();
                m.insert("barrier_id", Value::U32(b.id));
                m.insert("position", Value::from((b.x1, b.y1, b.x2, b.y2)));
                wire.push(m);
            }

            tracing::info!(
                count = wire.len(),
                zone_set = self.zone_set,
                "InputCapture step 3/3: SetPointerBarriers"
            );
            let results = call_and_await(
                &self.conn,
                INPUT_CAPTURE,
                "SetPointerBarriers",
                &tok,
                &(self.session.clone(), opts, wire, self.zone_set),
            )?;

            let failed = match results.get("failed_barriers") {
                None => Vec::new(),
                Some(v) => {
                    let arr = Array::try_from(v.clone()).map_err(bus)?;
                    let mut out = Vec::new();
                    for item in arr.iter() {
                        out.push(u32::try_from(item.try_clone().map_err(bus)?).map_err(bus)?);
                    }
                    out
                }
            };
            Ok(failed)
        }

        /// Arm the barriers.
        ///
        /// Deliberately not called by any test path: once enabled, the pointer is
        /// diverted to us on contact with a barrier, and with no libei client reading
        /// the events that would strand the pointer on the source machine.
        pub fn enable(&self) -> Result<(), PortalError> {
            let opts: HashMap<&str, Value> = HashMap::new();
            Proxy::new(&self.conn, PORTAL_SERVICE, PORTAL_PATH, INPUT_CAPTURE)
                .map_err(bus)?
                .call_method("Enable", &(self.session.clone(), opts))
                .map_err(bus)?;
            Ok(())
        }

        pub fn disable(&self) -> Result<(), PortalError> {
            let opts: HashMap<&str, Value> = HashMap::new();
            Proxy::new(&self.conn, PORTAL_SERVICE, PORTAL_PATH, INPUT_CAPTURE)
                .map_err(bus)?
                .call_method("Disable", &(self.session.clone(), opts))
                .map_err(bus)?;
            Ok(())
        }

        /// Open the EIS connection that carries captured input events.
        ///
        /// The portal only *arbitrates* capture: it decides when the pointer crosses a
        /// barrier, but the events themselves arrive over **libei** on this file
        /// descriptor. Returning an already-authorised fd means the client never needs
        /// access to the compositor's EIS socket directly.
        ///
        /// Direct D-Bus method, so unlike the session setup there is no Request to
        /// await here.
        pub fn connect_to_eis(&self) -> Result<OwnedFd, PortalError> {
            let opts: HashMap<&str, Value> = HashMap::new();
            let reply = Proxy::new(&self.conn, PORTAL_SERVICE, PORTAL_PATH, INPUT_CAPTURE)
                .map_err(bus)?
                .call_method("ConnectToEIS", &(self.session.clone(), opts))
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

    const FHD: Zone = Zone {
        width: 1920,
        height: 1080,
        x: 0,
        y: 0,
    };

    // The expected values below are not derived from the spec prose, which is
    // ambiguous. They are the encodings KDE Plasma 6.7.2 actually accepted when all
    // four were submitted together; an earlier all-inclusive reading was rejected.

    #[test]
    fn right_barrier_sits_on_the_boundary_line_not_the_last_pixel() {
        let b = FHD.barrier(Edge::Right, 1);
        assert_eq!(b.position(), (1920, 0, 1920, 1079));
        assert_ne!(
            b.x1, 1919,
            "x must be the boundary line, not the last column"
        );
        assert_ne!(
            b.y2, 1080,
            "y extent must be inclusive, not the boundary line"
        );
    }

    #[test]
    fn every_edge_matches_what_the_compositor_accepted() {
        assert_eq!(FHD.barrier(Edge::Left, 1).position(), (0, 0, 0, 1079));
        assert_eq!(
            FHD.barrier(Edge::Right, 2).position(),
            (1920, 0, 1920, 1079)
        );
        assert_eq!(FHD.barrier(Edge::Top, 3).position(), (0, 0, 1919, 0));
        assert_eq!(
            FHD.barrier(Edge::Bottom, 4).position(),
            (0, 1080, 1919, 1080)
        );
    }

    #[test]
    fn barriers_are_always_axis_aligned() {
        for edge in [Edge::Left, Edge::Right, Edge::Top, Edge::Bottom] {
            let b = FHD.barrier(edge, 7);
            assert!(b.is_axis_aligned(), "{edge:?} produced a diagonal barrier");
        }
        assert!(FHD.barrier(Edge::Left, 1).is_vertical());
        assert!(FHD.barrier(Edge::Top, 1).is_horizontal());
    }

    #[test]
    fn a_secondary_monitor_offset_shifts_the_barrier() {
        // Multi-monitor is the normal case, so an origin-only implementation would
        // pass a naive test and fail on real hardware.
        let second = Zone {
            width: 1920,
            height: 1080,
            x: 1920,
            y: 0,
        };
        assert_eq!(
            second.barrier(Edge::Right, 1).position(),
            (3840, 0, 3840, 1079)
        );
        assert_eq!(
            second.barrier(Edge::Left, 2).position(),
            (1920, 0, 1920, 1079)
        );
    }

    #[test]
    fn adjacent_monitors_share_one_boundary_line() {
        // The right edge of the primary and the left edge of the monitor beside it are
        // the same line. That identity is what makes edge crossing coherent, and it
        // only holds under the boundary-line convention.
        let a = Zone {
            width: 1920,
            height: 1080,
            x: 0,
            y: 0,
        };
        let b = Zone {
            width: 1920,
            height: 1080,
            x: 1920,
            y: 0,
        };
        assert_eq!(a.barrier(Edge::Right, 1).x1, b.barrier(Edge::Left, 2).x1);
    }

    #[test]
    fn negative_origins_are_handled() {
        let above = Zone {
            width: 1280,
            height: 720,
            x: -1280,
            y: -720,
        };
        assert_eq!(above.barrier(Edge::Right, 1).position(), (0, -720, 0, -1));
    }

    #[test]
    fn edges_pair_with_their_opposite() {
        // Leaving right means arriving left on the peer; pairing an edge with itself
        // sends the pointer straight back where it came from.
        assert_eq!(Edge::Right.opposite(), Edge::Left);
        assert_eq!(Edge::Top.opposite(), Edge::Bottom);
        for e in [Edge::Left, Edge::Right, Edge::Top, Edge::Bottom] {
            assert_eq!(e.opposite().opposite(), e);
        }
    }

    #[test]
    fn a_single_pixel_zone_does_not_underflow() {
        let tiny = Zone {
            width: 1,
            height: 1,
            x: 5,
            y: 5,
        };
        assert_eq!(tiny.right(), 5);
        assert_eq!(tiny.right_edge(), 6);
        assert_eq!(tiny.barrier(Edge::Right, 1).position(), (6, 5, 6, 5));
    }
}
