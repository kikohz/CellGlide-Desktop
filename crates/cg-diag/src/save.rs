//! Saving a diagnostics bundle to a file: where by default (the user's download folder), under
//! what name, and how, so an existing file is never overwritten and a failed write never leaves
//! half a bundle. The error is always the one sentence the window shows.

use cg_i18n::tr;
use jiff::Timestamp;
use jiff::tz::TimeZone;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// `cellglide-diagnostics-YYYYMMDD-HHMMSS.json` in local time, so the files sort by time.
pub fn file_name(now: Timestamp, tz: &TimeZone) -> String {
    format!(
        "cellglide-diagnostics-{}.json",
        now.to_zoned(tz.clone()).strftime("%Y%m%d-%H%M%S")
    )
}

fn failed(error: io::Error) -> String {
    tr("diag.save_failed", &[("reason", &error.to_string())])
}

/// Creates `path`, which must not exist yet, and writes `json` into it: readable only by the
/// user on Unix, the folder's own permissions on Windows. A failed write removes the file.
fn create(path: &Path, json: &str) -> io::Result<()> {
    create_with(path, |file| file.write_all(json.as_bytes()))
}

/// [`create`] with the writing passed in, so a test can make it fail half way.
fn create_with(
    path: &Path,
    write: impl FnOnce(&mut std::fs::File) -> io::Result<()>,
) -> io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(path)?;
    let written = write(&mut file);
    if written.is_err() {
        // Never leave half a bundle behind.
        drop(file);
        let _ = std::fs::remove_file(path);
    }
    written
}

/// Writes the bundle into `dir` as [`file_name`], never over an existing file: the same second
/// again is `-2`, `-3`, … up to `-20`.
pub fn save_bundle(
    dir: &Path,
    now: Timestamp,
    tz: &TimeZone,
    json: &str,
) -> Result<PathBuf, String> {
    let name = file_name(now, tz);
    let stem = name.trim_end_matches(".json");
    let mut last = None;
    for n in 1..=20 {
        let path = if n == 1 {
            dir.join(&name)
        } else {
            dir.join(format!("{stem}-{n}.json"))
        };
        match create(&path, json) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => last = Some(error),
            Err(error) => return Err(failed(error)),
        }
    }
    Err(failed(last.unwrap_or_else(|| {
        io::Error::from(io::ErrorKind::AlreadyExists)
    })))
}

/// Writes the bundle to the full path the user chose; an existing file there is an error,
/// never overwritten.
pub fn write_to(path: &Path, json: &str) -> Result<PathBuf, String> {
    create(path, json).map_err(failed)?;
    Ok(path.to_path_buf())
}

/// Writes the bundle to the full path the user chose and confirmed replacing: into a temporary
/// file beside it, then renamed over it, so the old file stays whole until the new one is.
/// A failed write leaves the old file (if any) as it was and no temporary file behind.
pub fn write_over(path: &Path, json: &str) -> Result<PathBuf, String> {
    write_over_with(path, |file| file.write_all(json.as_bytes())).map_err(failed)?;
    Ok(path.to_path_buf())
}

/// [`write_over`] with the writing passed in, so a test can make it fail half way.
fn write_over_with(
    path: &Path,
    write: impl FnOnce(&mut std::fs::File) -> io::Result<()>,
) -> io::Result<()> {
    let dir = match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    };
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    // A name of our own: never one that is already there (another save at the same time).
    let mut n = 0u32;
    let (temporary, mut file) = loop {
        let temporary = dir.join(format!(".{name}.{}.{n}.tmp", std::process::id()));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        match options.open(&temporary) {
            Ok(file) => break (temporary, file),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists && n < 1000 => n += 1,
            Err(error) => return Err(error),
        }
    };
    let written = write(&mut file)
        .and_then(|()| file.sync_all())
        .and_then(|()| {
            drop(file);
            std::fs::rename(&temporary, path)
        });
    if written.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    written
}

/// Where "Save to File" writes when the user picks nothing: the XDG download directory, else
/// `~/Downloads`, else home (Unix); the Downloads known folder, wherever it was moved (OneDrive
/// included), else `%USERPROFILE%` (Windows). `None` without a home.
#[cfg(unix)]
pub fn default_dir() -> Option<PathBuf> {
    Places::from_env().map(|places| places.download_dir())
}

#[cfg(windows)]
pub fn default_dir() -> Option<PathBuf> {
    known_downloads().or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
}

/// The Downloads known folder (`FOLDERID_Downloads`): its real place, also when the user or
/// OneDrive moved it.
#[cfg(windows)]
pub fn known_downloads() -> Option<PathBuf> {
    cg_tools::paths::known_folder(&cg_tools::paths::FOLDERID_Downloads)
}

/// Where the user's files are; a window is given one, so its tests never write into the real
/// home.
#[cfg(unix)]
#[derive(Clone, Debug)]
pub struct Places {
    pub home: PathBuf,
    /// Holds `user-dirs.dirs`.
    pub config_home: PathBuf,
}

#[cfg(unix)]
impl Places {
    pub fn from_env() -> Option<Places> {
        Some(Places {
            home: std::env::home_dir()?,
            config_home: cg_tools::paths::config_home(),
        })
    }

    /// The download directory as of now: `user-dirs.dirs` is read again each time.
    pub fn download_dir(&self) -> PathBuf {
        let user_dirs = std::fs::read_to_string(self.config_home.join("user-dirs.dirs")).ok();
        let xdg = user_dirs.and_then(|text| user_dirs_download(&text, &self.home));
        save_dir(xdg, self.home.clone(), Path::is_dir)
    }
}

/// `XDG_DOWNLOAD_DIR` from the text of `user-dirs.dirs`: `"$HOME/…"` or an absolute path. The
/// last such line wins, and a comment may follow the closing quote.
#[cfg(unix)]
pub fn user_dirs_download(user_dirs: &str, home: &Path) -> Option<PathBuf> {
    let line = user_dirs
        .lines()
        .rev()
        .map(str::trim)
        .find_map(|line| line.strip_prefix("XDG_DOWNLOAD_DIR="))?;
    let rest = line.trim().strip_prefix('"')?;
    let close = rest.find('"')?;
    let (value, after) = (&rest[..close], rest[close + 1..].trim());
    if !(after.is_empty() || after.starts_with('#')) {
        return None;
    }
    if let Some(rest) = value.strip_prefix("$HOME") {
        // `$HOMEfoo` is not the home directory.
        if !(rest.is_empty() || rest.starts_with('/')) {
            return None;
        }
        let rest = rest.trim_start_matches('/');
        return Some(if rest.is_empty() {
            home.to_path_buf()
        } else {
            home.join(rest)
        });
    }
    let path = PathBuf::from(value);
    path.is_absolute().then_some(path)
}

/// The XDG download directory if it is set and exists, else `~/Downloads` if it exists, else
/// home.
#[cfg(unix)]
pub fn save_dir(
    xdg_download: Option<PathBuf>,
    home: PathBuf,
    exists: impl Fn(&Path) -> bool,
) -> PathBuf {
    if let Some(dir) = xdg_download.filter(|dir| exists(dir)) {
        return dir;
    }
    let downloads = home.join("Downloads");
    if exists(&downloads) { downloads } else { home }
}

#[cfg(test)]
mod tests {
    use super::*;

    const J: &str = r#"{"bundle":1}"#;

    fn noon() -> Timestamp {
        "2026-10-30T12:00:00Z".parse().unwrap()
    }

    #[test]
    fn file_names_sort_by_time() {
        let at: Timestamp = "2026-10-19T08:05:09Z".parse().unwrap();
        assert_eq!(
            file_name(at, &TimeZone::UTC),
            "cellglide-diagnostics-20261019-080509.json"
        );
        let later: Timestamp = "2026-10-19T10:00:00Z".parse().unwrap();
        assert!(file_name(at, &TimeZone::UTC) < file_name(later, &TimeZone::UTC));
    }

    #[test]
    fn save_never_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join(file_name(noon(), &TimeZone::UTC));
        std::fs::write(&first, "old").unwrap();
        let saved = save_bundle(dir.path(), noon(), &TimeZone::UTC, "{}").unwrap();
        assert!(saved.to_string_lossy().ends_with("-2.json"), "{saved:?}");
        assert_eq!(std::fs::read_to_string(&first).unwrap(), "old");
        assert_eq!(std::fs::read_to_string(&saved).unwrap(), "{}");
        // And again: -3.
        let third = save_bundle(dir.path(), noon(), &TimeZone::UTC, "{}").unwrap();
        assert!(third.to_string_lossy().ends_with("-3.json"), "{third:?}");
    }

    #[test]
    fn twenty_taken_names_are_an_error() {
        let _guard = cg_i18n::lock_for_tests();
        let dir = tempfile::tempdir().unwrap();
        for _ in 0..20 {
            save_bundle(dir.path(), noon(), &TimeZone::UTC, "{}").unwrap();
        }
        let error = save_bundle(dir.path(), noon(), &TimeZone::UTC, "{}").unwrap_err();
        assert!(!error.is_empty() && !error.contains("%{"), "{error}");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 20);
    }

    #[test]
    fn failed_write_leaves_no_file() {
        let dir = tempfile::tempdir().unwrap();
        let not_a_dir = dir.path().join("file");
        std::fs::write(&not_a_dir, "x").unwrap();
        assert!(save_bundle(&not_a_dir, noon(), &TimeZone::UTC, "{}").is_err());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn a_write_that_fails_half_way_leaves_no_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(file_name(noon(), &TimeZone::UTC));
        let error = create_with(&path, |file| {
            file.write_all(b"{\"half\":")?;
            Err(io::Error::other("disk full"))
        })
        .unwrap_err();
        assert_eq!(error.to_string(), "disk full");
        assert!(!path.exists());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn write_over_replaces_a_confirmed_file_and_leaves_nothing_else() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("report.json");
        // New, then over the existing file.
        assert_eq!(write_over(&path, "{\"a\":1}"), Ok(path.clone()));
        std::fs::write(&path, "old").unwrap();
        assert_eq!(write_over(&path, "{}"), Ok(path.clone()));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{}");
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["report.json"]);
    }

    #[test]
    fn write_over_that_fails_half_way_keeps_the_old_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("report.json");
        std::fs::write(&path, "old").unwrap();
        let error = write_over_with(&path, |file| {
            file.write_all(b"{\"half\":")?;
            Err(io::Error::other("disk full"))
        })
        .unwrap_err();
        assert_eq!(error.to_string(), "disk full");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "old");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn write_to_refuses_an_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mine.json");
        std::fs::write(&path, "old").unwrap();
        assert!(write_to(&path, "{}").is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "old");
    }

    #[test]
    fn write_to_writes_a_new_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mine.json");
        assert_eq!(write_to(&path, J).unwrap(), path);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), J);
    }

    #[cfg(windows)]
    #[test]
    fn windows_download_dir_is_the_known_folder() {
        let out = std::process::Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                "[Console]::OutputEncoding=[Text.Encoding]::UTF8; (New-Object -ComObject Shell.Application).NameSpace('shell:Downloads').Self.Path",
            ])
            .output()
            .unwrap();
        let expected = String::from_utf8(out.stdout).unwrap();
        assert_eq!(
            known_downloads().unwrap(),
            std::path::PathBuf::from(expected.trim())
        );
        assert_eq!(default_dir(), known_downloads());
    }

    #[cfg(unix)]
    mod unix {
        use super::*;
        use std::os::unix::fs::PermissionsExt;

        #[test]
        fn saved_files_are_private_and_never_overwrite() {
            let _guard = cg_i18n::lock_for_tests();
            let home = tempfile::tempdir().unwrap();
            let config = tempfile::tempdir().unwrap();
            let downloads = home.path().join("Downloads");
            std::fs::create_dir(&downloads).unwrap();
            let places = Places {
                home: home.path().into(),
                config_home: config.path().into(),
            };
            let at: Timestamp = "2026-10-19T08:05:09Z".parse().unwrap();
            let first = save_bundle(&places.download_dir(), at, &TimeZone::UTC, J).unwrap();
            assert_eq!(
                first,
                downloads.join("cellglide-diagnostics-20261019-080509.json")
            );
            assert_eq!(std::fs::read_to_string(&first).unwrap(), J);
            let mode = std::fs::metadata(&first).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
            // The same second again: a new name, the first file untouched.
            let second = save_bundle(&places.download_dir(), at, &TimeZone::UTC, "other").unwrap();
            assert_ne!(second, first);
            assert_eq!(std::fs::read_to_string(&first).unwrap(), J);
            assert_eq!(second.parent(), Some(downloads.as_path()));

            // user-dirs.dirs in the config home names another directory.
            let mine = home.path().join("下载");
            std::fs::create_dir(&mine).unwrap();
            std::fs::write(
                config.path().join("user-dirs.dirs"),
                "XDG_DOWNLOAD_DIR=\"$HOME/下载\"\n",
            )
            .unwrap();
            let third = save_bundle(&places.download_dir(), at, &TimeZone::UTC, J).unwrap();
            assert_eq!(third.parent(), Some(mine.as_path()));

            // A file the user picked is private too.
            let picked = home.path().join("picked.json");
            write_to(&picked, J).unwrap();
            let mode = std::fs::metadata(&picked).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }

        #[test]
        fn download_dir_from_user_dirs() {
            let home = Path::new("/home/x");
            let file = "# written by xdg-user-dirs-update\nXDG_DESKTOP_DIR=\"$HOME/桌面\"\nXDG_DOWNLOAD_DIR=\"$HOME/下载\"\n";
            assert_eq!(
                user_dirs_download(file, home),
                Some(PathBuf::from("/home/x/下载"))
            );
            assert_eq!(
                user_dirs_download("XDG_DOWNLOAD_DIR=\"/data/dl\"\n", home),
                Some(PathBuf::from("/data/dl"))
            );
            assert_eq!(
                user_dirs_download("XDG_DOWNLOAD_DIR=\"$HOME\"\n", home),
                Some(PathBuf::from("/home/x"))
            );
            // Commented out, relative, or missing: no download directory.
            assert_eq!(
                user_dirs_download("#XDG_DOWNLOAD_DIR=\"$HOME/dl\"\n", home),
                None
            );
            assert_eq!(user_dirs_download("XDG_DOWNLOAD_DIR=\"dl\"\n", home), None);
            assert_eq!(
                user_dirs_download("XDG_DESKTOP_DIR=\"$HOME/d\"\n", home),
                None
            );

            let xdg = Some(PathBuf::from("/home/x/下载"));
            let all = |_: &Path| true;
            let none = |_: &Path| false;
            let only = |dir: &'static str| move |p: &Path| p == Path::new(dir);
            assert_eq!(
                save_dir(xdg.clone(), home.into(), all),
                PathBuf::from("/home/x/下载")
            );
            // Set but gone: ~/Downloads, then ~.
            assert_eq!(
                save_dir(xdg.clone(), home.into(), only("/home/x/Downloads")),
                PathBuf::from("/home/x/Downloads")
            );
            assert_eq!(save_dir(xdg, home.into(), none), PathBuf::from("/home/x"));
            assert_eq!(
                save_dir(None, home.into(), all),
                PathBuf::from("/home/x/Downloads")
            );
            assert_eq!(save_dir(None, home.into(), none), PathBuf::from("/home/x"));
        }

        #[test]
        fn user_dirs_edge_cases() {
            let home = Path::new("/home/x");
            // `$HOME` must end the value or be followed by `/`.
            assert_eq!(
                user_dirs_download("XDG_DOWNLOAD_DIR=\"$HOMEfoo\"\n", home),
                None
            );
            assert_eq!(
                user_dirs_download("XDG_DOWNLOAD_DIR=\"$HOME/\"\n", home),
                Some(PathBuf::from("/home/x"))
            );
            // The last line wins, as when a shell sources the file.
            assert_eq!(
                user_dirs_download(
                    "XDG_DOWNLOAD_DIR=\"$HOME/old\"\nXDG_DOWNLOAD_DIR=\"$HOME/new\"\n",
                    home
                ),
                Some(PathBuf::from("/home/x/new"))
            );
            // A comment after the closing quote is fine; other trailing text is not.
            assert_eq!(
                user_dirs_download("XDG_DOWNLOAD_DIR=\"$HOME/dl\"  # mine\n", home),
                Some(PathBuf::from("/home/x/dl"))
            );
            assert_eq!(
                user_dirs_download("XDG_DOWNLOAD_DIR=\"$HOME/dl\" junk\n", home),
                None
            );
            assert_eq!(
                user_dirs_download("XDG_DOWNLOAD_DIR=\"$HOME/dl\n", home),
                None
            );
        }
    }
}
