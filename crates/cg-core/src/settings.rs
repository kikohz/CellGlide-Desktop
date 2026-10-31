//! Settings kept across restarts in /var/lib/cellglide/settings.json. Writes are atomic
//! (temporary file, fsync, rename); an unreadable file is replaced by defaults.

use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{self, Write as _};
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Stored {
    pub version: u32,
    pub auto_connect: bool,
    /// The user pressed Disconnect; auto connect stays off until they press Connect.
    pub user_disconnected: bool,
    /// A phone the user picked explicitly.
    pub chosen_serial: Option<String>,
    /// Windows' strict DNS while connected; on unless the user turned it off.
    pub strict_dns: bool,
    pub last_versions: LastVersions,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LastVersions {
    pub computer: Option<String>,
    pub phone: Option<String>,
}

impl Default for Stored {
    fn default() -> Self {
        Stored {
            version: 1,
            auto_connect: true,
            user_disconnected: false,
            chosen_serial: None,
            strict_dns: true,
            last_versions: LastVersions::default(),
        }
    }
}

pub struct SettingsStore {
    path: Option<PathBuf>,
    current: Stored,
}

impl SettingsStore {
    pub fn memory(initial: Stored) -> SettingsStore {
        SettingsStore {
            path: None,
            current: initial,
        }
    }

    pub fn load(path: impl Into<PathBuf>) -> SettingsStore {
        let path = path.into();
        let current = match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|error| {
                tracing::warn!(
                    "settings at {} unreadable ({error}); using defaults",
                    path.display()
                );
                Stored::default()
            }),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Stored::default(),
            Err(error) => {
                tracing::warn!(
                    "can't read settings at {} ({error}); using defaults",
                    path.display()
                );
                Stored::default()
            }
        };
        let store = SettingsStore {
            path: Some(path),
            current,
        };
        if let Err(error) = store.write() {
            tracing::warn!("can't write settings: {error}");
        }
        store
    }

    pub fn get(&self) -> &Stored {
        &self.current
    }

    pub fn update(&mut self, change: impl FnOnce(&mut Stored)) {
        let mut next = self.current.clone();
        change(&mut next);
        if next == self.current {
            return;
        }
        self.current = next;
        if let Err(error) = self.write() {
            tracing::warn!("can't write settings: {error}");
        }
    }

    fn write(&self) -> io::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        {
            let mut file = fs::File::create(&tmp)?;
            file.write_all(&serde_json::to_vec_pretty(&self.current).map_err(io::Error::other)?)?;
            file.sync_all()?;
        }
        fs::rename(&tmp, path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(dir: &tempfile::TempDir) -> PathBuf {
        dir.path().join("state/settings.json")
    }

    #[test]
    fn missing_file_creates_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let store = SettingsStore::load(path(&dir));
        assert_eq!(*store.get(), Stored::default());
        assert!(path(&dir).exists());
    }

    #[test]
    fn updates_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = SettingsStore::load(path(&dir));
        store.update(|s| {
            s.user_disconnected = true;
            s.chosen_serial = Some("aaaa".into());
        });
        let again = SettingsStore::load(path(&dir));
        assert!(again.get().user_disconnected);
        assert_eq!(again.get().chosen_serial.as_deref(), Some("aaaa"));
        assert!(!dir.path().join("state/settings.json.tmp").exists());
    }

    #[test]
    fn corrupt_file_falls_back_to_defaults() {
        for bad in ["", "not json", "[1,2]", "{\"auto_connect\": \"yes\"}"] {
            let dir = tempfile::tempdir().unwrap();
            std::fs::create_dir_all(path(&dir).parent().unwrap()).unwrap();
            std::fs::write(path(&dir), bad).unwrap();
            let store = SettingsStore::load(path(&dir));
            assert_eq!(*store.get(), Stored::default(), "input {bad:?}");
            let written: Stored =
                serde_json::from_slice(&std::fs::read(path(&dir)).unwrap()).unwrap();
            assert_eq!(written, Stored::default());
        }
    }

    #[test]
    fn unknown_fields_are_kept_out() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(path(&dir).parent().unwrap()).unwrap();
        std::fs::write(path(&dir), "{\"auto_connect\": false, \"bogus\": 1}").unwrap();
        let store = SettingsStore::load(path(&dir));
        assert!(!store.get().auto_connect);
        assert!(
            store.get().chosen_serial.is_none(),
            "missing fields take defaults"
        );
        assert!(
            !std::fs::read_to_string(path(&dir))
                .unwrap()
                .contains("bogus")
        );
    }

    /// A file from before the strict DNS switch keeps strict DNS on.
    #[test]
    fn old_file_keeps_strict_dns_on() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(path(&dir).parent().unwrap()).unwrap();
        std::fs::write(
            path(&dir),
            "{\"version\": 1, \"auto_connect\": false, \"user_disconnected\": false}",
        )
        .unwrap();
        let store = SettingsStore::load(path(&dir));
        assert!(store.get().strict_dns);
        assert!(!store.get().auto_connect);
    }

    #[test]
    fn memory_store_never_touches_disk() {
        let mut store = SettingsStore::memory(Stored {
            auto_connect: false,
            ..Stored::default()
        });
        store.update(|s| s.user_disconnected = true);
        assert!(store.get().user_disconnected);
        assert!(!store.get().auto_connect);
    }
}
