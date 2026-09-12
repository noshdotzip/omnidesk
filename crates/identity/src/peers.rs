//! The peers this machine trusts, pinned by public key.
//!
//! # The key is the identity; the name is only a label
//! A peer announces a friendly name, and a name is whatever the other end says it is —
//! two machines may claim the same one, and a hostile one will claim yours. So nothing
//! here is keyed by name: the store is keyed by [`PeerKey`], the name exists to be
//! shown to the operator, and code that decides whether to accept a connection must
//! ask [`PeerStore::trusts`] rather than comparing strings.
//!
//! # Pairing is not a blank cheque
//! Trusting a device and letting it do *anything* are different decisions, and until now
//! they were the same one. [`Permissions`] splits them per peer, enforced on the machine
//! that owns the capability — a peer claiming it is allowed to do something is never
//! sufficient (docs/permissions.md). The default for a missing field is `false`, so a
//! store that is damaged, truncated, or written by a build that knew fewer permissions
//! grants less rather than more.
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
///
/// v2 added [`Permissions`]. A v1 file is migrated rather than defaulted: every peer in
/// it was pinned when pairing meant unrestricted access, so that is what it granted, and
/// silently reducing it to nothing would break a working setup with no explanation. The
/// migration writes the grant out explicitly, so what was implied becomes visible and
/// revocable.
pub const PEERS_VERSION: u32 = 2;

#[derive(Debug, thiserror::Error)]
pub enum PeerStoreError {
    #[error("could not write {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// A permission name this build does not know.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown permission {0:?}; expected one of: control-input, read-devices, list-windows")]
pub struct UnknownPermission(pub String);

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

/// What this machine lets one peer do to it.
///
/// Each field is a separate decision because the answers genuinely differ: someone may
/// want a work laptop able to read which speakers this machine has without also being
/// able to type on it.
///
/// # Why window listing is not folded into reading devices
/// A window list carries **titles**, and a title says what the operator is doing — the
/// document they have open, the site they are reading, the name of a customer. An audio
/// endpoint list says a machine has speakers. Granting one should not grant the other.
///
/// # Why the default is nothing
/// `#[serde(default)]` on every field means an absent or unrecognised entry reads as
/// `false`. A store this build cannot fully understand therefore grants *less* than the
/// file intended, never more.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Permissions {
    /// Move this machine's pointer, press its keys, turn its wheel.
    pub control_input: bool,
    /// Read what this machine *has* — its audio endpoints today; its monitors and
    /// topology as those messages land.
    pub read_devices: bool,
    /// List this machine's open windows, titles included.
    pub list_windows: bool,
}

impl Permissions {
    /// Everything this build knows how to grant.
    ///
    /// Used for the v1 migration and for the local IPC, and deliberately *not* the
    /// default for a new pairing.
    pub fn all() -> Self {
        Permissions {
            control_input: true,
            read_devices: true,
            list_windows: true,
        }
    }

    /// What a freshly paired peer gets.
    ///
    /// Input and device reading, because driving another machine is what someone paired
    /// two machines *for* and refusing it by default would make pairing look broken.
    /// Window titles are not included: they are the one thing here that leaks what the
    /// operator is doing, and nothing asks for them until window projection exists.
    pub fn on_pairing() -> Self {
        Permissions {
            control_input: true,
            read_devices: true,
            list_windows: false,
        }
    }

    /// The granted names, for showing an operator what a peer may do.
    pub fn granted(&self) -> Vec<&'static str> {
        let mut out = Vec::new();
        if self.control_input {
            out.push("control-input");
        }
        if self.read_devices {
            out.push("read-devices");
        }
        if self.list_windows {
            out.push("list-windows");
        }
        out
    }

    /// Set one permission by its operator-facing name.
    ///
    /// Returns `false` for a name this build does not know, so a typo at the command
    /// line is reported rather than silently changing nothing.
    pub fn set_named(&mut self, name: &str, allowed: bool) -> bool {
        match name {
            "control-input" => self.control_input = allowed,
            "read-devices" => self.read_devices = allowed,
            "list-windows" => self.list_windows = allowed,
            _ => return false,
        }
        true
    }

    /// Every name [`Permissions::set_named`] accepts, for usage messages.
    pub fn names() -> [&'static str; 3] {
        ["control-input", "read-devices", "list-windows"]
    }
}

/// One trusted device.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PairedPeer {
    pub key: PeerKey,
    /// Operator-facing label. Never used for identity — see the module note.
    pub name: String,
    /// Unix seconds, so the store needs no date library and no timezone.
    pub paired_at_unix: u64,
    /// What this machine lets the peer do. Absent in a v1 file; see [`PEERS_VERSION`].
    #[serde(default)]
    pub permissions: Permissions,
    /// Where this peer was last reached, as `host:port`.
    ///
    /// A **hint**, never an identity. Discovery does not exist yet, so without somewhere
    /// to remember the address every connection would need it typed in again; but an
    /// address can be taken over by another machine, and the only thing that decides
    /// whether a connection is this peer is the key. Nothing here compares addresses.
    ///
    /// Absent in a file written before it was recorded, and absent for a peer that has
    /// never been reached. That absence has no security meaning, which is why it needed
    /// no schema bump — unlike [`Permissions`], where a missing field had to be read as
    /// "granted nothing".
    #[serde(default)]
    pub address: Option<String>,
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
    /// The file was an older schema and was upgraded in memory.
    ///
    /// Reported rather than written here, because a function called `load` that silently
    /// rewrites the file it read is a surprise — and because a read-only command should
    /// stay read-only. The caller saves once, which is also what stops the migration
    /// note from being printed on every single launch forever.
    pub migrated: bool,
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
            permissions: Permissions::on_pairing(),
            address: None,
        });
        Ok(PinOutcome::Added)
    }

    /// Remember where a peer was last reached.
    ///
    /// Returns whether anything changed, so a caller can save only when it did rather
    /// than rewriting the file after every connection.
    ///
    /// Recording it for an unpaired key is refused silently by doing nothing: an address
    /// is only meaningful attached to a device this machine trusts, and creating an entry
    /// here would be creating trust.
    pub fn remember_address(&mut self, key: &PeerKey, address: &str) -> bool {
        let Some(peer) = self.peers.iter_mut().find(|p| &p.key == key) else {
            return false;
        };
        if peer.address.as_deref() == Some(address) {
            return false;
        }
        peer.address = Some(address.to_string());
        true
    }

    /// Where a peer was last reached, if anywhere.
    pub fn address(&self, key: &PeerKey) -> Option<&str> {
        self.get(key).and_then(|p| p.address.as_deref())
    }

    /// The single peer this machine trusts, when there is exactly one.
    ///
    /// A convenience for the common two-machine desk, and deliberately `None` when there
    /// are several: guessing which peer was meant is the kind of thing that silently
    /// sends input to the wrong machine.
    pub fn only_peer(&self) -> Option<&PairedPeer> {
        match self.peers.as_slice() {
            [one] => Some(one),
            _ => None,
        }
    }

    /// What a peer is allowed to do, or nothing at all if it is not paired.
    ///
    /// An unknown peer getting [`Permissions::default`] rather than an `Option` is the
    /// safe shape: a caller that forgets to handle the missing case still denies
    /// everything, where an `unwrap_or_else(Permissions::all)` slip would grant it.
    pub fn permissions(&self, key: &PeerKey) -> Permissions {
        self.get(key).map(|p| p.permissions).unwrap_or_default()
    }

    /// Grant or revoke one permission for one peer.
    ///
    /// `Ok(false)` means the peer is not paired — reported rather than silently creating
    /// a grant for a device this machine does not trust.
    pub fn set_permission(
        &mut self,
        key: &PeerKey,
        name: &str,
        allowed: bool,
    ) -> Result<bool, UnknownPermission> {
        let Some(peer) = self.peers.iter_mut().find(|p| &p.key == key) else {
            return Ok(false);
        };
        if !peer.permissions.set_named(name, allowed) {
            return Err(UnknownPermission(name.to_string()));
        }
        Ok(true)
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
        migrated: false,
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

    let mut peers = parsed.peers;
    let mut note = None;
    let migrated = parsed.version < PEERS_VERSION;
    if parsed.version < 2 {
        // Everything in a v1 file was pinned when pairing meant unrestricted access.
        // Granting that explicitly keeps a working setup working; defaulting it to
        // nothing would break it silently, which is the worse of the two surprises.
        for peer in &mut peers {
            peer.permissions = Permissions::all();
        }
        note = Some(format!(
            "{} was written before per-peer permissions existed; {} peer(s) kept the \
             unrestricted access pairing used to mean. Review it with `peers`.",
            path.display(),
            peers.len()
        ));
    }

    LoadedPeers {
        store: PeerStore { local, peers },
        note,
        migrated,
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
    fn a_new_pairing_grants_input_and_devices_but_not_window_titles() {
        let (local, peer, _) = keys();
        let mut store = PeerStore::new(local);
        store.pin(peer, "laptop", 1).unwrap();

        let p = store.permissions(&peer);
        assert!(
            p.control_input,
            "driving the machine is what pairing is for"
        );
        assert!(p.read_devices);
        assert!(
            !p.list_windows,
            "titles say what the operator is doing; nothing asks for them yet"
        );
    }

    #[test]
    fn a_peer_that_is_not_paired_is_allowed_nothing() {
        // The shape matters: returning `Permissions::default()` rather than an Option
        // means a caller that forgets the missing case still denies everything.
        let (local, stranger, _) = keys();
        assert_eq!(
            PeerStore::new(local).permissions(&stranger),
            Permissions::default()
        );
        assert!(Permissions::default().granted().is_empty());
    }

    #[test]
    fn permissions_can_be_granted_and_revoked_by_name() {
        let (local, peer, _) = keys();
        let mut store = PeerStore::new(local);
        store.pin(peer, "laptop", 1).unwrap();

        assert_eq!(store.set_permission(&peer, "list-windows", true), Ok(true));
        assert!(store.permissions(&peer).list_windows);

        assert_eq!(
            store.set_permission(&peer, "control-input", false),
            Ok(true)
        );
        assert!(!store.permissions(&peer).control_input);
        assert!(
            store.permissions(&peer).read_devices,
            "changing one permission must not disturb the others"
        );
    }

    #[test]
    fn granting_to_an_unpaired_device_reports_that_rather_than_creating_a_grant() {
        let (local, stranger, _) = keys();
        let mut store = PeerStore::new(local);
        assert_eq!(
            store.set_permission(&stranger, "read-devices", true),
            Ok(false)
        );
        assert!(!store.trusts(&stranger));
    }

    #[test]
    fn a_misspelled_permission_is_refused_rather_than_ignored() {
        // Silently doing nothing would leave an operator believing they had revoked
        // something.
        let (local, peer, _) = keys();
        let mut store = PeerStore::new(local);
        store.pin(peer, "laptop", 1).unwrap();
        assert!(store.set_permission(&peer, "control_input", false).is_err());
        assert!(
            store.permissions(&peer).control_input,
            "a refused change must change nothing"
        );
    }

    #[test]
    fn permissions_round_trip_through_the_file() {
        let d = TempDir::new("perm-round-trip");
        let (local, peer, _) = keys();
        let mut store = PeerStore::new(local);
        store.pin(peer, "arch", 1).unwrap();
        store.set_permission(&peer, "control-input", false).unwrap();
        store.set_permission(&peer, "list-windows", true).unwrap();
        store.save(d.path()).unwrap();

        let loaded = load(d.path(), local);
        let p = loaded.store.permissions(&peer);
        assert!(!p.control_input);
        assert!(p.read_devices);
        assert!(p.list_windows);
    }

    #[test]
    fn a_v1_file_is_migrated_to_the_access_it_used_to_mean() {
        // Everything in a v1 file was pinned when pairing meant unrestricted access.
        // Defaulting it to nothing would break a working setup with no explanation.
        let d = TempDir::new("perm-v1");
        let (local, peer, _) = keys();
        let raw = format!(
            r#"{{"version":1,"peers":[{{"key":"{peer}","name":"arch","paired_at_unix":7}}]}}"#
        );
        std::fs::write(d.path().join(PEERS_FILE), raw).unwrap();

        let loaded = load(d.path(), local);
        assert_eq!(loaded.store.permissions(&peer), Permissions::all());
        assert!(
            loaded.note.is_some(),
            "an implicit grant becoming explicit has to be told to the operator"
        );
        assert!(loaded.migrated, "the caller has to know to write it back");

        // Written back once, after which the file is current and says nothing further.
        loaded.store.save(d.path()).unwrap();
        let again = load(d.path(), local);
        assert!(!again.migrated);
        assert!(
            again.note.is_none(),
            "the migration note must not repeat on every launch"
        );
        assert_eq!(again.store.permissions(&peer), Permissions::all());
    }

    #[test]
    fn a_v2_entry_with_no_permissions_field_grants_nothing() {
        // Not the same case as v1: this file claims to know about permissions and simply
        // lists none, so the safe reading is that none were granted.
        let d = TempDir::new("perm-absent");
        let (local, peer, _) = keys();
        let raw = format!(
            r#"{{"version":2,"peers":[{{"key":"{peer}","name":"arch","paired_at_unix":7}}]}}"#
        );
        std::fs::write(d.path().join(PEERS_FILE), raw).unwrap();

        let loaded = load(d.path(), local);
        assert_eq!(loaded.store.permissions(&peer), Permissions::default());
        assert!(loaded.store.trusts(&peer), "still paired, just not allowed");
    }

    #[test]
    fn an_unrecognised_permission_in_the_file_is_ignored_rather_than_obeyed() {
        // A file from a build that knows more permissions must not be read as granting
        // something this build cannot enforce.
        let d = TempDir::new("perm-unknown");
        let (local, peer, _) = keys();
        let raw = format!(
            r#"{{"version":2,"peers":[{{"key":"{peer}","name":"arch","paired_at_unix":7,
               "permissions":{{"read_devices":true,"read_clipboard":true}}}}]}}"#
        );
        std::fs::write(d.path().join(PEERS_FILE), raw).unwrap();

        let loaded = load(d.path(), local);
        let p = loaded.store.permissions(&peer);
        assert!(p.read_devices, "the ones it does know still apply");
        assert_eq!(p.granted(), vec!["read-devices"]);
    }

    #[test]
    fn an_address_is_remembered_and_updated_but_only_for_a_paired_peer() {
        let (local, peer, stranger) = keys();
        let mut store = PeerStore::new(local);
        store.pin(peer, "arch", 1).unwrap();

        assert_eq!(
            store.address(&peer),
            None,
            "nothing is known before a connection"
        );
        assert!(store.remember_address(&peer, "192.168.137.9:45872"));
        assert_eq!(store.address(&peer), Some("192.168.137.9:45872"));

        assert!(
            !store.remember_address(&peer, "192.168.137.9:45872"),
            "an unchanged address must not make the caller rewrite the file"
        );
        assert!(
            store.remember_address(&peer, "10.0.0.5:45872"),
            "a move is recorded"
        );

        assert!(
            !store.remember_address(&stranger, "10.0.0.9:45872"),
            "an address for an unpaired key would be creating trust"
        );
        assert!(!store.trusts(&stranger));
    }

    #[test]
    fn the_only_peer_is_only_returned_when_there_is_exactly_one() {
        // Guessing which of several peers was meant is how input reaches the wrong
        // machine.
        let (local, a, b) = keys();
        let mut store = PeerStore::new(local);
        assert!(store.only_peer().is_none(), "none paired");

        store.pin(a, "arch", 1).unwrap();
        assert_eq!(store.only_peer().map(|p| p.key), Some(a));

        store.pin(b, "surface", 2).unwrap();
        assert!(store.only_peer().is_none(), "ambiguous");
    }

    #[test]
    fn an_address_survives_the_file() {
        let d = TempDir::new("peer-address");
        let (local, peer, _) = keys();
        let mut store = PeerStore::new(local);
        store.pin(peer, "arch", 1).unwrap();
        store.remember_address(&peer, "192.168.137.9:45872");
        store.save(d.path()).unwrap();

        let loaded = load(d.path(), local);
        assert_eq!(loaded.store.address(&peer), Some("192.168.137.9:45872"));
        assert!(loaded.note.is_none(), "adding a field is not a migration");
    }

    #[test]
    fn a_file_without_an_address_field_is_read_without_complaint() {
        // Unlike a missing permission, a missing address has no security meaning — which
        // is why adding it needed no schema bump.
        let d = TempDir::new("peer-no-address");
        let (local, peer, _) = keys();
        let raw = format!(
            r#"{{"version":2,"peers":[{{"key":"{peer}","name":"arch","paired_at_unix":7,
               "permissions":{{"control_input":true}}}}]}}"#
        );
        std::fs::write(d.path().join(PEERS_FILE), raw).unwrap();

        let loaded = load(d.path(), local);
        assert!(loaded.store.trusts(&peer));
        assert_eq!(loaded.store.address(&peer), None);
        assert!(loaded.note.is_none());
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
