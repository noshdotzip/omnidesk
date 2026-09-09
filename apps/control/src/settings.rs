//! Persisting the operator's arrangement and audio routes.
//!
//! # Persistence forces a stable identity
//! Everything here is keyed to a machine, and the control app once minted a fresh
//! random [`DeviceId`] on every launch — twice, in fact, so the Displays tab and the
//! Audio tab disagreed about which machine was "this machine". Nothing depended on it
//! before, so nothing broke. A saved file does depend on it: with a new id each launch,
//! no saved route could ever match a device again.
//!
//! The id is now **derived from this machine's Ed25519 identity** (`ultidesk_identity`)
//! rather than drawn at random, so it is the same id the agent and every peer use, and
//! it cannot be claimed by a machine that does not hold the private key. What is stored
//! here is a record of which id the saved routes were written against, so that a file
//! from before identities existed can be carried across —
//! see [`Settings::adopt_device_id`].
//!
//! # Writes are atomic
//! A settings file is rewritten on every change. Truncating the real file and then
//! writing into it means a crash or a power cut mid-write leaves a half-written file,
//! and the next launch loses the whole arrangement. Writing a temporary file and
//! renaming it over the target makes the replacement a single filesystem operation:
//! the reader sees either the old file or the new one.
//!
//! # A newer file is never overwritten
//! If a future build writes a schema this one cannot read, the safe response is not to
//! ignore it — the next save would destroy it. It is moved aside with its version in
//! the name, so downgrading and re-upgrading does not cost the operator their setup.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use ultidesk_core::DeviceId;
use ultidesk_topology::{Route, SavedLayout};

/// Schema version. Bump when a field changes meaning rather than merely being added.
pub const SETTINGS_VERSION: u32 = 1;

const FILE_NAME: &str = "settings.json";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    pub version: u32,
    /// Which machine the saved routes below are keyed to.
    ///
    /// No longer generated here: it is derived from this machine's Ed25519 identity
    /// (`ultidesk_identity`), so it is the same id the agent and every peer will use.
    /// It is still *stored*, because a saved route names it and the file has to record
    /// which id those routes were written against — see [`Settings::adopt_device_id`].
    pub local_device_id: DeviceId,
    pub layout: SavedLayout,
    pub audio_routes: Vec<Route>,
}

impl Settings {
    /// A fresh configuration for a machine that has never saved one.
    ///
    /// The id is a placeholder that [`Settings::adopt_device_id`] replaces as soon as
    /// the real identity is known; nothing is saved against it in between.
    pub fn fresh() -> Self {
        Settings {
            version: SETTINGS_VERSION,
            local_device_id: DeviceId::new(),
            layout: SavedLayout::default(),
            audio_routes: Vec::new(),
        }
    }

    /// Re-key this machine's saved state onto the id derived from its identity.
    ///
    /// Every existing settings file was written when the local id was a random uuid
    /// minted on first run. Simply overwriting the field would leave every saved audio
    /// route pointing at a device id that no longer names any machine, and the routes
    /// would silently vanish from the panel — the operator would see an empty list and
    /// no reason for it. So the routes are carried across with the id.
    ///
    /// Returns whether anything changed, so the caller can save once instead of on
    /// every launch.
    pub fn adopt_device_id(&mut self, derived: DeviceId) -> bool {
        if self.local_device_id == derived {
            return false;
        }
        let previous = self.local_device_id;
        for route in &mut self.audio_routes {
            // Only endpoints that belonged to *this* machine move. A route naming a
            // peer keeps that peer's id, which is the whole reason this is a rewrite
            // and not a blanket replacement.
            if route.source.device_id == previous {
                route.source.device_id = derived;
            }
            if route.sink.device_id == previous {
                route.sink.device_id = derived;
            }
        }
        self.local_device_id = derived;
        true
    }
}

/// The outcome of loading, including anything the operator should know about.
pub struct Loaded {
    pub settings: Settings,
    /// Set when the stored file could not be used. Surfaced in the UI rather than
    /// logged and forgotten: settings silently reverting to defaults is the kind of
    /// thing people notice weeks later and cannot explain.
    pub note: Option<String>,
}

// Where settings live is `ultidesk_core::paths`' rule, not this module's: the agent
// resolves the same directory, and two copies of the rule are two chances to disagree
// about which machine's configuration is being read.
pub use ultidesk_core::paths::config_dir;

/// Read settings from `dir`, falling back to a fresh configuration.
pub fn load_from(dir: &Path) -> Loaded {
    let path = dir.join(FILE_NAME);
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        // A missing file is the normal first run, not a problem worth reporting.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Loaded {
                settings: Settings::fresh(),
                note: None,
            }
        }
        Err(e) => {
            return Loaded {
                settings: Settings::fresh(),
                note: Some(format!("could not read {}: {e}", path.display())),
            }
        }
    };

    match serde_json::from_str::<Settings>(&raw) {
        Ok(settings) if settings.version > SETTINGS_VERSION => {
            let note = match set_aside(&path, settings.version) {
                Ok(moved) => format!(
                    "settings were written by a newer version (v{}); kept at {} and started fresh",
                    settings.version,
                    moved.display()
                ),
                Err(e) => format!(
                    "settings were written by a newer version (v{}) and could not be moved aside: {e}",
                    settings.version
                ),
            };
            Loaded {
                settings: Settings::fresh(),
                note: Some(note),
            }
        }
        Ok(settings) => Loaded {
            settings,
            note: None,
        },
        Err(e) => {
            // Malformed rather than merely older. Keep it: it may be hand-edited and
            // recoverable, and overwriting it removes the only evidence of what broke.
            let note = match set_aside(&path, 0) {
                Ok(moved) => format!(
                    "settings file was unreadable ({e}); kept at {} and started fresh",
                    moved.display()
                ),
                Err(move_err) => format!(
                    "settings file was unreadable ({e}) and could not be moved aside: {move_err}"
                ),
            };
            Loaded {
                settings: Settings::fresh(),
                note: Some(note),
            }
        }
    }
}

/// Write settings into `dir`, replacing any existing file atomically.
pub fn save_to(dir: &Path, settings: &Settings) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(FILE_NAME);
    let tmp = dir.join(format!("{FILE_NAME}.tmp"));

    let json = serde_json::to_string_pretty(settings)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(&tmp, json)?;
    // `rename` replaces an existing file on both platforms (Windows maps it to
    // MoveFileEx with MOVEFILE_REPLACE_EXISTING), so the swap is one operation.
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// Move an unusable settings file aside, returning where it went.
fn set_aside(path: &Path, version: u32) -> std::io::Result<PathBuf> {
    let suffix = if version > 0 {
        format!("v{version}.bak")
    } else {
        "unreadable.bak".to_string()
    };
    let moved = path.with_extension(format!("json.{suffix}"));
    std::fs::rename(path, &moved)?;
    Ok(moved)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch directory that cleans itself up.
    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "ultidesk-settings-test-{tag}-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            TempDir(dir)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn route(source: (DeviceId, &str), sink: (DeviceId, &str)) -> Route {
        use ultidesk_topology::DeviceKey;
        Route {
            source: DeviceKey {
                device_id: source.0,
                node: source.1.to_string(),
            },
            sink: DeviceKey {
                device_id: sink.0,
                node: sink.1.to_string(),
            },
        }
    }

    #[test]
    fn adopting_the_derived_id_carries_this_machines_routes_across() {
        // The upgrade path for every settings file written before identities existed.
        let mut s = Settings::fresh();
        let old = s.local_device_id;
        let peer = DeviceId::new();
        s.audio_routes = vec![
            route((old, "speakers"), (peer, "peer-sink")),
            route((peer, "peer-mic"), (old, "headset")),
        ];

        let derived = DeviceId::new();
        assert!(s.adopt_device_id(derived));

        assert_eq!(s.local_device_id, derived);
        assert_eq!(s.audio_routes[0].source.device_id, derived);
        assert_eq!(s.audio_routes[1].sink.device_id, derived);
        assert_eq!(
            s.audio_routes[0].sink.device_id, peer,
            "a peer's id is not this machine's and must not be rewritten"
        );
        assert_eq!(s.audio_routes[1].source.device_id, peer);
        assert_eq!(
            s.audio_routes[0].source.node, "speakers",
            "nodes are opaque"
        );
    }

    #[test]
    fn adopting_the_same_id_changes_nothing_and_says_so() {
        // Reported so the app saves once at the upgrade rather than on every launch.
        let mut s = Settings::fresh();
        let id = s.local_device_id;
        let before = s.clone();
        assert!(!s.adopt_device_id(id));
        assert_eq!(s, before);
    }

    #[test]
    fn a_first_run_loads_fresh_settings_without_complaining() {
        let d = TempDir::new("first");
        let loaded = load_from(d.path());
        assert!(
            loaded.note.is_none(),
            "a missing file is normal, not an error"
        );
        assert_eq!(loaded.settings.version, SETTINGS_VERSION);
        assert!(loaded.settings.audio_routes.is_empty());
    }

    #[test]
    fn settings_round_trip_through_the_file() {
        let d = TempDir::new("roundtrip");
        let mut s = Settings::fresh();
        s.layout.monitors.push(ultidesk_topology::SavedMonitor {
            name: "DISPLAY1".into(),
            x: 100.0,
            y: 200.0,
        });
        save_to(d.path(), &s).unwrap();
        let back = load_from(d.path());
        assert!(back.note.is_none());
        assert_eq!(back.settings, s);
    }

    #[test]
    fn the_device_id_survives_a_reload() {
        // The whole point: a new id each launch would orphan every saved route.
        let d = TempDir::new("identity");
        let s = Settings::fresh();
        save_to(d.path(), &s).unwrap();
        assert_eq!(
            load_from(d.path()).settings.local_device_id,
            s.local_device_id
        );
    }

    #[test]
    fn a_corrupt_file_starts_fresh_rather_than_failing_to_launch() {
        let d = TempDir::new("corrupt");
        std::fs::write(d.path().join(FILE_NAME), "{ this is not json").unwrap();
        let loaded = load_from(d.path());
        assert_eq!(loaded.settings.version, SETTINGS_VERSION);
        assert!(loaded.note.is_some(), "the operator must be told");
    }

    #[test]
    fn a_corrupt_file_is_kept_rather_than_destroyed() {
        // It may be hand-edited and recoverable; overwriting removes the evidence.
        let d = TempDir::new("corrupt-kept");
        std::fs::write(d.path().join(FILE_NAME), "{ broken").unwrap();
        load_from(d.path());
        let kept: Vec<_> = std::fs::read_dir(d.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".bak"))
            .collect();
        assert_eq!(kept.len(), 1, "the unreadable file should have been kept");
    }

    #[test]
    fn a_file_from_a_newer_version_is_not_overwritten() {
        // Downgrading then upgrading must not cost the operator their setup.
        let d = TempDir::new("newer");
        let raw = format!(
            r#"{{"version":{},"local_device_id":"{}","layout":{{"monitors":[]}},"audio_routes":[]}}"#,
            SETTINGS_VERSION + 5,
            uuid_text()
        );
        std::fs::write(d.path().join(FILE_NAME), raw).unwrap();

        let loaded = load_from(d.path());
        assert!(loaded.note.unwrap().contains("newer version"));

        // Saving now must not clobber the preserved copy.
        save_to(d.path(), &Settings::fresh()).unwrap();
        let backups: Vec<_> = std::fs::read_dir(d.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".bak"))
            .collect();
        assert_eq!(backups.len(), 1, "the newer file must still be there");
    }

    #[test]
    fn saving_leaves_no_temporary_file_behind() {
        // A stray .tmp would be picked up by nothing, but it is a sign the rename did
        // not happen — which is exactly the case where an interrupted write corrupts.
        let d = TempDir::new("tmp");
        save_to(d.path(), &Settings::fresh()).unwrap();
        let strays: Vec<_> = std::fs::read_dir(d.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(strays.is_empty(), "left a temporary file behind");
    }

    #[test]
    fn saving_twice_replaces_rather_than_erroring() {
        // `rename` over an existing file is the case that behaves differently across
        // platforms, so it is worth exercising rather than assuming.
        let d = TempDir::new("replace");
        save_to(d.path(), &Settings::fresh()).unwrap();
        let second = Settings::fresh();
        save_to(d.path(), &second).unwrap();
        assert_eq!(
            load_from(d.path()).settings.local_device_id,
            second.local_device_id
        );
    }

    #[test]
    fn save_creates_the_directory_if_it_is_missing() {
        let d = TempDir::new("mkdir");
        let nested = d.path().join("deeper").join("still");
        save_to(&nested, &Settings::fresh()).unwrap();
        assert!(nested.join(FILE_NAME).exists());
    }

    fn uuid_text() -> String {
        // A syntactically valid uuid; the value does not matter for the version check.
        "00000000-0000-4000-8000-000000000000".to_string()
    }
}
