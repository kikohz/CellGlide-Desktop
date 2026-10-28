//! `cellglide-service resume-check`: run as root by the systemd-sleep hook after resume.
//! On resume the kernel resets the iPhone's USB device instead of re-enumerating it, and
//! usbmuxd 1.1.1 then drops the phone and never re-adds it. If the phone is in sysfs but
//! usbmuxd does not list it, restart usbmuxd. Every error is logged and the exit is 0, so
//! the hook can never fail a resume.

use cg_core::mux::{self, MuxEndpoint};
use std::path::Path;
use std::time::Duration;

const APPLE_VENDOR: &str = "05ac";
/// How long usbmuxd gets to list the phone, and how often it is asked.
pub const DEADLINE: Duration = Duration::from_secs(10);
pub const INTERVAL: Duration = Duration::from_secs(1);

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// No iPhone with a usbmux interface in sysfs; nothing to do.
    NoPhone,
    /// usbmuxd lists a device.
    Listed,
    /// usbmuxd was restarted (or the restart was attempted).
    Restarted,
    /// usbmuxd could not be reached; its socket activation or udev handles that.
    Unreachable,
}

fn read_trimmed(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_ascii_lowercase())
}

/// An Apple USB device that has the usbmux interface (class ff, subclass fe, protocol 02).
pub fn phone_in_sysfs(root: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(root.join("sys/bus/usb/devices")) else {
        return false;
    };
    entries.flatten().any(|entry| {
        let dir = entry.path();
        if read_trimmed(&dir.join("idVendor")).as_deref() != Some(APPLE_VENDOR) {
            return false;
        }
        // The kernel lists interfaces as siblings named "<device>:<config>.<interface>".
        let prefix = format!("{}:", entry.file_name().to_string_lossy());
        std::fs::read_dir(root.join("sys/bus/usb/devices"))
            .into_iter()
            .flatten()
            .flatten()
            .filter(|i| i.file_name().to_string_lossy().starts_with(&prefix))
            .chain(
                // Also accept interface directories nested inside the device directory.
                std::fs::read_dir(&dir)
                    .into_iter()
                    .flatten()
                    .flatten()
                    .filter(|i| i.file_name().to_string_lossy().starts_with(&prefix)),
            )
            .any(|i| {
                let p = i.path();
                read_trimmed(&p.join("bInterfaceClass")).as_deref() == Some("ff")
                    && read_trimmed(&p.join("bInterfaceSubClass")).as_deref() == Some("fe")
                    && read_trimmed(&p.join("bInterfaceProtocol")).as_deref() == Some("02")
            })
    })
}

/// The decision, with the endpoint, timing and restart injected.
pub async fn run(
    root: &Path,
    endpoint: &MuxEndpoint,
    deadline: Duration,
    interval: Duration,
    restart: impl FnOnce() -> anyhow::Result<()>,
) -> Outcome {
    if !phone_in_sysfs(root) {
        return Outcome::NoPhone;
    }
    let start = tokio::time::Instant::now();
    let reachable = loop {
        let reachable = match mux::list_devices(endpoint).await {
            // Any listed device means usbmuxd works, so it is never restarted then, even if
            // the one it lost is a second iPhone next to an iPad or another phone: restarting
            // would cut the devices it still has. That lost phone stays lost until replugged.
            Ok(devices) if !devices.is_empty() => return Outcome::Listed,
            Ok(_) => true,
            Err(error) => {
                eprintln!("resume-check: usbmuxd: {error}");
                false
            }
        };
        if start.elapsed() >= deadline {
            break reachable;
        }
        tokio::time::sleep(interval).await;
    };
    if !reachable {
        return Outcome::Unreachable;
    }
    eprintln!("resume-check: the iPhone is in sysfs but usbmuxd lost it; restarting usbmuxd");
    if let Err(error) = restart() {
        eprintln!("resume-check: restarting usbmuxd failed: {error}");
    }
    Outcome::Restarted
}

fn systemctl_restart() -> anyhow::Result<()> {
    let status = std::process::Command::new("systemctl")
        .args(["try-restart", "usbmuxd.service"])
        .status()?;
    anyhow::ensure!(status.success(), "systemctl exited with {status}");
    Ok(())
}

/// Entry point; always returns normally so the process exits 0.
pub async fn main() {
    let outcome = run(
        Path::new("/"),
        &MuxEndpoint::system(),
        DEADLINE,
        INTERVAL,
        systemctl_restart,
    )
    .await;
    eprintln!("resume-check: {outcome:?}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use cg_core::testing::{self, FakeMux};
    use std::cell::Cell;
    use std::fs;

    fn write(dir: &Path, name: &str, value: &str) {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join(name), format!("{value}\n")).unwrap();
    }

    fn usb_device(root: &Path, name: &str, vendor: &str) {
        write(
            &root.join("sys/bus/usb/devices").join(name),
            "idVendor",
            vendor,
        );
    }

    fn interface(root: &Path, name: &str, class: &str, sub: &str, proto: &str) {
        let dir = root.join("sys/bus/usb/devices").join(name);
        write(&dir, "bInterfaceClass", class);
        write(&dir, "bInterfaceSubClass", sub);
        write(&dir, "bInterfaceProtocol", proto);
    }

    fn phone_root() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        usb_device(root.path(), "1-2", "05ac");
        interface(root.path(), "1-2:1.0", "06", "01", "01");
        interface(root.path(), "1-2:4.2", "ff", "fe", "02");
        root
    }

    const FAST: Duration = Duration::from_millis(5);
    const SHORT: Duration = Duration::from_millis(40);

    #[test]
    fn finds_a_phone_with_the_usbmux_interface() {
        assert!(phone_in_sysfs(phone_root().path()));
    }

    #[test]
    fn ignores_a_non_apple_device() {
        let root = tempfile::tempdir().unwrap();
        usb_device(root.path(), "1-3", "046d");
        interface(root.path(), "1-3:1.0", "ff", "fe", "02");
        assert!(!phone_in_sysfs(root.path()));
    }

    #[test]
    fn ignores_an_apple_device_without_the_usbmux_interface() {
        let root = tempfile::tempdir().unwrap();
        usb_device(root.path(), "1-4", "05AC");
        interface(root.path(), "1-4:1.0", "03", "01", "02");
        interface(root.path(), "1-4:1.1", "ff", "fe", "01");
        assert!(!phone_in_sysfs(root.path()));
        assert!(!phone_in_sysfs(Path::new("/nonexistent")));
    }

    #[tokio::test]
    async fn no_phone_in_sysfs_does_nothing() {
        let root = tempfile::tempdir().unwrap();
        let mux = FakeMux::start().await;
        let restarted = Cell::new(false);
        let outcome = run(root.path(), &mux.endpoint(), SHORT, FAST, || {
            restarted.set(true);
            Ok(())
        })
        .await;
        assert_eq!(outcome, Outcome::NoPhone);
        assert!(!restarted.get());
    }

    #[tokio::test]
    async fn listed_device_means_no_restart() {
        let root = phone_root();
        let mux = FakeMux::start().await;
        mux.attach(testing::device("00008140-000000000000001C", 1));
        let restarted = Cell::new(false);
        let outcome = run(root.path(), &mux.endpoint(), SHORT, FAST, || {
            restarted.set(true);
            Ok(())
        })
        .await;
        assert_eq!(outcome, Outcome::Listed);
        assert!(!restarted.get());
    }

    #[tokio::test]
    async fn empty_list_until_the_deadline_restarts_once() {
        let root = phone_root();
        let mux = FakeMux::start().await;
        let restarts = Cell::new(0);
        let outcome = run(root.path(), &mux.endpoint(), SHORT, FAST, || {
            restarts.set(restarts.get() + 1);
            Ok(())
        })
        .await;
        assert_eq!(outcome, Outcome::Restarted);
        assert_eq!(restarts.get(), 1);
    }

    #[tokio::test]
    async fn a_failing_restart_is_not_fatal() {
        let root = phone_root();
        let mux = FakeMux::start().await;
        let outcome = run(root.path(), &mux.endpoint(), SHORT, FAST, || {
            anyhow::bail!("no systemctl")
        })
        .await;
        assert_eq!(outcome, Outcome::Restarted);
    }

    #[tokio::test]
    async fn unreachable_usbmuxd_is_not_restarted() {
        let root = phone_root();
        let gone = tempfile::tempdir().unwrap();
        let endpoint = MuxEndpoint::Unix(gone.path().join("usbmuxd"));
        let restarted = Cell::new(false);
        let outcome = run(root.path(), &endpoint, SHORT, FAST, || {
            restarted.set(true);
            Ok(())
        })
        .await;
        assert_eq!(outcome, Outcome::Unreachable);
        assert!(!restarted.get());
    }

    #[tokio::test]
    async fn a_phone_that_shows_up_late_is_found() {
        let root = phone_root();
        let mux = FakeMux::start().await;
        let endpoint = mux.endpoint();
        let restarted = Cell::new(false);
        let late = async {
            tokio::time::sleep(Duration::from_millis(30)).await;
            mux.attach(testing::device("00008140-000000000000001C", 1));
        };
        let (outcome, ()) = tokio::join!(
            run(
                root.path(),
                &endpoint,
                Duration::from_millis(500),
                FAST,
                || {
                    restarted.set(true);
                    Ok(())
                }
            ),
            late
        );
        assert_eq!(outcome, Outcome::Listed);
        assert!(!restarted.get());
    }

    #[test]
    fn hook_runs_the_check_sandboxed_and_dev_installs_it() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let hook = fs::read_to_string(root.join("packaging/system-sleep/cellglide")).unwrap();
        for property in [
            "NoNewPrivileges=yes",
            "ProtectSystem=strict",
            "ProtectHome=yes",
            "PrivateTmp=yes",
        ] {
            assert!(hook.contains(&format!("-p {property}")), "{property}");
        }
        assert!(hook.contains("--collect") && hook.contains("/usr/bin/cellglide-service"));
        // The dev install points the hook at its own binary and removes it on --uninstall.
        let script = fs::read_to_string(root.join("scripts/install-dev.sh")).unwrap();
        assert!(script.contains(
            "sed \"s#/usr/bin/cellglide-service#$BIN/cellglide-service#\" packaging/system-sleep/cellglide"
        ));
        assert_eq!(
            script
                .matches("/usr/lib/systemd/system-sleep/cellglide")
                .count(),
            3,
            "installed, removed by --uninstall, and named in its manual hint"
        );
    }

    #[test]
    fn hook_is_packaged_in_deb_and_rpm_with_mode_755() {
        let manifest = include_str!("../Cargo.toml");
        let deb = manifest
            .lines()
            .find(|l| l.contains("usr/lib/systemd/system-sleep/cellglide"))
            .expect("deb asset");
        assert!(deb.contains("packaging/system-sleep/cellglide") && deb.contains("\"755\""));
        let rpm = manifest
            .lines()
            .find(|l| l.contains("\"/usr/lib/systemd/system-sleep/cellglide\""))
            .expect("rpm asset");
        assert!(rpm.contains("packaging/system-sleep/cellglide") && rpm.contains("mode = \"755\""));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let hook = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../packaging/system-sleep/cellglide");
            assert_eq!(
                fs::metadata(hook).unwrap().permissions().mode() & 0o777,
                0o755
            );
        }
    }
}
