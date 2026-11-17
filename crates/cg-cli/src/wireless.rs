//! `cellglide connect --wireless` and `cellglide wireless ...`: the parts that need no terminal
//! or service of their own (so the tests can drive them with a fake): what is printed for each
//! snapshot of a wireless attempt, how a name or number picks a paired iPhone, and the loop that
//! waits for the attempt to end, answers the code question and cancels on Ctrl-C.

use cg_i18n::tr;
use cg_ipc::wireless::{PairedPhone, WirelessInfo, WirelessPhaseView, WirelessStatus};
use cg_ipc::{ConnState, Request, Response, Snapshot, describe};
use std::future::Future;
use std::time::Duration;
use tokio::sync::mpsc;

/// The whole attempt may take this long: the service gives up after 300 seconds.
pub const LIMIT: Duration = Duration::from_secs(330);

/// The service, as far as the attempt needs it. Errors are the words to show.
pub trait Service {
    async fn send(&self, request: Request) -> Result<Response, String>;
}

/// Asks the person whether the code on the iPhone is the same.
pub trait Asker {
    /// Whether a person can answer. Without one the question is not asked.
    fn interactive(&self) -> bool;
    /// Shows the question for the code now on screen. What was typed before this is thrown
    /// away: it was not an answer to this question.
    fn prompt(&self);
    /// The answer to the latest question: `Some(matches)` for a line typed, `None` when there
    /// will be none (input ended or failed, e.g. a Ctrl-C interrupting the read). Safe to
    /// drop and call again.
    async fn answer(&self) -> Option<bool>;
}

/// Answers from lines of input that one long-lived reader thread forwards. A new question takes
/// the next line, never a thread of its own: an abandoned read would eat the next answer.
pub struct LineAsker {
    interactive: bool,
    /// Lines typed; `None` ends the input (end of file or a read error).
    lines: tokio::sync::Mutex<mpsc::UnboundedReceiver<Option<String>>>,
}

impl LineAsker {
    pub fn new(interactive: bool, lines: mpsc::UnboundedReceiver<Option<String>>) -> LineAsker {
        LineAsker {
            interactive,
            lines: tokio::sync::Mutex::new(lines),
        }
    }

    /// Standard input. Not a terminal: nothing is read. The reader thread is left behind when
    /// the process exits.
    pub fn terminal() -> LineAsker {
        use std::io::IsTerminal;
        let interactive = std::io::stdin().is_terminal();
        let (tx, rx) = mpsc::unbounded_channel();
        if interactive {
            std::thread::spawn(move || {
                loop {
                    let mut line = String::new();
                    match std::io::stdin().read_line(&mut line) {
                        Ok(n) if n > 0 => {
                            if tx.send(Some(line)).is_err() {
                                break;
                            }
                        }
                        _ => {
                            let _ = tx.send(None);
                            break;
                        }
                    }
                }
            });
        }
        LineAsker::new(interactive, rx)
    }
}

impl Asker for LineAsker {
    fn interactive(&self) -> bool {
        self.interactive
    }

    fn prompt(&self) {
        use std::io::Write;
        if let Ok(mut lines) = self.lines.try_lock() {
            while lines.try_recv().is_ok() {}
        }
        eprint!("{} ", tr("wireless.cli.confirm_prompt", &[]));
        let _ = std::io::stderr().flush();
    }

    async fn answer(&self) -> Option<bool> {
        let line = self.lines.lock().await.recv().await??;
        Some(is_yes(&line))
    }
}

/// Only `y` or `Y` is a yes.
fn is_yes(line: &str) -> bool {
    matches!(line.trim(), "y" | "Y")
}

/// Asks `question` on stderr and reads one line; only `y` or `Y` is a yes.
pub fn ask_yes_no(question: &str) -> bool {
    use std::io::Write;
    eprint!("{question} ");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).is_ok() && is_yes(&line)
}

/// How an attempt ended, as far as this command can tell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ending {
    Connected,
    Failed,
    /// The attempt is gone without a verdict (another connection took over, or it was
    /// cancelled elsewhere).
    Gone,
}

/// What one snapshot adds to what the person has seen.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Progress {
    pub lines: Vec<String>,
    /// A new code is on screen and its question has to be asked.
    pub ask: bool,
    pub ending: Option<Ending>,
}

/// Which step was last printed; a code counts as another step.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Shown {
    Starting,
    Waiting,
    Pairing { code: String, confirmed: bool },
    Connecting,
    Connected,
    Reconnecting,
    Failed,
}

impl Shown {
    fn of(phase: &WirelessPhaseView) -> Shown {
        match phase {
            WirelessPhaseView::Starting => Shown::Starting,
            WirelessPhaseView::Waiting => Shown::Waiting,
            WirelessPhaseView::Pairing {
                code, confirmed, ..
            } => Shown::Pairing {
                code: code.clone(),
                confirmed: *confirmed,
            },
            WirelessPhaseView::Connecting => Shown::Connecting,
            WirelessPhaseView::Connected => Shown::Connected,
            WirelessPhaseView::Reconnecting => Shown::Reconnecting,
            WirelessPhaseView::Failed { .. } => Shown::Failed,
        }
    }
}

/// Turns the snapshots of an attempt into lines: a line for each step reached (not for each
/// snapshot) and each hint once.
#[derive(Default)]
pub struct PhasePrinter {
    shown: Option<Shown>,
    hints: Vec<String>,
    seen: bool,
}

impl PhasePrinter {
    pub fn observe(&mut self, snapshot: &Snapshot) -> Progress {
        let mut progress = Progress::default();
        let Some(status) = &snapshot.wireless else {
            // No status: either the session is up and the attempt's status went with it, or the
            // attempt ended.
            if matches!(snapshot.state, ConnState::Connected { wireless: true, .. }) {
                if self.shown != Some(Shown::Connected) {
                    self.shown = Some(Shown::Connected);
                    progress.lines.push(tr("wireless.line.connected", &[]));
                }
                progress.ending = Some(Ending::Connected);
            } else if self.seen {
                progress.lines.push(tr("cli.wireless_ended", &[]));
                progress.ending = Some(Ending::Gone);
            }
            return progress;
        };
        self.seen = true;
        let shown = Shown::of(&status.phase);
        if self.shown.as_ref() != Some(&shown) {
            match &status.phase {
                WirelessPhaseView::Pairing {
                    confirmed: false, ..
                } => {
                    let text = describe::wireless_phase_text(status);
                    progress.lines.push(text.title);
                    progress.lines.push(text.detail);
                    progress.ask = true;
                }
                WirelessPhaseView::Failed { reason } => {
                    progress.lines.push(describe::wireless_failure_text(reason));
                }
                _ => progress.lines.extend(describe::wireless_line(snapshot)),
            }
            self.shown = Some(shown);
        }
        if has_trouble(status) {
            let detail = describe::wireless_phase_text(status).detail;
            if !self.hints.contains(&detail) {
                self.hints.push(detail.clone());
                progress.lines.push(detail);
            }
        }
        progress.ending = match status.phase {
            WirelessPhaseView::Connected => Some(Ending::Connected),
            WirelessPhaseView::Failed { .. } => Some(Ending::Failed),
            _ => None,
        };
        progress
    }
}

/// Something holds a waiting (or reconnecting) attempt up, and the phase's sentence says what.
fn has_trouble(status: &WirelessStatus) -> bool {
    match status.phase {
        WirelessPhaseView::Waiting => {
            status.name_clash || status.hint.is_some() || status.last_error.is_some()
        }
        WirelessPhaseView::Reconnecting => status.name_clash,
        _ => false,
    }
}

/// The wireless lines of `cellglide status`: the attempt or session, and what holds it up.
pub fn status_lines(snapshot: &Snapshot) -> Vec<String> {
    let mut lines = Vec::new();
    match (&snapshot.wireless, &snapshot.state) {
        (Some(status), _) => {
            lines.extend(describe::wireless_line(snapshot));
            if has_trouble(status) {
                lines.push(describe::wireless_phase_text(status).detail);
            }
        }
        (None, ConnState::Connected { wireless: true, .. }) => {
            lines.push(tr("wireless.line.connected", &[]));
        }
        (None, _) => {}
    }
    lines
}

/// The three steps (with the network name and password) and the note, as shown right after
/// the attempt starts.
pub fn intro_lines(info: &WirelessInfo) -> Vec<String> {
    let mut lines: Vec<String> = describe::wireless_steps(&info.ssid, &info.passphrase)
        .into_iter()
        .enumerate()
        .map(|(i, step)| format!("{}. {step}", i + 1))
        .collect();
    lines.push(describe::wireless_note());
    lines
}

/// How an attempt ended from here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Connected,
    Failed,
    Gone,
    /// Ctrl-C: the attempt was cancelled.
    Cancelled,
    /// 330 seconds passed: the attempt was cancelled.
    TimedOut,
    /// A code needs an answer and nobody can give one here: the attempt was cancelled.
    NoTerminal,
    ServiceStopped,
}

impl Outcome {
    pub fn exit_code(self) -> i32 {
        match self {
            Outcome::Connected => 0,
            Outcome::Cancelled => 130,
            Outcome::Failed
            | Outcome::Gone
            | Outcome::TimedOut
            | Outcome::NoTerminal
            | Outcome::ServiceStopped => 1,
        }
    }
}

/// Follows the attempt until it ends. `emit` prints a line. Ctrl-C (`cancel`) and the time
/// limit send `CancelWireless` first; a blocked question never holds them up.
pub async fn drive<S: Service, A: Asker>(
    service: &S,
    asker: &A,
    updates: &mut mpsc::Receiver<Snapshot>,
    cancel: impl Future<Output = ()>,
    limit: Duration,
    emit: &mut impl FnMut(String),
) -> Outcome {
    let mut printer = PhasePrinter::default();
    let mut pending = false;
    let deadline = tokio::time::sleep(limit);
    tokio::pin!(deadline, cancel);
    loop {
        tokio::select! {
            biased;
            () = &mut cancel => {
                let _ = service.send(Request::CancelWireless).await;
                emit(tr("cli.wireless_cancelled", &[]));
                return Outcome::Cancelled;
            }
            answer = asker.answer(), if pending => {
                pending = false;
                // No answer (input ended or was interrupted) is not a no: nothing is sent.
                if let Some(matches) = answer
                    && let Err(error) = service.send(Request::ConfirmCode { matches }).await
                {
                    emit(tr("cli.wireless_answer_failed", &[("error", &error)]));
                }
            }
            snapshot = updates.recv() => {
                let Some(snapshot) = snapshot else {
                    emit(tr("cli.service_stopped", &[]));
                    return Outcome::ServiceStopped;
                };
                let progress = printer.observe(&snapshot);
                for line in progress.lines {
                    emit(line);
                }
                match progress.ending {
                    Some(Ending::Connected) => return Outcome::Connected,
                    Some(Ending::Failed) => return Outcome::Failed,
                    Some(Ending::Gone) => return Outcome::Gone,
                    None => {}
                }
                if progress.ask {
                    if !asker.interactive() {
                        emit(tr("wireless.cli.not_terminal", &[]));
                        let _ = service.send(Request::CancelWireless).await;
                        return Outcome::NoTerminal;
                    }
                    asker.prompt();
                    pending = true;
                }
            }
            () = &mut deadline => {
                let _ = service.send(Request::CancelWireless).await;
                emit(tr("cli.wireless_timeout", &[]));
                return Outcome::TimedOut;
            }
        }
    }
}

/// Which paired iPhone a number or name means.
#[derive(Debug, PartialEq, Eq)]
pub enum Pick {
    One(usize),
    /// Indexes of the iPhones the name fits.
    Ambiguous(Vec<usize>),
    NotFound,
}

/// `key` is a number from `wireless list` (counted from 1) or a name: the whole name
/// (ignoring case) if any iPhone has it, else part of a name.
pub fn resolve(paired: &[PairedPhone], key: &str) -> Pick {
    let key = key.trim();
    if let Ok(number) = key.parse::<usize>()
        && (1..=paired.len()).contains(&number)
    {
        return Pick::One(number - 1);
    }
    let wanted = key.to_lowercase();
    if wanted.is_empty() {
        return Pick::NotFound;
    }
    let by = |fits: &dyn Fn(&str) -> bool| -> Vec<usize> {
        paired
            .iter()
            .enumerate()
            .filter(|(_, phone)| fits(&phone.name.to_lowercase()))
            .map(|(i, _)| i)
            .collect()
    };
    let mut found = by(&|name| name == wanted);
    if found.is_empty() {
        found = by(&|name| name.contains(&wanted));
    }
    match found.len() {
        0 => Pick::NotFound,
        1 => Pick::One(found[0]),
        _ => Pick::Ambiguous(found),
    }
}

/// "1. Test iPhone · Paired with a code · 2025-11-13".
pub fn list_line(number: usize, phone: &PairedPhone, zone: &jiff::tz::TimeZone) -> String {
    let created = i64::try_from(phone.created_at)
        .ok()
        .and_then(|secs| jiff::Timestamp::from_second(secs).ok())
        .map(|at| at.to_zoned(zone.clone()).date().to_string());
    let mut line = format!("{number}. {}", describe::paired_phone_line(phone));
    if let Some(date) = created {
        line.push_str(" · ");
        line.push_str(&date);
    }
    line
}

/// `wireless list`: every paired iPhone, numbered from 1.
pub fn list_lines(paired: &[PairedPhone], zone: &jiff::tz::TimeZone) -> Vec<String> {
    if paired.is_empty() {
        return vec![tr("cli.wireless_none", &[])];
    }
    paired
        .iter()
        .enumerate()
        .map(|(i, phone)| list_line(i + 1, phone, zone))
        .collect()
}

/// Why `key` picked no single iPhone, with the ones it fits when there are several; `None`
/// when it picked one.
pub fn pick_problem(
    paired: &[PairedPhone],
    key: &str,
    pick: &Pick,
    zone: &jiff::tz::TimeZone,
) -> Option<String> {
    match pick {
        Pick::One(_) => None,
        Pick::NotFound => Some(tr("cli.wireless_no_such", &[("key", key)])),
        Pick::Ambiguous(matches) => {
            let mut lines = vec![tr("cli.wireless_ambiguous", &[("key", key)])];
            lines.extend(matches.iter().map(|&i| list_line(i + 1, &paired[i], zone)));
            Some(lines.join("\n"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cg_ipc::wireless::{PairingMethod, WirelessFailure};
    use std::sync::Mutex;

    fn english() -> impl Sized {
        let guard = cg_i18n::lock_for_tests();
        cg_i18n::set(cg_i18n::Lang::En);
        guard
    }

    fn status(phase: WirelessPhaseView) -> WirelessStatus {
        WirelessStatus {
            phase,
            ssid: Some("CellGlide-AB12".into()),
            weak: false,
            name_clash: false,
            last_error: None,
            hint: None,
        }
    }

    fn snap(status: WirelessStatus) -> Snapshot {
        Snapshot {
            wireless: Some(status),
            ..Snapshot::default()
        }
    }

    fn pairing(confirmed: bool) -> WirelessPhaseView {
        WirelessPhaseView::Pairing {
            code: "384271".into(),
            phone_name: None,
            confirmed,
        }
    }

    fn phone(id: &str, name: &str) -> PairedPhone {
        PairedPhone {
            peer_id: id.into(),
            name: name.into(),
            method: PairingMethod::Code,
            stale: false,
            created_at: 1_763_000_000,
        }
    }

    /// A service that records what it was sent.
    #[derive(Default)]
    struct Fake {
        sent: Mutex<Vec<Request>>,
    }

    impl Service for Fake {
        async fn send(&self, request: Request) -> Result<Response, String> {
            self.sent.lock().unwrap().push(request);
            Ok(Response::Ok)
        }
    }

    fn channel(snapshots: Vec<Snapshot>) -> (mpsc::Sender<Snapshot>, mpsc::Receiver<Snapshot>) {
        let (tx, rx) = mpsc::channel(32);
        for snapshot in snapshots {
            tx.try_send(snapshot).unwrap();
        }
        (tx, rx)
    }

    /// One more thing to happen at a given time: a snapshot, or a line typed (`None`: the
    /// input ended or failed).
    enum Step {
        Snap(Box<Snapshot>),
        Line(Option<&'static str>),
    }

    fn shown(snapshot: Snapshot) -> Step {
        Step::Snap(Box::new(snapshot))
    }

    type Lines = mpsc::UnboundedSender<Option<String>>;

    /// A person at a terminal (or not) whose lines come from the sender.
    fn person(interactive: bool) -> (LineAsker, Lines) {
        let (tx, rx) = mpsc::unbounded_channel();
        (LineAsker::new(interactive, rx), tx)
    }

    /// Runs `drive` while a task plays the timeline (milliseconds from the start) into it.
    async fn run(
        service: &Fake,
        (asker, lines): (LineAsker, Lines),
        timeline: Vec<(u64, Step)>,
        cancel: impl Future<Output = ()>,
        limit: Duration,
    ) -> (Outcome, Vec<String>) {
        let (snaps, mut rx) = mpsc::channel(32);
        let player = tokio::spawn(async move {
            let start = tokio::time::Instant::now();
            for (at, step) in timeline {
                tokio::time::sleep_until(start + Duration::from_millis(at)).await;
                match step {
                    Step::Snap(snapshot) => snaps.send(*snapshot).await.unwrap(),
                    Step::Line(line) => lines.send(line.map(str::to_string)).unwrap(),
                }
            }
            // Nothing ends on its own: the senders stay open.
            std::future::pending::<()>().await;
        });
        let mut out = Vec::new();
        let outcome = drive(service, &asker, &mut rx, cancel, limit, &mut |l| {
            out.push(l)
        })
        .await;
        player.abort();
        (outcome, out)
    }

    #[test]
    fn the_printer_prints_each_step_and_hint_once() {
        let _lang = english();
        let hinted = |hint| WirelessStatus {
            hint: Some(hint),
            ..status(WirelessPhaseView::Waiting)
        };
        let sequence = [
            snap(status(WirelessPhaseView::Starting)),
            snap(status(WirelessPhaseView::Waiting)),
            snap(status(WirelessPhaseView::Waiting)),
            snap(hinted(WirelessFailure::NotSharing)),
            snap(hinted(WirelessFailure::NotSharing)),
            snap(status(WirelessPhaseView::Waiting)),
            snap(hinted(WirelessFailure::NotSharing)),
            snap(status(pairing(false))),
            snap(status(pairing(false))),
            snap(status(pairing(true))),
            snap(status(pairing(true))),
            snap(status(WirelessPhaseView::Connecting)),
            snap(status(WirelessPhaseView::Connected)),
        ];
        let mut printer = PhasePrinter::default();
        let mut lines = Vec::new();
        let mut asked = 0;
        let mut endings = Vec::new();
        for snapshot in &sequence {
            let progress = printer.observe(snapshot);
            lines.extend(progress.lines);
            asked += usize::from(progress.ask);
            endings.extend(progress.ending);
        }
        assert_eq!(
            lines,
            [
                "Wireless: opening the network…",
                "Wireless: waiting for the iPhone to join CellGlide-AB12",
                "Open CellGlide on the iPhone and start sharing; if VPN mode is on, turn it off first",
                "Confirm the code 384 271 on the iPhone",
                "Compare it with the number on the iPhone's screen.",
                "Wireless: waiting for the iPhone to confirm the code",
                "Wireless: connecting…",
                "Connected wirelessly · a cable is steadier",
            ]
        );
        assert_eq!(asked, 1);
        assert_eq!(endings, [Ending::Connected]);
    }

    #[test]
    fn a_failure_prints_its_text_and_ends() {
        let _lang = english();
        let mut printer = PhasePrinter::default();
        let failed = snap(status(WirelessPhaseView::Failed {
            reason: WirelessFailure::NoPhone,
        }));
        let progress = printer.observe(&failed);
        assert_eq!(progress.lines, ["No iPhone showed up"]);
        assert_eq!(progress.ending, Some(Ending::Failed));
        // The same text is not printed a second time.
        assert!(printer.observe(&failed).lines.is_empty());
    }

    #[test]
    fn an_attempt_that_vanishes_ends_but_one_not_yet_there_does_not() {
        let _lang = english();
        let mut printer = PhasePrinter::default();
        assert_eq!(printer.observe(&Snapshot::default()), Progress::default());
        printer.observe(&snap(status(WirelessPhaseView::Waiting)));
        let progress = printer.observe(&Snapshot::default());
        assert_eq!(progress.ending, Some(Ending::Gone));
        assert_eq!(progress.lines, ["The wireless attempt ended"]);
        // A wireless session whose status is gone is a connection.
        let up = Snapshot {
            state: ConnState::Connected {
                phone_version: "2.2.0".into(),
                dns: cg_ipc::DnsMode::Phone,
                wireless: true,
            },
            ..Snapshot::default()
        };
        assert_eq!(
            PhasePrinter::default().observe(&up).ending,
            Some(Ending::Connected)
        );
    }

    #[test]
    fn status_shows_the_attempt_and_what_holds_it_up() {
        let _lang = english();
        assert!(status_lines(&Snapshot::default()).is_empty());
        let waiting = snap(status(WirelessPhaseView::Waiting));
        assert_eq!(
            status_lines(&waiting),
            ["Wireless: waiting for the iPhone to join CellGlide-AB12"]
        );
        let held = snap(WirelessStatus {
            hint: Some(WirelessFailure::PairingBusy),
            ..status(WirelessPhaseView::Waiting)
        });
        assert_eq!(status_lines(&held).len(), 2);
        let up = snap(status(WirelessPhaseView::Connected));
        assert_eq!(
            status_lines(&up),
            ["Connected wirelessly · a cable is steadier"]
        );
        let failed = snap(status(WirelessPhaseView::Failed {
            reason: WirelessFailure::Lost,
        }));
        assert_eq!(status_lines(&failed).len(), 1);
    }

    #[test]
    fn the_intro_numbers_the_steps_and_shows_the_password() {
        let _lang = english();
        let info = WirelessInfo {
            available: Some(true),
            unavailable: None,
            ssid: "CellGlide-AB12".into(),
            passphrase: "abcd-efgh-2345".into(),
            paired: Vec::new(),
        };
        let lines = intro_lines(&info);
        assert_eq!(lines.len(), 4);
        assert_eq!(
            lines[0],
            "1. On the iPhone, go to Settings → Wi‑Fi and join CellGlide-AB12 with the password abcd-efgh-2345."
        );
        assert!(lines[1].starts_with("2. ") && lines[2].starts_with("3. "));
        assert!(lines[3].contains("no internet connection"));
    }

    #[test]
    fn a_number_or_a_name_picks_one_iphone() {
        let paired = [
            phone("A", "Anna's iPhone"),
            phone("B", "Ben's iPhone"),
            phone("C", "Anna's iPad"),
        ];
        assert_eq!(resolve(&paired, "2"), Pick::One(1));
        assert_eq!(resolve(&paired, " 3 "), Pick::One(2));
        assert_eq!(resolve(&paired, "ben's iphone"), Pick::One(1));
        assert_eq!(resolve(&paired, "ben"), Pick::One(1));
        assert_eq!(resolve(&paired, "iPad"), Pick::One(2));
        // Out of range, or no such name.
        assert_eq!(resolve(&paired, "4"), Pick::NotFound);
        assert_eq!(resolve(&paired, "0"), Pick::NotFound);
        assert_eq!(resolve(&paired, "Carl"), Pick::NotFound);
        assert_eq!(resolve(&paired, ""), Pick::NotFound);
        assert_eq!(resolve(&[], "1"), Pick::NotFound);
        // Part of a name that fits several is not a pick; neither are two with the same name.
        assert_eq!(resolve(&paired, "anna"), Pick::Ambiguous(vec![0, 2]));
        let twins = [
            phone("A", "iPhone"),
            phone("B", "iPhone"),
            phone("C", "iPhone 15"),
        ];
        assert_eq!(resolve(&twins, "iPhone"), Pick::Ambiguous(vec![0, 1]));
        assert_eq!(resolve(&twins, "15"), Pick::One(2));
        assert_eq!(resolve(&twins, "3"), Pick::One(2));
        // A name that is a number outside the list still works as a name.
        let numbered = [phone("A", "7")];
        assert_eq!(resolve(&numbered, "7"), Pick::One(0));
    }

    #[test]
    fn an_ambiguous_name_lists_the_matches_with_their_numbers() {
        let _lang = english();
        let zone = jiff::tz::TimeZone::UTC;
        let paired = [
            phone("A", "Anna's iPhone"),
            phone("B", "Ben's iPhone"),
            phone("C", "Anna's iPad"),
        ];
        let pick = resolve(&paired, "anna");
        let text = pick_problem(&paired, "anna", &pick, &zone).unwrap();
        assert_eq!(
            text,
            "More than one paired iPhone matches anna. Use its number instead:\n\
             1. Anna's iPhone · Paired with a code · 2025-11-13\n\
             3. Anna's iPad · Paired with a code · 2025-11-13"
        );
        let none = pick_problem(&paired, "Carl", &resolve(&paired, "Carl"), &zone).unwrap();
        assert!(none.contains("No paired iPhone matches Carl"), "{none}");
        assert_eq!(
            pick_problem(&paired, "2", &resolve(&paired, "2"), &zone),
            None
        );
    }

    #[test]
    fn the_list_shows_number_name_method_stale_and_date() {
        let _lang = english();
        let zone = jiff::tz::TimeZone::UTC;
        assert_eq!(
            list_lines(&[], &zone),
            ["No iPhone is paired for wireless connection"]
        );
        let mut stale = phone("B", "Ben's iPhone");
        stale.stale = true;
        stale.method = PairingMethod::Cable;
        assert_eq!(
            list_lines(&[phone("A", "Anna's iPhone"), stale], &zone),
            [
                "1. Anna's iPhone · Paired with a code · 2025-11-13",
                "2. Ben's iPhone · Paired over the cable · Needs pairing again · 2025-11-13",
            ]
        );
    }

    fn code(code: &str, confirmed: bool) -> WirelessPhaseView {
        WirelessPhaseView::Pairing {
            code: code.into(),
            phone_name: None,
            confirmed,
        }
    }

    #[tokio::test]
    async fn without_a_terminal_the_code_cancels_the_attempt() {
        let _lang = english();
        let service = Fake::default();
        let (outcome, lines) = run(
            &service,
            person(false),
            vec![(0, shown(snap(status(pairing(false)))))],
            std::future::pending(),
            LIMIT,
        )
        .await;
        assert_eq!(outcome, Outcome::NoTerminal);
        assert_eq!(outcome.exit_code(), 1);
        // Not a "no" to the iPhone: the attempt is cancelled.
        assert_eq!(*service.sent.lock().unwrap(), [Request::CancelWireless]);
        let note = lines.last().unwrap();
        assert!(
            note.starts_with("Can't ask here") && note.contains("cancelled"),
            "{lines:?}"
        );
    }

    #[tokio::test]
    async fn a_yes_is_sent_as_a_match_and_earlier_typing_is_not_an_answer() {
        let _lang = english();
        let service = Fake::default();
        let (outcome, lines) = run(
            &service,
            person(true),
            vec![
                // Typed before the question was shown.
                (0, Step::Line(Some("n\n"))),
                (0, shown(snap(status(pairing(false))))),
                (40, Step::Line(Some("y\n"))),
                (80, shown(snap(status(pairing(true))))),
                (120, shown(snap(status(WirelessPhaseView::Connected)))),
            ],
            std::future::pending(),
            LIMIT,
        )
        .await;
        assert_eq!(outcome, Outcome::Connected);
        assert_eq!(outcome.exit_code(), 0);
        assert_eq!(
            *service.sent.lock().unwrap(),
            [Request::ConfirmCode { matches: true }]
        );
        assert!(!lines.iter().any(|l| l.starts_with("Can't ask here")));
    }

    #[tokio::test]
    async fn an_answer_typed_after_the_code_changed_belongs_to_the_new_code() {
        let _lang = english();
        let service = Fake::default();
        let (outcome, lines) = run(
            &service,
            person(true),
            vec![
                (0, shown(snap(status(code("384271", false))))),
                // The first question is never answered; the code changes.
                (40, shown(snap(status(code("102938", false))))),
                (80, Step::Line(Some("y\n"))),
                (120, shown(snap(status(WirelessPhaseView::Connected)))),
            ],
            std::future::pending(),
            LIMIT,
        )
        .await;
        assert_eq!(outcome, Outcome::Connected);
        assert_eq!(
            *service.sent.lock().unwrap(),
            [Request::ConfirmCode { matches: true }]
        );
        assert!(lines.iter().any(|l| l.contains("384 271")));
        assert!(lines.iter().any(|l| l.contains("102 938")), "{lines:?}");
    }

    #[tokio::test]
    async fn a_no_is_sent_as_no() {
        let _lang = english();
        let service = Fake::default();
        let (outcome, _) = run(
            &service,
            person(true),
            vec![
                (0, shown(snap(status(pairing(false))))),
                (40, Step::Line(Some("\n"))),
                (
                    80,
                    shown(snap(status(WirelessPhaseView::Failed {
                        reason: WirelessFailure::PairingFailed,
                    }))),
                ),
            ],
            std::future::pending(),
            LIMIT,
        )
        .await;
        assert_eq!(outcome, Outcome::Failed);
        assert_eq!(
            *service.sent.lock().unwrap(),
            [Request::ConfirmCode { matches: false }]
        );
    }

    #[tokio::test]
    async fn the_end_of_input_is_no_answer_so_ctrl_c_only_cancels() {
        let _lang = english();
        let service = Fake::default();
        let (outcome, _) = run(
            &service,
            person(true),
            vec![
                (0, shown(snap(status(pairing(false))))),
                // A Ctrl-C interrupting the console read, or a closed input.
                (40, Step::Line(None)),
            ],
            async { tokio::time::sleep(Duration::from_millis(100)).await },
            LIMIT,
        )
        .await;
        assert_eq!(outcome, Outcome::Cancelled);
        assert_eq!(*service.sent.lock().unwrap(), [Request::CancelWireless]);
    }

    #[test]
    fn only_y_is_a_yes() {
        for yes in ["y", "Y", "y\n", " y\r\n", "\ty "] {
            assert!(is_yes(yes), "{yes:?}");
        }
        for no in ["", "\n", "n", "N", "yes", "Yes", "yy", "no", "1"] {
            assert!(!is_yes(no), "{no:?}");
        }
    }

    #[tokio::test]
    async fn ctrl_c_cancels_the_attempt_even_while_the_question_waits() {
        let _lang = english();
        let service = Fake::default();
        // The person never answers: the read stays blocked.
        let (outcome, lines) = run(
            &service,
            person(true),
            vec![(0, shown(snap(status(pairing(false)))))],
            async { tokio::time::sleep(Duration::from_millis(40)).await },
            LIMIT,
        )
        .await;
        assert_eq!(outcome, Outcome::Cancelled);
        assert_eq!(outcome.exit_code(), 130);
        assert_eq!(*service.sent.lock().unwrap(), [Request::CancelWireless]);
        assert_eq!(lines.last().map(String::as_str), Some("Cancelled"));
    }

    #[tokio::test]
    async fn the_limit_cancels_the_attempt_too() {
        let _lang = english();
        let service = Fake::default();
        let (outcome, lines) = run(
            &service,
            person(true),
            vec![(0, shown(snap(status(WirelessPhaseView::Waiting))))],
            std::future::pending(),
            Duration::from_millis(40),
        )
        .await;
        assert_eq!(outcome, Outcome::TimedOut);
        assert_eq!(outcome.exit_code(), 1);
        assert_eq!(*service.sent.lock().unwrap(), [Request::CancelWireless]);
        assert!(lines.last().unwrap().starts_with("Stopped waiting"));
    }

    #[tokio::test]
    async fn a_service_that_goes_away_ends_the_wait() {
        let _lang = english();
        let service = Fake::default();
        let (asker, _lines) = person(true);
        let (tx, mut rx) = channel(vec![]);
        drop(tx);
        let mut lines = Vec::new();
        let outcome = drive(
            &service,
            &asker,
            &mut rx,
            std::future::pending(),
            LIMIT,
            &mut |l| lines.push(l),
        )
        .await;
        assert_eq!(outcome, Outcome::ServiceStopped);
        assert_eq!(lines, ["The service stopped"]);
        assert!(service.sent.lock().unwrap().is_empty());
    }
}
