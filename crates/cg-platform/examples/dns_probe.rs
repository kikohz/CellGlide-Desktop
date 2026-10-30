//! W0 measurement: how does the Windows DNS client behave when cellglide0's only server
//! (`198.18.0.1`) answers late, with SERVFAIL, or not at all? Adds NO routes, so the
//! machine stays online. Run as administrator.
//!
//! ```text
//! dns_probe --answer | --delay <ms> | --servfail | --silent
//!           [--seconds N] [--stop-file P] [--upstream IP:PORT] [--v6-dns] [--strict]
//! dns_probe --bind-only        set up cellglide0, try UDP+TCP 198.18.0.1:53, report, exit
//! dns_probe --hold-wildcard    bind 0.0.0.0:53 UDP+TCP with SO_EXCLUSIVEADDRUSE, hold N s
//! ```
//!
//! Server modes create cellglide0 with its addresses and interface metric 0, set its DNS
//! to `198.18.0.1` only, bind UDP and TCP on `198.18.0.1:53`, print `ready`, and log every
//! query with the time since start. They stop after `--seconds` (default 90) or as soon as
//! the stop file exists, then clear the DNS, flush, and close the adapter. If the process
//! dies instead, the adapter goes with it.
//!
//! Only names starting with `probe-` get the mode's treatment; every other query (the
//! system's own background lookups) is forwarded to `--upstream`, so the machine keeps
//! resolving and no fake answer lands in its cache. `--v6-dns` also serves
//! `[fd6c:6779:6465::1]:53` and lists it as cellglide0's IPv6 server. `--strict` turns on
//! strict DNS (Task 11b: a dynamic WFP session blocking port 53 off cellglide0) once the
//! sockets are bound; queries forwarded to an upstream off cellglide0 are then blocked too.

#[cfg(windows)]
fn main() {
    probe::main();
}

#[cfg(not(windows))]
fn main() {}

#[cfg(windows)]
mod probe {
    use std::io::{self, Write};
    use std::net::{SocketAddr, TcpListener, UdpSocket};
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use cg_core::resolver::servfail;
    use cg_core::tunnel::{
        ADDRESS_V4, ADDRESS_V6, INTERFACE_NAME, WINDOWS_ADAPTER_GUID, WINDOWS_RESOLVER,
        WINDOWS_TUNNEL_TYPE,
    };
    use cg_platform::windows::ip::{self, Family};
    use cg_platform::windows::wintun::{Adapter, SESSION_CAPACITY, Session, Wintun};
    use cg_platform::windows::{dns, strict_dns};
    use windows::Win32::Networking::WinSock::{
        AF_INET, IPPROTO_TCP, IPPROTO_UDP, SO_EXCLUSIVEADDRUSE, SOCK_DGRAM, SOCK_STREAM, SOCKADDR,
        SOCKADDR_IN, SOCKET, SOL_SOCKET, WINSOCK_SOCKET_TYPE, WSADATA, WSAGetLastError, WSAStartup,
        bind, closesocket, listen, setsockopt, socket,
    };

    /// The fixed answer for A queries (TEST-NET-1).
    const ANSWER: [u8; 4] = [192, 0, 2, 1];

    #[derive(Clone, Copy, Debug)]
    enum Mode {
        Answer,
        Delay(u64),
        Servfail,
        Silent,
        BindOnly,
        HoldWildcard,
    }

    struct Args {
        mode: Mode,
        seconds: u64,
        stop_file: Option<PathBuf>,
        upstream: Option<SocketAddr>,
        v6_dns: bool,
        strict: bool,
    }

    fn args() -> Args {
        let mut mode = None;
        let mut seconds = 90;
        let mut stop_file = None;
        let mut upstream = None;
        let mut v6_dns = false;
        let mut strict = false;
        let mut it = std::env::args().skip(1);
        while let Some(arg) = it.next() {
            match arg.as_str() {
                "--answer" => mode = Some(Mode::Answer),
                "--delay" => mode = Some(Mode::Delay(number(it.next()))),
                "--servfail" => mode = Some(Mode::Servfail),
                "--silent" => mode = Some(Mode::Silent),
                "--bind-only" => mode = Some(Mode::BindOnly),
                "--hold-wildcard" => mode = Some(Mode::HoldWildcard),
                "--seconds" => seconds = number(it.next()),
                "--stop-file" => stop_file = it.next().map(PathBuf::from),
                "--upstream" => {
                    upstream = Some(
                        it.next()
                            .and_then(|a| a.parse().ok())
                            .unwrap_or_else(|| usage("expected ip:port")),
                    );
                }
                "--v6-dns" => v6_dns = true,
                "--strict" => strict = true,
                other => usage(other),
            }
        }
        Args {
            mode: mode.unwrap_or_else(|| usage("no mode")),
            seconds,
            stop_file,
            upstream,
            v6_dns,
            strict,
        }
    }

    fn number(arg: Option<String>) -> u64 {
        arg.and_then(|a| a.parse().ok())
            .unwrap_or_else(|| usage("expected a number"))
    }

    fn usage(problem: &str) -> ! {
        eprintln!(
            "{problem}\nusage: dns_probe --answer|--delay <ms>|--servfail|--silent|--bind-only|--hold-wildcard [--seconds N] [--stop-file PATH] [--upstream IP:PORT] [--v6-dns] [--strict]"
        );
        std::process::exit(2)
    }

    pub fn main() {
        let args = args();
        let start = Instant::now();
        let mut data = WSADATA::default();
        // SAFETY: data is writable; version 2.2.
        unsafe { WSAStartup(0x0202, &mut data) };
        if let Mode::HoldWildcard = args.mode {
            hold_wildcard(args.seconds);
            return;
        }

        let dll = Wintun::load().unwrap_or_else(|e| fail("load", e));
        let adapter = Adapter::create(
            &dll,
            INTERFACE_NAME,
            WINDOWS_TUNNEL_TYPE,
            WINDOWS_ADAPTER_GUID,
        )
        .unwrap_or_else(|e| fail("create", e));
        let luid = adapter.luid();
        let result = adapter
            .start_session(SESSION_CAPACITY)
            .and_then(|session| run(&args, luid, session, start));
        let cleared = dns::clear_dns(WINDOWS_ADAPTER_GUID);
        let flushed = ip::flush(luid);
        drop(adapter);
        println!(
            "{} teardown: clear_dns {cleared:?}, flush {flushed:?}, adapter closed",
            stamp(start)
        );
        if let Err(e) = result {
            fail("run", e);
        }
    }

    fn run(args: &Args, luid: u64, session: Session, start: Instant) -> io::Result<()> {
        ip::configure_interface(luid, Family::V4)?;
        ip::configure_interface(luid, Family::V6)?;
        ip::add_address(luid, ADDRESS_V4.into(), 32)?;
        ip::add_address(luid, ADDRESS_V6.into(), 128)?;
        dns::set_dns(WINDOWS_ADAPTER_GUID, &[ADDRESS_V4.into()])?;
        println!(
            "{} cellglide0 up, luid {luid:#x}, DNS 198.18.0.1",
            stamp(start)
        );

        let udp = retry_bind(|| UdpSocket::bind(WINDOWS_RESOLVER));
        let tcp = retry_bind(|| TcpListener::bind(WINDOWS_RESOLVER));
        println!(
            "{} bind udp {WINDOWS_RESOLVER}: {}",
            stamp(start),
            outcome(&udp)
        );
        println!(
            "{} bind tcp {WINDOWS_RESOLVER}: {}",
            stamp(start),
            outcome(&tcp)
        );
        if let Mode::BindOnly = args.mode {
            drop(session);
            return Ok(());
        }
        let udp = udp?;
        let _tcp = tcp?;
        let mut sockets = vec![udp];
        let mut _listeners = Vec::new();
        if args.v6_dns {
            let resolver_v6 = SocketAddr::new(ADDRESS_V6.into(), 53);
            let udp6 = retry_bind(|| UdpSocket::bind(resolver_v6));
            let tcp6 = retry_bind(|| TcpListener::bind(resolver_v6));
            println!(
                "{} bind udp {resolver_v6}: {}",
                stamp(start),
                outcome(&udp6)
            );
            println!(
                "{} bind tcp {resolver_v6}: {}",
                stamp(start),
                outcome(&tcp6)
            );
            sockets.push(udp6?);
            _listeners.push(tcp6?);
            dns::set_dns(
                WINDOWS_ADAPTER_GUID,
                &[ADDRESS_V4.into(), ADDRESS_V6.into()],
            )?;
            println!("{} DNS now 198.18.0.1 and {ADDRESS_V6}", stamp(start));
        }
        let _strict = if args.strict {
            let strict = strict_dns::start(luid)?;
            println!("{} strict DNS on", stamp(start));
            Some(strict)
        } else {
            None
        };
        println!(
            "{} ready mode={:?} upstream={:?}",
            stamp(start),
            args.mode,
            args.upstream
        );
        io::stdout().flush()?;

        let deadline = start + Duration::from_secs(args.seconds);
        let stopped =
            || Instant::now() >= deadline || args.stop_file.as_ref().is_some_and(|p| p.exists());
        std::thread::scope(|scope| {
            let workers: Vec<_> = sockets
                .iter()
                .map(|socket| scope.spawn(|| serve(socket, args, start, &stopped)))
                .collect();
            while !stopped() {
                // Keep the session's ring from filling with whatever the stack sends us.
                while let Ok(Some(_)) = session.try_receive() {}
                std::thread::sleep(Duration::from_millis(100));
            }
            workers
                .into_iter()
                .try_for_each(|worker| worker.join().expect("worker panicked"))
        })?;
        drop(session);
        Ok(())
    }

    /// Answers probe names (`probe-*`) as the mode says; forwards every other name to the
    /// upstream so the rest of the machine keeps resolving while we hold its DNS.
    fn serve(
        udp: &UdpSocket,
        args: &Args,
        start: Instant,
        stopped: &dyn Fn() -> bool,
    ) -> io::Result<()> {
        udp.set_read_timeout(Some(Duration::from_millis(100)))?;
        let mut buffer = [0u8; 1500];
        while !stopped() {
            let (length, peer) = match udp.recv_from(&mut buffer) {
                Ok(received) => received,
                Err(e)
                    if e.kind() == io::ErrorKind::WouldBlock
                        || e.kind() == io::ErrorKind::TimedOut =>
                {
                    continue;
                }
                // WSAECONNRESET from an earlier reply to a port that went away.
                Err(e) if e.raw_os_error() == Some(10054) => continue,
                Err(e) => return Err(e),
            };
            let query = buffer[..length].to_vec();
            let (name, qtype) = question(&query);
            let id = u16::from_be_bytes([query[0], query[1]]);
            let local = udp.local_addr()?;
            if !name.to_ascii_lowercase().starts_with("probe-") {
                println!(
                    "{} other query at {local} from {peer} id={id:#06x} {name} type {qtype}: forwarded",
                    stamp(start)
                );
                if let Some(upstream) = args.upstream {
                    let socket = udp.try_clone()?;
                    std::thread::spawn(move || forward(&socket, &query, peer, upstream));
                }
                continue;
            }
            let action = reply(args.mode, &query);
            println!(
                "{} query at {local} from {peer} id={id:#06x} {name} type {qtype}: {}",
                stamp(start),
                action.label
            );
            io::stdout().flush()?;
            if let Some(answer) = action.packet {
                match action.delay {
                    0 => send(udp, &answer, peer, start),
                    delay => {
                        let socket = udp.try_clone()?;
                        std::thread::spawn(move || {
                            std::thread::sleep(Duration::from_millis(delay));
                            send(&socket, &answer, peer, start);
                        });
                    }
                }
            }
        }
        Ok(())
    }

    /// Relays one query to the upstream and its reply back; silence if the upstream is.
    fn forward(socket: &UdpSocket, query: &[u8], peer: SocketAddr, upstream: SocketAddr) {
        let relay = (|| {
            let out = UdpSocket::bind(if upstream.is_ipv4() {
                "0.0.0.0:0"
            } else {
                "[::]:0"
            })?;
            out.set_read_timeout(Some(Duration::from_secs(3)))?;
            out.send_to(query, upstream)?;
            let mut buffer = [0u8; 1500];
            let length = out.recv(&mut buffer)?;
            socket.send_to(&buffer[..length], peer)
        })();
        if let Err(e) = relay {
            println!("forward to {upstream} failed: {e}");
        }
    }

    struct Action {
        packet: Option<Vec<u8>>,
        delay: u64,
        label: String,
    }

    fn reply(mode: Mode, query: &[u8]) -> Action {
        let (packet, delay) = match mode {
            Mode::Answer => (answer(query), 0),
            Mode::Delay(ms) => (answer(query), ms),
            Mode::Servfail => (servfail(query), 0),
            _ => (None, 0),
        };
        let label = match (&packet, mode) {
            (None, _) => "no reply".to_owned(),
            (Some(_), Mode::Servfail) => "SERVFAIL now".to_owned(),
            (Some(_), _) if delay > 0 => format!("answer in {delay} ms"),
            (Some(_), _) => "answer now".to_owned(),
        };
        Action {
            packet,
            delay,
            label,
        }
    }

    fn send(socket: &UdpSocket, packet: &[u8], peer: SocketAddr, start: Instant) {
        match socket.send_to(packet, peer) {
            Ok(_) => println!(
                "{} reply sent to {peer} id={:#06x}",
                stamp(start),
                u16::from_be_bytes([packet[0], packet[1]])
            ),
            Err(e) => println!("{} reply to {peer} failed: {e}", stamp(start)),
        }
        let _ = io::stdout().flush();
    }

    /// NOERROR; for an A question, one A record pointing at `ANSWER` with TTL 60.
    fn answer(query: &[u8]) -> Option<Vec<u8>> {
        let end = question_end(query)?;
        let mut reply = query[..end].to_vec();
        reply[2] = 0x80 | (query[2] & 0x79); // QR, keep opcode and RD
        reply[3] = 0x80; // RA, NOERROR
        reply[6..12].fill(0);
        let qtype = u16::from_be_bytes([query[end - 4], query[end - 3]]);
        if qtype == 1 {
            reply[7] = 1; // ANCOUNT
            reply.extend_from_slice(&[0xc0, 0x0c, 0, 1, 0, 1, 0, 0, 0, 60, 0, 4]);
            reply.extend_from_slice(&ANSWER);
        }
        Some(reply)
    }

    /// End of the single question (after QTYPE and QCLASS), if the query has exactly one.
    fn question_end(query: &[u8]) -> Option<usize> {
        if query.len() < 12 || query[2] & 0x80 != 0 || query[4..6] != [0, 1] {
            return None;
        }
        let mut end = 12;
        while *query.get(end)? != 0 {
            if query[end] & 0xc0 != 0 {
                return None;
            }
            end += query[end] as usize + 1;
        }
        end += 5;
        (end <= query.len()).then_some(end)
    }

    fn question(query: &[u8]) -> (String, u16) {
        let Some(end) = question_end(query) else {
            return ("?".to_owned(), 0);
        };
        let mut labels = Vec::new();
        let mut at = 12;
        while query[at] != 0 {
            let length = query[at] as usize;
            labels.push(String::from_utf8_lossy(&query[at + 1..at + 1 + length]).into_owned());
            at += length + 1;
        }
        (
            labels.join("."),
            u16::from_be_bytes([query[end - 4], query[end - 3]]),
        )
    }

    /// The address was just added; give the stack a moment if the first bind is refused.
    fn retry_bind<T>(mut bind: impl FnMut() -> io::Result<T>) -> io::Result<T> {
        let until = Instant::now() + Duration::from_secs(3);
        loop {
            match bind() {
                Err(e) if e.raw_os_error() == Some(10049) && Instant::now() < until => {
                    std::thread::sleep(Duration::from_millis(100));
                }
                other => return other,
            }
        }
    }

    fn outcome<T>(result: &io::Result<T>) -> String {
        match result {
            Ok(_) => "OK".to_owned(),
            Err(e) => format!("FAILED: {e} (code {:?})", e.raw_os_error()),
        }
    }

    /// Binds `0.0.0.0:53` for UDP and TCP with `SO_EXCLUSIVEADDRUSE` (as a service like
    /// SharedAccess might), reports, and holds both for `seconds`.
    fn hold_wildcard(seconds: u64) {
        let udp = exclusive_wildcard(SOCK_DGRAM, IPPROTO_UDP.0);
        let tcp = exclusive_wildcard(SOCK_STREAM, IPPROTO_TCP.0);
        let show = |r: &Result<SOCKET, String>| match r {
            Ok(_) => "OK".to_owned(),
            Err(e) => format!("FAILED: {e}"),
        };
        println!("hold udp 0.0.0.0:53 exclusive: {}", show(&udp));
        println!("hold tcp 0.0.0.0:53 exclusive: {}", show(&tcp));
        println!("ready");
        let _ = io::stdout().flush();
        std::thread::sleep(Duration::from_secs(seconds));
        for s in [udp, tcp].into_iter().flatten() {
            // SAFETY: our socket, closed once.
            unsafe { closesocket(s) };
        }
    }

    fn exclusive_wildcard(kind: WINSOCK_SOCKET_TYPE, protocol: i32) -> Result<SOCKET, String> {
        // SAFETY: plain socket creation.
        let s = unsafe { socket(AF_INET.0.into(), kind, protocol) }.map_err(|e| e.to_string())?;
        let on = 1u32.to_ne_bytes();
        // SAFETY: the option takes a 4-byte BOOL.
        if unsafe { setsockopt(s, SOL_SOCKET, SO_EXCLUSIVEADDRUSE, Some(&on)) } != 0 {
            return Err(format!("setsockopt WSA {}", wsa_error()));
        }
        let address = SOCKADDR_IN {
            sin_family: AF_INET,
            sin_port: 53u16.to_be(),
            ..Default::default()
        };
        let length = std::mem::size_of::<SOCKADDR_IN>() as i32;
        // SAFETY: address is a SOCKADDR_IN of `length` bytes.
        if unsafe { bind(s, (&raw const address).cast::<SOCKADDR>(), length) } != 0 {
            return Err(format!("bind WSA {}", wsa_error()));
        }
        // SAFETY: a bound stream socket.
        if kind == SOCK_STREAM && unsafe { listen(s, 16) } != 0 {
            return Err(format!("listen WSA {}", wsa_error()));
        }
        Ok(s)
    }

    fn wsa_error() -> i32 {
        // SAFETY: reads the thread's last Winsock error.
        unsafe { WSAGetLastError() }.0
    }

    fn stamp(start: Instant) -> String {
        format!("[{:>8.3}s]", start.elapsed().as_secs_f64())
    }

    fn fail(step: &str, e: io::Error) -> ! {
        eprintln!("{step}: {e} (code {:?})", e.raw_os_error());
        std::process::exit(1)
    }
}
