//! The service's own log: tracing written to `logs\service.log.<date>` under the data
//! directory, one file per (UTC) day, the newest seven kept. Diagnostics read it back
//! here, as the Linux service reads the journal.

use jiff::Timestamp;
use jiff::civil::Date;
use std::collections::VecDeque;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{Builder, Rotation};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, fmt};

const PREFIX: &str = "service.log";
/// Longest line kept before the bundle's own caps apply; the rest is dropped.
const LINE_BYTES: usize = 8 * 1024;

/// Sends tracing to the daily files in `dir` and, with `console`, to standard error too.
/// Keep the guard until the process exits: dropping it flushes what is still queued.
/// If the directory can't be written, standard error is all there is.
pub fn init(dir: &Path, console: bool) -> WorkerGuard {
    let filter = || EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let appender = std::fs::create_dir_all(dir)
        .map_err(|e| e.to_string())
        .and_then(|()| {
            Builder::new()
                .rotation(Rotation::DAILY)
                .filename_prefix(PREFIX)
                .max_log_files(7)
                .build(dir)
                .map_err(|e| e.to_string())
        });
    match appender {
        Ok(appender) => {
            let (writer, guard) = tracing_appender::non_blocking(appender);
            let file = fmt::layer().with_ansi(false).with_writer(writer);
            let stderr = console.then(|| fmt::layer().with_writer(std::io::stderr));
            tracing_subscriber::registry()
                .with(filter())
                .with(file)
                .with(stderr)
                .init();
            guard
        }
        Err(error) => {
            let (writer, guard) = tracing_appender::non_blocking(std::io::stderr());
            tracing_subscriber::registry()
                .with(filter())
                .with(fmt::layer().with_writer(writer))
                .init();
            tracing::warn!("not logging to {}: {error}", dir.display());
            guard
        }
    }
}

/// The log lines written at or after `from`, the newest `max`, oldest first. A line is in
/// range by the RFC 3339 time it starts with; a line without one (the rest of a multi-line
/// message) goes with the line before it. Files are read in the order of the dates in
/// their names, and a day that ended before `from` is not read at all.
pub fn read_since(dir: &Path, from: Timestamp, max: usize) -> Result<Vec<String>, String> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let first_day = from.to_zoned(jiff::tz::TimeZone::UTC).date();
    let mut files: Vec<(Option<Date>, PathBuf)> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let date = match name.strip_prefix(PREFIX)? {
                "" => None,
                rest => Some(rest.strip_prefix('.')?.parse::<Date>().ok()?),
            };
            Some((date, entry.path()))
        })
        .filter(|(date, _)| date.is_none_or(|d| d >= first_day))
        .collect();
    files.sort();

    let mut lines = VecDeque::new();
    for (_, path) in files {
        let Ok(file) = std::fs::File::open(&path) else {
            continue;
        };
        let mut reader = BufReader::new(file);
        let mut raw = Vec::new();
        let mut keeping = false;
        loop {
            raw.clear();
            match reader.read_until(b'\n', &mut raw) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            while matches!(raw.last(), Some(b'\n' | b'\r')) {
                raw.pop();
            }
            raw.truncate(LINE_BYTES);
            let line = String::from_utf8_lossy(&raw).into_owned();
            if line.trim().is_empty() {
                continue;
            }
            if let Some(at) = line
                .split_whitespace()
                .next()
                .and_then(|t| t.parse::<Timestamp>().ok())
            {
                keeping = at >= from;
            }
            if !keeping {
                continue;
            }
            if lines.len() == max {
                lines.pop_front();
            }
            if max > 0 {
                lines.push_back(line);
            }
        }
    }
    Ok(lines.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, lines: &[&str]) {
        let mut text = lines.join("\r\n");
        text.push('\n');
        std::fs::write(dir.join(name), text).unwrap();
    }

    fn two_days() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        // Written newest first: the order comes from the names, not the directory.
        write(
            dir.path(),
            "service.log.2026-10-16",
            &[
                "2026-10-16T01:00:00.000001Z  INFO cellglide_service: b",
                "2026-10-16T02:00:00.000001Z  WARN cellglide_service: c",
                "    caused by: x",
                "2026-10-16T03:00:00.000001Z  INFO cellglide_service: d",
            ],
        );
        write(
            dir.path(),
            "service.log.2026-10-15",
            &[
                "2026-10-15T10:00:00.000001Z  INFO cellglide_service: old",
                "    old detail",
                "2026-10-15T23:00:00.000001Z  INFO cellglide_service: a",
            ],
        );
        write(
            dir.path(),
            "notes.txt",
            &["2026-10-16T04:00:00Z  INFO not ours"],
        );
        dir
    }

    #[test]
    fn read_since_keeps_the_newest_lines_in_range() {
        let dir = two_days();
        let from: Timestamp = "2026-10-15T12:00:00Z".parse().unwrap();
        let lines = read_since(dir.path(), from, 3).unwrap();
        assert_eq!(
            lines,
            [
                "2026-10-16T02:00:00.000001Z  WARN cellglide_service: c",
                "    caused by: x",
                "2026-10-16T03:00:00.000001Z  INFO cellglide_service: d",
            ]
        );
        let all = read_since(dir.path(), from, 100).unwrap();
        assert_eq!(all.len(), 5);
        assert!(all[0].ends_with(": a"), "{all:?}");
        assert!(
            !all.iter()
                .any(|l| l.contains("old") || l.contains("not ours"))
        );
    }

    #[test]
    fn days_before_the_range_are_not_read() {
        let dir = two_days();
        // A file from a day that ended before `from` is skipped by its name.
        write(
            dir.path(),
            "service.log.2026-10-01",
            &["2026-10-16T05:00:00Z  INFO misplaced"],
        );
        let from: Timestamp = "2026-10-16T00:00:00Z".parse().unwrap();
        let lines = read_since(dir.path(), from, 100).unwrap();
        assert_eq!(lines.len(), 4, "{lines:?}");
        assert!(lines[0].ends_with(": b"));
    }

    #[test]
    fn a_missing_directory_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let error = read_since(&dir.path().join("nope"), Timestamp::UNIX_EPOCH, 10).unwrap_err();
        assert!(error.contains("nope"), "{error}");
    }

    /// What `init` writes is what `read_since` reads: a line starting with its UTC time.
    #[test]
    fn the_file_layer_writes_lines_read_since_understands() {
        let dir = tempfile::tempdir().unwrap();
        let appender = Builder::new()
            .rotation(Rotation::DAILY)
            .filename_prefix(PREFIX)
            .max_log_files(7)
            .build(dir.path())
            .unwrap();
        let (writer, guard) = tracing_appender::non_blocking(appender);
        let subscriber =
            tracing_subscriber::registry().with(fmt::layer().with_ansi(false).with_writer(writer));
        let before = Timestamp::now() - jiff::SignedDuration::from_secs(1);
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!("hello from the test");
        });
        drop(guard);
        let lines = read_since(dir.path(), before, 10).unwrap();
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].ends_with("hello from the test"), "{lines:?}");
    }
}
