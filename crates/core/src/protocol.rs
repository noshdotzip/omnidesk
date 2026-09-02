//! Protocol version and the wire-envelope shape shared by all Ultidesk messages.
//!
//! The canonical message *schema* lives in `protocol/ultidesk.proto`; this module
//! holds the version constant and negotiation logic that must be identical on both
//! peers. Keep [`PROTOCOL_VERSION`] in sync with the `.proto` package version.

use serde::{Deserialize, Serialize};

/// Application-level protocol version. Bumped on any breaking wire change.
/// Peers exchange this in the Hello handshake and refuse to proceed on mismatch
/// unless capability negotiation covers the delta.
pub const PROTOCOL_VERSION: u32 = 1;

/// Hard upper bound on any single decoded control message. Anything larger is
/// rejected before allocation to bound memory against a hostile/buggy peer.
pub const MAX_MESSAGE_BYTES: usize = 1 << 20; // 1 MiB

/// Maximum number of forwarding hops an input/clipboard event may take before it is
/// dropped. Prevents A→B→C→A style loops even if origin de-dup is defeated.
pub const MAX_HOPS: u8 = 4;

/// Result of comparing two protocol versions during the Hello handshake.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VersionCompat {
    /// Identical versions — full compatibility.
    Exact,
    /// Different versions; the caller must consult capability bits and may refuse.
    Divergent { local: u32, peer: u32 },
}

/// Compare the local protocol version against a peer-advertised version.
pub fn compare_version(peer: u32) -> VersionCompat {
    if peer == PROTOCOL_VERSION {
        VersionCompat::Exact
    } else {
        VersionCompat::Divergent {
            local: PROTOCOL_VERSION,
            peer,
        }
    }
}

/// Capability bits advertised in discovery and Hello. Receivers must treat unknown
/// bits as "unsupported", never as "assume yes".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Capabilities {
    pub bits: u64,
}

impl Capabilities {
    pub const KVM_INPUT: u64 = 1 << 0;
    pub const WINDOW_PROJECTION: u64 = 1 << 1;
    pub const CLIPBOARD_TEXT: u64 = 1 << 2;
    pub const FILE_TRANSFER: u64 = 1 << 3;

    pub fn with(mut self, bit: u64) -> Self {
        self.bits |= bit;
        self
    }

    pub fn has(&self, bit: u64) -> bool {
        self.bits & bit == bit
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_version_matches() {
        assert_eq!(compare_version(PROTOCOL_VERSION), VersionCompat::Exact);
    }

    #[test]
    fn divergent_version_reports_both_sides() {
        assert_eq!(
            compare_version(PROTOCOL_VERSION + 7),
            VersionCompat::Divergent {
                local: PROTOCOL_VERSION,
                peer: PROTOCOL_VERSION + 7
            }
        );
    }

    #[test]
    fn capability_bits_require_full_match() {
        let caps = Capabilities::default()
            .with(Capabilities::KVM_INPUT)
            .with(Capabilities::WINDOW_PROJECTION);
        assert!(caps.has(Capabilities::KVM_INPUT));
        assert!(caps.has(Capabilities::WINDOW_PROJECTION));
        assert!(!caps.has(Capabilities::FILE_TRANSFER));
    }
}
