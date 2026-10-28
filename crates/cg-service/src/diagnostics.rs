//! The diagnostics bundle, built by the service: the environment summary, the connection
//! timeline and CellGlide's own journald lines, redacted for the person who asked.
//! The service reads the journal (it is in the `systemd-journal` group) so the user's own
//! account needs no extra access; the client only shows and uploads what comes back.

use cg_core::health::log::HealthLog;
use cg_diag::bundle::{DiagnosticsBundle, MAX_LOG_LINES, sessions};
use cg_ipc::phone::{PhoneCard, model_name};
use cg_ipc::{Check, ConnState, Contact, DiagRange, DnsMode, Reason, Snapshot};
use jiff::{SignedDuration, Timestamp};
use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::Semaphore;

const JOURNAL_TIMEOUT: Duration = Duration::from_secs(10);
/// Longest journal line kept before the bundle's own caps apply; the rest is dropped.
const JOURNAL_LINE_BYTES: usize = 8 * 1024;
const STDERR_BYTES: u64 = 2048;
const COMPUTER: &str = "computer";
const USER: &str = "User";

/// Where the bundle's facts come from; tests point these at fakes.
pub struct Sources {
    /// The journal command and its leading arguments; the range start is appended.
    pub journal: Vec<String>,
    /// The file system root for `etc/os-release`, `proc/…` and `sys/…`.
    pub root: PathBuf,
    pub health: Option<HealthLog>,
    pub hostname: Option<String>,
}

impl Sources {
    pub fn system() -> Sources {
        Sources {
            journal: [
                "journalctl",
                "-u",
                "cellglide.service",
                "-o",
                "short-iso",
                "--no-pager",
                "-q",
                // journalctl reads only what the bundle keeps, however busy the range was.
                "-n",
                "2000",
                "--since",
            ]
            .map(String::from)
            .to_vec(),
            root: "/".into(),
            health: None,
            hostname: nix::unistd::gethostname()
                .ok()
                .and_then(|h| h.into_string().ok()),
        }
    }
}

/// One bundle at a time for the whole service: each reads up to a week of journal and
/// redacts it. A second request waits for the first.
static BUILDING: Semaphore = Semaphore::const_new(1);

/// The whole bundle as the JSON text the client shows and uploads. `requester` is the uid
/// of the process on the other end of the control socket: that user's login and full name
/// are replaced, along with the machine's host name.
pub async fn build(
    src: &Sources,
    range: DiagRange,
    contact: Option<Contact>,
    snapshot: &Snapshot,
    checks: &[Check],
    requester: Option<u32>,
    now: Timestamp,
) -> Result<String, Reason> {
    // Never closed, so acquiring only waits.
    let _turn = BUILDING.acquire().await.map_err(internal)?;
    let span = match range {
        DiagRange::Day => SignedDuration::from_hours(24),
        DiagRange::Week => SignedDuration::from_hours(24 * 7),
    };
    let from = now.checked_sub(span).unwrap_or(now);

    let mut environment = environment(snapshot, checks, &src.root);
    let events = match src.health.clone() {
        Some(log) => tokio::task::spawn_blocking(move || log.events_between(from, now))
            .await
            .unwrap_or_default(),
        None => Vec::new(),
    };
    let timeline: Vec<String> = events
        .iter()
        .filter_map(|e| serde_json::to_string(e).ok())
        .collect();
    let found = sessions(&events);

    let log = match read_journal(&src.journal, from).await {
        Ok(lines) => {
            let note = if lines.is_empty() {
                "ok, no lines (or the service may not be allowed to read the journal)".to_string()
            } else {
                format!("ok, {} lines", lines.len())
            };
            environment.insert("Journal".into(), note);
            lines
        }
        Err(error) => {
            environment.insert("Journal".into(), format!("unavailable: {error}"));
            Vec::new()
        }
    };

    let user = requester.and_then(|uid| {
        nix::unistd::User::from_uid(nix::unistd::Uid::from_raw(uid))
            .ok()
            .flatten()
    });
    let (names, user_name) = names(src.hostname.as_deref(), user.as_ref());

    let mut bundle = DiagnosticsBundle::new(
        "linux",
        now,
        env!("CARGO_PKG_VERSION").to_string(),
        os_line(&src.root),
        device(&src.root),
        from,
        now,
        environment,
        Some(timeline),
        None,
        log,
    );
    bundle.contact = contact.filter(|c| c.email.is_some() || c.note.is_some());
    bundle.sessions = Some(found).filter(|s| !s.is_empty());
    // Redacting a big journal is CPU work: keep it off the async threads.
    let prepared = tokio::task::spawn_blocking(move || {
        bundle.prepared(&names, user_name.as_deref()).encoded()
    })
    .await
    .map_err(internal)?;
    Ok(String::from_utf8_lossy(&prepared).into_owned())
}

/// A failure inside the service (a panicked or cancelled blocking task) as the client's reply.
fn internal(error: impl std::fmt::Display) -> Reason {
    tracing::error!("building diagnostics: {error}");
    Reason::Internal {
        detail: "diagnostics".into(),
    }
}

/// What to replace and with what: the host name (short and as given) becomes `computer`,
/// the requester's full name from the account's comment field becomes `User`, and the
/// requester's login name is passed on separately for `/home/<name>` and plain mentions.
pub fn names(
    hostname: Option<&str>,
    requester: Option<&nix::unistd::User>,
) -> (BTreeMap<String, String>, Option<String>) {
    let mut names = BTreeMap::new();
    if let Some(host) = hostname.map(str::trim).filter(|h| !h.is_empty()) {
        names.insert(host.to_string(), COMPUTER.to_string());
        // "alice-thinkpad.local" or "box.example.com" is also written as just "alice-thinkpad".
        if let Some(short) = host.split('.').next().filter(|s| !s.is_empty()) {
            names.insert(short.to_string(), COMPUTER.to_string());
        }
    }
    let mut user_name = None;
    if let Some(user) = requester {
        if let Some(full) = user.gecos.to_str().ok().and_then(|g| g.split(',').next()) {
            let full = full.trim();
            if !full.is_empty() {
                names.insert(full.to_string(), USER.to_string());
            }
        }
        user_name = Some(user.name.clone()).filter(|n| !n.is_empty());
    }
    (names, user_name)
}

/// The facts about this machine and the service's state, as English support data.
pub fn environment(snapshot: &Snapshot, checks: &[Check], root: &Path) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    map.insert("Distribution".into(), distribution(root));
    map.insert("Kernel".into(), kernel(root));
    map.insert("CellGlide".into(), env!("CARGO_PKG_VERSION").into());
    map.insert("State".into(), state_text(&snapshot.state));
    let chosen = snapshot.settings.chosen_serial.as_deref().unwrap_or("auto");
    map.insert(
        "Settings".into(),
        format!(
            "auto connect {}, phone {chosen}",
            if snapshot.settings.auto_connect {
                "on"
            } else {
                "off"
            }
        ),
    );
    let health = snapshot
        .health
        .cause
        .and_then(|c| serde_json::to_value(c).ok())
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| "ok".into());
    map.insert("Health".into(), health);
    if !checks.is_empty() {
        let text = checks
            .iter()
            .map(|c| {
                let id = serde_json::to_value(c.id)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_string))
                    .unwrap_or_default();
                format!("{id} {}", if c.ok { "ok" } else { "fail" })
            })
            .collect::<Vec<_>>()
            .join(", ");
        map.insert("Checks".into(), text);
    }
    for (key, value) in phone_rows(snapshot.phone.as_ref()) {
        map.insert(key, value);
    }
    map
}

fn state_text(state: &ConnState) -> String {
    match state {
        ConnState::NoUsbService => "no usb service".into(),
        ConnState::NoPhone => "no phone".into(),
        ConnState::ChoosePhone => "choose phone".into(),
        ConnState::Idle => "idle".into(),
        ConnState::Connecting => "connecting".into(),
        ConnState::Connected { phone_version, dns } => {
            let dns = match dns {
                DnsMode::Phone => "phone",
                DnsMode::Public => "public",
                DnsMode::Unmanaged => "unmanaged",
            };
            format!("connected (phone {phone_version}, dns {dns})")
        }
        ConnState::Retrying { reason } => {
            let key = reason.key();
            let code = key.strip_prefix("reason.").unwrap_or(key);
            match reason {
                Reason::Tunnel { detail } | Reason::Internal { detail } => {
                    format!("retrying ({code}: {detail})")
                }
                _ => format!("retrying ({code})"),
            }
        }
    }
}

/// The Mac's `diagnosticsSummary` phone rows plus `PhoneSummary.rows`: "Phone" (carrier and radio
/// from the latest frame, or "not connected" without one), then the USB reading's "iPhone",
/// "SIMs" (with the carrier data is on) and "Baseband". Rows and parts of rows that cannot be
/// read are left out, never shown as "?"; a frame without carrier or radio says so in words.
/// Never the phone's name or any SIM identifier. No card at all means no rows.
fn phone_rows(card: Option<&PhoneCard>) -> Vec<(String, String)> {
    let Some(card) = card else {
        return Vec::new();
    };
    let mut rows = Vec::new();
    rows.push((
        "Phone".to_string(),
        match &card.frame {
            Some(frame) => {
                let parts: Vec<&str> = [frame.carrier.as_deref(), frame.radio.as_deref()]
                    .into_iter()
                    .flatten()
                    .collect();
                if parts.is_empty() {
                    "sharing, no details".into()
                } else {
                    parts.join(" · ")
                }
            }
            None => "not connected".into(),
        },
    ));
    let Some(usb) = &card.usb else {
        return rows;
    };
    if let Some(model) = &usb.model {
        let mut text = format!("{} ({model})", model_name(model));
        if let Some(os) = &usb.os_version {
            text.push_str(&format!(" · iOS {os}"));
        }
        rows.push(("iPhone".into(), text));
    }
    // Each SIM as its carrier bundle and MCC-MNC, whichever can be read.
    let sims: Vec<String> = usb
        .sims
        .iter()
        .filter_map(|sim| {
            let bundle = sim
                .bundle
                .as_deref()
                .map(|b| b.strip_prefix("com.apple.").unwrap_or(b));
            let code = sim
                .mcc
                .as_deref()
                .zip(sim.mnc.as_deref())
                .map(|(mcc, mnc)| format!("{mcc}-{mnc}"));
            match (bundle, code) {
                (Some(bundle), Some(code)) => Some(format!("{bundle} {code}")),
                (Some(bundle), None) => Some(bundle.to_string()),
                (None, code) => code,
            }
        })
        .collect();
    if !sims.is_empty() {
        let mut text = sims.join(", ");
        if let Some(carrier) = card.frame.as_ref().and_then(|f| f.carrier.as_deref()) {
            text.push_str(&format!(" · data on {carrier}"));
        }
        rows.push(("SIMs".into(), text));
    }
    if let Some(baseband) = &usb.baseband {
        let tray = match usb.sim_tray.as_deref() {
            Some("kCTSIMSupportSIMTrayAbsent") => Some("no SIM tray (eSIM only)"),
            Some("kCTSIMSupportSIMTrayInsertedWithSIM") => Some("physical SIM inserted"),
            Some("kCTSIMSupportSIMTrayInsertedNoSIM") => Some("SIM tray empty"),
            _ => None,
        };
        rows.push((
            "Baseband".into(),
            match tray {
                Some(tray) => format!("{baseband} · {tray}"),
                None => baseband.clone(),
            },
        ));
    }
    rows
}

fn read_trimmed(path: PathBuf) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    Some(text.trim().to_string()).filter(|t| !t.is_empty())
}

fn distribution(root: &Path) -> String {
    ["etc/os-release", "usr/lib/os-release"]
        .iter()
        .filter_map(|p| std::fs::read_to_string(root.join(p)).ok())
        .find_map(|text| {
            text.lines()
                .find_map(|l| l.strip_prefix("PRETTY_NAME="))
                .map(|v| v.trim().trim_matches(['"', '\'']).to_string())
                .filter(|v| !v.is_empty())
        })
        .unwrap_or_else(|| "unknown".into())
}

fn kernel(root: &Path) -> String {
    read_trimmed(root.join("proc/sys/kernel/osrelease")).unwrap_or_else(|| "unknown".into())
}

fn os_line(root: &Path) -> String {
    format!("{} (Linux {})", distribution(root), kernel(root))
}

fn device(root: &Path) -> String {
    read_trimmed(root.join("sys/class/dmi/id/product_name")).unwrap_or_else(|| "unknown".into())
}

/// The journal lines since `from`, the newest `MAX_LOG_LINES`. An error says why none came.
async fn read_journal(command: &[String], from: Timestamp) -> Result<Vec<String>, String> {
    let (program, args) = command.split_first().ok_or("no journal command")?;
    let mut child = Command::new(program)
        .args(args)
        // `@<seconds>` is unambiguous to journalctl in any locale and time zone.
        .arg(format!("@{}", from.as_second()))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| e.to_string())?;
    let stdout = child.stdout.take().ok_or("no stdout")?;
    let stderr = child.stderr.take().ok_or("no stderr")?;
    let work = async {
        let (lines, error) = tokio::join!(tail_lines(stdout), head_text(stderr));
        let status = child.wait().await.map_err(|e| e.to_string())?;
        if status.success() {
            Ok(lines)
        } else {
            let detail = error.lines().next().unwrap_or_default().trim().to_string();
            Err(format!("{status}: {detail}"))
        }
    };
    // Dropping the future on timeout kills the child (kill_on_drop).
    tokio::time::timeout(JOURNAL_TIMEOUT, work)
        .await
        .map_err(|_| "timed out".to_string())?
}

/// The last `MAX_LOG_LINES` lines of a stream, each cut to `JOURNAL_LINE_BYTES`.
async fn tail_lines(stream: impl AsyncRead + Unpin) -> Vec<String> {
    let mut reader = BufReader::new(stream);
    let mut lines = VecDeque::new();
    let mut raw = Vec::new();
    loop {
        raw.clear();
        match reader.read_until(b'\n', &mut raw).await {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        while matches!(raw.last(), Some(b'\n' | b'\r')) {
            raw.pop();
        }
        raw.truncate(JOURNAL_LINE_BYTES);
        let line = String::from_utf8_lossy(&raw).into_owned();
        if line.is_empty() {
            continue;
        }
        if lines.len() == MAX_LOG_LINES {
            lines.pop_front();
        }
        lines.push_back(line);
    }
    lines.into()
}

async fn head_text(stream: impl AsyncRead + Unpin) -> String {
    let mut text = Vec::new();
    let mut limited = stream.take(STDERR_BYTES);
    let _ = limited.read_to_end(&mut text).await;
    // Keep draining so the child never blocks on a full pipe.
    let _ = tokio::io::copy(&mut limited.into_inner(), &mut tokio::io::sink()).await;
    String::from_utf8_lossy(&text).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use cg_ipc::CheckId;
    use cg_ipc::health::{EventKind, HealthEvent};
    use cg_ipc::phone::{SimCarrier, UsbPhone};
    use jiff::tz::TimeZone;

    fn now() -> Timestamp {
        "2026-10-16T04:00:00Z".parse().unwrap()
    }

    fn fake_root() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let write = |path: &str, text: &str| {
            let full = dir.path().join(path);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, text).unwrap();
        };
        write(
            "etc/os-release",
            "NAME=\"Ubuntu\"\nPRETTY_NAME=\"Ubuntu 26.04 LTS\"\nID=ubuntu\n",
        );
        write("proc/sys/kernel/osrelease", "6.17.0-5-generic\n");
        write("sys/class/dmi/id/product_name", "Example Desktop 1000\n");
        dir
    }

    fn journal_line() -> Vec<String> {
        [
            "sh",
            "-c",
            "printf '%s\\n' \"2026-10-16T12:00:00+0800 alice-thinkpad cellglide-service[1]: peer 203.0.113.212\"",
            "sh",
        ]
        .map(String::from)
        .to_vec()
    }

    fn sources(root: &Path, health: Option<HealthLog>) -> Sources {
        Sources {
            journal: journal_line(),
            root: root.to_path_buf(),
            health,
            hostname: Some("alice-thinkpad".into()),
        }
    }

    fn connected_snapshot() -> Snapshot {
        Snapshot {
            state: ConnState::Connected {
                phone_version: "2.2.0".into(),
                dns: DnsMode::Phone,
            },
            ..Snapshot::default()
        }
    }

    fn checks() -> Vec<Check> {
        vec![
            Check::new(CheckId::Usbmuxd, true),
            Check::new(CheckId::Phone, true),
            Check::new(CheckId::Trust, true),
            Check::new(CheckId::Resolved, true),
            Check::new(CheckId::Polkit, true),
            Check::new(CheckId::PolkitRule, true),
            Check::new(CheckId::Tun, false),
        ]
    }

    #[tokio::test]
    async fn bundle_is_linux_redacted_and_complete() {
        let root = fake_root();
        let src = sources(root.path(), None);
        let contact = Contact {
            email: Some("a@b.co".into()),
            note: Some("断网".into()),
        };
        let json = build(
            &src,
            DiagRange::Day,
            Some(contact),
            &connected_snapshot(),
            &checks(),
            None,
            now(),
        )
        .await
        .unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["format"], 1);
        assert_eq!(v["platform"], "linux");
        assert_eq!(v["app"], env!("CARGO_PKG_VERSION"));
        assert_eq!(v["os"], "Ubuntu 26.04 LTS (Linux 6.17.0-5-generic)");
        assert_eq!(v["device"], "Example Desktop 1000");
        // What the user wrote themselves is not redacted.
        assert_eq!(v["contact"]["email"], "a@b.co");
        assert_eq!(
            v["log"][0],
            "2026-10-16T12:00:00+0800 computer cellglide-service[1]: peer 203.x.x.x"
        );
        assert!(
            v["environment"]["State"]
                .as_str()
                .unwrap()
                .contains("connected")
        );
        assert_eq!(
            v["environment"]["Checks"],
            "usbmuxd ok, phone ok, trust ok, resolved ok, polkit ok, polkit_rule ok, tun fail"
        );
        assert!(v["timeline"].is_array());
        assert_eq!(v["to"].as_str().unwrap(), "2026-10-16T04:00:00Z");
        assert!(!json.contains("alice-thinkpad"));
    }

    #[tokio::test]
    async fn missing_journal_is_noted_not_fatal() {
        let root = fake_root();
        let mut src = sources(root.path(), None);
        src.journal = vec!["/nonexistent/journalctl".into()];
        let json = build(
            &src,
            DiagRange::Day,
            None,
            &connected_snapshot(),
            &checks(),
            None,
            now(),
        )
        .await
        .unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["log"].as_array().unwrap().len(), 0);
        assert!(
            v["environment"]["Journal"]
                .as_str()
                .unwrap()
                .starts_with("unavailable")
        );
        assert_eq!(v["platform"], "linux");
    }

    #[tokio::test]
    async fn failing_journal_reports_its_exit() {
        let root = fake_root();
        let mut src = sources(root.path(), None);
        src.journal = ["sh", "-c", "echo denied >&2; exit 1", "sh"]
            .map(String::from)
            .to_vec();
        let json = build(
            &src,
            DiagRange::Day,
            None,
            &connected_snapshot(),
            &[],
            None,
            now(),
        )
        .await
        .unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        let note = v["environment"]["Journal"].as_str().unwrap();
        assert!(
            note.starts_with("unavailable") && note.contains("denied"),
            "{note}"
        );
    }

    #[test]
    fn the_journal_is_asked_for_the_newest_lines_only() {
        let journal = Sources::system().journal;
        let n = journal.iter().position(|a| a == "-n").expect("-n");
        assert_eq!(journal[n + 1], MAX_LOG_LINES.to_string());
        // The range start is appended right after `--since`.
        assert_eq!(journal.last().map(String::as_str), Some("--since"));
    }

    /// Final review Important 3: one build at a time. A journal command that fails when
    /// another copy of it is running shows any overlap; the second request waits.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn builds_run_one_at_a_time() {
        let root = fake_root();
        let locks = tempfile::tempdir().unwrap();
        let lock = locks.path().join("busy");
        let script = format!(
            "mkdir '{0}' || exit 3; sleep 0.3; rmdir '{0}'; echo line",
            lock.display()
        );
        let mut src = sources(root.path(), None);
        src.journal = ["sh", "-c", &script, "sh"].map(String::from).to_vec();
        let src = std::sync::Arc::new(src);
        let one = |src: std::sync::Arc<Sources>| async move {
            let json = build(
                &src,
                DiagRange::Day,
                None,
                &connected_snapshot(),
                &[],
                None,
                now(),
            )
            .await
            .unwrap();
            let v: serde_json::Value = serde_json::from_str(&json).unwrap();
            v["environment"]["Journal"].as_str().unwrap().to_string()
        };
        let (a, b) = tokio::join!(
            tokio::spawn(one(src.clone())),
            tokio::spawn(one(src.clone()))
        );
        assert_eq!(a.unwrap(), "ok, 1 lines");
        assert_eq!(b.unwrap(), "ok, 1 lines");
    }

    #[tokio::test]
    async fn a_panicked_task_is_an_internal_error() {
        let joined = tokio::task::spawn_blocking(|| -> Vec<u8> { panic!("redaction") }).await;
        assert_eq!(
            joined.map_err(internal),
            Err(Reason::Internal {
                detail: "diagnostics".into()
            })
        );
    }

    #[tokio::test]
    async fn only_the_newest_lines_are_kept() {
        let root = fake_root();
        let mut src = sources(root.path(), None);
        src.journal = ["sh", "-c", "seq 1 5000", "sh"].map(String::from).to_vec();
        let lines = read_journal(&src.journal, now()).await.unwrap();
        assert_eq!(lines.len(), MAX_LOG_LINES);
        assert_eq!(lines.last().map(String::as_str), Some("5000"));
        assert_eq!(lines[0], "3001");
    }

    #[tokio::test]
    async fn week_range_reads_seven_days_of_timeline() {
        let root = fake_root();
        let dir = tempfile::tempdir().unwrap();
        let log = HealthLog::new(dir.path().to_path_buf(), TimeZone::UTC);
        let day = SignedDuration::from_hours(24);
        for (ago, detail) in [(8, "too old"), (6, "six days ago"), (0, "now")] {
            let mut event = HealthEvent::new(now() - day * ago, EventKind::VersionChanged);
            event.detail = Some(detail.into());
            log.append(&event).unwrap();
        }
        let src = sources(root.path(), Some(log));
        let timeline_of = |range| {
            let src = &src;
            async move {
                let json = build(src, range, None, &connected_snapshot(), &[], None, now())
                    .await
                    .unwrap();
                serde_json::from_str::<serde_json::Value>(&json).unwrap()
            }
        };
        let week = timeline_of(DiagRange::Week).await;
        assert_eq!(week["from"], "2026-10-09T04:00:00Z");
        let text = week["timeline"].to_string();
        assert!(text.contains("six days ago") && text.contains("now"));
        assert!(!text.contains("too old"));
        let day = timeline_of(DiagRange::Day).await;
        assert_eq!(day["from"], "2026-10-15T04:00:00Z");
        let text = day["timeline"].to_string();
        assert!(text.contains("now") && !text.contains("six days ago"));
    }

    #[test]
    fn environment_summarises_state_settings_health_and_phone() {
        let root = fake_root();
        let mut snapshot = connected_snapshot();
        snapshot.settings.auto_connect = true;
        snapshot.settings.chosen_serial = Some("00008110-001A2B3C4D5E601E".into());
        snapshot.health.cause = Some(cg_ipc::health::IssueCause::PhoneNetwork);
        snapshot.phone = Some(PhoneCard {
            usb: Some(UsbPhone {
                model: Some("iPhone17,5".into()),
                os_version: Some("18.5".into()),
                sims: vec![SimCarrier {
                    bundle: Some("com.apple.CMCC_cn".into()),
                    mcc: Some("460".into()),
                    mnc: Some("00".into()),
                }],
                baseband: Some("1.20.01".into()),
                sim_tray: Some("kCTSIMSupportSIMTrayAbsent".into()),
                battery: None,
            }),
            frame: Some(cg_ipc::phone::PhoneFrame {
                carrier: Some("Cricket".into()),
                radio: Some("5G NSA".into()),
                ..Default::default()
            }),
            ..PhoneCard::default()
        });
        let env = environment(&snapshot, &checks(), root.path());
        assert_eq!(env["Distribution"], "Ubuntu 26.04 LTS");
        assert_eq!(env["Kernel"], "6.17.0-5-generic");
        assert_eq!(env["CellGlide"], env!("CARGO_PKG_VERSION"));
        assert_eq!(env["State"], "connected (phone 2.2.0, dns phone)");
        assert_eq!(
            env["Settings"],
            "auto connect on, phone 00008110-001A2B3C4D5E601E"
        );
        assert_eq!(env["Health"], "phoneNetwork");
        assert_eq!(env["Phone"], "Cricket · 5G NSA");
        assert_eq!(env["iPhone"], "iPhone 16e (iPhone17,5) · iOS 18.5");
        assert_eq!(env["SIMs"], "CMCC_cn 460-00 · data on Cricket");
        assert!(env["SIMs"].ends_with(" · data on Cricket"));
        assert!(env["Baseband"].contains("no SIM tray"));
        let bare = environment(&Snapshot::default(), &[], root.path());
        assert_eq!(bare["Health"], "ok");
        assert_eq!(bare["Settings"], "auto connect on, phone auto");
        assert!(!bare.contains_key("Phone") && !bare.contains_key("Checks"));
        let missing = environment(&Snapshot::default(), &[], Path::new("/nonexistent"));
        assert_eq!(missing["Distribution"], "unknown");
    }

    #[test]
    fn phone_row_without_frame_says_not_connected() {
        let root = fake_root();
        let mut snapshot = connected_snapshot();
        snapshot.phone = Some(PhoneCard {
            usb: Some(UsbPhone {
                model: Some("iPhone17,5".into()),
                ..UsbPhone::default()
            }),
            ..PhoneCard::default()
        });
        let env = environment(&snapshot, &[], root.path());
        assert_eq!(env["Phone"], "not connected");
        assert_eq!(env["iPhone"], "iPhone 16e (iPhone17,5)");
        assert!(!env.contains_key("SIMs") && !env.contains_key("Baseband"));
    }

    #[test]
    fn rows_leave_out_what_cannot_be_read() {
        let root = fake_root();
        let env_with = |carrier: Option<&str>, radio: Option<&str>, sims: Vec<SimCarrier>| {
            let mut snapshot = connected_snapshot();
            snapshot.phone = Some(PhoneCard {
                usb: Some(UsbPhone {
                    sims,
                    ..UsbPhone::default()
                }),
                frame: Some(cg_ipc::phone::PhoneFrame {
                    carrier: carrier.map(Into::into),
                    radio: radio.map(Into::into),
                    ..Default::default()
                }),
                ..PhoneCard::default()
            });
            environment(&snapshot, &[], root.path())
        };
        let sim = |bundle: Option<&str>, mcc: Option<&str>, mnc: Option<&str>| SimCarrier {
            bundle: bundle.map(Into::into),
            mcc: mcc.map(Into::into),
            mnc: mnc.map(Into::into),
        };
        assert_eq!(env_with(Some("Cricket"), None, vec![])["Phone"], "Cricket");
        assert_eq!(env_with(None, Some("5G NSA"), vec![])["Phone"], "5G NSA");
        assert_eq!(env_with(None, None, vec![])["Phone"], "sharing, no details");
        let env = env_with(
            None,
            None,
            vec![
                sim(Some("com.apple.CMCC_cn"), None, Some("00")),
                sim(None, Some("310"), Some("150")),
                sim(None, None, None),
            ],
        );
        assert_eq!(env["SIMs"], "CMCC_cn, 310-150");
        let env = env_with(None, None, vec![sim(None, Some("310"), None)]);
        assert!(!env.contains_key("SIMs"));
        for value in env.values() {
            assert!(!value.contains('?'), "{value}");
        }
    }

    #[test]
    fn the_serial_in_settings_is_shortened_by_redaction() {
        let root = fake_root();
        let mut snapshot = Snapshot::default();
        snapshot.settings.chosen_serial = Some("00008110-001A2B3C4D5E601E".into());
        let mut bundle = DiagnosticsBundle::new(
            "linux",
            now(),
            "1".into(),
            "o".into(),
            "d".into(),
            now(),
            now(),
            environment(&snapshot, &[], root.path()),
            None,
            None,
            vec![],
        );
        bundle.contact = None;
        let prepared = bundle.prepared(&BTreeMap::new(), None);
        assert_eq!(
            prepared.environment["Settings"],
            "auto connect on, phone …601E"
        );
    }

    #[test]
    fn names_replace_host_and_account() {
        let (names, user) = names(Some("alice-thinkpad.local"), None);
        assert_eq!(names["alice-thinkpad.local"], "computer");
        assert_eq!(names["alice-thinkpad"], "computer");
        assert_eq!(user, None);
        let (names, user) = super::names(None, None);
        assert!(names.is_empty() && user.is_none());
    }

    #[test]
    fn names_use_the_requesters_account() {
        let me = nix::unistd::User::from_uid(nix::unistd::getuid())
            .unwrap()
            .expect("the test user has an account");
        let (names, user) = super::names(Some("box"), Some(&me));
        assert_eq!(user.as_deref(), Some(me.name.as_str()));
        assert_eq!(names["box"], "computer");
        if let Some(full) = me.gecos.to_str().unwrap().split(',').next()
            && !full.trim().is_empty()
        {
            assert_eq!(names[full.trim()], "User");
        }
    }

    #[tokio::test]
    async fn the_requesters_login_is_replaced_in_the_log() {
        let me = nix::unistd::User::from_uid(nix::unistd::getuid())
            .unwrap()
            .unwrap();
        if me.name.len() < 3 || ["root", "user", "admin"].contains(&me.name.as_str()) {
            return; // too generic to replace by design
        }
        let root = fake_root();
        let mut src = sources(root.path(), None);
        src.journal = vec![
            "sh".into(),
            "-c".into(),
            format!(
                "echo 'Oct 16 sudo: {} : opened /home/{}/x'",
                me.name, me.name
            ),
            "sh".into(),
        ];
        let json = build(
            &src,
            DiagRange::Day,
            None,
            &connected_snapshot(),
            &[],
            Some(me.uid.as_raw()),
            now(),
        )
        .await
        .unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        let line = v["log"][0].as_str().unwrap();
        assert!(!line.contains(&me.name), "{line}");
        assert!(line.contains("/home/~/x"));
    }
}
