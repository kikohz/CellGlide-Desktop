//! The computer's side of code pairing (`code_message`) with an iPhone reached over the
//! wireless pairing channel. See `code_message` for the lines.
//!
//! `run` sends the `CG1 PAIR` request, takes the phone's commitment, sends its own key and
//! nonce, checks the phone's reveal against the commitment and with a key agreement (a mismatch
//! or a low-order key aborts before any code), then asks its own user to `confirm` the code
//! while the phone asks its user. Only when both said yes and the phone's `OK` proof matches the
//! transcript does it call `save` and then send `F`; anything else saves nothing. Both sides
//! confirming is what keeps a device posing as an iPhone (which can always answer `OK`) from
//! becoming this computer's paired phone.

use std::future::Future;
use std::io;
use std::pin::pin;
use std::time::Duration;

use tokio::io::{
    AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader, ReadHalf,
    WriteHalf,
};
use tokio::time::Instant;

use super::code_message::{self as message, Answer, Side, Transcript};
use super::encoding::display_name;
use super::pairing::{Key, PairingError, Role, Session};
use super::request::Request;

/// The computer waits this long for each of the phone's lines (C and P).
pub const STEP_TIMEOUT: Duration = message::STEP_TIMEOUT;
/// Both users' answers and the phone's OK, counted from when the code is shown.
pub const CONFIRM_WINDOW: Duration = message::CONFIRM_WINDOW;

/// What a finished pairing leaves behind; the caller keeps it (the key store) via `save`.
#[derive(Debug)]
pub struct Paired {
    pub phone_id: String,
    /// The PSK identity the phone chose for this pairing.
    pub identity: String,
    /// The phone's name made safe to show (`display_name`).
    pub phone_name: String,
    pub long_term_key: Key,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CodePairingError {
    /// The phone's user said no (or `NO` came for any reason).
    #[error("not allowed on the phone")]
    Denied,
    /// The user on this computer did not confirm the code.
    #[error("the code was not confirmed on this computer")]
    Cancelled,
    #[error("the phone did not answer in time")]
    Timeout,
    /// The `C` line names another phone than the one we connected to.
    #[error("the connection came from another phone than expected")]
    WrongPhone,
    #[error("the phone's key does not match its commitment")]
    BadCommitment,
    /// The phone's `OK` proof does not match what was sent and received: something in between
    /// rewrote a line.
    #[error("the pairing lines were changed on the way")]
    BadProof,
    #[error("the phone's key is unusable")]
    WeakKey,
    #[error("unexpected reply from the phone")]
    Malformed,
    /// The connection failed or closed, or the pairing could not be saved.
    #[error("{0}")]
    Io(String),
}

type Reader<S> = BufReader<ReadHalf<S>>;

/// One line, without its newline. A line that does not end within `MAX_LINE_LENGTH` bytes
/// (newline included) is malformed; the end of the stream is an I/O failure.
async fn read_line<S: AsyncRead + Unpin>(
    reader: &mut Reader<S>,
) -> Result<String, CodePairingError> {
    let mut buffer = Vec::new();
    let mut limited = reader.take(message::MAX_LINE_LENGTH as u64);
    let read = limited
        .read_until(b'\n', &mut buffer)
        .await
        .map_err(|e| CodePairingError::Io(e.to_string()))?;
    if read == 0 {
        return Err(closed());
    }
    if buffer.pop() != Some(b'\n') {
        return Err(if buffer.len() >= message::MAX_LINE_LENGTH - 1 {
            CodePairingError::Malformed
        } else {
            closed()
        });
    }
    String::from_utf8(buffer).map_err(|_| CodePairingError::Malformed)
}

fn closed() -> CodePairingError {
    CodePairingError::Io("the phone closed the connection".into())
}

async fn write_text<S: AsyncWrite + Unpin>(
    writer: &mut WriteHalf<S>,
    text: &str,
) -> Result<(), CodePairingError> {
    async {
        writer.write_all(text.as_bytes()).await?;
        writer.flush().await
    }
    .await
    .map_err(|e| CodePairingError::Io(e.to_string()))
}

/// One of the phone's lines within `STEP_TIMEOUT`, added to the transcript as received.
async fn step<S: AsyncRead + Unpin>(
    reader: &mut Reader<S>,
    transcript: &mut Transcript,
) -> Result<String, CodePairingError> {
    let line = tokio::time::timeout(STEP_TIMEOUT, read_line(reader))
        .await
        .map_err(|_| CodePairingError::Timeout)??;
    transcript.add(&line);
    transcript.add("\n");
    Ok(line)
}

/// The phone's answer and this computer's user's confirmation, in whichever order they come.
/// A `NO`, a refusal here, an error or the deadline ends both at once (dropping `confirm`
/// withdraws the question).
async fn answers<S, Fut>(
    reader: &mut Reader<S>,
    confirm: Fut,
    deadline: Instant,
) -> Result<Answer, CodePairingError>
where
    S: AsyncRead + Unpin,
    Fut: Future<Output = bool>,
{
    let both = async {
        let mut confirm = pin!(confirm);
        let mut line = pin!(read_line(reader));
        let mut confirmed = false;
        let mut answer: Option<Answer> = None;
        loop {
            if confirmed && let Some(answer) = answer.take() {
                return Ok(answer);
            }
            tokio::select! {
                ok = &mut confirm, if !confirmed => {
                    if !ok {
                        return Err(CodePairingError::Cancelled);
                    }
                    confirmed = true;
                }
                text = &mut line, if answer.is_none() => {
                    match message::parse_answer(&text?) {
                        None => return Err(CodePairingError::Malformed),
                        Some(Answer::Denied) => return Err(CodePairingError::Denied),
                        Some(allowed) => answer = Some(allowed),
                    }
                }
            }
        }
    };
    tokio::time::timeout_at(deadline, both)
        .await
        .map_err(|_| CodePairingError::Timeout)?
}

/// Runs code pairing on `stream`, which must already be the pairing channel (TLS with the
/// public identity). `expected_phone_id` is the phone the caller connected to (its Bonjour
/// instance name): a `C` line naming another phone aborts. `confirm` shows the 6-digit code and
/// returns whether this computer's user says it matches the phone's; dropping its future must
/// withdraw the question. `save` is called once, only after both users agreed and the phone's
/// proof checked out, and before `F` is sent; if it fails nothing is sent. Callers pass the name
/// through `wireless::computer_name`; an empty one is replaced the same way here anyway, because
/// the phone refuses an `M` line with an empty name and counts it toward its lockout.
pub async fn run<S, C, Fut, V>(
    stream: S,
    expected_phone_id: &str,
    computer_id: &str,
    computer_name: &str,
    confirm: C,
    save: V,
) -> Result<Paired, CodePairingError>
where
    S: AsyncRead + AsyncWrite + Unpin,
    C: FnOnce(String) -> Fut,
    Fut: Future<Output = bool>,
    V: FnOnce(&Paired) -> io::Result<()>,
{
    run_saving(
        stream,
        expected_phone_id,
        computer_id,
        computer_name,
        confirm,
        move |paired: &Paired| std::future::ready(save(paired)),
    )
    .await
}

/// `run` with a `save` that may wait (a store writing on a blocking thread): the future it
/// returns must complete before `F` is sent, and its error sends nothing, exactly as for `run`.
/// The future may not borrow `Paired`; `save` takes what it needs before returning it.
pub async fn run_saving<S, C, Fut, V, SaveFut>(
    stream: S,
    expected_phone_id: &str,
    computer_id: &str,
    computer_name: &str,
    confirm: C,
    save: V,
) -> Result<Paired, CodePairingError>
where
    S: AsyncRead + AsyncWrite + Unpin,
    C: FnOnce(String) -> Fut,
    Fut: Future<Output = bool>,
    V: FnOnce(&Paired) -> SaveFut,
    SaveFut: Future<Output = io::Result<()>>,
{
    let (read_half, mut writer) = tokio::io::split(stream);
    let mut reader = BufReader::new(read_half);
    let mut transcript = Transcript::new();

    write_text(&mut writer, &Request::Pair.line()).await?;
    let c = step(&mut reader, &mut transcript).await?;
    let (commitment, phone_id) =
        message::parse_commitment(&c).ok_or(CodePairingError::Malformed)?;
    // The saved pairing must be for the phone we connected to, not whatever id the other end
    // claims (it could name the user's real iPhone and replace its good pairing).
    if phone_id != expected_phone_id {
        return Err(CodePairingError::WrongPhone);
    }

    let session = Session::new(Role::Computer);
    let m = message::computer(
        &session.public_key(),
        &session.nonce(),
        computer_id,
        &super::computer_name(Some(computer_name)),
    );
    transcript.add(&m);
    write_text(&mut writer, &m).await?;

    let p = step(&mut reader, &mut transcript).await?;
    let (phone_key, phone_nonce) = message::parse_phone(&p).ok_or(CodePairingError::Malformed)?;
    if !super::pairing::verify_commitment(&commitment, &phone_key, &phone_nonce) {
        return Err(CodePairingError::BadCommitment);
    }
    // A low-order key fails here, before any code is shown.
    let agreed = session
        .agree(&phone_key, &phone_nonce)
        .map_err(|e| match e {
            PairingError::Malformed => CodePairingError::Malformed,
            PairingError::WeakKey => CodePairingError::WeakKey,
        })?;

    let deadline = Instant::now() + CONFIRM_WINDOW;
    let answer = answers(&mut reader, confirm(agreed.code), deadline).await?;
    let Answer::Allowed {
        identity,
        phone_name,
        proof,
        signed,
    } = answer
    else {
        return Err(CodePairingError::Denied);
    };
    transcript.add(&signed);
    if !message::verify(&proof, Side::Phone, &agreed.long_term_key, &transcript) {
        return Err(CodePairingError::BadProof);
    }

    let paired = Paired {
        phone_id,
        identity,
        phone_name: display_name(&phone_name),
        long_term_key: agreed.long_term_key,
    };
    save(&paired)
        .await
        .map_err(|e| CodePairingError::Io(e.to_string()))?;
    // If F never arrives the phone saves nothing and this pairing is dead; pairing again
    // replaces it.
    let f = message::finished(&message::proof(
        Side::Computer,
        &paired.long_term_key,
        &transcript,
    ));
    let _ = write_text(&mut writer, &f).await;
    // Close gracefully, as the Swift side does; the pairing is already saved either way.
    let _ = writer.shutdown().await;
    Ok(paired)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::Digest;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use tokio::io::{DuplexStream, duplex};
    use tokio::task::JoinHandle;

    const PHONE_ID: &str = "4961212D-61DD-4BD0-9FE9-8989FB6B8952";
    const COMPUTER_ID: &str = "7C1E5B52-2D6A-4A0F-8F3B-0A9D5E4C1B22";
    const IDENTITY: &str = "9B2E0F5A-7C3D-4E1B-A6D4-5F8C2B1E7A90";

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Reply {
        Allow,
        Deny,
        AllowWithBadProof,
        /// Sends P, then nothing at all.
        NeverAnswer,
    }

    #[derive(Clone)]
    struct Phone {
        phone_id: String,
        reply: Reply,
        /// Wait this long after P before replying.
        reply_delay: Duration,
        /// Raw text sent instead of C / P.
        raw_c: Option<String>,
        raw_p: Option<Vec<u8>>,
        /// Commit to another key than the one revealed in P.
        commit_to_other_key: bool,
        /// Reveal this public key (a low-order one) in P.
        reveal_key: Option<[u8; 32]>,
        /// Wait this long before the first line.
        c_delay: Duration,
        /// Stop after reading M and never send P.
        silent_after_m: bool,
    }

    impl Phone {
        fn new(reply: Reply) -> Phone {
            Phone {
                phone_id: PHONE_ID.into(),
                reply,
                reply_delay: Duration::ZERO,
                raw_c: None,
                raw_p: None,
                commit_to_other_key: false,
                reveal_key: None,
                c_delay: Duration::ZERO,
                silent_after_m: false,
            }
        }
    }

    #[derive(Debug, Default)]
    struct PhoneReport {
        request_seen: bool,
        got_m: bool,
        /// How long after sending C the phone had M (virtual time).
        m_after: Option<Duration>,
        code: Option<String>,
        /// The computer's name as the phone decoded it from M.
        computer_name: Option<String>,
        /// None: no F arrived.
        f_valid: Option<bool>,
        saved_when_f_arrived: Option<bool>,
    }

    async fn read_line<R: AsyncRead + Unpin>(r: &mut BufReader<R>) -> Option<String> {
        let mut line = String::new();
        let n = r.read_line(&mut line).await.ok()?;
        (n > 0 && line.ends_with('\n')).then_some(line)
    }

    /// Waits for the other side to hang up.
    async fn until_closed<R: AsyncRead + Unpin>(r: &mut BufReader<R>) {
        while read_line(r).await.is_some() {}
    }

    async fn send<W: AsyncWrite + Unpin>(w: &mut W, text: &[u8]) {
        let _ = w.write_all(text).await;
        let _ = w.flush().await;
    }

    async fn fake_phone(stream: DuplexStream, p: Phone, saved: Arc<AtomicBool>) -> PhoneReport {
        let mut report = PhoneReport::default();
        let (r, mut w) = tokio::io::split(stream);
        let mut r = BufReader::new(r);
        let mut transcript = Transcript::new();

        match read_line(&mut r).await {
            Some(l) if l == Request::Pair.line() => report.request_seen = true,
            _ => return report,
        }
        tokio::time::sleep(p.c_delay).await;
        let session = Session::fixed(Role::Phone, [0x11; 32], [0x22; 16]);
        let commitment = if p.commit_to_other_key {
            Session::fixed(Role::Phone, [0x77; 32], [0x22; 16]).commitment()
        } else {
            session.commitment()
        };
        let c = p
            .raw_c
            .clone()
            .unwrap_or_else(|| message::commitment(&commitment, &p.phone_id));
        transcript.add(&c);
        send(&mut w, c.as_bytes()).await;
        let sent_c = Instant::now();

        // The real phone waits only FIRST_STEP_TIMEOUT for M.
        let Ok(Some(m_line)) =
            tokio::time::timeout(message::FIRST_STEP_TIMEOUT, read_line(&mut r)).await
        else {
            return report;
        };
        report.got_m = true;
        report.m_after = Some(sent_c.elapsed());
        transcript.add(&m_line);
        let Some(m) = message::parse_computer(m_line.trim_end_matches('\n')) else {
            return report;
        };
        report.computer_name = Some(m.computer_name.clone());
        if p.silent_after_m {
            until_closed(&mut r).await;
            return report;
        }
        let public = p.reveal_key.unwrap_or_else(|| session.public_key());
        let pl = p
            .raw_p
            .clone()
            .unwrap_or_else(|| message::phone(&public, &session.nonce()).into_bytes());
        transcript.add(&String::from_utf8_lossy(&pl));
        send(&mut w, &pl).await;
        let Ok(agreed) = session.agree(&m.public_key, &m.nonce) else {
            return report;
        };
        report.code = Some(agreed.code.clone());

        tokio::time::sleep(p.reply_delay).await;
        match p.reply {
            Reply::NeverAnswer => {
                until_closed(&mut r).await;
                return report;
            }
            Reply::Deny => {
                send(&mut w, message::DENIED.as_bytes()).await;
                return report;
            }
            Reply::Allow | Reply::AllowWithBadProof => {
                let signed = message::allowed_signed(IDENTITY, "Eve’s iPhone");
                transcript.add(&signed);
                let mut proof = message::proof(Side::Phone, &agreed.long_term_key, &transcript);
                if p.reply == Reply::AllowWithBadProof {
                    proof[0] ^= 1;
                }
                let ok = message::allowed(IDENTITY, "Eve’s iPhone", &proof);
                send(&mut w, ok.as_bytes()).await;
            }
        }
        if let Some(f) = read_line(&mut r).await {
            report.saved_when_f_arrived = Some(saved.load(Ordering::SeqCst));
            report.f_valid = Some(
                message::parse_finished(f.trim_end_matches('\n')).is_some_and(|proof| {
                    message::verify(&proof, Side::Computer, &agreed.long_term_key, &transcript)
                }),
            );
        }
        report
    }

    /// Wraps the computer's end of the stream and, when the first byte of the F line is written,
    /// records whether `save` had already run. This is a fact about the order of the calls
    /// inside `run`, so it holds on any runtime, unlike reading the flag on the phone's side.
    struct SpyStream {
        inner: DuplexStream,
        saved: Arc<AtomicBool>,
        saved_at_f: Arc<std::sync::Mutex<Option<bool>>>,
    }

    impl AsyncRead for SpyStream {
        fn poll_read(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            buf: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<io::Result<()>> {
            std::pin::Pin::new(&mut self.inner).poll_read(cx, buf)
        }
    }

    impl AsyncWrite for SpyStream {
        fn poll_write(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            buf: &[u8],
        ) -> std::task::Poll<io::Result<usize>> {
            if buf.starts_with(b"F ") {
                let mut seen = self.saved_at_f.lock().unwrap();
                if seen.is_none() {
                    *seen = Some(self.saved.load(Ordering::SeqCst));
                }
            }
            std::pin::Pin::new(&mut self.inner).poll_write(cx, buf)
        }

        fn poll_flush(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<io::Result<()>> {
            std::pin::Pin::new(&mut self.inner).poll_flush(cx)
        }

        fn poll_shutdown(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<io::Result<()>> {
            std::pin::Pin::new(&mut self.inner).poll_shutdown(cx)
        }
    }

    #[derive(Default)]
    struct Probe {
        /// Whether `save` had run when `run` first wrote the F line (None: F never written).
        saved_at_f_write: Arc<std::sync::Mutex<Option<bool>>>,
        confirm_calls: AtomicUsize,
        shown_code: std::sync::Mutex<Option<String>>,
        save_calls: AtomicUsize,
        saved: Arc<AtomicBool>,
        saved_key: std::sync::Mutex<Option<[u8; 32]>>,
    }

    struct Outcome {
        /// Virtual time `run` took.
        elapsed: Duration,
        result: Result<Paired, CodePairingError>,
        phone: PhoneReport,
        probe: Arc<Probe>,
    }

    #[derive(Clone, Copy)]
    enum Confirm {
        Yes,
        No,
        YesAfter(Duration),
        Never,
    }

    async fn pair_with(
        phone: Phone,
        confirm: Confirm,
        expected: &str,
        save_fails: bool,
    ) -> Outcome {
        let (computer_end, phone_end) = duplex(4096);
        pair_over(
            computer_end,
            phone_end,
            phone,
            confirm,
            expected,
            save_fails,
        )
        .await
    }

    async fn pair_over(
        computer_end: DuplexStream,
        phone_end: DuplexStream,
        phone: Phone,
        confirm: Confirm,
        expected: &str,
        save_fails: bool,
    ) -> Outcome {
        pair_named_over(
            computer_end,
            phone_end,
            phone,
            confirm,
            expected,
            save_fails,
            "Elok’s PC",
        )
        .await
    }

    async fn pair_named_over(
        computer_end: DuplexStream,
        phone_end: DuplexStream,
        phone: Phone,
        confirm: Confirm,
        expected: &str,
        save_fails: bool,
        computer_name: &str,
    ) -> Outcome {
        let probe = Arc::new(Probe::default());
        let phone_task = tokio::spawn(fake_phone(phone_end, phone, probe.saved.clone()));
        let computer_end = SpyStream {
            inner: computer_end,
            saved: probe.saved.clone(),
            saved_at_f: probe.saved_at_f_write.clone(),
        };
        let p = probe.clone();
        let q = probe.clone();
        let started = Instant::now();
        let result = run(
            computer_end,
            expected,
            COMPUTER_ID,
            computer_name,
            move |code: String| async move {
                p.confirm_calls.fetch_add(1, Ordering::SeqCst);
                *p.shown_code.lock().unwrap() = Some(code);
                match confirm {
                    Confirm::Yes => true,
                    Confirm::No => false,
                    Confirm::YesAfter(d) => {
                        tokio::time::sleep(d).await;
                        true
                    }
                    Confirm::Never => std::future::pending().await,
                }
            },
            move |paired: &Paired| {
                q.save_calls.fetch_add(1, Ordering::SeqCst);
                *q.saved_key.lock().unwrap() = Some(*paired.long_term_key.as_bytes());
                if save_fails {
                    return Err(io::Error::other("disk full"));
                }
                q.saved.store(true, Ordering::SeqCst);
                Ok(())
            },
        )
        .await;
        let elapsed = started.elapsed();
        let phone = phone_task.await.unwrap();
        Outcome {
            elapsed,
            result,
            phone,
            probe,
        }
    }

    async fn pair(phone: Phone, confirm: Confirm) -> Outcome {
        pair_with(phone, confirm, PHONE_ID, false).await
    }

    fn save_calls(o: &Outcome) -> usize {
        o.probe.save_calls.load(Ordering::SeqCst)
    }

    fn confirm_calls(o: &Outcome) -> usize {
        o.probe.confirm_calls.load(Ordering::SeqCst)
    }

    #[tokio::test(start_paused = true)]
    async fn pairs_and_saves_before_f_is_sent() {
        let o = pair(Phone::new(Reply::Allow), Confirm::Yes).await;
        let paired = o.result.as_ref().expect("paired");
        assert_eq!(paired.phone_id, PHONE_ID);
        assert_eq!(paired.identity, IDENTITY);
        // Shown the way `display_name` makes it safe: the quote character is dropped.
        assert_eq!(paired.phone_name, "Eves iPhone");
        // Both sides showed the same code, and it is six digits.
        let shown = o.probe.shown_code.lock().unwrap().clone().unwrap();
        assert_eq!(Some(shown.clone()), o.phone.code);
        assert_eq!(shown.len(), 6);
        assert!(o.phone.request_seen);
        // Saved exactly once, with the key both sides derived, before F reached the phone.
        assert_eq!(save_calls(&o), 1);
        assert_eq!(
            o.probe.saved_key.lock().unwrap().unwrap(),
            *paired.long_term_key.as_bytes()
        );
        assert_eq!(*o.probe.saved_at_f_write.lock().unwrap(), Some(true));
        assert_eq!(o.phone.saved_when_f_arrived, Some(true));
        assert_eq!(o.phone.f_valid, Some(true));
        assert_eq!(confirm_calls(&o), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn m_carries_the_computer_name() {
        let o = pair(Phone::new(Reply::Allow), Confirm::Yes).await;
        assert!(o.result.is_ok());
        assert_eq!(o.phone.computer_name.as_deref(), Some("Elok’s PC"));
    }

    /// The phone refuses an M line with an empty name and counts it toward its lockout: an
    /// empty name, or one that cleans to nothing, goes out as the fallback and pairing works.
    #[tokio::test(start_paused = true)]
    async fn an_empty_computer_name_goes_out_as_windows_pc() {
        for name in ["", "   ", "\u{0}\n\u{7f}"] {
            let (computer_end, phone_end) = duplex(4096);
            let o = pair_named_over(
                computer_end,
                phone_end,
                Phone::new(Reply::Allow),
                Confirm::Yes,
                PHONE_ID,
                false,
                name,
            )
            .await;
            assert!(o.result.is_ok(), "{name:?}");
            assert_eq!(
                o.phone.computer_name.as_deref(),
                Some("Windows PC"),
                "{name:?}"
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn m_follows_c_immediately() {
        let o = pair(Phone::new(Reply::Allow), Confirm::Yes).await;
        assert!(o.result.is_ok());
        assert_eq!(o.phone.m_after, Some(Duration::ZERO));
    }

    #[tokio::test(start_paused = true)]
    async fn another_phones_id_is_refused_before_m() {
        let mut phone = Phone::new(Reply::Allow);
        phone.phone_id = "11111111-2222-4333-8444-555555555555".into();
        let o = pair(phone, Confirm::Yes).await;
        assert_eq!(
            o.result.as_ref().unwrap_err().clone(),
            CodePairingError::WrongPhone
        );
        assert!(!o.phone.got_m, "M must not be sent");
        assert_eq!(confirm_calls(&o), 0);
        assert_eq!(save_calls(&o), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn a_key_that_breaks_the_commitment_is_refused_before_any_code() {
        let mut phone = Phone::new(Reply::Allow);
        phone.commit_to_other_key = true;
        let o = pair(phone, Confirm::Yes).await;
        assert_eq!(
            o.result.as_ref().unwrap_err().clone(),
            CodePairingError::BadCommitment
        );
        assert_eq!(confirm_calls(&o), 0);
        assert_eq!(save_calls(&o), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn a_low_order_key_is_refused_before_any_code() {
        // Commit to the zero key honestly, so only the key agreement can object.
        let mut phone = Phone::new(Reply::Allow);
        phone.reveal_key = Some([0; 32]);
        phone.raw_c = Some(message::commitment(
            &sha2::Sha256::digest([&[0u8; 32][..], &[0x22u8; 16][..]].concat()).into(),
            PHONE_ID,
        ));
        let o = pair(phone, Confirm::Yes).await;
        assert_eq!(
            o.result.as_ref().unwrap_err().clone(),
            CodePairingError::WeakKey
        );
        assert_eq!(confirm_calls(&o), 0);
        assert_eq!(save_calls(&o), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn the_phone_saying_no_ends_it_at_once() {
        let o = pair(Phone::new(Reply::Deny), Confirm::Never).await;
        assert_eq!(
            o.result.as_ref().unwrap_err().clone(),
            CodePairingError::Denied
        );
        assert_eq!(save_calls(&o), 0);
        assert_eq!(o.phone.f_valid, None);
    }

    #[tokio::test(start_paused = true)]
    async fn a_wrong_phone_proof_is_refused() {
        let o = pair(Phone::new(Reply::AllowWithBadProof), Confirm::Yes).await;
        assert_eq!(
            o.result.as_ref().unwrap_err().clone(),
            CodePairingError::BadProof
        );
        assert_eq!(save_calls(&o), 0);
        assert_eq!(o.phone.f_valid, None, "no F after a bad proof");
    }

    #[tokio::test(start_paused = true)]
    async fn declining_here_cancels_without_saving_or_sending_f() {
        let o = pair(Phone::new(Reply::Allow), Confirm::No).await;
        assert_eq!(
            o.result.as_ref().unwrap_err().clone(),
            CodePairingError::Cancelled
        );
        assert_eq!(save_calls(&o), 0);
        assert_eq!(o.phone.f_valid, None);
    }

    #[tokio::test(start_paused = true)]
    async fn declining_here_wins_over_a_bad_proof() {
        // As in the Mac: the user's answer is checked before the proof.
        let o = pair(Phone::new(Reply::AllowWithBadProof), Confirm::No).await;
        assert_eq!(
            o.result.as_ref().unwrap_err().clone(),
            CodePairingError::Cancelled
        );
    }

    #[tokio::test(start_paused = true)]
    async fn phone_first_then_user() {
        let o = pair(
            Phone::new(Reply::Allow),
            Confirm::YesAfter(Duration::from_secs(5)),
        )
        .await;
        assert!(o.result.is_ok());
        assert_eq!(o.phone.f_valid, Some(true));
        assert_eq!(save_calls(&o), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn user_first_then_phone() {
        let mut phone = Phone::new(Reply::Allow);
        phone.reply_delay = Duration::from_secs(5);
        let o = pair(phone, Confirm::Yes).await;
        assert!(o.result.is_ok());
        assert_eq!(o.phone.f_valid, Some(true));
        assert_eq!(save_calls(&o), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_slow_but_in_time_pair_of_answers_works() {
        let mut phone = Phone::new(Reply::Allow);
        phone.reply_delay = Duration::from_secs(74);
        let o = pair(phone, Confirm::YesAfter(Duration::from_secs(70))).await;
        assert!(o.result.is_ok());
    }

    #[tokio::test(start_paused = true)]
    async fn a_failing_save_aborts_without_f() {
        let o = pair_with(Phone::new(Reply::Allow), Confirm::Yes, PHONE_ID, true).await;
        assert_eq!(
            o.result.as_ref().unwrap_err().clone(),
            CodePairingError::Io("disk full".into())
        );
        assert_eq!(save_calls(&o), 1);
        assert_eq!(o.phone.f_valid, None, "F must not be sent");
    }

    // The phone and computer talk through a relay that can rewrite one line.

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Dir {
        ToPhone,
        ToComputer,
    }

    async fn forward(
        from: tokio::io::ReadHalf<DuplexStream>,
        mut to: tokio::io::WriteHalf<DuplexStream>,
        dir: Dir,
        tamper: Arc<dyn Fn(Dir, String) -> String + Send + Sync>,
    ) {
        let mut from = BufReader::new(from);
        while let Some(line) = read_line(&mut from).await {
            let body = line.trim_end_matches('\n').to_string();
            let out = tamper(dir, body);
            send(&mut to, format!("{out}\n").as_bytes()).await;
        }
        let _ = to.shutdown().await;
    }

    fn relay(
        tamper: impl Fn(Dir, String) -> String + Send + Sync + 'static,
    ) -> (DuplexStream, DuplexStream, Vec<JoinHandle<()>>) {
        let tamper: Arc<dyn Fn(Dir, String) -> String + Send + Sync> = Arc::new(tamper);
        let (computer_end, relay_computer) = duplex(4096);
        let (relay_phone, phone_end) = duplex(4096);
        let (rc_r, rc_w) = tokio::io::split(relay_computer);
        let (rp_r, rp_w) = tokio::io::split(relay_phone);
        let tasks = vec![
            tokio::spawn(forward(rc_r, rp_w, Dir::ToPhone, tamper.clone())),
            tokio::spawn(forward(rp_r, rc_w, Dir::ToComputer, tamper)),
        ];
        (computer_end, phone_end, tasks)
    }

    fn flip_char(token: &str) -> String {
        // Change one base64url character to another valid one.
        let mut chars: Vec<char> = token.chars().collect();
        chars[0] = if chars[0] == 'B' { 'C' } else { 'B' };
        chars.into_iter().collect()
    }

    async fn tampered(
        expected: &str,
        tamper: impl Fn(Dir, String) -> String + Send + Sync + 'static,
    ) -> Outcome {
        let (c, p, _tasks) = relay(tamper);
        pair_over(
            c,
            p,
            Phone::new(Reply::Allow),
            Confirm::Yes,
            expected,
            false,
        )
        .await
    }

    fn replace_field(line: &str, tag: &str, index: usize, f: impl Fn(&str) -> String) -> String {
        let mut parts: Vec<String> = line.split(' ').map(str::to_string).collect();
        if parts[0] == tag {
            parts[index] = f(&parts[index]);
        }
        parts.join(" ")
    }

    #[tokio::test(start_paused = true)]
    async fn an_untouched_relay_changes_nothing() {
        let o = tampered(PHONE_ID, |_, l| l).await;
        assert!(o.result.is_ok());
    }

    #[tokio::test(start_paused = true)]
    async fn rewriting_a_line_in_transit_is_caught() {
        let other = "AAAAAAAA-0000-4000-8000-000000000000";
        // (what is rewritten, expected id, rewrite, allowed outcomes)
        type Case = (
            &'static str,
            &'static str,
            Box<dyn Fn(Dir, String) -> String + Send + Sync>,
            Vec<CodePairingError>,
        );
        let cases: Vec<Case> = vec![
            (
                "C commitment",
                PHONE_ID,
                Box::new(|d, l| {
                    if d == Dir::ToComputer {
                        replace_field(&l, "C", 1, flip_char)
                    } else {
                        l
                    }
                }),
                vec![CodePairingError::BadCommitment],
            ),
            (
                "C phone id",
                other,
                Box::new(move |d, l| {
                    if d == Dir::ToComputer {
                        replace_field(&l, "C", 2, |_| other.to_string())
                    } else {
                        l
                    }
                }),
                vec![CodePairingError::BadProof],
            ),
            (
                "M computer id",
                PHONE_ID,
                Box::new(move |d, l| {
                    if d == Dir::ToPhone {
                        replace_field(&l, "M", 3, |_| other.to_string())
                    } else {
                        l
                    }
                }),
                vec![CodePairingError::BadProof],
            ),
            (
                "M computer name",
                PHONE_ID,
                Box::new(|d, l| {
                    if d == Dir::ToPhone {
                        replace_field(&l, "M", 4, |_| "RXZpbA".to_string())
                    } else {
                        l
                    }
                }),
                vec![CodePairingError::BadProof],
            ),
            (
                "M public key",
                PHONE_ID,
                Box::new(|d, l| {
                    if d == Dir::ToPhone {
                        replace_field(&l, "M", 1, flip_char)
                    } else {
                        l
                    }
                }),
                vec![CodePairingError::BadProof],
            ),
            (
                "P nonce",
                PHONE_ID,
                Box::new(|d, l| {
                    if d == Dir::ToComputer {
                        replace_field(&l, "P", 2, flip_char)
                    } else {
                        l
                    }
                }),
                vec![CodePairingError::BadCommitment],
            ),
            (
                "OK identity",
                PHONE_ID,
                Box::new(|d, l| {
                    if d == Dir::ToComputer {
                        replace_field(&l, "OK", 1, |_| {
                            "FFFFFFFF-0000-4000-8000-000000000000".into()
                        })
                    } else {
                        l
                    }
                }),
                vec![CodePairingError::BadProof],
            ),
            (
                "OK name",
                PHONE_ID,
                Box::new(|d, l| {
                    if d == Dir::ToComputer {
                        replace_field(&l, "OK", 2, |_| "RXZpbA".into())
                    } else {
                        l
                    }
                }),
                vec![CodePairingError::BadProof],
            ),
            (
                "OK proof",
                PHONE_ID,
                Box::new(|d, l| {
                    if d == Dir::ToComputer {
                        replace_field(&l, "OK", 3, flip_char)
                    } else {
                        l
                    }
                }),
                vec![CodePairingError::BadProof],
            ),
        ];
        for (what, expected, rewrite, allowed) in cases {
            let o = tampered(expected, rewrite).await;
            let error = o.result.as_ref().expect_err(what).clone();
            assert!(allowed.contains(&error), "{what}: got {error:?}");
            assert_eq!(save_calls(&o), 0, "{what}");
            assert_eq!(o.phone.f_valid, None, "{what}");
            if what == "M public key" {
                // The codes differ too, so a user comparing them would stop it as well.
                let shown = o.probe.shown_code.lock().unwrap().clone();
                assert_ne!(shown, o.phone.code, "{what}");
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn overlong_and_misshapen_lines_are_malformed() {
        let long_c = format!("C {} {}\n", "A".repeat(600), PHONE_ID);
        for raw_c in [
            long_c,
            // No newline inside 512 bytes.
            "C ".to_string() + &"A".repeat(520),
            "C onlyonefield\n".to_string(),
            format!("C {} {PHONE_ID} extra\n", "A".repeat(43)),
            "\n".to_string(),
        ] {
            let mut phone = Phone::new(Reply::Allow);
            phone.raw_c = Some(raw_c.clone());
            let o = pair(phone, Confirm::Yes).await;
            assert_eq!(
                o.result.as_ref().unwrap_err().clone(),
                CodePairingError::Malformed,
                "{:?}",
                &raw_c[..raw_c.len().min(30)]
            );
            assert!(!o.phone.got_m);
        }
        for raw_p in [
            format!("P {}\n", "A".repeat(43)).into_bytes(),
            format!("P {} {} x\n", "A".repeat(43), "A".repeat(22)).into_bytes(),
            format!("P {} {}\n", "A".repeat(44), "A".repeat(22)).into_bytes(),
            (b"P "
                .iter()
                .chain(&[b'A'; 600])
                .copied()
                .collect::<Vec<u8>>()),
            vec![b'P', b' ', 0xFF, 0xFE, b'\n'],
        ] {
            let mut phone = Phone::new(Reply::Allow);
            phone.raw_p = Some(raw_p);
            let o = pair(phone, Confirm::Yes).await;
            assert_eq!(
                o.result.as_ref().unwrap_err().clone(),
                CodePairingError::Malformed
            );
            assert_eq!(confirm_calls(&o), 0);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_malformed_answer_is_refused() {
        // After showing the code the phone replies with garbage instead of OK / NO.
        // The fake phone's `Reply::Allow` is replaced by a raw line through a relay.
        let (c, p, _tasks) = relay(|d, l| {
            if d == Dir::ToComputer && l.starts_with("OK ") {
                "OK too few".to_string()
            } else {
                l
            }
        });
        let o = pair_over(
            c,
            p,
            Phone::new(Reply::Allow),
            Confirm::Yes,
            PHONE_ID,
            false,
        )
        .await;
        assert_eq!(
            o.result.as_ref().unwrap_err().clone(),
            CodePairingError::Malformed
        );
        assert_eq!(save_calls(&o), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn silence_after_the_request_times_out_after_60_seconds() {
        let mut phone = Phone::new(Reply::Allow);
        phone.c_delay = Duration::from_secs(3600);
        let o = pair(phone, Confirm::Yes).await;
        assert_eq!(
            o.result.as_ref().unwrap_err().clone(),
            CodePairingError::Timeout
        );
        assert_eq!(o.elapsed, STEP_TIMEOUT);
    }

    #[tokio::test(start_paused = true)]
    async fn no_p_within_60_seconds_times_out() {
        let mut phone = Phone::new(Reply::Allow);
        phone.silent_after_m = true;
        let o = pair(phone, Confirm::Yes).await;
        assert_eq!(
            o.result.as_ref().unwrap_err().clone(),
            CodePairingError::Timeout
        );
        assert_eq!(o.elapsed, STEP_TIMEOUT);
        assert_eq!(confirm_calls(&o), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn answers_not_complete_75_seconds_after_the_code_time_out() {
        // The phone's user never answers.
        let o = pair(Phone::new(Reply::NeverAnswer), Confirm::Yes).await;
        assert_eq!(
            o.result.as_ref().unwrap_err().clone(),
            CodePairingError::Timeout
        );
        assert_eq!(o.elapsed, CONFIRM_WINDOW);
        assert_eq!(save_calls(&o), 0);

        // This computer's user never answers.
        let o = pair(Phone::new(Reply::Allow), Confirm::Never).await;
        assert_eq!(
            o.result.as_ref().unwrap_err().clone(),
            CodePairingError::Timeout
        );
        assert_eq!(o.elapsed, CONFIRM_WINDOW);
        assert_eq!(save_calls(&o), 0);
        assert_eq!(o.phone.f_valid, None);
    }

    #[tokio::test(start_paused = true)]
    async fn an_answer_just_inside_the_window_still_counts() {
        let mut phone = Phone::new(Reply::Allow);
        phone.reply_delay = CONFIRM_WINDOW - Duration::from_secs(1);
        let o = pair(phone, Confirm::Yes).await;
        assert!(o.result.is_ok());
    }

    #[tokio::test(start_paused = true)]
    async fn a_closed_connection_is_an_io_error() {
        let (computer_end, phone_end) = duplex(4096);
        drop(phone_end);
        let result = run(
            computer_end,
            PHONE_ID,
            COMPUTER_ID,
            "PC",
            |_code: String| async { true },
            |_: &Paired| Ok(()),
        )
        .await;
        assert!(matches!(result, Err(CodePairingError::Io(_))), "{result:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn the_phone_hanging_up_while_the_user_decides_ends_it() {
        let (computer_end, phone_end) = duplex(4096);
        let phone = tokio::spawn(async move {
            let (r, mut w) = tokio::io::split(phone_end);
            let mut r = BufReader::new(r);
            let session = Session::fixed(Role::Phone, [0x11; 32], [0x22; 16]);
            let _ = read_line(&mut r).await;
            send(
                &mut w,
                message::commitment(&session.commitment(), PHONE_ID).as_bytes(),
            )
            .await;
            let _ = read_line(&mut r).await;
            send(
                &mut w,
                message::phone(&session.public_key(), &session.nonce()).as_bytes(),
            )
            .await;
            // Hang up.
        });
        let result = run(
            computer_end,
            PHONE_ID,
            COMPUTER_ID,
            "PC",
            |_code: String| std::future::pending::<bool>(),
            |_: &Paired| Ok(()),
        )
        .await;
        phone.await.unwrap();
        assert!(matches!(result, Err(CodePairingError::Io(_))), "{result:?}");
    }
}
