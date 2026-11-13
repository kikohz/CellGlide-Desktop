//! Milestone 1 gate (Windows 1.1 wireless, plan A task 1): can this computer, as the user or as
//! LocalSystem, open the access point, find the phone on it and complete the TLS-PSK handshake?
//!
//! ```text
//! wireless_gate <ssid> <passphrase> <seconds> [--mdns system|raw|both]
//!               [--data <identity> <long-term key, 64 hex> <port>]
//! ```
//!
//! Starts the access point, then every 2 s logs the connected devices and what each mDNS method
//! (`both` by default: the system DNS-SD API and our own queries) finds on the access point's
//! interface. Once a phone is found it connects once:
//!
//! - default: identity `CG1-pair` with the public pairing key, sends `CG1 PAIR\n`, logs the
//!   phone's `C` line and closes (no pairing goes further);
//! - `--data`: the pairing's identity with `link_key(<long-term key>)`, sends
//!   `CG1 DATA <port>\n`; for port 8088 also the hello request, and logs the first 300 bytes of
//!   the reply.
//!
//! A failed handshake is retried at most every 10 s. Stops after `<seconds>` or on Ctrl-C;
//! either way the access point is stopped (it also vanishes if the process is killed). Logs to
//! stdout with local timestamps, one flushed line per event, so it can run as LocalSystem with
//! stdout redirected to a file. Changes no setting of any adapter.

#[cfg(windows)]
fn main() {
    std::process::exit(gate::main());
}

#[cfg(not(windows))]
fn main() {}

#[cfg(windows)]
mod gate {
    use std::io::Write;
    use std::net::{IpAddr, SocketAddr};
    use std::time::{Duration, Instant};

    use cg_core::wireless::pairing::{Key, PAIRING_IDENTITY, link_key, pairing_channel_key};
    use cg_platform::windows::access_point::AccessPoint;
    use cg_platform::windows::mdns::{self, FoundPhone, Method};
    use cg_platform::windows::wireless_tls;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    const TICK: Duration = Duration::from_secs(2);
    const BROWSE: Duration = Duration::from_secs(2);
    const RETRY: Duration = Duration::from_secs(10);
    const READ: Duration = Duration::from_secs(10);
    /// The address Windows gives the Wi-Fi Direct group owner by default.
    const DEFAULT_LOCAL: IpAddr = IpAddr::V4(std::net::Ipv4Addr::new(192, 168, 137, 1));

    fn log(message: impl std::fmt::Display) {
        let now = jiff::Zoned::now().strftime("%Y-%m-%d %H:%M:%S%.3f");
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{now} {message}");
        let _ = out.flush();
    }

    /// The library's own log lines (access point events), in the same format.
    struct LocalTime;

    impl tracing_subscriber::fmt::time::FormatTime for LocalTime {
        fn format_time(
            &self,
            w: &mut tracing_subscriber::fmt::format::Writer<'_>,
        ) -> std::fmt::Result {
            write!(
                w,
                "{}",
                jiff::Zoned::now().strftime("%Y-%m-%d %H:%M:%S%.3f")
            )
        }
    }

    struct Data {
        identity: String,
        long_term: Key,
        port: u16,
    }

    struct Args {
        ssid: String,
        passphrase: String,
        seconds: u64,
        methods: Vec<Method>,
        data: Option<Data>,
    }

    const USAGE: &str = "usage: wireless_gate <ssid> <passphrase> <seconds> [--mdns system|raw|both] \
                         [--data <identity> <long-term key hex> <port>]";

    fn parse_args() -> Result<Args, String> {
        let mut args = std::env::args().skip(1);
        let mut positional = Vec::new();
        let mut methods = vec![Method::System, Method::Raw];
        let mut data = None;
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--mdns" => {
                    methods = match args.next().as_deref() {
                        Some("system") => vec![Method::System],
                        Some("raw") => vec![Method::Raw],
                        Some("both") => vec![Method::System, Method::Raw],
                        _ => return Err("--mdns takes system, raw or both".into()),
                    }
                }
                "--data" => {
                    let (Some(identity), Some(key), Some(port)) =
                        (args.next(), args.next(), args.next())
                    else {
                        return Err("--data takes <identity> <long-term key hex> <port>".into());
                    };
                    let key = parse_key(&key).ok_or("the long-term key is 64 hex digits")?;
                    let port = port.parse().map_err(|_| "the port is a number")?;
                    data = Some(Data {
                        identity,
                        long_term: Key::from_bytes(key),
                        port,
                    });
                }
                _ => positional.push(arg),
            }
        }
        let [ssid, passphrase, seconds] =
            <[String; 3]>::try_from(positional).map_err(|_| USAGE.to_string())?;
        let seconds = seconds
            .parse()
            .map_err(|_| "seconds is a number".to_string())?;
        Ok(Args {
            ssid,
            passphrase,
            seconds,
            methods,
            data,
        })
    }

    fn parse_key(hex: &str) -> Option<[u8; 32]> {
        if hex.len() != 64 || !hex.is_ascii() {
            return None;
        }
        let mut key = [0u8; 32];
        for (i, byte) in key.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok()?;
        }
        Some(key)
    }

    pub fn main() -> i32 {
        let args = match parse_args() {
            Ok(args) => args,
            Err(message) => {
                eprintln!("{message}");
                return 2;
            }
        };
        tracing_subscriber::fmt()
            .with_timer(LocalTime)
            .with_ansi(false)
            .with_writer(std::io::stdout)
            .init();
        let runtime = match tokio::runtime::Runtime::new() {
            Ok(runtime) => runtime,
            Err(error) => {
                log(format!("no runtime: {error}"));
                return 1;
            }
        };
        runtime.block_on(run(args))
    }

    async fn run(args: Args) -> i32 {
        log(format!(
            "wireless_gate: ssid {} for {} s, mDNS {:?}, {}, user {}",
            args.ssid,
            args.seconds,
            args.methods,
            if args.data.is_some() {
                "data connection"
            } else {
                "pairing channel"
            },
            std::env::var("USERNAME").unwrap_or_default(),
        ));
        let (ssid, passphrase) = (args.ssid.clone(), args.passphrase.clone());
        let started = Instant::now();
        let access_point =
            match tokio::task::spawn_blocking(move || AccessPoint::start(&ssid, &passphrase)).await
            {
                Ok(Ok(access_point)) => access_point,
                Ok(Err(error)) => {
                    log(format!(
                        "access point: FAILED to start: {error} ({error:?})"
                    ));
                    return 1;
                }
                Err(error) => {
                    log(format!("access point: start panicked: {error}"));
                    return 1;
                }
            };
        log(format!(
            "access point: started in {} ms",
            started.elapsed().as_millis()
        ));

        let deadline = tokio::time::Instant::now() + Duration::from_secs(args.seconds);
        let ctrl_c = tokio::signal::ctrl_c();
        tokio::pin!(ctrl_c);
        let mut ticker = tokio::time::interval(TICK);
        let mut last_attempt: Option<Instant> = None;
        let mut connected_once = false;
        let mut last_clients = Vec::new();
        loop {
            tokio::select! {
                _ = tokio::time::sleep_until(deadline) => {
                    log("time limit reached");
                    break;
                }
                _ = &mut ctrl_c => {
                    log("Ctrl-C");
                    break;
                }
                _ = ticker.tick() => {}
            }
            let clients = access_point.clients();
            if clients != last_clients || clients.is_empty() {
                log(format!("clients: {}", describe_clients(&clients)));
                last_clients = clients.clone();
            }
            // Without an accepted device (accepting failed?) still look on the address Windows
            // gives the access point, so discovery is tested on its own.
            let local = clients
                .first()
                .map(|&(local, _)| local)
                .unwrap_or(DEFAULT_LOCAL);

            let mut found: Vec<FoundPhone> = Vec::new();
            let browses: Vec<_> = args
                .methods
                .iter()
                .map(|&method| {
                    (
                        method,
                        tokio::task::spawn_blocking(move || {
                            mdns::browse_with(method, local, BROWSE)
                        }),
                    )
                })
                .collect();
            for (method, browse) in browses {
                match browse.await {
                    Ok(Ok(phones)) => {
                        log(format!(
                            "mDNS {method:?} on {local}: {}",
                            describe_phones(&phones)
                        ));
                        for phone in phones {
                            if !found.iter().any(|known| known.instance == phone.instance) {
                                found.push(phone);
                            }
                        }
                    }
                    Ok(Err(error)) => log(format!("mDNS {method:?} on {local}: FAILED: {error}")),
                    Err(error) => log(format!("mDNS {method:?}: panicked: {error}")),
                }
            }
            // A phone's address should also be one of the devices the access point accepted
            // (design §7); the gate only notes a mismatch, so each step is tested on its own.
            let Some(phone) = found.first() else { continue };
            if !clients.iter().any(|(_, remote)| *remote == phone.address) {
                log(format!(
                    "note: {} is not among the accepted devices",
                    phone.address
                ));
            }
            if connected_once || last_attempt.is_some_and(|at| at.elapsed() < RETRY) {
                continue;
            }
            last_attempt = Some(Instant::now());
            let address = SocketAddr::new(phone.address, phone.port);
            log(format!("phone {} at {address}: connecting", phone.instance));
            match &args.data {
                None => connected_once = pairing_channel(address).await,
                Some(data) => connected_once = data_connection(address, data).await,
            }
        }
        drop(access_point);
        log("access point: stop requested; done");
        0
    }

    fn describe_clients(clients: &[(IpAddr, IpAddr)]) -> String {
        if clients.is_empty() {
            return "none".into();
        }
        clients
            .iter()
            .map(|(local, remote)| format!("{remote} (via {local})"))
            .collect::<Vec<_>>()
            .join(", ")
    }

    fn describe_phones(phones: &[FoundPhone]) -> String {
        if phones.is_empty() {
            return "nothing".into();
        }
        phones
            .iter()
            .map(|phone| format!("{} at {}:{}", phone.instance, phone.address, phone.port))
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// `CG1 PAIR` on the pairing channel; logs the `C` line. True when the line arrived.
    async fn pairing_channel(address: SocketAddr) -> bool {
        let started = Instant::now();
        let key = pairing_channel_key();
        let mut stream =
            match wireless_tls::connect(address, PAIRING_IDENTITY, key.as_bytes()).await {
                Ok(stream) => stream,
                Err(error) => {
                    log(format!(
                        "TLS ({PAIRING_IDENTITY}): FAILED: {error} ({error:?})"
                    ));
                    return false;
                }
            };
        log(format!(
            "TLS ({PAIRING_IDENTITY}): handshake done in {} ms, {} {}",
            started.elapsed().as_millis(),
            stream.ssl().version_str(),
            stream
                .ssl()
                .current_cipher()
                .map(|c| c.name())
                .unwrap_or("?"),
        ));
        if let Err(error) = stream.write_all(b"CG1 PAIR\n").await {
            log(format!("sending CG1 PAIR failed: {error}"));
            return false;
        }
        let line = read_line(&mut stream).await;
        log(format!("phone said: {line:?}"));
        line.starts_with("C ")
    }

    /// `CG1 DATA <port>` with the pairing's link key; for 8088 also the hello request.
    async fn data_connection(address: SocketAddr, data: &Data) -> bool {
        let started = Instant::now();
        let key = link_key(&data.long_term);
        let mut stream = match wireless_tls::connect(address, &data.identity, key.as_bytes()).await
        {
            Ok(stream) => stream,
            Err(error) => {
                log(format!(
                    "TLS ({}): FAILED: {error} ({error:?})",
                    data.identity
                ));
                return false;
            }
        };
        log(format!(
            "TLS ({}): handshake done in {} ms, {} {}",
            data.identity,
            started.elapsed().as_millis(),
            stream.ssl().version_str(),
            stream
                .ssl()
                .current_cipher()
                .map(|c| c.name())
                .unwrap_or("?"),
        ));
        let mut request = format!("CG1 DATA {}\n", data.port).into_bytes();
        if data.port == 8088 {
            request.extend_from_slice(cg_protocol::phone::HELLO_REQUEST);
        }
        if let Err(error) = stream.write_all(&request).await {
            log(format!("sending the request failed: {error}"));
            return false;
        }
        let mut reply = Vec::new();
        let mut buffer = [0u8; 300];
        let read = tokio::time::timeout(READ, async {
            while reply.len() < 300 {
                match stream.read(&mut buffer[..300 - reply.len()]).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => reply.extend_from_slice(&buffer[..n]),
                }
            }
        })
        .await;
        if read.is_err() {
            log("reply: no more within 10 s");
        }
        log(format!(
            "reply ({} bytes): {:?}",
            reply.len(),
            String::from_utf8_lossy(&reply)
        ));
        !reply.is_empty()
    }

    /// One line (without the newline), at most 512 bytes, within 10 s.
    async fn read_line(stream: &mut wireless_tls::TlsStream) -> String {
        let mut line = Vec::new();
        let result = tokio::time::timeout(READ, async {
            let mut byte = [0u8; 1];
            while line.len() < 512 {
                match stream.read(&mut byte).await {
                    Ok(0) => return "closed by the phone",
                    Ok(_) if byte[0] == b'\n' => return "",
                    Ok(_) => line.push(byte[0]),
                    Err(_) => return "read error",
                }
            }
            "line too long"
        })
        .await;
        let note = result.unwrap_or("no line within 10 s");
        let text = String::from_utf8_lossy(&line).into_owned();
        if note.is_empty() {
            text
        } else {
            format!("{text} [{note}]")
        }
    }
}
