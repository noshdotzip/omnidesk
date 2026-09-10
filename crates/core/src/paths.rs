//! Where Ultidesk keeps per-user configuration and per-session runtime state, as one
//! rule shared by every process.
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

/// The environment [`runtime_dir`] reads.
#[derive(Debug, Default, Clone)]
pub struct RuntimeEnv {
    pub override_dir: Option<PathBuf>,
    /// `LOCALAPPDATA` on Windows — machine-local rather than roaming, because a socket
    /// path or a pid means nothing on another machine.
    pub local_appdata: Option<PathBuf>,
    /// `XDG_RUNTIME_DIR` on Linux.
    pub xdg_runtime_dir: Option<PathBuf>,
    pub temp_dir: PathBuf,
    /// Only used to name the fallback directory, so two users on one machine do not
    /// collide in a shared `/tmp`.
    pub user: Option<String>,
}

impl RuntimeEnv {
    pub fn from_process() -> Self {
        let var = |k: &str| std::env::var_os(k).map(PathBuf::from);
        RuntimeEnv {
            override_dir: var("ULTIDESK_DEV_DIR"),
            local_appdata: var("LOCALAPPDATA"),
            xdg_runtime_dir: var("XDG_RUNTIME_DIR"),
            temp_dir: std::env::temp_dir(),
            user: std::env::var("USER")
                .ok()
                .or_else(|| std::env::var("USERNAME").ok()),
        }
    }
}

/// Where this session's runtime state lives — the IPC socket and the handshake file.
///
/// Distinct from [`config_dir`] on purpose. Configuration outlives a login; a socket
/// path and a pid do not, and leaving either behind after logout is how a client ends up
/// connecting to nothing and reporting it as a mysterious failure.
pub fn runtime_dir() -> PathBuf {
    resolve_runtime_dir(&RuntimeEnv::from_process(), cfg!(windows))
}

/// The platform rules, as pure logic. `windows` is passed rather than read from `cfg!`
/// so both branches are exercised from either host.
///
/// # Why `XDG_RUNTIME_DIR` is the answer on Linux, and not merely a convention
/// `pam_systemd` creates it mode `0700`, owned by the user, and removes it at logout.
/// That directory permission is a real access control, enforced by the kernel on every
/// `connect()` — which is *stronger* than what the Windows named pipe has today, where
/// the per-launch token is the only gate and ACL restriction is still tracked debt in
/// docs/threat-model.md. Putting the socket anywhere world-traversable would throw that
/// away and leave the token doing all the work again.
///
/// The fallback matters for exactly that reason: when `XDG_RUNTIME_DIR` is unset the
/// directory under the temp dir has to be created `0700` by the caller, because `/tmp`
/// is world-writable and a socket sitting in it is reachable by every user on the box.
pub fn resolve_runtime_dir(env: &RuntimeEnv, windows: bool) -> PathBuf {
    if let Some(dir) = &env.override_dir {
        return dir.clone();
    }
    if windows {
        return match &env.local_appdata {
            Some(local) => local.join("Ultidesk"),
            None => env.temp_dir.join("Ultidesk"),
        };
    }
    if let Some(runtime) = &env.xdg_runtime_dir {
        return runtime.join("ultidesk");
    }
    // Named per user: `/tmp` is shared, and two people logged into one machine must not
    // land on the same path and fight over it.
    let user = env.user.as_deref().unwrap_or("user");
    env.temp_dir.join(format!("ultidesk-{user}"))
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

    fn runtime_env(
        override_dir: Option<&str>,
        local_appdata: Option<&str>,
        xdg_runtime: Option<&str>,
        user: Option<&str>,
    ) -> RuntimeEnv {
        RuntimeEnv {
            override_dir: override_dir.map(PathBuf::from),
            local_appdata: local_appdata.map(PathBuf::from),
            xdg_runtime_dir: xdg_runtime.map(PathBuf::from),
            temp_dir: PathBuf::from("/tmp"),
            user: user.map(String::from),
        }
    }

    #[test]
    fn the_runtime_override_wins_on_both_platforms() {
        for windows in [true, false] {
            let e = runtime_env(
                Some("/dev-dir"),
                Some("C:/Local"),
                Some("/run/user/1000"),
                None,
            );
            assert_eq!(resolve_runtime_dir(&e, windows), PathBuf::from("/dev-dir"));
        }
    }

    #[test]
    fn windows_runtime_state_is_machine_local_not_roaming() {
        // LOCALAPPDATA rather than APPDATA: a socket path and a pid are meaningless on
        // whatever other machine a roaming profile follows the user to.
        let e = runtime_env(None, Some("C:/Users/n/AppData/Local"), None, None);
        assert_eq!(
            resolve_runtime_dir(&e, true),
            PathBuf::from("C:/Users/n/AppData/Local").join("Ultidesk")
        );
    }

    #[test]
    fn linux_uses_the_xdg_runtime_dir() {
        let e = runtime_env(None, None, Some("/run/user/1000"), Some("nosh"));
        assert_eq!(
            resolve_runtime_dir(&e, false),
            PathBuf::from("/run/user/1000/ultidesk")
        );
    }

    #[test]
    fn the_linux_fallback_is_named_per_user() {
        // `/tmp` is shared. Two people logged into one machine must not resolve to the
        // same path and fight over the socket in it.
        let a = runtime_env(None, None, None, Some("alice"));
        let b = runtime_env(None, None, None, Some("bob"));
        assert_eq!(
            resolve_runtime_dir(&a, false),
            PathBuf::from("/tmp/ultidesk-alice")
        );
        assert_ne!(
            resolve_runtime_dir(&a, false),
            resolve_runtime_dir(&b, false)
        );
    }

    #[test]
    fn a_runtime_dir_is_always_produced() {
        // Unlike the config directory, there is no "nowhere to put it" answer: the agent
        // cannot serve IPC at all without a socket path, so the temp fallback is the
        // last resort rather than a `None` the caller has to handle.
        for windows in [true, false] {
            let e = runtime_env(None, None, None, None);
            assert!(resolve_runtime_dir(&e, windows).is_absolute() || cfg!(windows));
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
