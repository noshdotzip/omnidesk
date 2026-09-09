//! The peers this machine trusts, pinned by public key.
//!
//! # The key is the identity; the name is only a label
//! A peer announces a friendly name, and a name is whatever the other end says it is —
//! two machines may claim the same one, and a hostile one will claim yours. So nothing
//! here is keyed by name: the store is keyed by [`PeerKey`], the name exists to be
//! shown to the operator, and code that decides whether to accept a connection must
//! ask [`PeerStore::trusts`] rather than comparing strings.
//!
//! # An unreadable store trusts nobody
//! If the file cannot be parsed, the store loads empty and the damaged file is moved
//! aside instead of being overwritten. Empty is the safe direction: every connection is
//! then refused until the operator re-pairs, which is visible and annoying. The unsafe
//! direction — carrying on with a partially parsed list — would silently trust or
//! silently distrust a specific peer, and neither would be noticed.

use std::path::{Path, PathBuf};

use crate::file::{write_atomic, Visibility};
use crate::key::PeerKey;

/// File name inside the configuration directory.
pub const PEERS_FILE: &str = "peers.json";

/// Schema version. Bumped when a field changes meaning rather than merely being added.
pub const PEERS_VERSION: u32 = 1;

#[derive(Debug, thiserror::Error)]
pub enum PeerStoreError {
    #[error("could not write {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Why a pin was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PinError {
    /// Pairing a machine with itself. It would make this device its own peer, so
    /// input forwarded away could be accepted back as if it came from elsewhere —
    /// exactly the loop `ultidesk_core::input_guard` exists to prevent, but arriving
    /// through the front door with a valid signature.
    #[error("a device cannot pair with itself")]
    SelfPairing,
    /// A blank name would show as an empty row that cannot be told from any other.
    #[error("a peer needs a name to be shown under")]
    EmptyName,
}

/// What [`PeerStore::pin`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PinOutcome {
    Added,
    /// The key was already pinned; only the label changed.
    Renamed {
        previous: String,
    },
    /// The key was already pinned under this exact name — pairing twice is not an
    /// error, so that a re-run of pairing is harmless.
    Unchanged,
}

/// One trusted device.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PairedPeer {
    pub key: PeerKey,
    /// Operator-facing label. Never used for identity — see the module note.
    pub name: String,
    /// Unix seconds, so the store needs no date library and no timezone.
    pub paired_at_unix: u64,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct PeersFile {
    version: u32,
    peers: Vec<PairedPeer>,
}

/// The peers this machine trusts.
///
/// Holds the local key so that [`PeerStore::pin`] can refuse self-pairing without every
/// caller having to remember to check.
#[derive(Debug, Clone)]
pub struct PeerStore {
    local: PeerKey,
    peers: Vec<PairedPeer>,
}

/// The outcome of [`load`], including anything the operator should be told.
pub struct LoadedPeers {
    pub store: PeerStore,
    pub note: Option<String>,
}

impl PeerStore {
    pub fn new(local: PeerKey) -> Self {
        PeerStore {
            local,
            peers: Vec::new(),
        }
    }

    pub fn local(&self) -> PeerKey {
        self.local
    }

    /// The question every incoming connection asks.
    pub fn trusts(&self, key: &PeerKey) -> bool {
        self.peers.iter().any(|p| &p.key == key)
    }

    pub fn get(&self, key: &PeerKey) -> Option<&PairedPeer> {
        self.peers.iter().find(|p| &p.key == key)
    }

    pub fn peers(&self) -> &[PairedPeer] {
        &self.peers
    }

    /// Every trusted key, for handing to the transport's pinning verifier.
    pub fn keys(&self) -> Vec<PeerKey> {
        self.peers.iter().map(|p| p.key).collect()
    }

    /// Trust a device, or rename one already trusted.
    ///
    /// A key already present is updated rather than appended: two entries for one key
    /// would let a stale name outlive a rename, and `forget` would then only remove one
    /// of them, leaving the peer still trusted.
    pub fn pin(&mut self, key: PeerKey, name: &str, now_unix: u64) -> Result<PinOutcome, PinError> {
        if key == self.local {
            return Err(PinError::SelfPairing);
        }
        let name = name.trim();
        if name.is_empty() {
            return Err(PinError::EmptyName);
        }

        if let Some(existing) = self.peers.iter_mut().find(|p| p.key == key) {
            if existing.name == name {
                return Ok(PinOutcome::Unchanged);
            }
            let previous = std::mem::replace(&mut existing.name, name.to_string());
            return Ok(PinOutcome::Renamed { previous });
        }

        self.peers.push(PairedPeer {
            key,
            name: name.to_string(),
            paired_at_unix: now_unix,
        });
        Ok(PinOutcome::Added)
    }

    /// Revoke trust. Returns whether anything was actually removed, so a caller can
    /// tell "unpaired" from "was never paired" instead of reporting success either way.
    pub fn forget(&mut self, key: &PeerKey) -> bool {
        let before = self.peers.len();
        self.peers.retain(|p| &p.key != key);
        self.peers.len() != before
    }

    pub fn save(&self, dir: &Path) -> Result<(), PeerStoreError> {
        let path = dir.join(PEERS_FILE);
        let file = PeersFile {
            version: PEERS_VERSION,
            peers: self.peers.clone(),
        };
        let json = serde_json::to_string_pretty(&file).expect("the peer list always serialises");
        // Not `Private`: the file holds public keys and labels, nothing secret. Marking
        // it private would imply a protection it does not need and does not have.
        write_atomic(&path, json.as_bytes(), Visibility::Normal).map_err(|source| {
            PeerStoreError::Io {
                path: path.clone(),
                source,
            }
        })
    }
}

/// Seconds since the Unix epoch, or 0 on a machine whose clock is before it.
///
/// The timestamp is shown to the operator and never used to decide trust, so a wrong
/// clock costs a wrong date in the UI rather than an expiry that fires early.
pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Read the trusted peers, defaulting to trusting nobody.
pub fn load(dir: &Path, local: PeerKey) -> LoadedPeers {
    let path = dir.join(PEERS_FILE);
    let empty = |note: Option<String>| LoadedPeers {
        store: PeerStore::new(local),
        note,
    };

    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return empty(None),
        Err(e) => {
            return empty(Some(format!(
                "could not read {}: {e}; no peers are trusted this session",
                path.display()
            )))
        }
    };

    let parsed: PeersFile = match serde_json::from_str(&raw) {
        Ok(parsed) => parsed,
        Err(e) => {
            let note = set_aside(&path, &format!("invalid json: {e}"));
            return empty(Some(note));
        }
    };
    if parsed.version > PEERS_VERSION {
        let note = set_aside(
            &path,
            &format!(
                "version {} is newer than this build understands (v{PEERS_VERSION})",
                parsed.version
            ),
        );
        return empty(Some(note));
    }

    LoadedPeers {
        store: PeerStore {
            local,
            peers: parsed.peers,
        },
        note: None,
    }
}

/// Move an unusable file out of the way so the next save does not destroy it.
fn set_aside(path: &Path, why: &str) -> String {
    let aside = path.with_extension("json.unreadable");
    match std::fs::rename(path, &aside) {
        Ok(()) => format!(
            "{} could not be read ({why}); it was moved to {} and no peers are trusted until they are paired again",
            path.display(),
            aside.display()
        ),
        Err(e) => format!(
            "{} could not be read ({why}) and could not be moved aside ({e}); no peers are trusted",
            path.display()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::key::Identity;
    use crate::test_support::TempDir;

    fn keys() -> (PeerKey, PeerKey, PeerKey) {
        (
            Identity::from_secret_bytes([1u8; 32]).public(),
            Identity::from_secret_bytes([2u8; 32]).public(),
            Identity::from_secret_bytes([3u8; 32]).public(),
        )
    }

    #[test]
    fn a_fresh_store_trusts_nobody() {
        let (local, other, _) = keys();
        assert!(!PeerStore::new(local).trusts(&other));
    }

    #[test]
    fn pinning_makes_a_peer_trusted_and_forgetting_undoes_it() {
        let (local, peer, _) = keys();
        let mut store = PeerStore::new(local);

        assert_eq!(store.pin(peer, "workstation", 100), Ok(PinOutcome::Added));
        assert!(store.trusts(&peer));
        assert_eq!(store.get(&peer).unwrap().name, "workstation");

        assert!(store.forget(&peer));
        assert!(!store.trusts(&peer));
        assert!(
            !store.forget(&peer),
            "forgetting twice must report that nothing was removed"
        );
    }

    #[test]
    fn a_device_cannot_pair_with_itself() {
        let (local, _, _) = keys();
        let mut store = PeerStore::new(local);
        assert_eq!(store.pin(local, "me", 0), Err(PinError::SelfPairing));
        assert!(!store.trusts(&local));
    }

    #[test]
    fn pinning_the_same_key_twice_updates_rather_than_duplicates() {
        // Two rows for one key would let `forget` remove only one of them.
        let (local, peer, _) = keys();
        let mut store = PeerStore::new(local);

        store.pin(peer, "laptop", 1).unwrap();
        assert_eq!(store.pin(peer, "laptop", 2), Ok(PinOutcome::Unchanged));
        assert_eq!(
            store.pin(peer, "arch box", 3),
            Ok(PinOutcome::Renamed {
                previous: "laptop".to_string()
            })
        );

        assert_eq!(store.peers().len(), 1);
        assert_eq!(store.get(&peer).unwrap().name, "arch box");
        assert_eq!(
            store.get(&peer).unwrap().paired_at_unix,
            1,
            "a rename is not a re-pairing and must not restamp the date"
        );

        store.forget(&peer);
        assert!(!store.trusts(&peer));
    }

    #[test]
    fn two_peers_may_share_a_name_because_the_key_is_the_identity() {
        let (local, a, b) = keys();
        let mut store = PeerStore::new(local);
        store.pin(a, "desktop", 1).unwrap();
        store.pin(b, "desktop", 2).unwrap();
        assert_eq!(store.peers().len(), 2);
        assert!(store.trusts(&a) && store.trusts(&b));
    }

    #[test]
    fn a_nameless_peer_is_refused() {
        let (local, peer, _) = keys();
        let mut store = PeerStore::new(local);
        assert_eq!(store.pin(peer, "   ", 0), Err(PinError::EmptyName));
    }

    #[test]
    fn the_store_round_trips_through_the_file() {
        let d = TempDir::new("peers-round-trip");
        let (local, a, b) = keys();

        let mut store = PeerStore::new(local);
        store.pin(a, "arch", 111).unwrap();
        store.pin(b, "surface", 222).unwrap();
        store.save(d.path()).unwrap();

        let loaded = load(d.path(), local);
        assert!(loaded.note.is_none());
        assert!(loaded.store.trusts(&a) && loaded.store.trusts(&b));
        assert_eq!(loaded.store.get(&a).unwrap().paired_at_unix, 111);
        assert_eq!(loaded.store.keys().len(), 2);
    }

    #[test]
    fn a_missing_file_is_a_first_run_not_a_complaint() {
        let d = TempDir::new("peers-missing");
        let (local, _, _) = keys();
        let loaded = load(d.path(), local);
        assert!(loaded.note.is_none());
        assert!(loaded.store.peers().is_empty());
    }

    #[test]
    fn an_unreadable_file_trusts_nobody_and_is_kept() {
        let d = TempDir::new("peers-corrupt");
        let (local, a, _) = keys();
        std::fs::write(d.path().join(PEERS_FILE), "{ truncated").unwrap();

        let loaded = load(d.path(), local);
        assert!(!loaded.store.trusts(&a), "must fail closed");
        assert!(loaded.note.is_some(), "the operator has to be told");
        assert!(
            d.path().join("peers.json.unreadable").exists(),
            "the damaged file must survive for inspection"
        );
    }

    #[test]
    fn a_file_from_a_newer_version_is_set_aside_rather_than_overwritten() {
        let d = TempDir::new("peers-newer");
        let (local, _, _) = keys();
        let raw = format!(r#"{{"version":{},"peers":[]}}"#, PEERS_VERSION + 1);
        std::fs::write(d.path().join(PEERS_FILE), &raw).unwrap();

        let loaded = load(d.path(), local);
        assert!(loaded.note.is_some());
        assert_eq!(
            std::fs::read_to_string(d.path().join("peers.json.unreadable")).unwrap(),
            raw
        );
    }
}
