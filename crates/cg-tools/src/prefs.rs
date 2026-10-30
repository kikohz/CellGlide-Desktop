//! The window's own preferences: `$XDG_CONFIG_HOME/cellglide/gui.json` on Linux,
//! `%LOCALAPPDATA%\CellGlide\prefs.json` on Windows. Starting at login is not here: its state
//! is the autostart file itself.

use serde::{Deserialize, Serialize};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    /// The user ticked "don't ask again" on the speed test's data warning.
    pub speed_confirmed: bool,
    /// The speed test also measures Wi-Fi.
    pub include_wifi: bool,
    /// Open the window when an iPhone is plugged in (off by default, as on the Mac).
    pub open_on_plug: bool,
}

impl Default for Prefs {
    fn default() -> Self {
        Prefs {
            speed_confirmed: false,
            include_wifi: true,
            open_on_plug: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrefChange {
    SpeedConfirmed,
    IncludeWifi(bool),
    OpenOnPlug(bool),
}

impl Prefs {
    pub fn apply(&mut self, change: PrefChange) {
        match change {
            PrefChange::SpeedConfirmed => self.speed_confirmed = true,
            PrefChange::IncludeWifi(on) => self.include_wifi = on,
            PrefChange::OpenOnPlug(on) => self.open_on_plug = on,
        }
    }
}

/// The file under a config home (see `crate::paths`): the Linux layout.
pub fn path_in(config_home: PathBuf) -> PathBuf {
    config_home.join("cellglide").join("gui.json")
}

#[cfg(not(windows))]
pub fn path() -> PathBuf {
    path_in(crate::paths::config_home())
}

/// Beside the speed test history, in `%LOCALAPPDATA%\CellGlide`.
#[cfg(windows)]
pub fn path() -> PathBuf {
    crate::paths::app_dir().join("prefs.json")
}

/// The saved preferences; defaults when the file is missing or unreadable.
pub fn load(path: &Path) -> Prefs {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Writes a temporary file beside the target and renames it over, so a crash never leaves half a file.
pub fn save(path: &Path, prefs: &Prefs) -> io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let temp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
    let written = (|| {
        let mut file = std::fs::File::create(&temp)?;
        file.write_all(&serde_json::to_vec_pretty(prefs).map_err(io::Error::other)?)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        std::fs::rename(&temp, path)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    written
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_or_corrupt_file_gives_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cellglide/gui.json");
        let prefs = load(&path);
        assert_eq!(prefs, Prefs::default());
        assert!(prefs.include_wifi);
        assert!(!prefs.speed_confirmed);
        assert!(!prefs.open_on_plug);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"{ not json").unwrap();
        assert_eq!(load(&path), Prefs::default());
        // A file from an older build with fewer fields keeps the defaults for the rest.
        std::fs::write(&path, br#"{"speed_confirmed": true}"#).unwrap();
        let prefs = load(&path);
        assert!(prefs.speed_confirmed);
        assert!(prefs.include_wifi);
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cellglide/gui.json");
        let mut prefs = Prefs::default();
        prefs.apply(PrefChange::SpeedConfirmed);
        prefs.apply(PrefChange::IncludeWifi(false));
        prefs.apply(PrefChange::OpenOnPlug(true));
        save(&path, &prefs).unwrap();
        assert_eq!(load(&path), prefs);
        // Nothing but the file itself is left behind.
        let names: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["gui.json"]);
    }

    // XDG paths are a Unix rule: "/home/x" is not absolute on Windows.
    #[cfg(unix)]
    #[test]
    fn relative_xdg_config_home_falls_back_to_home() {
        use crate::paths::config_home_from;
        let home = Some(PathBuf::from("/home/x"));
        assert_eq!(
            path_in(config_home_from(Some("rel".into()), home.clone())),
            PathBuf::from("/home/x/.config/cellglide/gui.json")
        );
        assert_eq!(
            path_in(config_home_from(Some("/cfg".into()), home)),
            PathBuf::from("/cfg/cellglide/gui.json")
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_prefs_live_beside_the_speed_history() {
        assert_eq!(path().parent(), crate::speed::default_path().parent());
        assert_eq!(path().file_name().unwrap(), "prefs.json");
    }
}
