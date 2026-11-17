//! cellglide: control the CellGlide service from a terminal.

mod out;
mod speedtest;
mod wireless;

mod cli {
    use crate::out::{out, outln};
    use crate::wireless;
    use anyhow::{Result, bail};
    use cg_i18n::tr;
    use cg_ipc::client::{self, ClientError};
    use cg_ipc::{
        Check, CheckId, ConnState, Endpoint, PhoneChoice, Reason, Request, Response, describe,
    };
    use clap::{Parser, Subcommand};
    use std::path::PathBuf;
    use std::time::Duration;

    #[derive(Parser)]
    #[command(
        name = "cellglide",
        version,
        about = "CellGlide: use the iPhone's cellular network over USB"
    )]
    struct Cli {
        /// Control socket of cellglide-service.
        #[arg(long, default_value_t = Endpoint::system().to_string())]
        socket: String,
        #[command(subcommand)]
        command: Cmd,
    }

    impl Cli {
        fn endpoint(&self) -> Endpoint {
            Endpoint::parse(&self.socket)
        }
    }

    #[derive(Subcommand)]
    enum Cmd {
        /// Show the connection state
        Status,
        /// Start using the phone's network
        Connect {
            /// The serial of the iPhone to use when several are plugged in
            #[arg(long, conflicts_with = "wireless")]
            serial: Option<String>,
            /// Windows: connect over Wi-Fi, without a cable
            #[arg(long, hide = cfg!(not(windows)))]
            wireless: bool,
        },
        /// Windows: the iPhones paired for wireless connection
        #[command(hide = cfg!(not(windows)))]
        Wireless {
            #[command(subcommand)]
            command: WirelessCmd,
        },
        /// Stop using the phone's network
        Disconnect,
        /// Check the setup and suggest fixes
        Doctor,
        /// Follow state changes
        Watch,
        /// Show the connection history of one of the last 7 days (default: today)
        History {
            /// The day to show, YYYY-MM-DD
            #[arg(long, conflicts_with = "days_ago")]
            day: Option<String>,
            /// The day to show, counted back from today (1 = yesterday)
            #[arg(long, value_parser = clap::value_parser!(u8).range(0..7))]
            days_ago: Option<u8>,
        },
        /// Build the diagnostics bundle: show it, save it, or send it to us
        Diagnostics {
            /// Cover the last 7 days instead of the last 24 hours
            #[arg(long)]
            week: bool,
            /// Write the bundle to this file (on Linux, mode 0600) instead of printing it
            #[arg(long, value_name = "FILE")]
            save: Option<PathBuf>,
            /// Upload the bundle after showing it and asking for confirmation
            #[arg(long)]
            send: bool,
            /// Your email address, if you want a reply
            #[arg(long)]
            email: Option<String>,
            /// A note for us, e.g. what went wrong
            #[arg(long)]
            note: Option<String>,
            /// With --send: send the bundle without asking (required when not running in a
            /// terminal). Long form only, so it is never typed by accident.
            #[arg(long)]
            yes: bool,
        },
        /// Check which DNS servers this computer's lookups really go through
        DnsLeak,
        /// Measure the phone's speed, and optionally this computer's Wi-Fi or Ethernet to compare
        Speedtest {
            /// Also measure this computer's Wi-Fi or Ethernet, after the phone
            #[arg(long)]
            compare: bool,
            /// Skip the phone data warning (required when not running in a terminal)
            #[arg(long, short = 'y')]
            yes: bool,
            /// Show the saved results and exit
            #[arg(long)]
            history: bool,
        },
        /// Show or change settings
        Settings {
            /// on or off
            #[arg(long, value_parser = ["on", "off"])]
            auto_connect: Option<String>,
            /// "auto", or the serial of the iPhone to use
            #[arg(long)]
            phone: Option<String>,
            /// Windows: on or off. Blocks DNS queries to other networks while connected
            #[arg(long, value_parser = ["on", "off"], hide = cfg!(not(windows)))]
            strict_dns: Option<String>,
            /// Windows: on or off. Lets the iPhone connect over Wi-Fi when there is no cable
            #[arg(long, value_parser = ["on", "off"], hide = cfg!(not(windows)))]
            wireless: Option<String>,
        },
    }

    #[derive(Subcommand)]
    enum WirelessCmd {
        /// List the paired iPhones
        List,
        /// Remove a pairing (by its number in the list, or its name)
        Forget {
            /// The number from `wireless list`, or the iPhone's name
            target: String,
            /// Don't ask (required when not running in a terminal)
            #[arg(long)]
            yes: bool,
        },
        /// Pair an iPhone again with a code, then connect to it
        Repair {
            /// The number from `wireless list`, or the iPhone's name
            target: String,
        },
        /// Choose a new network name and password
        Regenerate {
            /// Don't ask (required when not running in a terminal)
            #[arg(long)]
            yes: bool,
        },
    }

    fn on_off(on: bool) -> String {
        tr(if on { "cli.on" } else { "cli.off" }, &[])
    }

    fn print_settings(settings: &cg_ipc::Settings) {
        outln!(
            "{}",
            tr(
                "cli.settings_auto_connect",
                &[("value", &on_off(settings.auto_connect))]
            )
        );
        #[cfg(windows)]
        outln!(
            "{}",
            tr(
                "cli.settings_strict_dns",
                &[("value", &on_off(settings.strict_dns))]
            )
        );
        #[cfg(windows)]
        outln!(
            "{}",
            tr(
                "cli.settings_wireless",
                &[("value", &on_off(settings.wireless_enabled))]
            )
        );
        let phone = settings
            .chosen_serial
            .clone()
            .unwrap_or_else(|| tr("cli.phone_auto", &[]));
        outln!("{}", tr("cli.settings_phone", &[("value", &phone)]));
    }

    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    /// Under sudo the diagnostics only hide root's user name, not the invoking user's.
    fn sudo_hint(euid_is_root: bool, sudo_user: Option<&str>) -> Option<String> {
        (euid_is_root && sudo_user.is_some_and(|u| !u.is_empty()))
            .then(|| tr("diag.sudo_hint", &[]))
    }

    fn print_snapshot(snapshot: &cg_ipc::Snapshot) {
        outln!("{}", describe::state_line(snapshot));
        for line in wireless::status_lines(snapshot) {
            outln!("{line}");
        }
        if let Some(line) = describe::strict_dns_line(snapshot) {
            outln!("{line}");
        }
        if let Some(card) = &snapshot.phone {
            for line in describe::phone_lines(card) {
                outln!("{line}");
            }
        }
        if let Some(line) = describe::status_health_line(
            snapshot,
            jiff::Timestamp::now(),
            &jiff::tz::TimeZone::system(),
        ) {
            outln!("{line}");
        }
    }

    /// `--days-ago N` as a date in this machine's zone, the zone the service names its days in.
    fn days_ago(n: u8) -> Option<String> {
        let today = jiff::Zoned::now().date();
        today
            .checked_sub(jiff::Span::new().days(i64::from(n)))
            .ok()
            .map(|d| d.to_string())
    }

    fn unreachable(error: &ClientError) -> anyhow::Error {
        anyhow::anyhow!("{}", describe::unreachable(error))
    }

    fn version_mismatch(service: &str) -> anyhow::Error {
        anyhow::anyhow!("{}", describe::version_mismatch(service))
    }

    /// Refuse to talk to a service that speaks another protocol version.
    async fn ensure_protocol(cli: &Cli) -> Result<()> {
        hello_verdict(client::request(&cli.endpoint(), &Request::Hello).await)
    }

    fn hello_verdict(reply: std::result::Result<Response, ClientError>) -> Result<()> {
        match reply {
            Ok(Response::Hello { protocol, .. }) if protocol == cg_ipc::PROTOCOL_VERSION => Ok(()),
            Ok(Response::Hello { protocol, .. }) => Err(version_mismatch(&protocol.to_string())),
            // Turned away (Linux: not in the cellglide group; Windows: not an administrator
            // or the active session's user): the same words as when the OS refuses the
            // connection itself, fix included.
            Ok(Response::Error {
                reason: Reason::NotInGroup | Reason::NotAllowed,
            }) => Err(unreachable(&ClientError::Io(
                std::io::ErrorKind::PermissionDenied.into(),
            ))),
            Ok(Response::Error { reason }) => bail!("{}", describe::reason_text(&reason)),
            // A v1 reply this client can parse but doesn't expect; unparsable ones are covered by `unreachable`.
            Ok(_) => Err(version_mismatch("1")),
            Err(error) => Err(unreachable(&error)),
        }
    }

    async fn call(cli: &Cli, request: Request) -> Result<Response> {
        match client::request(&cli.endpoint(), &request).await {
            Ok(Response::Error { reason }) => bail!("{}", describe::reason_text(&reason)),
            Ok(response) => Ok(response),
            Err(error) => Err(unreachable(&error)),
        }
    }

    /// The service the wireless attempt talks to.
    struct Live<'a>(&'a Cli);

    impl wireless::Service for Live<'_> {
        async fn send(&self, request: Request) -> std::result::Result<Response, String> {
            match client::request(&self.0.endpoint(), &request).await {
                Ok(Response::Error { reason }) => Err(describe::reason_text(&reason)),
                Ok(response) => Ok(response),
                Err(error) => Err(describe::unreachable(&error)),
            }
        }
    }

    /// The access point's name and password and the paired iPhones; refuses where wireless
    /// can't work, with the reason.
    async fn wireless_info(cli: &Cli) -> Result<cg_ipc::wireless::WirelessInfo> {
        let Response::Wireless { info } = call(cli, Request::WirelessInfo).await? else {
            bail!("{}", tr("cli.unexpected_reply", &[]));
        };
        match info.available {
            Some(true) => Ok(info),
            Some(false) => bail!(
                "{}",
                describe::wireless_unavailable_text(&info)
                    .unwrap_or_else(|| tr("cli.wireless_unsupported", &[]))
            ),
            None => bail!("{}", tr("cli.wireless_unsupported", &[])),
        }
    }

    /// Starts a wireless attempt with `start` (`ConnectWireless` or `RepairPhone`), shows the
    /// steps, and follows it to the end. Ctrl-C cancels it.
    async fn wireless_attempt(cli: &Cli, start: Request) -> Result<()> {
        use wireless::Service;
        // Listen before asking, so a Ctrl-C in between is not lost.
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                let _ = stop.send(());
            }
        });
        let service = Live(cli);
        call(cli, start).await?;
        // Whatever fails from here on must not leave the access point open.
        let giving_up = async |error: anyhow::Error| {
            let _ = service.send(Request::CancelWireless).await;
            error
        };
        let info = match call(cli, Request::WirelessInfo).await {
            Ok(Response::Wireless { info }) => info,
            Ok(_) => {
                return Err(
                    giving_up(anyhow::anyhow!("{}", tr("cli.unexpected_reply", &[]))).await,
                );
            }
            Err(error) => return Err(giving_up(error).await),
        };
        for line in wireless::intro_lines(&info) {
            outln!("{line}");
        }
        let mut updates = match client::subscribe(&cli.endpoint()).await {
            Ok(updates) => updates,
            Err(error) => return Err(giving_up(unreachable(&error)).await),
        };
        let outcome = wireless::drive(
            &service,
            &wireless::LineAsker::terminal(),
            &mut updates,
            async {
                // Ends only by Ctrl-C; a listener that could not start never fires.
                if stopped.await.is_err() {
                    std::future::pending::<()>().await;
                }
            },
            wireless::LIMIT,
            &mut |line| outln!("{line}"),
        )
        .await;
        match outcome.exit_code() {
            0 => Ok(()),
            code => std::process::exit(code),
        }
    }

    /// A script without --yes must not hang on a prompt, nor change anything: these commands
    /// ask first, so without a terminal they refuse.
    fn needs_yes(command: &Cmd, terminal: bool) -> bool {
        matches!(
            command,
            Cmd::Wireless {
                command: WirelessCmd::Forget { yes: false, .. }
                    | WirelessCmd::Regenerate { yes: false }
            }
        ) && !terminal
    }

    /// `cellglide wireless ...`.
    async fn wireless_command(cli: &Cli, command: &WirelessCmd) -> Result<()> {
        let zone = jiff::tz::TimeZone::system();
        match command {
            WirelessCmd::List => {
                let info = wireless_info(cli).await?;
                for line in wireless::list_lines(&info.paired, &zone) {
                    outln!("{line}");
                }
            }
            WirelessCmd::Forget { target, yes } => {
                let info = wireless_info(cli).await?;
                let pick = wireless::resolve(&info.paired, target);
                if let Some(problem) = wireless::pick_problem(&info.paired, target, &pick, &zone) {
                    bail!("{problem}");
                }
                let wireless::Pick::One(index) = pick else {
                    bail!("{}", tr("cli.unexpected_reply", &[]));
                };
                let phone = &info.paired[index];
                if !*yes
                    && !wireless::ask_yes_no(&tr(
                        "cli.wireless_forget_confirm",
                        &[("name", &phone.name)],
                    ))
                {
                    outln!("{}", tr("cli.wireless_cancelled", &[]));
                    return Ok(());
                }
                call(
                    cli,
                    Request::ForgetPhone {
                        peer_id: phone.peer_id.clone(),
                    },
                )
                .await?;
                outln!("{}", tr("cli.wireless_forgotten", &[("name", &phone.name)]));
            }
            WirelessCmd::Repair { target } => {
                let info = wireless_info(cli).await?;
                let pick = wireless::resolve(&info.paired, target);
                if let Some(problem) = wireless::pick_problem(&info.paired, target, &pick, &zone) {
                    bail!("{problem}");
                }
                let wireless::Pick::One(index) = pick else {
                    bail!("{}", tr("cli.unexpected_reply", &[]));
                };
                let peer_id = info.paired[index].peer_id.clone();
                return wireless_attempt(cli, Request::RepairPhone { peer_id }).await;
            }
            WirelessCmd::Regenerate { yes } => {
                if !*yes
                    && !wireless::ask_yes_no(&format!(
                        "{} {}",
                        tr("wireless.action.regenerate_confirm", &[]),
                        tr("cli.yn", &[])
                    ))
                {
                    outln!("{}", tr("cli.wireless_cancelled", &[]));
                    return Ok(());
                }
                call(cli, Request::RegenerateAccessPoint).await?;
                let info = wireless_info(cli).await?;
                outln!("{}", tr("cli.wireless_new_ssid", &[("ssid", &info.ssid)]));
                outln!(
                    "{}",
                    tr(
                        "cli.wireless_new_passphrase",
                        &[("passphrase", &info.passphrase)]
                    )
                );
            }
        }
        Ok(())
    }

    /// Writes `text` to `path`, readable only by the owner.
    #[cfg(unix)]
    fn save_private(path: &std::path::Path, text: &str) -> std::io::Result<()> {
        use std::io::Write;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        // An existing file keeps its old mode when opened; make it private too.
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        file.write_all(text.as_bytes())
    }

    /// Writes `text` to `path`. Windows has no mode bits: the file inherits its folder's
    /// permissions (a user's own folders are private to them).
    #[cfg(not(unix))]
    fn save_private(path: &std::path::Path, text: &str) -> std::io::Result<()> {
        std::fs::write(path, text)
    }

    /// Makes the console show what we print as UTF-8 (Chinese text, ✓ and ✗), whatever
    /// code page it was set to.
    #[cfg(windows)]
    fn utf8_console() {
        use windows::Win32::System::Console::SetConsoleOutputCP;
        // SAFETY: no preconditions; without a console it fails, which changes nothing.
        let _ = unsafe { SetConsoleOutputCP(65001) };
    }

    /// Asks `question` on stderr and reads one line; only `y` or `Y` is a yes.
    fn confirmed() -> bool {
        use std::io::Write;
        eprint!("{} ", tr("diag.confirm", &[]));
        let _ = std::io::stderr().flush();
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer).is_ok() && matches!(answer.trim(), "y" | "Y")
    }

    /// Sends the exact text the user was shown. Always the production endpoint.
    async fn send_bundle(json: &str) -> Result<()> {
        use cg_diag::upload::{Target, UploadError, upload};
        eprintln!("{}", tr("diag.sending", &[]));
        match upload(json.as_bytes(), &Target::production()).await {
            Ok(code) => {
                outln!("{}", tr("diag.sent", &[("code", &code)]));
                Ok(())
            }
            Err(UploadError::TooFrequent) => bail!("{}", tr("diag.too_frequent", &[])),
            Err(_) => bail!("{}", tr("diag.failed", &[])),
        }
    }

    /// `cellglide speedtest`. Exits 130 after Ctrl-C and 1 when a leg failed.
    async fn speedtest_command(cli: &Cli, compare: bool, yes: bool, history: bool) -> Result<()> {
        use crate::speedtest;
        use cg_tools::speed::session::{history_record, labelled_routes, run_legs};
        use cg_tools::speed::text::{progress_line, verdict_line};
        use cg_tools::speed::{
            HistoryStore, Params, Server, Stage, default_path, measure, needs_confirmation, plan,
        };
        use cg_tools::uplink::physical_uplink;
        use std::io::{IsTerminal, Write};
        use tokio_util::sync::CancellationToken;

        if history {
            let store = HistoryStore::open(default_path());
            for line in speedtest::history_lines(store.records(), &jiff::tz::TimeZone::system()) {
                outln!("{line}");
            }
            return Ok(());
        }
        let snapshot = match call(cli, Request::Status).await? {
            Response::Status { snapshot } => snapshot,
            _ => bail!("{}", tr("cli.unexpected_reply", &[])),
        };
        let connected = matches!(snapshot.state, ConnState::Connected { .. });
        // The bound uplink is a physical Wi-Fi or Ethernet default route; never an unbound socket.
        let Some(plan) = plan(connected, physical_uplink(), compare) else {
            bail!("{}", tr("speed.no_phone", &[]));
        };
        if needs_confirmation(&plan, yes) {
            if !std::io::stdin().is_terminal() {
                bail!("{}", tr("speed.needs_yes", &[]));
            }
            eprint!("{} ", tr("speed.data_warning", &[]));
            let _ = std::io::stderr().flush();
            if !speedtest::confirmed(&mut std::io::stdin().lock()) {
                outln!("{}", tr("speed.cancelled", &[]));
                return Ok(());
            }
        }
        // The comparison was asked for and has no interface: say why before the phone leg starts.
        for line in speedtest::intro_lines(&plan) {
            outln!("{line}");
        }
        let card = snapshot.phone.as_ref().and_then(|c| c.frame.as_ref());
        let carrier = card.and_then(|f| f.carrier.clone());
        let generation = card.and_then(|f| f.radio.clone());

        let cancel = CancellationToken::new();
        tokio::spawn({
            let cancel = cancel.clone();
            async move {
                let _ = tokio::signal::ctrl_c().await;
                cancel.cancel();
            }
        });
        let live = std::io::stderr().is_terminal();
        let run = run_legs(
            labelled_routes(&plan),
            &cancel,
            |route, progress, cancel| {
                measure(
                    route,
                    Server::cloudflare(),
                    Params::default(),
                    progress,
                    cancel,
                )
            },
            |_, label, progress| {
                if live && let Some(line) = progress_line(label, progress) {
                    eprint!("\r{line}\x1b[K");
                    let _ = std::io::stderr().flush();
                }
            },
        )
        .await;
        if live {
            eprint!("\r\x1b[K");
        }
        for leg in &run.legs {
            for line in speedtest::leg_lines(leg) {
                outln!("{line}");
            }
        }
        if run.cancelled {
            outln!("{}", tr("speed.cancelled", &[]));
            std::process::exit(130);
        }
        let download = |phone: bool| {
            run.legs
                .iter()
                .find(|l| {
                    (l.route == cg_tools::speed::Route::Phone) == phone && l.stage == Stage::Done
                })
                .and_then(|l| l.result.download)
        };
        if let Some(line) = verdict_line(download(true), download(false)) {
            outln!("{line}");
        }
        if let Some(record) =
            history_record(&plan, &run, jiff::Timestamp::now(), carrier, generation)
        {
            let mut store = HistoryStore::open(default_path());
            if let Err(error) = store.add(record) {
                eprintln!("{}: {error}", default_path().display());
            }
        }
        if run.legs.iter().any(|l| matches!(l.stage, Stage::Failed(_))) {
            std::process::exit(1);
        }
        outln!("{}", tr("speed.done", &[]));
        Ok(())
    }

    #[tokio::main]
    pub async fn main() -> Result<()> {
        #[cfg(windows)]
        utf8_console();
        cg_i18n::init();
        let cli = Cli::parse();
        // Before any word with the service.
        if needs_yes(
            &cli.command,
            std::io::IsTerminal::is_terminal(&std::io::stdin()),
        ) {
            bail!("{}", tr("cli.wireless_needs_yes", &[]));
        }
        // Doctor must work without a service, and the saved speed results are a local file.
        if !matches!(
            cli.command,
            Cmd::Doctor | Cmd::Speedtest { history: true, .. }
        ) {
            ensure_protocol(&cli).await?;
        }
        match &cli.command {
            Cmd::Status => {
                if let Response::Status { snapshot } = call(&cli, Request::Status).await? {
                    print_snapshot(&snapshot);
                }
            }
            Cmd::Wireless { command } => return wireless_command(&cli, command).await,
            Cmd::Connect { wireless: true, .. } => {
                return wireless_attempt(&cli, Request::ConnectWireless).await;
            }
            Cmd::Connect { serial, .. } => {
                call(
                    &cli,
                    Request::Connect {
                        serial: serial.clone(),
                    },
                )
                .await?;
                let mut updates = client::subscribe(&cli.endpoint())
                    .await
                    .map_err(|e| unreachable(&e))?;
                let waited = tokio::time::timeout(Duration::from_secs(20), async {
                    while let Some(snapshot) = updates.recv().await {
                        if matches!(
                            snapshot.state,
                            ConnState::Connected { .. }
                                | ConnState::Retrying { .. }
                                | ConnState::ChoosePhone
                                | ConnState::NoPhone
                                | ConnState::NoUsbService
                        ) {
                            return Some(snapshot);
                        }
                    }
                    None
                })
                .await;
                match waited {
                    Ok(Some(snapshot)) => print_snapshot(&snapshot),
                    _ => outln!("{}", tr("cli.still_connecting", &[])),
                }
            }
            Cmd::Disconnect => {
                call(&cli, Request::Disconnect).await?;
                outln!("{}", tr("cli.disconnected", &[]));
            }
            Cmd::Doctor => {
                let checks = match client::request(&cli.endpoint(), &Request::Doctor).await {
                    // Windows has no cellglide group (design §6.4).
                    Ok(Response::Doctor { checks }) => [
                        Some(Check::new(CheckId::Service, true)),
                        cfg!(unix).then(|| Check::new(CheckId::Group, true)),
                    ]
                    .into_iter()
                    .flatten()
                    .chain(checks)
                    .collect(),
                    Ok(Response::Error {
                        reason: Reason::NotInGroup,
                    }) => vec![
                        Check::new(CheckId::Service, true),
                        Check::new(CheckId::Group, false),
                    ],
                    Ok(Response::Error { reason }) => {
                        bail!("{}", describe::reason_text(&reason))
                    }
                    Ok(_) => bail!("{}", tr("cli.unexpected_reply", &[])),
                    Err(ClientError::Json(_)) => bail!("{}", describe::version_mismatch("1")),
                    Err(error) => vec![describe::service_check(&error)],
                };
                out!("{}", describe::checks(&checks));
                if checks.iter().any(|c| !c.ok) {
                    std::process::exit(1);
                }
            }
            Cmd::Settings {
                auto_connect,
                phone,
                strict_dns,
                wireless,
            } => {
                if auto_connect.is_some()
                    || phone.is_some()
                    || strict_dns.is_some()
                    || wireless.is_some()
                {
                    // Turning it on tries the access point, which can take over 10 seconds.
                    if wireless.as_deref() == Some("on") {
                        outln!("{}", tr("cli.wireless_checking", &[]));
                    }
                    call(
                        &cli,
                        Request::SetSettings {
                            auto_connect: auto_connect.as_deref().map(|v| v == "on"),
                            phone: phone.as_deref().map(|p| match p {
                                "auto" => PhoneChoice::Auto,
                                serial => PhoneChoice::Serial {
                                    serial: serial.into(),
                                },
                            }),
                            strict_dns: strict_dns.as_deref().map(|v| v == "on"),
                            wireless: wireless.as_deref().map(|v| v == "on"),
                        },
                    )
                    .await?;
                    outln!("{}", tr("cli.settings_saved", &[]));
                }
                if let Response::Status { snapshot } = call(&cli, Request::Status).await? {
                    print_settings(&snapshot.settings);
                }
            }
            Cmd::History {
                day,
                days_ago: back,
            } => {
                let day = day.clone().or_else(|| back.and_then(days_ago));
                if let Response::History { day } = call(&cli, Request::History { day }).await? {
                    for line in describe::history_lines(&day, jiff::Timestamp::now()) {
                        outln!("{line}");
                    }
                }
            }
            Cmd::Diagnostics {
                week,
                save,
                send,
                email,
                note,
                yes,
            } => {
                use std::io::IsTerminal;
                #[cfg(target_os = "linux")]
                {
                    // SAFETY: geteuid has no preconditions and cannot fail.
                    let root = unsafe { libc::geteuid() } == 0;
                    let user = std::env::var("SUDO_USER").ok();
                    if let Some(hint) = sudo_hint(root, user.as_deref()) {
                        eprintln!("{hint}");
                    }
                }
                // Decide before building anything: a script without --yes must not hang on a prompt.
                if *send && !*yes && !std::io::stdin().is_terminal() {
                    bail!("{}", tr("diag.needs_yes", &[]));
                }
                let contact = (email.is_some() || note.is_some()).then(|| cg_ipc::Contact {
                    email: email.clone(),
                    note: note.clone(),
                });
                let range = if *week {
                    cg_ipc::DiagRange::Week
                } else {
                    cg_ipc::DiagRange::Day
                };
                let Response::Diagnostics { json } =
                    call(&cli, Request::Diagnostics { range, contact }).await?
                else {
                    bail!("{}", tr("cli.unexpected_reply", &[]));
                };
                if let Some(path) = save {
                    save_private(path, &json)
                        .map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
                    outln!(
                        "{}",
                        tr("diag.saved", &[("path", &path.display().to_string())])
                    );
                }
                if *send {
                    // Everything that will leave this computer is shown first.
                    eprintln!("{}", tr("diag.about", &[]));
                    eprintln!("{json}");
                    if !*yes && !confirmed() {
                        outln!("{}", tr("diag.cancelled", &[]));
                        return Ok(());
                    }
                    send_bundle(&json).await?;
                } else if save.is_none() {
                    outln!("{json}");
                }
            }
            Cmd::DnsLeak => {
                use cg_tools::ToolError;
                use cg_tools::dns_leak::{self, Verdict};
                use cg_tools::https::Target;
                let sharing = match call(&cli, Request::Status).await? {
                    Response::Status { snapshot } => match snapshot.state {
                        ConnState::Connected { dns, .. } => Some(dns),
                        _ => None,
                    },
                    _ => None,
                };
                outln!("{}", tr("leak.running", &[]));
                let report = match dns_leak::run(dns_leak::system_resolve, &Target::leakcheck())
                    .await
                {
                    Ok(report) => report,
                    Err(ToolError::Status(_)) => bail!("{}", tr("leak.unavailable", &[])),
                    Err(error) => bail!("{}", tr("leak.failed", &[("error", &error.to_string())])),
                };
                let verdict = dns_leak::judge(&report, sharing);
                let (title, detail) = dns_leak::describe(&verdict, cg_i18n::current());
                outln!("\n{title}\n{detail}\n");
                out!("{}", dns_leak::format_report(&report, cg_i18n::current()));
                if matches!(verdict, Verdict::Leak { .. } | Verdict::Unmanaged { .. }) {
                    std::process::exit(1);
                }
            }
            Cmd::Speedtest {
                compare,
                yes,
                history,
            } => return speedtest_command(&cli, *compare, *yes, *history).await,
            Cmd::Watch => {
                let mut updates = client::subscribe(&cli.endpoint())
                    .await
                    .map_err(|e| unreachable(&e))?;
                while let Some(snapshot) = updates.recv().await {
                    print_snapshot(&snapshot);
                }
                outln!("{}", tr("cli.service_stopped", &[]));
            }
        }
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use clap::CommandFactory;

        /// A client the service turns away is told why, not that the service speaks
        /// protocol 1.
        #[test]
        fn a_refused_hello_is_not_a_version_mismatch() {
            let _lang = cg_i18n::lock_for_tests();
            cg_i18n::set(cg_i18n::Lang::En);
            let mismatch = version_mismatch("1").to_string();
            for reason in [Reason::NotAllowed, Reason::NotInGroup] {
                let refused = hello_verdict(Ok(Response::Error { reason })).unwrap_err();
                let text = refused.to_string();
                assert_ne!(text, mismatch);
                #[cfg(windows)]
                assert!(
                    text.contains("Only an administrator")
                        && text.contains("use the account that is signed in"),
                    "{text}"
                );
                #[cfg(unix)]
                assert!(text.contains("cellglide group"), "{text}");
            }
            let other = hello_verdict(Ok(Response::Error {
                reason: Reason::ShuttingDown,
            }))
            .unwrap_err();
            assert_eq!(
                other.to_string(),
                describe::reason_text(&Reason::ShuttingDown)
            );
            assert!(hello_verdict(Ok(Response::Ok)).is_err());
        }

        #[test]
        fn sudo_hint_only_for_sudo() {
            assert!(sudo_hint(true, Some("x")).is_some());
            assert!(sudo_hint(true, None).is_none());
            assert!(sudo_hint(false, Some("x")).is_none());
        }

        #[test]
        fn socket_defaults_to_the_system_endpoint() {
            let cli = Cli::try_parse_from(["cellglide", "status"]).unwrap();
            assert_eq!(cli.endpoint(), Endpoint::system());
            #[cfg(unix)]
            assert_eq!(cli.socket, cg_ipc::SOCKET_PATH);
            #[cfg(windows)]
            assert_eq!(cli.socket, cg_ipc::PIPE_NAME);
            let cli =
                Cli::try_parse_from(["cellglide", "--socket", "/tmp/c.sock", "status"]).unwrap();
            assert_eq!(cli.endpoint(), Endpoint::parse("/tmp/c.sock"));
        }

        #[test]
        fn forget_and_regenerate_refuse_without_a_terminal_unless_told_yes() {
            let cmd = |args: &[&str]| {
                Cli::try_parse_from(std::iter::once("cellglide").chain(args.iter().copied()))
                    .unwrap()
                    .command
            };
            for args in [
                &["wireless", "forget", "1"][..],
                &["wireless", "regenerate"],
            ] {
                assert!(needs_yes(&cmd(args), false), "{args:?}");
                assert!(!needs_yes(&cmd(args), true), "{args:?}");
            }
            for args in [
                &["wireless", "forget", "1", "--yes"][..],
                &["wireless", "regenerate", "--yes"],
                &["wireless", "list"],
                &["wireless", "repair", "1"],
                &["status"],
            ] {
                assert!(!needs_yes(&cmd(args), false), "{args:?}");
            }
        }

        #[test]
        fn the_wireless_arguments_parse() {
            let parse = |args: &[&str]| {
                Cli::try_parse_from(std::iter::once("cellglide").chain(args.iter().copied()))
            };
            let cli = parse(&["connect", "--wireless"]).unwrap();
            assert!(matches!(
                cli.command,
                Cmd::Connect {
                    wireless: true,
                    serial: None
                }
            ));
            assert!(matches!(
                parse(&["connect"]).unwrap().command,
                Cmd::Connect {
                    wireless: false,
                    ..
                }
            ));
            // A cable's serial and the wireless way exclude each other.
            assert!(parse(&["connect", "--wireless", "--serial", "X"]).is_err());

            assert!(matches!(
                parse(&["wireless", "list"]).unwrap().command,
                Cmd::Wireless {
                    command: WirelessCmd::List
                }
            ));
            let Cmd::Wireless {
                command: WirelessCmd::Forget { target, yes },
            } = parse(&["wireless", "forget", "Anna's iPhone", "--yes"])
                .unwrap()
                .command
            else {
                panic!("forget");
            };
            assert_eq!((target.as_str(), yes), ("Anna's iPhone", true));
            assert!(matches!(
                parse(&["wireless", "forget", "2"]).unwrap().command,
                Cmd::Wireless {
                    command: WirelessCmd::Forget { yes: false, .. }
                }
            ));
            assert!(parse(&["wireless", "forget"]).is_err());
            assert!(matches!(
                parse(&["wireless", "repair", "1"]).unwrap().command,
                Cmd::Wireless {
                    command: WirelessCmd::Repair { .. }
                }
            ));
            assert!(matches!(
                parse(&["wireless", "regenerate", "--yes"]).unwrap().command,
                Cmd::Wireless {
                    command: WirelessCmd::Regenerate { yes: true }
                }
            ));
            // Only the long --yes, as for diagnostics, so it is never typed by accident.
            assert!(parse(&["wireless", "regenerate", "-y"]).is_err());
            assert!(parse(&["wireless"]).is_err());

            for (value, on) in [("on", true), ("off", false)] {
                let Cmd::Settings { wireless, .. } =
                    parse(&["settings", "--wireless", value]).unwrap().command
                else {
                    panic!("settings");
                };
                assert_eq!(wireless.as_deref().map(|v| v == "on"), Some(on));
            }
            assert!(parse(&["settings", "--wireless", "maybe"]).is_err());
            assert!(matches!(
                parse(&["settings"]).unwrap().command,
                Cmd::Settings { wireless: None, .. }
            ));
        }

        #[test]
        fn diagnostics_takes_only_the_long_yes() {
            let parse = |args: &[&str]| Cli::try_parse_from(args.iter().copied());
            assert!(parse(&["cellglide", "diagnostics", "--send", "-y"]).is_err());
            assert!(parse(&["cellglide", "diagnostics", "--send", "--yes"]).is_ok());
            assert!(parse(&["cellglide", "speedtest", "-y"]).is_ok());
            let cli = Cli::command();
            let yes = cli
                .find_subcommand("diagnostics")
                .and_then(|d| d.get_arguments().find(|a| a.get_id() == "yes"))
                .expect("--yes");
            assert!(
                yes.get_help()
                    .is_some_and(|h| h.to_string().contains("without asking")),
                "{:?}",
                yes.get_help()
            );
        }
    }
}

fn main() {
    // Print the error as is: no English "Error:" prefix in front of translated text.
    if let Err(error) = cli::main() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
