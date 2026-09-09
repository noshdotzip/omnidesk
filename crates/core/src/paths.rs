//! Where Ultidesk keeps per-user configuration, as one rule shared by every process.
//!
//! The control app and the agent must resolve the *same* directory or they disagree
//! about which machine this is: the control app would edit one `settings.json` while
//! the agent read another, and the identity in one would not be the identity in the
//! other. That is why the rule lives here — in the crate both already depend on —
//! rather than being written once per binary.
//!
//! This module reads process environment variables, which is the one concession to the
//! crate's "no I/O" rule: env vars are process state rather than a filesystem or a
//! network, and the resolution itself is a pure function over a struct so both
//! platforms' rules can be tested from either host.

use std::path::PathBuf;

/// The environment [`config_dir`] reads, named so the rules can be tested without
/// mutating the real process environment.
///
/// Env vars are global to the process and `cargo test` runs tests on parallel threads,
/// so a test that sets one can change what a concurrently running test sees. Passing
/// them in makes both platforms' rules deterministic to test.
#[derive(Debug, Default, Clone)]
pub struct ConfigEnv {
    pub override_dir: Option<PathBuf>,
    pub appdata: Option<PathBuf>,
    pub xdg_config_home: Option<PathBuf>,
    pub home: Option<PathBuf>,
}

impl ConfigEnv {
    /// Read the real environment.
    pub fn from_process() -> Self {
        let var = |k: &str| std::env::var_os(k).map(PathBuf::from);
        ConfigEnv {
            override_dir: var("ULTIDESK_CONFIG_DIR"),
            appdata: var("APPDATA"),
            xdg_config_home: var("XDG_CONFIG_HOME"),
            home: var("HOME"),
        }
    }
}

/// Where configuration lives on this platform.
///
/// `ULTIDESK_CONFIG_DIR` overrides everything, which is what lets a portable install
/// keep its configuration beside the binary — and what lets a test point a whole
/// process at a temporary directory.
pub fn config_dir() -> Option<PathBuf> {
    resolve_config_dir(&ConfigEnv::from_process(), cfg!(windows))
}

/// The platform rules, as pure logic.
///
/// `windows` is passed rather than read from `cfg!` at this level so both branches can
/// be exercised from either host — the Windows rule is otherwise never tested on Linux
/// and vice versa, which is exactly how one of them rots.
pub fn resolve_config_dir(env: &ConfigEnv, windows: bool) -> Option<PathBuf> {
    if let Some(dir) = &env.override_dir {
        return Some(dir.clone());
    }
    if windows {
        return env.appdata.as_ref().map(|a| a.join("Ultidesk"));
    }
    // The XDG default, with the documented fallback rather than an assumption that
    // XDG_CONFIG_HOME is always set — on a plain login shell it usually is not.
    if let Some(x) = &env.xdg_config_home {
        return Some(x.join("ultidesk"));
    }
    env.home
        .as_ref()
        .map(|h| h.join(".config").join("ultidesk"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(
        override_dir: Option<&str>,
        appdata: Option<&str>,
        xdg: Option<&str>,
        home: Option<&str>,
    ) -> ConfigEnv {
        ConfigEnv {
            override_dir: override_dir.map(PathBuf::from),
            appdata: appdata.map(PathBuf::from),
            xdg_config_home: xdg.map(PathBuf::from),
            home: home.map(PathBuf::from),
        }
    }

    #[test]
    fn the_override_wins_on_both_platforms() {
        // What a portable install relies on.
        for windows in [true, false] {
            let e = env(
                Some("/portable"),
                Some("C:/AppData"),
                Some("/xdg"),
                Some("/home/n"),
            );
            assert_eq!(
                resolve_config_dir(&e, windows),
                Some(PathBuf::from("/portable")),
                "override ignored (windows={windows})"
            );
        }
    }

    #[test]
    fn windows_uses_appdata() {
        let e = env(None, Some("C:/Users/n/AppData/Roaming"), None, None);
        assert_eq!(
            resolve_config_dir(&e, true),
            Some(PathBuf::from("C:/Users/n/AppData/Roaming").join("Ultidesk"))
        );
    }

    #[test]
    fn linux_prefers_xdg_config_home() {
        let e = env(None, None, Some("/home/n/.config"), Some("/home/n"));
        assert_eq!(
            resolve_config_dir(&e, false),
            Some(PathBuf::from("/home/n/.config").join("ultidesk"))
        );
    }

    #[test]
    fn linux_falls_back_to_home_when_xdg_is_unset() {
        // On a plain login shell XDG_CONFIG_HOME usually is not set, so this fallback
        // is the common path rather than an edge case.
        let e = env(None, None, None, Some("/home/n"));
        assert_eq!(
            resolve_config_dir(&e, false),
            Some(PathBuf::from("/home/n/.config/ultidesk"))
        );
    }

    #[test]
    fn no_home_and_no_appdata_means_no_config_directory() {
        // Reported as None so the caller can say it cannot save, rather than writing to
        // the current working directory and scattering settings files around.
        assert_eq!(resolve_config_dir(&ConfigEnv::default(), true), None);
        assert_eq!(resolve_config_dir(&ConfigEnv::default(), false), None);
    }

    #[test]
    fn windows_does_not_consult_xdg_and_linux_does_not_consult_appdata() {
        // Each platform ignoring the other's variable is what keeps a cross-platform
        // dev box from resolving to a surprising directory.
        let e = env(None, Some("C:/AppData"), Some("/xdg"), None);
        assert_eq!(
            resolve_config_dir(&e, true),
            Some(PathBuf::from("C:/AppData").join("Ultidesk"))
        );
        assert_eq!(
            resolve_config_dir(&e, false),
            Some(PathBuf::from("/xdg").join("ultidesk"))
        );
    }
}
