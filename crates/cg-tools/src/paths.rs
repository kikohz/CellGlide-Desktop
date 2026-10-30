//! Where the user's own config and state live, by the XDG rule: the variable is used only when
//! it is an absolute path, otherwise the default under the home directory.

use std::ffi::OsString;
use std::path::PathBuf;

fn base_from(xdg: Option<OsString>, home: Option<PathBuf>, under_home: &str) -> PathBuf {
    xdg.map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| home.map(|home| home.join(under_home)))
        .unwrap_or_else(|| PathBuf::from(under_home))
}

/// `$XDG_CONFIG_HOME`, or `~/.config` (also when the variable is a relative path).
pub fn config_home_from(xdg: Option<OsString>, home: Option<PathBuf>) -> PathBuf {
    base_from(xdg, home, ".config")
}

/// `$XDG_STATE_HOME`, or `~/.local/state` (also when the variable is a relative path).
pub fn state_home_from(xdg: Option<OsString>, home: Option<PathBuf>) -> PathBuf {
    base_from(xdg, home, ".local/state")
}

pub fn config_home() -> PathBuf {
    config_home_from(std::env::var_os("XDG_CONFIG_HOME"), std::env::home_dir())
}

pub fn state_home() -> PathBuf {
    state_home_from(std::env::var_os("XDG_STATE_HOME"), std::env::home_dir())
}

// XDG paths are a Unix rule: "/home/x" is not absolute on Windows.
#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn relative_xdg_config_home_falls_back_to_home() {
        let home = Some(PathBuf::from("/home/x"));
        assert_eq!(
            config_home_from(Some("rel".into()), home.clone()),
            PathBuf::from("/home/x/.config")
        );
        assert_eq!(
            config_home_from(Some("/etc/xdg".into()), home.clone()),
            PathBuf::from("/etc/xdg")
        );
        assert_eq!(
            config_home_from(None, home.clone()),
            PathBuf::from("/home/x/.config")
        );
        assert_eq!(
            state_home_from(Some("rel".into()), home),
            PathBuf::from("/home/x/.local/state")
        );
        assert_eq!(
            state_home_from(Some("/var/state".into()), Some(PathBuf::from("/home/x"))),
            PathBuf::from("/var/state")
        );
        assert_eq!(state_home_from(None, None), PathBuf::from(".local/state"));
    }
}
