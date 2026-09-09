//! Writing small state files without losing them.
//!
//! # Atomic replacement
//! Truncating the real file and writing into it means a crash or a power cut mid-write
//! leaves a half-written file. For settings that costs an arrangement; for the identity
//! key it costs every pairing on the machine. So the bytes go to a temporary file in
//! the same directory and are then renamed over the target, which is a single
//! filesystem operation: a reader sees either the old file or the new one.
//!
//! Same directory matters — a rename across filesystems is a copy, and stops being
//! atomic.
//!
//! # Private files
//! The identity key is a secret and is created `0600` on Unix. Windows has no mode
//! bits, and per-user ACL hardening is tracked as debt in `docs/threat-model.md`
//! alongside the named pipe's; pretending otherwise here would be worse than saying so.

use std::io;
use std::path::Path;

/// Whether a file holds a secret and must not be readable by other users.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    Private,
    Normal,
}

pub fn write_atomic(path: &Path, bytes: &[u8], visibility: Visibility) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));

    // Created with the restrictive mode rather than chmod-ed afterwards: between the
    // create and the chmod there is a window in which the key is world-readable, and a
    // window is all an attacker needs.
    let mut file = create(&tmp, visibility)?;
    {
        use std::io::Write;
        file.write_all(bytes)?;
        // Rename does not imply the data reached the disk. Without this, a power cut
        // can leave the new name pointing at an empty file — which for the identity is
        // indistinguishable from having no identity at all.
        file.sync_all()?;
    }
    drop(file);

    match std::fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

#[cfg(unix)]
fn create(path: &Path, visibility: Visibility) -> io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    let mode = match visibility {
        Visibility::Private => 0o600,
        Visibility::Normal => 0o644,
    };
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(mode)
        .open(path)
}

#[cfg(not(unix))]
fn create(path: &Path, _visibility: Visibility) -> io::Result<std::fs::File> {
    std::fs::File::create(path)
}

/// Report a secret file that other users on this machine can read.
///
/// Returned as a note rather than an error: refusing to start because of a permission
/// bit would lock the operator out of their own machine, and the situation is one they
/// can fix in a second once told. Always `None` on Windows, where the mode bits do not
/// exist — see the module note.
pub fn over_permissive(path: &Path) -> Option<String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(path).ok()?.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            return Some(format!(
                "{} is mode {mode:o}; it holds a private key and should be 600 (chmod 600 {})",
                path.display(),
                path.display()
            ));
        }
        None
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writing_creates_the_directory_and_leaves_no_temporary_behind() {
        let dir = std::env::temp_dir().join(format!("ultidesk-file-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("nested").join("thing.json");

        write_atomic(&path, b"one", Visibility::Private).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"one");

        // Replacing must overwrite rather than fail, which is where a naive
        // "rename only if absent" implementation breaks on the second save.
        write_atomic(&path, b"two", Visibility::Private).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"two");

        let leftovers: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n != "thing.json")
            .collect();
        assert!(leftovers.is_empty(), "temporary files left: {leftovers:?}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_private_file_is_created_unreadable_by_anyone_else() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("ultidesk-mode-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("secret");

        write_atomic(&path, b"x", Visibility::Private).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "created mode was {mode:o}");
        assert_eq!(over_permissive(&path), None);

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(over_permissive(&path).is_some(), "644 should be reported");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
