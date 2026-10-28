//! Starting at login. The system-wide `/etc/xdg/autostart/cellglide-gui.desktop` (installed by
//! the package) starts the window in the background; turning it off is a user-level file of the
//! same name with `Hidden=true`, which the desktop reads as "this entry does not exist". Turning
//! it on removes that file again if it is ours. The system directory is never touched, and a
//! file the user wrote themselves is edited (only `Hidden=`), never deleted.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

pub const FILE: &str = "cellglide-gui.desktop";

const HEADER: &str = "[Desktop Entry]";
const MARKER: &str = "X-CellGlide-Override=true";

/// `<config_home>/autostart/cellglide-gui.desktop`.
pub fn user_file(config_home: &Path) -> PathBuf {
    config_home.join("autostart").join(FILE)
}

/// The file we write to switch it off.
pub fn override_text() -> &'static str {
    "[Desktop Entry]\nType=Application\nName=CellGlide\nHidden=true\nX-CellGlide-Override=true\n"
}

/// The value of `key` in the `[Desktop Entry]` group.
fn value<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    let mut inside = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            inside = line == HEADER;
        } else if inside
            && let Some((k, v)) = line.split_once('=')
            && k.trim() == key
        {
            return Some(v.trim());
        }
    }
    None
}

/// Whether the user's file leaves starting at login on: no file means the system entry applies.
pub fn is_enabled(user_text: Option<&str>) -> bool {
    let Some(text) = user_text else {
        return true;
    };
    value(text, "Hidden") != Some("true")
        && value(text, "X-GNOME-Autostart-enabled") != Some("false")
}

/// Sets `key=new` in the `[Desktop Entry]` group (replacing every earlier line of that key), or
/// adds it at the end of the group. `new` of `None` removes the key.
fn edit(text: &str, key: &str, new: Option<&str>) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut inside = false;
    let mut seen_group = false;
    let mut placed = false;
    // Index in `out` just after the group's last non-blank line.
    let mut group_end = 0;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            inside = trimmed == HEADER;
            seen_group |= inside;
        } else if inside
            && trimmed
                .split_once('=')
                .is_some_and(|(k, _)| k.trim() == key)
        {
            if let Some(new) = new
                && !placed
            {
                out.push(format!("{key}={new}"));
                placed = true;
                group_end = out.len();
            }
            continue;
        }
        out.push(line.to_string());
        if inside && !trimmed.is_empty() {
            group_end = out.len();
        }
    }
    if let Some(new) = new
        && !placed
    {
        if seen_group {
            out.insert(group_end, format!("{key}={new}"));
        } else {
            out.insert(0, HEADER.to_string());
            out.insert(1, format!("{key}={new}"));
        }
    }
    let mut result = out.join("\n");
    result.push('\n');
    result
}

/// A name no other write (this process or another) is using.
fn unique_suffix() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    format!(
        "{}.{}.{nanos:x}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

/// Where a write really goes: the file itself, or what a symlink (dotfile managers) points
/// at, so the link stays. A link that points nowhere is refused rather than replaced.
fn resolve(path: &Path) -> io::Result<PathBuf> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => std::fs::canonicalize(path).map_err(|e| {
            io::Error::new(
                e.kind(),
                format!("{} is a symlink that points nowhere: {e}", path.display()),
            )
        }),
        _ => Ok(path.to_path_buf()),
    }
}

/// A temporary file (mode 0644) beside the target, renamed over it; a symlink target is
/// written through, not replaced.
fn write_atomic(path: &Path, text: &str) -> io::Result<()> {
    let path = resolve(path)?;
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let temp = dir.join(format!(".{FILE}.{}.tmp", unique_suffix()));
    let written = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // Whatever the umask says, desktops read this file as any user.
            file.set_permissions(std::fs::Permissions::from_mode(0o644))?;
        }
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&temp, &path)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    written
}

/// The user's file, if there is one.
pub fn read(config_home: &Path) -> io::Result<Option<String>> {
    match std::fs::read_to_string(user_file(config_home)) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// Starting at login as the files say it is now.
pub fn current(config_home: &Path) -> io::Result<bool> {
    Ok(is_enabled(read(config_home)?.as_deref()))
}

/// Turns starting at login on or off.
pub fn set(config_home: &Path, enabled: bool) -> io::Result<()> {
    let path = user_file(config_home);
    let existing = read(config_home)?;
    match (enabled, existing) {
        (false, None) => write_atomic(&path, override_text()),
        (false, Some(text)) => write_atomic(&path, &edit(&text, "Hidden", Some("true"))),
        (true, None) => Ok(()),
        (true, Some(text)) if text.lines().any(|l| l.trim() == MARKER) => {
            // The file may already be gone (another instance, the user): same result.
            match std::fs::remove_file(&path) {
                Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
                _ => Ok(()),
            }
        }
        (true, Some(text)) => {
            let text = edit(&text, "Hidden", Some("false"));
            write_atomic(&path, &edit(&text, "X-GNOME-Autostart-enabled", None))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disable_writes_hidden_override() {
        let dir = tempfile::tempdir().unwrap();
        set(dir.path(), false).unwrap();
        let text = std::fs::read_to_string(user_file(dir.path())).unwrap();
        assert_eq!(text, override_text());
        assert!(!is_enabled(Some(&text)));
        assert!(!current(dir.path()).unwrap());
        // Nothing but the file is left in the directory.
        let names: Vec<_> = std::fs::read_dir(dir.path().join("autostart"))
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, [FILE]);
    }

    #[test]
    fn enable_removes_only_our_override() {
        let dir = tempfile::tempdir().unwrap();
        set(dir.path(), false).unwrap();
        set(dir.path(), true).unwrap();
        assert!(!user_file(dir.path()).exists());
        assert!(is_enabled(None));
        assert!(current(dir.path()).unwrap());
        // Already on and no file: nothing to do.
        set(dir.path(), true).unwrap();
    }

    #[test]
    fn user_file_is_edited_not_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let path = user_file(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "[Desktop Entry]\nType=Application\nExec=cellglide-gui --background --foo\n\n[Desktop Action x]\nName=x\n",
        )
        .unwrap();
        set(dir.path(), false).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("Exec=cellglide-gui --background --foo"));
        assert!(text.contains("Hidden=true"));
        assert!(!is_enabled(Some(&text)));
        // Added inside the entry's own group, not the action group after it.
        assert!(text.find("Hidden=true").unwrap() < text.find("[Desktop Action x]").unwrap());
        set(dir.path(), true).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("Hidden=false"));
        assert!(!text.contains("Hidden=true"));
        assert!(text.contains("Exec=cellglide-gui --background --foo"));
        assert!(is_enabled(Some(&text)));
    }

    #[test]
    fn turning_back_on_clears_the_gnome_off_key_too() {
        let dir = tempfile::tempdir().unwrap();
        let path = user_file(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "[Desktop Entry]\nExec=x\nHidden=true\nX-GNOME-Autostart-enabled=false\n",
        )
        .unwrap();
        set(dir.path(), true).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("Exec=x") && text.contains("Hidden=false"));
        assert!(!text.contains("X-GNOME-Autostart-enabled"));
        assert!(current(dir.path()).unwrap());
    }

    #[test]
    fn gnome_disabled_key_counts_as_off() {
        assert!(!is_enabled(Some(
            "[Desktop Entry]\nX-GNOME-Autostart-enabled=false\n"
        )));
        assert!(is_enabled(Some("[Desktop Entry]\nExec=x\n")));
        // A key in another group means nothing.
        assert!(is_enabled(Some("[Desktop Entry]\n[Other]\nHidden=true\n")));
    }

    #[cfg(unix)]
    #[test]
    fn the_file_is_world_readable_whatever_the_umask() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        set(dir.path(), false).unwrap();
        let mode = std::fs::metadata(user_file(dir.path()))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o644);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_file_is_written_through_and_stays_a_link() {
        let dir = tempfile::tempdir().unwrap();
        let managed = dir.path().join("dotfiles/cellglide-gui.desktop");
        std::fs::create_dir_all(managed.parent().unwrap()).unwrap();
        std::fs::write(&managed, "[Desktop Entry]\nExec=mine\n").unwrap();
        let home = dir.path().join("config");
        let link = user_file(&home);
        std::fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&managed, &link).unwrap();

        set(&home, false).unwrap();
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        let text = std::fs::read_to_string(&managed).unwrap();
        assert!(text.contains("Exec=mine") && text.contains("Hidden=true"));
        assert!(!current(&home).unwrap());
        set(&home, true).unwrap();
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(
            std::fs::read_to_string(&managed)
                .unwrap()
                .contains("Hidden=false")
        );
        assert!(current(&home).unwrap());
        // Nothing stray beside the managed file.
        assert_eq!(
            std::fs::read_dir(managed.parent().unwrap())
                .unwrap()
                .count(),
            1
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_dangling_symlink_is_refused_not_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let link = user_file(dir.path());
        std::fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(dir.path().join("nowhere"), &link).unwrap();
        // Reading a dangling link finds no file; writing must not replace the link.
        let error = set(dir.path(), false).unwrap_err();
        assert!(error.to_string().contains("points nowhere"), "{error}");
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[test]
    fn enabling_when_the_override_vanished_meanwhile_is_fine() {
        let dir = tempfile::tempdir().unwrap();
        assert!(set(dir.path(), true).is_ok());
    }

    #[test]
    fn temporary_names_are_unique() {
        assert_ne!(unique_suffix(), unique_suffix());
    }

    #[test]
    fn an_unreadable_location_is_an_error_not_a_guess() {
        let dir = tempfile::tempdir().unwrap();
        // `autostart` is a file, so nothing can be created below it.
        std::fs::write(dir.path().join("autostart"), b"x").unwrap();
        assert!(set(dir.path(), false).is_err());
    }
}
