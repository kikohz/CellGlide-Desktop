//! Environment checks run by the service (as root); clients render them with cg-i18n.

use cg_core::mux::{self, MuxEndpoint};
use cg_ipc::{Check, CheckId};
use std::path::Path;

pub async fn checks(endpoint: &MuxEndpoint, root: &Path) -> Vec<Check> {
    let listed = mux::list_devices(endpoint).await;
    let installed = ["usr/sbin/usbmuxd", "usr/bin/usbmuxd"]
        .iter()
        .any(|p| root.join(p).exists());
    let usbmuxd = match &listed {
        Ok(_) => Check::new(CheckId::Usbmuxd, true).with("running", "true"),
        Err(_) if installed => Check::new(CheckId::Usbmuxd, true).with("running", "false"),
        Err(_) => Check::new(CheckId::Usbmuxd, false),
    };
    let mut result = vec![usbmuxd];
    match &listed {
        Ok(devices) if !devices.is_empty() => {
            result.push(Check::new(CheckId::Phone, true).with("count", devices.len().to_string()));
            let mut untrusted = Vec::new();
            for device in devices {
                if !mux::has_pair_record(endpoint, &device.serial)
                    .await
                    .unwrap_or(false)
                {
                    untrusted.push(device.serial.clone());
                }
            }
            result.push(if untrusted.is_empty() {
                Check::new(CheckId::Trust, true)
            } else {
                Check::new(CheckId::Trust, false).with("serials", untrusted.join(", "))
            });
        }
        _ => result.push(Check::new(CheckId::Phone, false)),
    }
    result.push(Check::new(
        CheckId::Resolved,
        root.join("run/systemd/resolve/stub-resolv.conf").exists(),
    ));
    // Without polkit (and our rule) resolved refuses a non-root caller; the tunnel still works.
    let polkit = [
        "usr/bin/pkaction",
        "usr/lib/polkit-1/polkitd",
        "usr/lib/policykit-1/polkitd",
    ];
    result.push(Check::new(CheckId::Polkit, any_present(root, &polkit)));
    let rule = [
        "usr/share/polkit-1/rules.d/50-cellglide.rules",
        "var/lib/polkit-1/localauthority/10-vendor.d/cellglide.pkla",
        "etc/polkit-1/rules.d/50-cellglide.rules",
        "etc/polkit-1/localauthority/50-local.d/cellglide.pkla",
    ];
    result.push(Check::new(CheckId::PolkitRule, any_present(root, &rule)));
    result.push(Check::new(CheckId::Tun, root.join("dev/net/tun").exists()));
    result
}

/// True when a path exists, or when one can't be looked at: polkit's directories are not
/// readable by the service user (e.g. /etc/polkit-1/rules.d is root:polkitd 0750), and an
/// unknown answer must not show a false failure. Filesystem lookups only.
fn any_present(root: &Path, paths: &[&str]) -> bool {
    paths
        .iter()
        .any(|p| root.join(p).try_exists().unwrap_or(true))
}

#[cfg(test)]
mod tests {
    use super::*;
    use cg_core::testing::{FakeMux, device};

    fn root_with(files: &[&str]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for file in files {
            let path = dir.path().join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "").unwrap();
        }
        dir
    }

    #[tokio::test]
    async fn healthy_machine_passes_everything() {
        let mux = FakeMux::start().await;
        mux.attach(device("aaaa", 1));
        mux.add_pair_record("aaaa");
        let root = root_with(&[
            "usr/sbin/usbmuxd",
            "run/systemd/resolve/stub-resolv.conf",
            "usr/lib/polkit-1/polkitd",
            "usr/share/polkit-1/rules.d/50-cellglide.rules",
            "dev/net/tun",
        ]);
        let checks = checks(&mux.endpoint(), root.path()).await;
        let ids: Vec<_> = checks.iter().map(|c| c.id).collect();
        assert_eq!(
            ids,
            [
                CheckId::Usbmuxd,
                CheckId::Phone,
                CheckId::Trust,
                CheckId::Resolved,
                CheckId::Polkit,
                CheckId::PolkitRule,
                CheckId::Tun
            ]
        );
        assert!(checks.iter().all(|c| c.ok), "{checks:?}");
        assert_eq!(checks[0].args["running"], "true");
    }

    #[tokio::test]
    async fn untrusted_phone_and_missing_pieces_have_fixes() {
        let mux = FakeMux::start().await;
        mux.attach(device("aaaa", 1));
        let root = root_with(&[]);
        let checks = checks(&mux.endpoint(), root.path()).await;
        // usbmuxd answers (the fake), so only trust, resolved, polkit, the rule and tun fail.
        assert!(checks[0].ok);
        let failed: Vec<_> = checks.iter().filter(|c| !c.ok).collect();
        let failed_ids: Vec<_> = failed.iter().map(|c| c.id).collect();
        assert_eq!(
            failed_ids,
            [
                CheckId::Trust,
                CheckId::Resolved,
                CheckId::Polkit,
                CheckId::PolkitRule,
                CheckId::Tun
            ],
            "{checks:?}"
        );
        assert_eq!(failed[0].args["serials"], "aaaa");
    }

    fn check(checks: &[Check], id: CheckId) -> &Check {
        checks.iter().find(|c| c.id == id).unwrap()
    }

    #[tokio::test]
    async fn polkit_and_rule_are_found_in_every_known_place() {
        let gone = MuxEndpoint::Unix("/nonexistent/usbmuxd".into());
        // Ubuntu 22.04 (policykit-1), development installs (pkla and JS rule).
        for files in [
            [
                "usr/lib/policykit-1/polkitd",
                "var/lib/polkit-1/localauthority/10-vendor.d/cellglide.pkla",
            ],
            [
                "usr/bin/pkaction",
                "etc/polkit-1/rules.d/50-cellglide.rules",
            ],
            [
                "usr/lib/polkit-1/polkitd",
                "etc/polkit-1/localauthority/50-local.d/cellglide.pkla",
            ],
        ] {
            let root = root_with(&files);
            let checks = checks(&gone, root.path()).await;
            assert!(check(&checks, CheckId::Polkit).ok, "{files:?}");
            assert!(check(&checks, CheckId::PolkitRule).ok, "{files:?}");
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_unreadable_polkit_directory_is_not_reported_missing() {
        use std::os::unix::fs::PermissionsExt;
        if nix::unistd::geteuid().is_root() {
            return; // root reads through permissions
        }
        let gone = MuxEndpoint::Unix("/nonexistent/usbmuxd".into());
        let root = root_with(&["usr/bin/pkaction", "etc/polkit-1/rules.d/other.rules"]);
        let rules = root.path().join("etc/polkit-1/rules.d");
        std::fs::set_permissions(&rules, std::fs::Permissions::from_mode(0o000)).unwrap();
        let checks = checks(&gone, root.path()).await;
        std::fs::set_permissions(&rules, std::fs::Permissions::from_mode(0o755)).unwrap();
        // /etc/polkit-1/rules.d is root:polkitd 0750; the service user can't tell, so no false alarm.
        assert!(check(&checks, CheckId::PolkitRule).ok, "{checks:?}");
    }

    #[tokio::test]
    async fn no_phone_plugged_is_not_a_usbmuxd_failure() {
        let gone = MuxEndpoint::Unix("/nonexistent/usbmuxd".into());
        let root = root_with(&["usr/sbin/usbmuxd"]);
        let checks = checks(&gone, root.path()).await;
        assert!(
            checks[0].ok,
            "installed usbmuxd starts when a phone is plugged in"
        );
        assert_eq!(checks[0].args["running"], "false");
        assert_eq!(checks[1].id, CheckId::Phone);
        assert!(!checks[1].ok);
    }
}
