//! The Windows environment checks (design §6.4), run by the service as LocalSystem; clients
//! render them with cg-i18n. In order: Apple's device service, usbmuxd on port 27015, the
//! phone, trust, `wintun.dll`, and the resolver's DNS port.

use super::system::{self, HKEY_LOCAL_MACHINE};
use cg_core::mux::{self, MuxEndpoint};
use cg_ipc::{Check, CheckId};
use std::net::Ipv4Addr;
use std::path::Path;
use std::time::Duration;

/// iTunes (and Apple Mobile Device Support) installs this service.
const CLASSIC_SERVICE: &str = r"SYSTEM\CurrentControlSet\Services\Apple Mobile Device Service";
/// Every installed packaged app has a key here, machine-wide, while any user has it.
const PACKAGES: &str = r"SOFTWARE\Classes\Local Settings\Software\Microsoft\Windows\CurrentVersion\AppModel\PackageRepository\Packages";
/// The Microsoft Store's Apple Devices app: `<name>_<version>_<arch>_<resource>_<publisher>`.
const STORE_NAME: &str = "AppleInc.AppleDevices_";
const STORE_PUBLISHER: &str = "_nzyj5cx40ttqa";
/// How long usbmuxd may take to accept or answer before it counts as not running.
const MUX_TIMEOUT: Duration = Duration::from_secs(3);

pub async fn checks(endpoint: &MuxEndpoint, install_dir: &Path) -> Vec<Check> {
    let packages = system::reg_subkeys(HKEY_LOCAL_MACHINE, PACKAGES);
    let classic = system::reg_key_exists(HKEY_LOCAL_MACHINE, CLASSIC_SERVICE).then(|| {
        system::reg_string(HKEY_LOCAL_MACHINE, CLASSIC_SERVICE, "ImagePath").unwrap_or_default()
    });
    let kind = apple_kind(classic.as_deref(), &packages);

    // Before anyone signs in (the Store app runs in the user's session), nothing answers:
    // expected, and said as "installed, not running yet".
    let running = tokio::time::timeout(MUX_TIMEOUT, endpoint.open())
        .await
        .is_ok_and(|opened| opened.is_ok());
    let apple = match kind {
        Some(kind) => Check::new(CheckId::AppleService, true)
            .with("kind", kind)
            .with("running", running.to_string()),
        None => Check::new(CheckId::AppleService, false).with("running", running.to_string()),
    };

    let listed = tokio::time::timeout(MUX_TIMEOUT, mux::list_devices(endpoint))
        .await
        .ok()
        .and_then(Result::ok);
    let usbmuxd = match &listed {
        Some(_) => Check::new(CheckId::Usbmuxd, true).with("running", "true"),
        None if kind.is_some() => Check::new(CheckId::Usbmuxd, true).with("running", "false"),
        None => Check::new(CheckId::Usbmuxd, false),
    };
    let serials = listed.map(|devices| devices.into_iter().map(|d| d.serial).collect::<Vec<_>>());

    let mut result = vec![apple, usbmuxd];
    result.extend(crate::doctor::phone_and_trust(endpoint, serials.as_deref()).await);
    result.push(wintun(install_dir));
    result.push(dns());
    result
}

/// Which Apple package provides usbmuxd: the classic service (`store` when its program
/// lives under WindowsApps, as the Store's iTunes installs it, else `itunes`), or else the
/// Store's Apple Devices app, which has no service of its own.
fn apple_kind(classic_image: Option<&str>, packages: &[String]) -> Option<&'static str> {
    if let Some(image) = classic_image {
        return Some(if image.to_ascii_lowercase().contains("windowsapps") {
            "store"
        } else {
            "itunes"
        });
    }
    packages
        .iter()
        .any(|p| p.starts_with(STORE_NAME) && p.ends_with(STORE_PUBLISHER))
        .then_some("store")
}

/// `wintun.dll` beside the service, with a valid Authenticode signature.
fn wintun(install_dir: &Path) -> Check {
    let dll = install_dir.join("wintun.dll");
    let ok = dll.is_file() && signature_is_valid(&dll);
    Check::new(CheckId::Wintun, ok)
}

/// WinVerifyTrust's verdict on a file's embedded signature. Revocation isn't checked, so
/// an offline computer gets the same answer.
fn signature_is_valid(path: &Path) -> bool {
    use windows::Win32::Foundation::{HANDLE, HWND};
    use windows::Win32::Security::WinTrust::{
        WINTRUST_ACTION_GENERIC_VERIFY_V2, WINTRUST_DATA, WINTRUST_DATA_0, WINTRUST_FILE_INFO,
        WTD_CHOICE_FILE, WTD_REVOKE_NONE, WTD_STATEACTION_CLOSE, WTD_STATEACTION_VERIFY,
        WTD_UI_NONE, WinVerifyTrust,
    };
    use windows::core::{GUID, HSTRING, PCWSTR};

    let wide = HSTRING::from(path.as_os_str());
    let mut file = WINTRUST_FILE_INFO {
        cbStruct: size_of::<WINTRUST_FILE_INFO>() as u32,
        pcwszFilePath: PCWSTR(wide.as_ptr()),
        hFile: HANDLE::default(),
        pgKnownSubject: std::ptr::null_mut(),
    };
    let mut data = WINTRUST_DATA {
        cbStruct: size_of::<WINTRUST_DATA>() as u32,
        dwUIChoice: WTD_UI_NONE,
        fdwRevocationChecks: WTD_REVOKE_NONE,
        dwUnionChoice: WTD_CHOICE_FILE,
        Anonymous: WINTRUST_DATA_0 { pFile: &mut file },
        dwStateAction: WTD_STATEACTION_VERIFY,
        ..Default::default()
    };
    let mut action: GUID = WINTRUST_ACTION_GENERIC_VERIFY_V2;
    // SAFETY: `data` and the file info and path it points to outlive both calls; the
    // state opened by VERIFY is released by CLOSE.
    let verdict = unsafe { WinVerifyTrust(HWND::default(), &mut action, (&raw mut data).cast()) };
    data.dwStateAction = WTD_STATEACTION_CLOSE;
    // SAFETY: as above.
    let _ = unsafe { WinVerifyTrust(HWND::default(), &mut action, (&raw mut data).cast()) };
    verdict == 0
}

/// The resolver's port, `198.18.0.1:53` (UDP), free for us: nothing else bound to it or to
/// all addresses on port 53. While connected our own resolver holds it, which passes.
fn dns() -> Check {
    let me = std::process::id();
    let others = conflicting_owners(&udp_port_53(), me);
    match others.first() {
        None => Check::new(CheckId::Dns, true),
        Some(&pid) => {
            let owner = system::process_name(pid).unwrap_or_else(|| format!("PID {pid}"));
            Check::new(CheckId::Dns, false).with("owner", owner)
        }
    }
}

/// The processes other than `me` whose UDP sockets would collide with the resolver's.
/// A socket on another specific address (ICS on its own subnet) doesn't.
fn conflicting_owners(sockets: &[(Ipv4Addr, u32)], me: u32) -> Vec<u32> {
    let resolver = match cg_core::tunnel::WINDOWS_RESOLVER.ip() {
        std::net::IpAddr::V4(ip) => ip,
        std::net::IpAddr::V6(_) => Ipv4Addr::UNSPECIFIED,
    };
    let mut owners: Vec<u32> = sockets
        .iter()
        .filter(|(address, pid)| *pid != me && (*address == resolver || address.is_unspecified()))
        .map(|&(_, pid)| pid)
        .collect();
    owners.dedup();
    owners
}

/// The IPv4 UDP sockets bound to port 53, with their owning process.
fn udp_port_53() -> Vec<(Ipv4Addr, u32)> {
    use windows::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, NO_ERROR};
    use windows::Win32::NetworkManagement::IpHelper::{
        GetExtendedUdpTable, MIB_UDPROW_OWNER_PID, MIB_UDPTABLE_OWNER_PID, UDP_TABLE_OWNER_PID,
    };
    use windows::Win32::Networking::WinSock::AF_INET;

    let mut size = 0u32;
    let mut buffer: Vec<u64> = Vec::new();
    // The table can grow between the size query and the read: try a few times.
    for _ in 0..4 {
        // SAFETY: `buffer` holds `size` bytes (none on the first, sizing, call).
        let result = unsafe {
            GetExtendedUdpTable(
                (!buffer.is_empty()).then(|| buffer.as_mut_ptr().cast()),
                &mut size,
                false,
                u32::from(AF_INET.0),
                UDP_TABLE_OWNER_PID,
                0,
            )
        };
        if result == NO_ERROR.0 && !buffer.is_empty() {
            // SAFETY: on success the buffer starts with a MIB_UDPTABLE_OWNER_PID whose
            // dwNumEntries rows follow it within `size` bytes; u64 alignment suffices.
            let rows = unsafe {
                let table = &*buffer.as_ptr().cast::<MIB_UDPTABLE_OWNER_PID>();
                std::slice::from_raw_parts(table.table.as_ptr(), table.dwNumEntries as usize)
            };
            return rows
                .iter()
                .filter(|row: &&MIB_UDPROW_OWNER_PID| u16::from_be(row.dwLocalPort as u16) == 53)
                .map(|row| {
                    (
                        Ipv4Addr::from(u32::from_be(row.dwLocalAddr)),
                        row.dwOwningPid,
                    )
                })
                .collect();
        }
        if result != ERROR_INSUFFICIENT_BUFFER.0 && result != NO_ERROR.0 {
            tracing::debug!("GetExtendedUdpTable failed: {result}");
            return Vec::new();
        }
        buffer = vec![0u64; (size as usize).div_ceil(8) + 1];
        size = (buffer.len() * 8) as u32;
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use cg_core::testing::{FakeMux, device};

    #[tokio::test]
    async fn wintun_check_fails_without_dll() {
        let dir = tempfile::tempdir().unwrap();
        let mux = FakeMux::start().await;
        let checks = checks(&mux.endpoint(), dir.path()).await;
        let wintun = checks.iter().find(|c| c.id == CheckId::Wintun).unwrap();
        assert!(!wintun.ok);
        // A file that isn't signed doesn't pass either.
        std::fs::write(dir.path().join("wintun.dll"), b"MZ not really").unwrap();
        assert!(!super::wintun(dir.path()).ok);
    }

    #[tokio::test]
    async fn checks_come_in_order() {
        let mux = FakeMux::start().await;
        mux.attach(device("aaaa", 1));
        mux.add_pair_record("aaaa");
        let dir = tempfile::tempdir().unwrap();
        let checks = checks(&mux.endpoint(), dir.path()).await;
        let ids: Vec<_> = checks.iter().map(|c| c.id).collect();
        assert_eq!(
            ids,
            [
                CheckId::AppleService,
                CheckId::Usbmuxd,
                CheckId::Phone,
                CheckId::Trust,
                CheckId::Wintun,
                CheckId::Dns
            ]
        );
        // The fake answers: usbmuxd is running, one trusted phone.
        assert_eq!(checks[0].args["running"], "true");
        assert!(checks[1].ok && checks[1].args["running"] == "true");
        assert!(checks[2].ok && checks[2].args["count"] == "1");
        assert!(checks[3].ok);
    }

    #[tokio::test]
    async fn nothing_on_the_port_is_not_running() {
        // A port nothing listens on: bind one, note it, let it go.
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let gone = MuxEndpoint::Tcp(([127, 0, 0, 1], port).into());
        let dir = tempfile::tempdir().unwrap();
        let checks = checks(&gone, dir.path()).await;
        assert_eq!(checks[0].args["running"], "false");
        // Installed or not, usbmuxd's check follows the Apple check: idle when installed.
        assert_eq!(checks[1].ok, checks[0].ok);
        assert_eq!(checks[2].id, CheckId::Phone);
        assert!(!checks[2].ok);
        assert_eq!(checks.len(), 5, "no trust check without a phone");
    }

    #[test]
    fn apple_kind_from_the_service_or_the_store_package() {
        let store = [
            "Microsoft.WindowsCalculator_11.2405.2.0_x64__8wekyb3d8bbwe".to_string(),
            "AppleInc.AppleDevices_1.1792.39681.0_x64__nzyj5cx40ttqa".to_string(),
        ];
        assert_eq!(apple_kind(None, &store), Some("store"));
        assert_eq!(apple_kind(None, &store[..1]), None);
        assert_eq!(apple_kind(None, &[]), None);
        // A look-alike from another publisher isn't Apple's.
        assert_eq!(
            apple_kind(
                None,
                &["AppleInc.AppleDevices_1.0_x64__0000000000000".into()]
            ),
            None
        );
        assert_eq!(
            apple_kind(
                Some(
                    r#""C:\Program Files\Common Files\Apple\Mobile Device Support\AppleMobileDeviceService.exe""#
                ),
                &[]
            ),
            Some("itunes")
        );
        assert_eq!(
            apple_kind(
                Some(
                    r"C:\Program Files\WindowsApps\AppleInc.iTunes_12.13_x64__nzyj5cx40ttqa\AMDS\AppleMobileDeviceService.exe"
                ),
                &store
            ),
            Some("store")
        );
    }

    #[test]
    fn only_sockets_on_the_resolver_or_every_address_conflict() {
        let resolver = Ipv4Addr::new(198, 18, 0, 1);
        let ics = Ipv4Addr::new(192, 168, 137, 1);
        let sockets = [
            (ics, 100),
            (Ipv4Addr::UNSPECIFIED, 200),
            (resolver, 300),
            (resolver, 42),
        ];
        assert_eq!(conflicting_owners(&sockets, 42), [200, 300]);
        assert!(conflicting_owners(&[(ics, 100), (resolver, 42)], 42).is_empty());
    }

    #[test]
    fn a_signed_system_file_passes_and_the_udp_table_reads() {
        let system = std::path::PathBuf::from(std::env::var("SystemRoot").unwrap());
        // Catalog-signed system files have no embedded signature; ntoskrnl's is embedded.
        assert!(signature_is_valid(&system.join(r"System32\ntoskrnl.exe")));
        assert!(!signature_is_valid(&system.join("no-such-file.dll")));
        // Whatever is on port 53 here, the table reads without failing.
        let _ = udp_port_53();
        assert_eq!(dns().id, CheckId::Dns);
    }
}
