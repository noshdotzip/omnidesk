//! A scratch directory for the tests that touch the filesystem.
//!
//! Named per test rather than per process: `cargo test` runs tests on parallel threads,
//! so two tests sharing one directory would delete each other's files and fail in a way
//! that looks like a bug in the code under test.

use std::path::{Path, PathBuf};

pub struct TempDir(PathBuf);

impl TempDir {
    pub fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "ultidesk-identity-test-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
