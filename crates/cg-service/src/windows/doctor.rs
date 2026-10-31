//! The Windows environment checks (design §6.4), run by the service as LocalSystem; clients
//! render them with cg-i18n. In order: Apple's device service, usbmuxd on port 27015, the
//! phone, trust, `wintun.dll`, and the resolver's DNS port.

use super::system::{self, HKEY_LOCAL_MACHINE};
use cg_core::mux::{self, MuxEndpoint};
use cg_ipc::{Check, CheckId};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
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

/// The resolver's port, `198.18.0.1:53`, free for us: no other process bound to it or to all
/// addresses (IPv4 or IPv6) on port 53, over UDP or listening over TCP. While connected our
/// own resolver holds it, which passes. A table that can't be read makes the check fail as
/// "couldn't check" rather than pass, unless another table already names an owner.
fn dns() -> Check {
    let (rows, unread) = bindings();
    dns_check(&port_53_owners(&rows, std::process::id()), &unread)
}

/// The check from the owners found and the tables that couldn't be read.
fn dns_check(owners: &[u32], unread: &[String]) -> Check {
    if let Some(&pid) = owners.first() {
        let owner = system::process_name(pid).unwrap_or_else(|| format!("PID {pid}"));
        return Check::new(CheckId::Dns, false).with("owner", owner);
    }
    if unread.is_empty() {
        return Check::new(CheckId::Dns, true);
    }
    let error = unread.join("; ");
    tracing::warn!("doctor: could not read the port tables: {error}");
    Check::new(CheckId::Dns, false)
        .with("cause", "unreadable")
        .with("error", error)
}

/// A local socket from the UDP table or a listener from the TCP table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Binding {
    pub tcp: bool,
    pub addr: IpAddr,
    pub port: u16,
    pub pid: u32,
}

/// The processes other than `own_pid` bound to port 53 on all addresses (`0.0.0.0`, `::`)
/// or on the resolver's, in table order without repeats. A socket on another specific
/// address (ICS on its own subnet, a loopback stub) doesn't collide with the resolver.
pub(crate) fn port_53_owners(rows: &[Binding], own_pid: u32) -> Vec<u32> {
    let resolver = cg_core::tunnel::WINDOWS_RESOLVER.ip();
    let mut owners = Vec::new();
    for row in rows {
        if row.port == 53
            && row.pid != own_pid
            && (row.addr.is_unspecified() || row.addr == resolver)
            && !owners.contains(&row.pid)
        {
            owners.push(row.pid);
        }
    }
    owners
}

/// Every UDP socket and TCP listener, IPv4 and IPv6, with its owning process, and why each
/// table that couldn't be read wasn't.
fn bindings() -> (Vec<Binding>, Vec<String>) {
    use windows::Win32::NetworkManagement::IpHelper::{
        GetExtendedTcpTable, GetExtendedUdpTable, MIB_TCP6ROW_OWNER_PID, MIB_TCPROW_OWNER_PID,
        MIB_UDP6ROW_OWNER_PID, MIB_UDPROW_OWNER_PID, TCP_TABLE_OWNER_PID_LISTENER,
        UDP_TABLE_OWNER_PID,
    };
    use windows::Win32::Networking::WinSock::{AF_INET, AF_INET6};

    let (v4, v6) = (u32::from(AF_INET.0), u32::from(AF_INET6.0));
    // Ports are in network order in the low 16 bits; IPv4 addresses in network order.
    let port = |raw: u32| u16::from_be(raw as u16);
    let v4_addr = |raw: u32| IpAddr::V4(Ipv4Addr::from(u32::from_be(raw)));
    let mut found = Vec::new();
    let mut unread = Vec::new();
    let mut add = |table: Result<Vec<Binding>, String>| match table {
        Ok(rows) => found.extend(rows),
        Err(error) => unread.push(error),
    };
    add(
        // SAFETY: `buffer` is None or holds `size` bytes (read_table's contract).
        read_table::<MIB_UDPROW_OWNER_PID>("UDP (IPv4)", |buffer, size| unsafe {
            GetExtendedUdpTable(buffer, size, false, v4, UDP_TABLE_OWNER_PID, 0)
        })
        .map(|rows| {
            rows.iter()
                .map(|row| Binding {
                    tcp: false,
                    addr: v4_addr(row.dwLocalAddr),
                    port: port(row.dwLocalPort),
                    pid: row.dwOwningPid,
                })
                .collect()
        }),
    );
    add(
        // SAFETY: as above.
        read_table::<MIB_UDP6ROW_OWNER_PID>("UDP (IPv6)", |buffer, size| unsafe {
            GetExtendedUdpTable(buffer, size, false, v6, UDP_TABLE_OWNER_PID, 0)
        })
        .map(|rows| {
            rows.iter()
                .map(|row| Binding {
                    tcp: false,
                    addr: IpAddr::V6(Ipv6Addr::from(row.ucLocalAddr)),
                    port: port(row.dwLocalPort),
                    pid: row.dwOwningPid,
                })
                .collect()
        }),
    );
    add(
        // SAFETY: as above.
        read_table::<MIB_TCPROW_OWNER_PID>("TCP (IPv4)", |buffer, size| unsafe {
            GetExtendedTcpTable(buffer, size, false, v4, TCP_TABLE_OWNER_PID_LISTENER, 0)
        })
        .map(|rows| {
            rows.iter()
                .map(|row| Binding {
                    tcp: true,
                    addr: v4_addr(row.dwLocalAddr),
                    port: port(row.dwLocalPort),
                    pid: row.dwOwningPid,
                })
                .collect()
        }),
    );
    add(
        // SAFETY: as above.
        read_table::<MIB_TCP6ROW_OWNER_PID>("TCP (IPv6)", |buffer, size| unsafe {
            GetExtendedTcpTable(buffer, size, false, v6, TCP_TABLE_OWNER_PID_LISTENER, 0)
        })
        .map(|rows| {
            rows.iter()
                .map(|row| Binding {
                    tcp: true,
                    addr: IpAddr::V6(Ipv6Addr::from(row.ucLocalAddr)),
                    port: port(row.dwLocalPort),
                    pid: row.dwOwningPid,
                })
                .collect()
        }),
    );
    (found, unread)
}

/// Where an IP Helper table's rows of type `R` start: every such table is a `u32`
/// `dwNumEntries` followed by the rows, padded to their alignment.
fn rows_offset<R>() -> usize {
    std::mem::size_of::<u32>().next_multiple_of(std::mem::align_of::<R>())
}

/// The rows of one IP Helper table. `fetch` fills a buffer of `size` bytes or, given none,
/// reports the size needed; it returns the Win32 error code. The table can grow between
/// the size query and the read, so this tries a few times. The row count is trusted only
/// as far as the bytes written reach.
fn read_table<R: Copy>(
    what: &str,
    fetch: impl Fn(Option<*mut std::ffi::c_void>, &mut u32) -> u32,
) -> Result<Vec<R>, String> {
    use windows::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, NO_ERROR};

    let mut size = 0u32;
    let mut buffer: Vec<u64> = Vec::new();
    for _ in 0..4 {
        let result = fetch(
            (!buffer.is_empty()).then(|| buffer.as_mut_ptr().cast()),
            &mut size,
        );
        if result == NO_ERROR.0 && !buffer.is_empty() {
            let bytes = (size as usize).min(buffer.len() * 8);
            let offset = rows_offset::<R>();
            if bytes < offset {
                return Err(format!("{what}: short table ({bytes} bytes)"));
            }
            let base = buffer.as_ptr().cast::<u8>();
            // SAFETY: the buffer holds at least `offset` >= 4 initialised bytes and is
            // u64-aligned, so the leading u32 count reads in bounds and aligned.
            let claimed = unsafe { base.cast::<u32>().read() } as usize;
            let fits = (bytes - offset) / std::mem::size_of::<R>();
            if claimed > fits {
                return Err(format!("{what}: {claimed} rows claimed, {fits} fit"));
            }
            // SAFETY: `claimed` rows of R start at `offset` (aligned: the buffer is
            // u64-aligned and `offset` is a multiple of R's alignment) and end within the
            // `bytes` the call wrote; the pointer derives from the whole buffer.
            let rows = unsafe { std::slice::from_raw_parts(base.add(offset).cast::<R>(), claimed) };
            return Ok(rows.to_vec());
        }
        if result != ERROR_INSUFFICIENT_BUFFER.0 && result != NO_ERROR.0 {
            return Err(format!("{what}: error {result}"));
        }
        buffer = vec![0u64; (size as usize).div_ceil(8) + 1];
        size = (buffer.len() * 8) as u32;
    }
    Err(format!("{what}: the table kept growing"))
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

    fn b(tcp: bool, addr: &str, port: u16, pid: u32) -> Binding {
        Binding {
            tcp,
            addr: addr.parse().unwrap(),
            port,
            pid,
        }
    }

    #[test]
    fn only_sockets_on_the_resolver_or_every_address_conflict() {
        let sockets = [
            b(false, "192.168.137.1", 53, 100),
            b(false, "0.0.0.0", 53, 200),
            b(false, "198.18.0.1", 53, 300),
            b(false, "198.18.0.1", 53, 42),
        ];
        assert_eq!(port_53_owners(&sockets, 42), [200, 300]);
        let ours = [
            b(false, "192.168.137.1", 53, 100),
            b(true, "::1", 53, 100),
            b(false, "198.18.0.1", 53, 42),
        ];
        assert!(port_53_owners(&ours, 42).is_empty());
    }

    #[test]
    fn owners_cover_tcp_and_ipv6_wildcards() {
        let rows = [
            b(false, "127.0.0.1", 53, 7),
            b(true, "0.0.0.0", 53, 8),
            b(false, "::", 53, 9),
            b(false, "198.18.0.1", 53, 1),
            b(true, "0.0.0.0", 80, 10),
            b(false, "::", 53, 9),
        ];
        assert_eq!(port_53_owners(&rows, 1), vec![8, 9]);
    }

    #[test]
    fn an_unreadable_table_is_not_a_pass() {
        let unread = ["TCP (IPv6): error 87".to_string()];
        let check = dns_check(&[], &unread);
        assert!(!check.ok);
        assert_eq!(check.args["cause"], "unreadable");
        assert_eq!(check.args["error"], "TCP (IPv6): error 87");
        // An owner found in another table still names it.
        let check = dns_check(&[u32::MAX], &unread);
        assert!(!check.ok && check.args.contains_key("owner"));
        assert!(dns_check(&[], &[]).ok);
    }

    #[test]
    fn rows_start_where_the_tables_put_them() {
        use std::mem::offset_of;
        use windows::Win32::NetworkManagement::IpHelper::{
            MIB_TCP6ROW_OWNER_PID, MIB_TCP6TABLE_OWNER_PID, MIB_TCPROW_OWNER_PID,
            MIB_TCPTABLE_OWNER_PID, MIB_UDP6ROW_OWNER_PID, MIB_UDP6TABLE_OWNER_PID,
            MIB_UDPROW_OWNER_PID, MIB_UDPTABLE_OWNER_PID,
        };
        assert_eq!(
            rows_offset::<MIB_UDPROW_OWNER_PID>(),
            offset_of!(MIB_UDPTABLE_OWNER_PID, table)
        );
        assert_eq!(
            rows_offset::<MIB_UDP6ROW_OWNER_PID>(),
            offset_of!(MIB_UDP6TABLE_OWNER_PID, table)
        );
        assert_eq!(
            rows_offset::<MIB_TCPROW_OWNER_PID>(),
            offset_of!(MIB_TCPTABLE_OWNER_PID, table)
        );
        assert_eq!(
            rows_offset::<MIB_TCP6ROW_OWNER_PID>(),
            offset_of!(MIB_TCP6TABLE_OWNER_PID, table)
        );
    }

    /// A fake table call: asks for `need` bytes, then writes `count` and `rows` u32 rows.
    fn fake_table(
        need: u32,
        count: u32,
        rows: &[u32],
    ) -> impl Fn(Option<*mut std::ffi::c_void>, &mut u32) -> u32 {
        let rows = rows.to_vec();
        move |buffer, size| {
            let Some(buffer) = buffer else {
                *size = need;
                return 122; // ERROR_INSUFFICIENT_BUFFER
            };
            let words = [count].into_iter().chain(rows.iter().copied());
            for (i, word) in words.enumerate() {
                // SAFETY: the tests ask for at least as many bytes as they write.
                unsafe { buffer.cast::<u32>().add(i).write(word) };
            }
            *size = need;
            0
        }
    }

    #[test]
    fn row_counts_are_checked_against_the_bytes() {
        assert_eq!(
            read_table::<u32>("t", fake_table(12, 2, &[7, 9])),
            Ok(vec![7, 9])
        );
        // Claims three rows but only two fit in the 12 bytes written.
        let error = read_table::<u32>("t", fake_table(12, 3, &[7, 9])).unwrap_err();
        assert!(error.contains("3 rows claimed, 2 fit"), "{error}");
        let failing = |_: Option<*mut std::ffi::c_void>, _: &mut u32| 5u32;
        assert_eq!(read_table::<u32>("t", failing), Err("t: error 5".into()));
    }

    #[test]
    fn a_signed_system_file_passes_and_the_socket_tables_read() {
        let system = std::path::PathBuf::from(std::env::var("SystemRoot").unwrap());
        // Catalog-signed system files have no embedded signature; ntoskrnl's is embedded.
        assert!(signature_is_valid(&system.join(r"System32\ntoskrnl.exe")));
        assert!(!signature_is_valid(&system.join("no-such-file.dll")));
        // Our own sockets show up in all four tables, with their address, port and pid.
        let me = std::process::id();
        let tcp4 = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let tcp6 = std::net::TcpListener::bind("[::1]:0").unwrap();
        let udp4 = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let udp6 = std::net::UdpSocket::bind("[::1]:0").unwrap();
        let (found, unread) = bindings();
        assert!(unread.is_empty(), "{unread:?}");
        for (tcp, local) in [
            (true, tcp4.local_addr().unwrap()),
            (true, tcp6.local_addr().unwrap()),
            (false, udp4.local_addr().unwrap()),
            (false, udp6.local_addr().unwrap()),
        ] {
            let expected = Binding {
                tcp,
                addr: local.ip(),
                port: local.port(),
                pid: me,
            };
            assert!(found.contains(&expected), "{expected:?} missing");
        }
        assert_eq!(dns().id, CheckId::Dns);
    }
}
