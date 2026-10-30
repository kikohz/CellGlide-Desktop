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

/// Windows keeps the user's own files (preferences, speed test history) in one folder:
/// `%LOCALAPPDATA%\CellGlide`, from the shell's known folder; the `LOCALAPPDATA` variable when
/// the shell doesn't answer; `.\CellGlide` when neither is there.
#[cfg(windows)]
pub fn app_dir() -> PathBuf {
    local_app_data()
        .or_else(|| std::env::var_os("LOCALAPPDATA").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("CellGlide")
}

#[cfg(windows)]
fn local_app_data() -> Option<PathBuf> {
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{FOLDERID_LocalAppData, KF_FLAG_DEFAULT, SHGetKnownFolderPath};
    // SAFETY: on success the path is CoTaskMemAlloc'd and freed below.
    let path =
        unsafe { SHGetKnownFolderPath(&FOLDERID_LocalAppData, KF_FLAG_DEFAULT, None) }.ok()?;
    // SAFETY: a NUL-terminated string from the call above.
    let text = unsafe { path.to_string() };
    // SAFETY: freed exactly once.
    unsafe { CoTaskMemFree(Some(path.0.cast())) };
    text.ok().filter(|text| !text.is_empty()).map(PathBuf::from)
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;

    #[test]
    fn app_dir_is_under_local_app_data() {
        let dir = app_dir();
        assert!(dir.ends_with("CellGlide"), "{dir:?}");
        assert!(
            dir.starts_with(std::env::var_os("LOCALAPPDATA").unwrap()),
            "{dir:?}"
        );
    }
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
