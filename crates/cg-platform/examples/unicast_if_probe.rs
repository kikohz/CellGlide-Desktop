//! W0 measurement: can `IP_UNICAST_IF` / `IPV6_UNICAST_IF` send a TCP connection out the
//! physical adapter while cellglide0 holds the four /1 routes?
//!
//! THIS TAKES THE MACHINE OFF THE INTERNET while it runs (there is no forwarding engine
//! behind the routes). Run it only in the foreground, as administrator. It tears down on
//! every exit path:
//! - normal end and `Err`: `teardown` flushes our routes and addresses, then the adapter is
//!   dropped (`WintunCloseAdapter` removes it, and its routes with it);
//! - panic: the panic hook flushes and exits; unwinding would also drop the adapter;
//! - hang: a watchdog thread flushes and hard-exits after 20 s;
//! - any process death: Windows closes the wintun handle and the adapter disappears.
//!
//! The physical adapter is the lowest-metric `0.0.0.0/0` (or `::/0`) row of
//! `GetIpForwardTable2` that is not cellglide0. The target address is resolved before the
//! routes go in.

#[cfg(windows)]
fn main() {
    probe::main();
}

#[cfg(not(windows))]
fn main() {}

#[cfg(windows)]
mod probe {
    use std::io;
    use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant};

    use cg_core::tunnel::{
        ADDRESS_V4, ADDRESS_V6, INTERFACE_NAME, IPV4_ROUTES, IPV6_ROUTES, WINDOWS_ADAPTER_GUID,
        WINDOWS_TUNNEL_TYPE,
    };
    use cg_platform::windows::ip::{self, Family};
    use cg_platform::windows::wintun::{Adapter, SESSION_CAPACITY, Session, Wintun};
    use windows::Win32::Foundation::NO_ERROR;
    use windows::Win32::NetworkManagement::IpHelper::{
        ConvertInterfaceLuidToAlias, FreeMibTable, GetIpForwardTable2, GetIpInterfaceEntry,
        InitializeIpInterfaceEntry, MIB_IPFORWARD_TABLE2, MIB_IPINTERFACE_ROW,
    };
    use windows::Win32::NetworkManagement::Ndis::NET_LUID_LH;
    use windows::Win32::Networking::WinSock::{
        ADDRESS_FAMILY, AF_INET, AF_INET6, FD_SET, FIONBIO, IN_ADDR, IN_ADDR_0, IN6_ADDR,
        IN6_ADDR_0, IP_UNICAST_IF, IPPROTO_IP, IPPROTO_IPV6, IPPROTO_TCP, IPV6_UNICAST_IF,
        SO_ERROR, SOCK_STREAM, SOCKADDR, SOCKADDR_IN, SOCKADDR_IN6, SOCKADDR_STORAGE, SOCKET,
        SOL_SOCKET, TIMEVAL, WSADATA, WSAGetLastError, WSAStartup, closesocket, connect,
        getsockname, getsockopt, ioctlsocket, select, setsockopt, socket,
    };

    const TARGET: &str = "speed.cloudflare.com:443";
    const WATCHDOG: Duration = Duration::from_secs(20);
    const PLAIN_TIMEOUT: Duration = Duration::from_secs(3);
    const BOUND_TIMEOUT: Duration = Duration::from_secs(4);

    /// Our adapter's LUID once the routes may exist; 0 before that.
    static LUID: AtomicU64 = AtomicU64::new(0);

    fn emergency_flush() {
        let luid = LUID.load(Ordering::SeqCst);
        if luid != 0 {
            let _ = ip::flush(luid);
        }
    }

    pub fn main() {
        let start = Instant::now();
        std::thread::spawn(|| {
            std::thread::sleep(WATCHDOG);
            eprintln!("watchdog: {WATCHDOG:?} passed, flushing and exiting");
            emergency_flush();
            std::process::exit(3);
        });
        let default_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            default_hook(info);
            emergency_flush();
            std::process::exit(101);
        }));
        winsock_startup();

        // Resolve first: once the routes are in, the system resolver can't reach anything.
        let targets: Vec<SocketAddr> = TARGET
            .to_socket_addrs()
            .unwrap_or_else(|e| fail("resolve", e))
            .collect();
        let v4 = targets.iter().copied().find(SocketAddr::is_ipv4);
        let v6 = targets.iter().copied().find(SocketAddr::is_ipv6);
        println!("{TARGET}: v4 {v4:?}, v6 {v6:?}");

        let dll = Wintun::load().unwrap_or_else(|e| fail("load", e));
        let adapter = Adapter::create(
            &dll,
            INTERFACE_NAME,
            WINDOWS_TUNNEL_TYPE,
            WINDOWS_ADAPTER_GUID,
        )
        .unwrap_or_else(|e| fail("create", e));
        let luid = adapter.luid();
        LUID.store(luid, Ordering::SeqCst);
        let result = match adapter.start_session(SESSION_CAPACITY) {
            Ok(session) => {
                let result = run(luid, &session, v4, v6);
                drop(session);
                result
            }
            Err(e) => Err(e),
        };
        let torn = ip::flush(luid);
        LUID.store(0, Ordering::SeqCst);
        drop(adapter);
        println!(
            "teardown: flush {torn:?}, adapter closed, {:.1} s total",
            start.elapsed().as_secs_f64()
        );
        if let Err(e) = result {
            fail("run", e);
        }
    }

    fn run(
        luid: u64,
        session: &Session,
        v4: Option<SocketAddr>,
        v6: Option<SocketAddr>,
    ) -> io::Result<()> {
        let physical_v4 = physical(AF_INET, luid)?;
        let physical_v6 = physical(AF_INET6, luid)?;
        println!("physical v4: {physical_v4:?}");
        println!("physical v6: {physical_v6:?}");

        ip::configure_interface(luid, Family::V4)?;
        ip::configure_interface(luid, Family::V6)?;
        ip::add_address(luid, ADDRESS_V4.into(), 32)?;
        ip::add_address(luid, ADDRESS_V6.into(), 128)?;
        for route in IPV4_ROUTES.iter().chain(&IPV6_ROUTES) {
            ip::add_route(luid, route)?;
        }
        println!("routes added: {IPV4_ROUTES:?} {IPV6_ROUTES:?}");
        std::thread::sleep(Duration::from_millis(500));

        for (family, target, physical) in [("v4", v4, physical_v4), ("v6", v6, physical_v6)] {
            let Some(target) = target else {
                println!("[{family}] no {family} address for {TARGET}: skipped");
                continue;
            };
            let Some((if_index, alias)) = physical else {
                println!("[{family}] no physical default route: skipped");
                continue;
            };
            drain(session);
            let plain = tcp_connect(target, None, PLAIN_TIMEOUT);
            println!(
                "[{family}] plain connect {target}: {plain}; packets to it in the tunnel: {}",
                drain_count(session, target.ip())
            );
            let bound = tcp_connect(target, Some(if_index), BOUND_TIMEOUT);
            println!("[{family}] UNICAST_IF={if_index} ({alias}) connect {target}: {bound}");
        }
        Ok(())
    }

    /// The lowest (route + interface) metric default route that is not ours:
    /// `(ifIndex, alias)`.
    fn physical(family: ADDRESS_FAMILY, ours: u64) -> io::Result<Option<(u32, String)>> {
        let mut table: *mut MIB_IPFORWARD_TABLE2 = std::ptr::null_mut();
        // SAFETY: on success the table is freed below with FreeMibTable.
        let result = unsafe { GetIpForwardTable2(family, &mut table) };
        if result != NO_ERROR {
            return Err(io::Error::from_raw_os_error(result.0 as i32));
        }
        // SAFETY: the table holds NumEntries rows until FreeMibTable.
        let rows = unsafe {
            std::slice::from_raw_parts((*table).Table.as_ptr(), (*table).NumEntries as usize)
        };
        let mut best: Option<(u32, u32, NET_LUID_LH)> = None;
        for row in rows {
            // SAFETY: NET_LUID is a 64-bit union.
            let luid = unsafe { row.InterfaceLuid.Value };
            if row.DestinationPrefix.PrefixLength != 0 || luid == ours {
                continue;
            }
            let metric = row
                .Metric
                .saturating_add(interface_metric(family, row.InterfaceLuid));
            if best.is_none_or(|(m, _, _)| metric < m) {
                best = Some((metric, row.InterfaceIndex, row.InterfaceLuid));
            }
        }
        // SAFETY: the table came from GetIpForwardTable2 and `rows` is no longer used.
        unsafe { FreeMibTable(table.cast()) };
        Ok(best.map(|(_, index, luid)| (index, alias(luid))))
    }

    fn interface_metric(family: ADDRESS_FAMILY, luid: NET_LUID_LH) -> u32 {
        let mut row = MIB_IPINTERFACE_ROW::default();
        // SAFETY: the row is valid and writable; Family and InterfaceLuid select it.
        unsafe { InitializeIpInterfaceEntry(&mut row) };
        row.Family = family;
        row.InterfaceLuid = luid;
        // SAFETY: as above.
        if unsafe { GetIpInterfaceEntry(&mut row) } == NO_ERROR {
            row.Metric
        } else {
            u32::MAX / 2
        }
    }

    fn alias(luid: NET_LUID_LH) -> String {
        let mut buffer = [0u16; 257];
        // SAFETY: the buffer is writable and its length is passed.
        if unsafe { ConvertInterfaceLuidToAlias(&luid, &mut buffer) } != NO_ERROR {
            return "?".to_owned();
        }
        let end = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
        String::from_utf16_lossy(&buffer[..end])
    }

    fn drain(session: &Session) {
        while let Ok(Some(_)) = session.try_receive() {}
    }

    /// Packets waiting in the tunnel whose destination is `target`.
    fn drain_count(session: &Session, target: IpAddr) -> usize {
        let mut count = 0;
        while let Ok(Some(packet)) = session.try_receive() {
            let destination = match packet.first().map(|b| b >> 4) {
                Some(4) if packet.len() >= 20 => {
                    IpAddr::from(<[u8; 4]>::try_from(&packet[16..20]).unwrap())
                }
                Some(6) if packet.len() >= 40 => {
                    IpAddr::from(<[u8; 16]>::try_from(&packet[24..40]).unwrap())
                }
                _ => continue,
            };
            count += usize::from(destination == target);
        }
        count
    }

    /// A non-blocking TCP connect with a timeout, optionally pinned to `if_index` with
    /// `IP_UNICAST_IF` (network byte order) or `IPV6_UNICAST_IF` (host byte order).
    fn tcp_connect(target: SocketAddr, if_index: Option<u32>, timeout: Duration) -> String {
        let started = Instant::now();
        let family = if target.is_ipv4() { AF_INET } else { AF_INET6 };
        // SAFETY: plain socket creation.
        let s = match unsafe { socket(family.0.into(), SOCK_STREAM, IPPROTO_TCP.0) } {
            Ok(s) => Socket(s),
            Err(e) => return format!("socket failed: {e}"),
        };
        if let Some(index) = if_index {
            let (level, name, value) = match target {
                SocketAddr::V4(_) => (IPPROTO_IP.0, IP_UNICAST_IF, index.to_be()),
                SocketAddr::V6(_) => (IPPROTO_IPV6.0, IPV6_UNICAST_IF, index),
            };
            // SAFETY: the option value is a 4-byte DWORD, as both options require.
            if unsafe { setsockopt(s.0, level, name, Some(&value.to_ne_bytes())) } != 0 {
                return format!("setsockopt UNICAST_IF failed: WSA {}", wsa_error());
            }
        }
        let mut nonblocking = 1u32;
        // SAFETY: FIONBIO takes a u32 flag.
        unsafe { ioctlsocket(s.0, FIONBIO, &mut nonblocking) };
        let (storage, length) = sockaddr(target);
        // SAFETY: storage holds a sockaddr of `length` bytes.
        let result = unsafe { connect(s.0, (&raw const storage).cast::<SOCKADDR>(), length) };
        if result != 0 && wsa_error() != 10035 {
            return format!("connect failed at once: WSA {}", wsa_error());
        }
        let mut writable = fd_set(s.0);
        let mut failed = fd_set(s.0);
        let wait = TIMEVAL {
            tv_sec: timeout.as_secs() as i32,
            tv_usec: timeout.subsec_micros() as i32,
        };
        // SAFETY: the sets and timeout are valid for the call.
        let ready = unsafe { select(0, None, Some(&mut writable), Some(&mut failed), Some(&wait)) };
        let elapsed = started.elapsed().as_millis();
        if ready == 0 {
            return format!("TIMED OUT after {elapsed} ms");
        }
        if ready < 0 {
            return format!("select failed: WSA {}", wsa_error());
        }
        if failed.fd_count > 0 {
            let mut code = [0u8; 4];
            let mut size = 4i32;
            // SAFETY: SO_ERROR fills a 4-byte int.
            unsafe {
                getsockopt(
                    s.0,
                    SOL_SOCKET,
                    SO_ERROR,
                    windows::core::PSTR(code.as_mut_ptr()),
                    &mut size,
                )
            };
            return format!(
                "FAILED after {elapsed} ms: WSA {}",
                i32::from_ne_bytes(code)
            );
        }
        let mut local = SOCKADDR_STORAGE::default();
        let mut size = std::mem::size_of::<SOCKADDR_STORAGE>() as i32;
        // SAFETY: local is a writable SOCKADDR_STORAGE of `size` bytes.
        unsafe { getsockname(s.0, (&raw mut local).cast::<SOCKADDR>(), &mut size) };
        format!(
            "CONNECTED in {elapsed} ms from {}",
            local_address(&local).map_or("?".to_owned(), |a| a.to_string())
        )
    }

    fn fd_set(s: SOCKET) -> FD_SET {
        let mut set = FD_SET {
            fd_count: 1,
            ..Default::default()
        };
        set.fd_array[0] = s;
        set
    }

    fn sockaddr(address: SocketAddr) -> (SOCKADDR_STORAGE, i32) {
        let mut storage = SOCKADDR_STORAGE::default();
        let length = match address {
            SocketAddr::V4(v4) => {
                let raw = SOCKADDR_IN {
                    sin_family: AF_INET,
                    sin_port: v4.port().to_be(),
                    sin_addr: IN_ADDR {
                        S_un: IN_ADDR_0 {
                            S_addr: u32::from_ne_bytes(v4.ip().octets()),
                        },
                    },
                    ..Default::default()
                };
                // SAFETY: SOCKADDR_STORAGE is large enough and suitably aligned.
                unsafe { (&raw mut storage).cast::<SOCKADDR_IN>().write(raw) };
                std::mem::size_of::<SOCKADDR_IN>()
            }
            SocketAddr::V6(v6) => {
                let raw = SOCKADDR_IN6 {
                    sin6_family: AF_INET6,
                    sin6_port: v6.port().to_be(),
                    sin6_addr: IN6_ADDR {
                        u: IN6_ADDR_0 {
                            Byte: v6.ip().octets(),
                        },
                    },
                    ..Default::default()
                };
                // SAFETY: as above.
                unsafe { (&raw mut storage).cast::<SOCKADDR_IN6>().write(raw) };
                std::mem::size_of::<SOCKADDR_IN6>()
            }
        };
        (storage, length as i32)
    }

    fn local_address(storage: &SOCKADDR_STORAGE) -> Option<IpAddr> {
        if storage.ss_family == AF_INET {
            // SAFETY: the family says this is a SOCKADDR_IN.
            let raw = unsafe { &*(storage as *const SOCKADDR_STORAGE).cast::<SOCKADDR_IN>() };
            // SAFETY: IN_ADDR is a 32-bit union.
            Some(IpAddr::from(
                unsafe { raw.sin_addr.S_un.S_addr }.to_ne_bytes(),
            ))
        } else if storage.ss_family == AF_INET6 {
            // SAFETY: the family says this is a SOCKADDR_IN6.
            let raw = unsafe { &*(storage as *const SOCKADDR_STORAGE).cast::<SOCKADDR_IN6>() };
            // SAFETY: IN6_ADDR is a 16-byte union.
            Some(IpAddr::from(unsafe { raw.sin6_addr.u.Byte }))
        } else {
            None
        }
    }

    struct Socket(SOCKET);

    impl Drop for Socket {
        fn drop(&mut self) {
            // SAFETY: the socket is ours and closed once.
            unsafe { closesocket(self.0) };
        }
    }

    fn winsock_startup() {
        let mut data = WSADATA::default();
        // SAFETY: data is writable; version 2.2.
        unsafe { WSAStartup(0x0202, &mut data) };
    }

    fn wsa_error() -> i32 {
        // SAFETY: reads the thread's last Winsock error.
        unsafe { WSAGetLastError() }.0
    }

    fn fail(step: &str, e: impl std::fmt::Display) -> ! {
        eprintln!("{step}: {e}");
        emergency_flush();
        std::process::exit(1)
    }
}
