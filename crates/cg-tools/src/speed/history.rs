//! Speed test history: the last 20 results, newest first, in the user's state directory
//! (`speedtests.json`). Ported from the Mac's SpeedHistoryStore (SpeedComparison.swift).

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// How many results are kept.
pub const LIMIT: usize = 20;
/// A history file bigger than this isn't ours.
pub const MAX_FILE: u64 = 1024 * 1024;

/// One leg's numbers: Mbps and milliseconds; missing values stay missing.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Summary {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub download: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upload: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Record {
    pub id: Uuid,
    pub date: Timestamp,
    /// `None` for a Wi‑Fi-only test (no phone connected).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phone: Option<Summary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wifi: Option<Summary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub carrier: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<String>,
}

/// `$XDG_STATE_HOME/cellglide/speedtests.json`, or `~/.local/state/cellglide/speedtests.json`.
pub fn default_path() -> PathBuf {
    crate::paths::state_home()
        .join("cellglide")
        .join("speedtests.json")
}

/// The history file and its records, newest first.
#[derive(Debug)]
pub struct HistoryStore {
    path: PathBuf,
    records: Vec<Record>,
}

impl HistoryStore {
    /// Reads `path`; a missing or unreadable file is an empty history, and so is one over
    /// 1 MiB (20 records are a few KiB), with a warning. At most 20 records are kept.
    pub fn open(path: PathBuf) -> HistoryStore {
        let mut records = read(&path).unwrap_or_default();
        records.truncate(LIMIT);
        HistoryStore { path, records }
    }

    /// The history file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn records(&self) -> &[Record] {
        &self.records
    }

    /// Puts `record` first, keeps the newest 20, and saves. The record stays in memory even
    /// when saving fails.
    pub fn add(&mut self, record: Record) -> io::Result<()> {
        self.records.insert(0, record);
        self.records.truncate(LIMIT);
        save(&self.path, &self.records)
    }

    /// Forgets every record and removes the file.
    pub fn clear(&mut self) -> io::Result<()> {
        self.records.clear();
        match std::fs::remove_file(&self.path) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }
}

fn read(path: &Path) -> Option<Vec<Record>> {
    let size = std::fs::metadata(path).ok()?.len();
    if size > MAX_FILE {
        // Shared by the CLI and the GUI, neither of which installs a tracing subscriber.
        eprintln!(
            "{}: speed test history is too big ({size} bytes); starting empty",
            path.display()
        );
        return None;
    }
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

/// Writes a temporary file next to `path` (unique per process and call, mode 0600), flushes it
/// to disk, then renames it over, so a crash never leaves half a history.
fn save(path: &Path, records: &[Record]) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let data = serde_json::to_vec_pretty(records).map_err(io::Error::other)?;
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let temporary = path.with_file_name(format!(
        ".{name}.{}.{}.tmp",
        std::process::id(),
        Uuid::new_v4().simple()
    ));
    let written = (|| {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut file = options.open(&temporary)?;
        file.write_all(&data)?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    written
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(i: i64) -> Record {
        Record {
            id: Uuid::new_v4(),
            date: Timestamp::from_second(i).unwrap(),
            phone: Some(Summary {
                download: Some(i as f64),
                upload: None,
                latency: None,
            }),
            wifi: None,
            carrier: None,
            generation: None,
        }
    }

    #[test]
    fn history_newest_first_capped_and_persisted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state/cellglide/speedtests.json");
        let mut store = HistoryStore::open(path.clone());
        assert!(store.records().is_empty());
        for i in 0..25 {
            store.add(record(i)).unwrap();
        }
        assert_eq!(store.records().len(), 20);
        assert_eq!(
            store.records()[0].phone.as_ref().unwrap().download,
            Some(24.0)
        );
        let reopened = HistoryStore::open(path.clone());
        assert_eq!(reopened.records().len(), 20);
        assert_eq!(reopened.records(), store.records());
        store.clear().unwrap();
        assert!(store.records().is_empty());
        assert!(HistoryStore::open(path).records().is_empty());
    }

    #[test]
    fn wifi_only_record_round_trips_without_nulls() {
        let record = Record {
            id: Uuid::new_v4(),
            date: Timestamp::from_second(1).unwrap(),
            phone: None,
            wifi: Some(Summary {
                download: Some(60.0),
                upload: Some(20.0),
                latency: Some(10.0),
            }),
            carrier: None,
            generation: None,
        };
        let json = serde_json::to_string(&record).unwrap();
        assert!(!json.contains("null") && !json.contains("phone"), "{json}");
        assert!(json.contains("\"date\":\"1970-01-01T00:00:01Z\""), "{json}");
        assert_eq!(serde_json::from_str::<Record>(&json).unwrap(), record);
    }

    #[test]
    fn unreadable_file_is_an_empty_history() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("speedtests.json");
        std::fs::write(&path, b"not json").unwrap();
        assert!(HistoryStore::open(path).records().is_empty());
    }

    #[test]
    fn open_keeps_at_most_twenty_and_refuses_huge_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("speedtests.json");
        let many: Vec<Record> = (0..30).map(record).collect();
        std::fs::write(&path, serde_json::to_vec(&many).unwrap()).unwrap();
        assert_eq!(HistoryStore::open(path.clone()).records(), &many[..20]);
        let mut huge = serde_json::to_vec(&many).unwrap();
        huge.resize(MAX_FILE as usize + 1, b' ');
        std::fs::write(&path, huge).unwrap();
        assert!(HistoryStore::open(path).records().is_empty());
    }

    #[test]
    fn saving_leaves_one_private_file_and_no_temporaries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("speedtests.json");
        let mut store = HistoryStore::open(path.clone());
        store.add(record(1)).unwrap();
        store.add(record(2)).unwrap();
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from("speedtests.json")]);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn default_path_is_under_the_state_directory() {
        let path = default_path();
        assert!(path.ends_with("cellglide/speedtests.json"), "{path:?}");
    }
}
