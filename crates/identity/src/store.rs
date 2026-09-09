//! Keeping the identity across restarts.
//!
//! # A damaged identity file is never replaced
//! [`crate::peers`] and the settings file both fall back to a fresh empty state when
//! they cannot read what is on disk, because losing an arrangement is annoying and
//! recoverable. The identity is different: silently generating a new key would change
//! who this machine *is*, un-pair every peer that trusts it, and do so without saying
//! anything — the operator would see "peer not paired" on both machines and have no
//! reason to suspect the key.
//!
//! So an unreadable identity file is an error the caller must handle, and the bytes on
//! disk are left exactly as they are for a human to inspect.

use std::path::{Path, PathBuf};

use crate::file::{over_permissive, write_atomic, Visibility};
use crate::hex;
use crate::key::Identity;

/// File name inside the configuration directory (`ultidesk_core::paths::config_dir`).
pub const IDENTITY_FILE: &str = "identity.json";

/// Schema version. Bumped when a field changes meaning rather than merely being added.
pub const IDENTITY_VERSION: u32 = 1;

const ALGORITHM: &str = "ed25519";

#[derive(Debug, thiserror::Error)]
pub enum IdentityError {
    #[error("could not access {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// Deliberately fatal. See the module note: the alternative is a silent re-key.
    #[error("{path} is not a usable identity file ({detail}); it has been left untouched — move it aside to start over, and every peer will need re-pairing")]
    Unusable { path: PathBuf, detail: String },
}

/// The outcome of [`load_or_create`].
///
/// `Debug` comes from [`Identity`]'s, which prints only the public fingerprint.
#[derive(Debug)]
pub struct LoadedIdentity {
    pub identity: Identity,
    /// True when this run minted the key, so the caller can say "this machine's
    /// identity was created" once rather than on every launch.
    pub created: bool,
    /// Something the operator should know but which does not stop the agent — today,
    /// a key file other users can read.
    pub note: Option<String>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct IdentityFile {
    version: u32,
    algorithm: String,
    /// The 32-byte Ed25519 seed, hex.
    secret_key: String,
}

pub fn identity_path(dir: &Path) -> PathBuf {
    dir.join(IDENTITY_FILE)
}

/// Read this machine's identity, creating one on first run.
pub fn load_or_create(dir: &Path) -> Result<LoadedIdentity, IdentityError> {
    match load(dir)? {
        Some(identity) => {
            let note = over_permissive(&identity_path(dir));
            Ok(LoadedIdentity {
                identity,
                created: false,
                note,
            })
        }
        None => {
            let identity = Identity::generate();
            save(dir, &identity)?;
            Ok(LoadedIdentity {
                identity,
                created: true,
                note: None,
            })
        }
    }
}

/// Read the identity, or `None` when the machine has never had one.
pub fn load(dir: &Path) -> Result<Option<Identity>, IdentityError> {
    let path = identity_path(dir);
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(IdentityError::Io { path, source }),
    };

    let parsed: IdentityFile = serde_json::from_str(&raw).map_err(|e| IdentityError::Unusable {
        path: path.clone(),
        detail: format!("invalid json: {e}"),
    })?;

    // A newer schema is refused rather than guessed at: writing this version's shape
    // over it would destroy a key the next build knows how to read.
    if parsed.version > IDENTITY_VERSION {
        return Err(IdentityError::Unusable {
            path,
            detail: format!(
                "version {} is newer than this build understands (v{IDENTITY_VERSION})",
                parsed.version
            ),
        });
    }
    if parsed.algorithm != ALGORITHM {
        return Err(IdentityError::Unusable {
            path,
            detail: format!("algorithm {:?} is not {ALGORITHM:?}", parsed.algorithm),
        });
    }

    let bytes = hex::decode(&parsed.secret_key).ok_or_else(|| IdentityError::Unusable {
        path: path.clone(),
        detail: "the secret key is not hex".to_string(),
    })?;
    let bytes: [u8; 32] = bytes.try_into().map_err(|_| IdentityError::Unusable {
        path,
        detail: "the secret key is not 32 bytes".to_string(),
    })?;

    Ok(Some(Identity::from_secret_bytes(bytes)))
}

pub fn save(dir: &Path, identity: &Identity) -> Result<(), IdentityError> {
    let path = identity_path(dir);
    let file = IdentityFile {
        version: IDENTITY_VERSION,
        algorithm: ALGORITHM.to_string(),
        secret_key: hex::encode(&identity.secret_bytes()),
    };
    let json = serde_json::to_string_pretty(&file).expect("the identity file always serialises");
    write_atomic(&path, json.as_bytes(), Visibility::Private).map_err(|source| IdentityError::Io {
        path: path.clone(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    #[test]
    fn the_identity_is_created_once_and_then_reused() {
        let d = TempDir::new("identity-create");

        let first = load_or_create(d.path()).unwrap();
        assert!(first.created);

        let second = load_or_create(d.path()).unwrap();
        assert!(!second.created, "the second launch must not re-key");
        assert_eq!(second.identity.public(), first.identity.public());
        assert_eq!(second.identity.device_id(), first.identity.device_id());
    }

    #[test]
    fn a_missing_file_is_a_first_run_not_a_failure() {
        let d = TempDir::new("identity-missing");
        assert!(load(d.path()).unwrap().is_none());
    }

    #[test]
    fn a_corrupt_file_is_refused_and_left_on_disk() {
        // The whole point: no silent re-key, and the damaged bytes stay available.
        let d = TempDir::new("identity-corrupt");
        let path = identity_path(d.path());
        std::fs::write(&path, "{ not json").unwrap();

        let err = load_or_create(d.path()).unwrap_err();
        assert!(matches!(err, IdentityError::Unusable { .. }), "{err}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{ not json");
    }

    #[test]
    fn a_file_from_a_newer_version_is_refused_rather_than_overwritten() {
        let d = TempDir::new("identity-newer");
        let path = identity_path(d.path());
        let raw = format!(
            r#"{{"version":{},"algorithm":"ed25519","secret_key":"{}"}}"#,
            IDENTITY_VERSION + 1,
            "11".repeat(32)
        );
        std::fs::write(&path, &raw).unwrap();

        assert!(matches!(
            load_or_create(d.path()).unwrap_err(),
            IdentityError::Unusable { .. }
        ));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), raw);
    }

    #[test]
    fn a_key_of_the_wrong_length_is_refused_rather_than_padded() {
        let d = TempDir::new("identity-short");
        let raw = r#"{"version":1,"algorithm":"ed25519","secret_key":"aabb"}"#;
        std::fs::write(identity_path(d.path()), raw).unwrap();
        assert!(matches!(
            load(d.path()).unwrap_err(),
            IdentityError::Unusable { .. }
        ));
    }

    #[test]
    fn an_unexpected_algorithm_is_refused() {
        // A future build might store a different curve; reading its bytes as Ed25519
        // would produce a valid-looking identity that is not the machine's.
        let d = TempDir::new("identity-algo");
        let raw = format!(
            r#"{{"version":1,"algorithm":"p256","secret_key":"{}"}}"#,
            "22".repeat(32)
        );
        std::fs::write(identity_path(d.path()), raw).unwrap();
        assert!(matches!(
            load(d.path()).unwrap_err(),
            IdentityError::Unusable { .. }
        ));
    }

    #[test]
    fn the_saved_file_carries_the_secret_and_nothing_else_identifying() {
        let d = TempDir::new("identity-shape");
        let identity = Identity::generate();
        save(d.path(), &identity).unwrap();

        let raw = std::fs::read_to_string(identity_path(d.path())).unwrap();
        assert!(raw.contains(&hex::encode(&identity.secret_bytes())));
        let back = load(d.path()).unwrap().unwrap();
        assert_eq!(back.public(), identity.public());
    }
}
