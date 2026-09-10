//! Building the one layout both machines' pointers live in.
//!
//! The KVM's whole question — "has the pointer left this machine, and where does it
//! arrive?" — is asked against a layout that spans both. Until now nothing assembled one:
//! the control app had the local screens and a placeholder, and the agent had no idea the
//! peer had screens at all.
//!
//! It can now. This machine's monitors come from its own windowless enumerator, the
//! peer's arrive over the authenticated channel, and `ultidesk_topology::left_to_right`
//! puts them in a strip.
//!
//! # A peer that cannot be reached is not a layout with a gap in it
//! It is *no* layout. A strip built from one machine has no border to another, so every
//! crossing check would answer "stay put" — which is correct but indistinguishable from
//! "the peer is there and the pointer is nowhere near an edge". Reporting the failure
//! instead means the operator is told the peer is unreachable rather than left wondering
//! why the pointer will not cross.
//!
//! # Saved arrangements are not read here yet
//! The operator's arrangement lives in the control app's settings file, which this
//! process does not own. Until settings move behind the IPC — so there is one writer —
//! the daemon uses the default strip, which is coherent and gives a shared border, but is
//! not necessarily where the operator put things. Recorded in next.md rather than
//! silently differing from what the editor shows.

use std::net::SocketAddr;

use ultidesk_identity::{Identity, PeerStore};
use ultidesk_topology::{Layout, MachineMonitors, Monitor};

/// Both machines' screens in one plane, and which of them are this machine's.
pub struct Topology {
    pub layout: Layout,
    /// Indices into `layout.monitors` that belong to this machine.
    ///
    /// Kept rather than recomputed from the device id at each use: the crossing check
    /// runs on every pointer motion, and rediscovering which screens are ours on each
    /// one would put a scan on the hot path.
    pub local: Vec<usize>,
}

impl Topology {
    /// Which of this machine's screens the pointer is on, if any.
    ///
    /// `None` when the pointer is outside every local screen, which happens in the gap
    /// between two monitors that are not flush — a real position the OS allows, and not
    /// somewhere a crossing can be computed from.
    pub fn local_monitor_at(&self, x: f64, y: f64) -> Option<usize> {
        self.local.iter().copied().find(|&i| {
            let m = &self.layout.monitors[i];
            x >= m.logical_x && x < m.right() && y >= m.logical_y && y < m.bottom()
        })
    }

    /// Every border this machine shares with another one, for reporting.
    pub fn shared_borders(&self) -> Vec<(usize, usize, ultidesk_topology::Adjacency)> {
        let mut out = Vec::new();
        for &local in &self.local {
            for (other, monitor) in self.layout.monitors.iter().enumerate() {
                if monitor.device_id == self.layout.monitors[local].device_id {
                    continue;
                }
                if let Some(adjacency) = self.layout.adjacency(local, other) {
                    out.push((local, other, adjacency));
                }
            }
        }
        out
    }
}

/// Assemble the layout by asking this machine and then the peer.
pub async fn assemble(
    identity: &Identity,
    peers: &PeerStore,
    local: Vec<Monitor>,
    peer_addr: SocketAddr,
) -> anyhow::Result<Topology> {
    if local.is_empty() {
        anyhow::bail!("this machine reports no monitors, so there is nothing to cross from");
    }
    let peer = crate::quic::peer_monitors(identity, peers, peer_addr).await?;
    if peer.is_empty() {
        anyhow::bail!("the peer reports no monitors, so there is nowhere to cross to");
    }

    let local_device = local[0].device_id;
    let local_count = local.len();
    let layout = ultidesk_topology::left_to_right(vec![
        MachineMonitors {
            device_id: local_device,
            monitors: local,
        },
        MachineMonitors {
            device_id: peer[0].device_id,
            monitors: peer,
        },
    ]);

    // This machine is the first block, so its screens keep the indices they went in with.
    // Asserted rather than assumed, because the crossing check consults `local` on every
    // motion and a wrong index there would send the pointer to the wrong machine.
    let local_indices: Vec<usize> = layout
        .monitors
        .iter()
        .enumerate()
        .filter(|(_, m)| m.device_id == local_device)
        .map(|(i, _)| i)
        .collect();
    debug_assert_eq!(local_indices.len(), local_count);

    Ok(Topology {
        layout,
        local: local_indices,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ultidesk_core::DeviceId;
    use ultidesk_topology::{MonitorId, Rotation};

    fn machine(seed: u8) -> DeviceId {
        DeviceId::from_uuid(uuid::Uuid::from_bytes([seed; 16]))
    }

    fn monitor(device: DeviceId, name: &str, x: f64, w: f64) -> Monitor {
        Monitor {
            device_id: device,
            monitor_id: MonitorId(1),
            friendly_name: name.into(),
            logical_x: x,
            logical_y: 0.0,
            logical_width: w,
            logical_height: 1080.0,
            native_pixel_width: w as u32,
            native_pixel_height: 1080,
            scale_factor: 1.0,
            rotation: Rotation::Landscape,
            refresh_rate: None,
            primary: false,
        }
    }

    fn topology() -> Topology {
        let (a, b) = (machine(1), machine(2));
        let layout = ultidesk_topology::left_to_right(vec![
            MachineMonitors {
                device_id: a,
                monitors: vec![
                    monitor(a, "a1", 0.0, 1920.0),
                    monitor(a, "a2", 1920.0, 1920.0),
                ],
            },
            MachineMonitors {
                device_id: b,
                monitors: vec![monitor(b, "b1", 0.0, 1920.0)],
            },
        ]);
        Topology {
            layout,
            local: vec![0, 1],
        }
    }

    #[test]
    fn the_pointer_is_found_on_the_screen_it_is_on() {
        let t = topology();
        assert_eq!(t.local_monitor_at(10.0, 10.0), Some(0));
        assert_eq!(t.local_monitor_at(2000.0, 10.0), Some(1));
    }

    #[test]
    fn a_boundary_pixel_belongs_to_exactly_one_screen() {
        // Two screens meeting at x=1920: that pixel is the second screen's first, not the
        // first screen's last. Both claiming it would make the answer depend on
        // iteration order.
        let t = topology();
        assert_eq!(t.local_monitor_at(1919.0, 10.0), Some(0));
        assert_eq!(t.local_monitor_at(1920.0, 10.0), Some(1));
    }

    #[test]
    fn a_position_on_the_peer_is_not_one_of_ours() {
        // The peer's screen is in the same layout; asking where a *local* pointer is must
        // not answer with it.
        let t = topology();
        assert_eq!(t.local_monitor_at(4000.0, 10.0), None);
    }

    #[test]
    fn the_shared_border_is_with_the_peer_and_not_between_our_own_screens() {
        let t = topology();
        let borders = t.shared_borders();
        assert_eq!(borders.len(), 1, "one border, to the peer: {borders:?}");
        let (from, to, adjacency) = &borders[0];
        assert_eq!(*from, 1, "our rightmost screen");
        assert_eq!(*to, 2, "the peer");
        assert_eq!(adjacency.span(), 1080.0);
    }
}
