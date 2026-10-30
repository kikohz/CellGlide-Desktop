//! One measurement along one route: latency (median of three small requests), then several
//! streams download and then upload for up to 10 s each, the first 2 s left out. Ported from the
//! Mac's SpeedEngine.swift with the same numbers and the same rate math. Upload alone counts
//! differently: by the bytes the server confirmed, in requests of `UPLOAD_REQUEST`.

use super::transport::{
    Route, Server, SpeedError, Stream, connect, download_request, ping, read_head, upload_request,
};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::sync::watch;
use tokio::task::JoinSet;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

/// How long one latency request may take, connecting included (the Mac's 20 s).
pub const LATENCY_TIMEOUT: Duration = Duration::from_secs(20);
/// Upload writes the same 64 KiB block again and again.
const CHUNK: usize = 64 * 1024;
const READ_BUFFER: usize = 256 * 1024;
/// Every upload request carries this many bytes (the last one of a stream may carry fewer),
/// whatever `Params.request_size` says (that is for downloads only): upload counts a request
/// once the server answered it, so requests must stay small enough to be answered during the
/// test. See `upload_stream`.
pub const UPLOAD_REQUEST: u64 = 1_000_000;

/// The measurement's knobs. `Default` is the Mac's: caps bound a 10 s test on a very fast link
/// (about 800 Mbps down, 240 Mbps up); Cloudflare refuses single downloads of 100 MB or more,
/// so each request asks for at most 90 MB.
#[derive(Clone, Debug, PartialEq)]
pub struct Params {
    pub streams: usize,
    pub duration: Duration,
    pub warmup: Duration,
    pub download_cap: u64,
    pub upload_cap: u64,
    pub request_size: u64,
    pub tick: Duration,
    pub latency_samples: usize,
}

impl Default for Params {
    fn default() -> Params {
        Params {
            streams: 6,
            duration: Duration::from_secs(10),
            warmup: Duration::from_secs(2),
            download_cap: 1_000_000_000,
            upload_cap: 300_000_000,
            request_size: 90_000_000,
            tick: Duration::from_millis(500),
            latency_samples: 3,
        }
    }
}

impl Params {
    /// Parameters that can measure something: without streams, or with a cap too small to
    /// give every stream a byte, a direction would "finish" at 0 Mbps.
    fn check(&self) -> Result<(), SpeedError> {
        let streams = self.streams as u64;
        if streams == 0 || self.download_cap < streams || self.upload_cap < streams {
            return Err(SpeedError::Io(format!(
                "invalid speed test parameters: {} streams, caps {}/{}",
                self.streams, self.download_cap, self.upload_cap
            )));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub enum Stage {
    #[default]
    Idle,
    Latency,
    Download,
    Upload,
    Done,
    Failed(SpeedError),
    Cancelled,
}

/// Something the user should know about a finished result.
#[derive(Clone, Debug, PartialEq)]
pub enum Note {
    /// A direction moved all its capped bytes before 10 s: the rate rests on `seconds` only.
    CapReached { uploading: bool, seconds: f64 },
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SpeedResult {
    /// Milliseconds, the median of the latency samples.
    pub latency: Option<f64>,
    /// Mbps.
    pub download: Option<f64>,
    pub upload: Option<f64>,
    /// Download throughput every tick (Mbps), for a curve.
    pub download_series: Vec<f64>,
    pub notes: Vec<Note>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Progress {
    pub stage: Stage,
    /// 0–1 across both directions; the latency check counts as the start.
    pub progress: f64,
    /// Seconds into the current direction.
    pub elapsed: f64,
    pub result: SpeedResult,
    /// Payload bytes moved by this measurement so far, both directions.
    pub payload_bytes: u64,
}

/// Measures `route`. Progress, partial numbers included, goes to `progress`; on return its
/// stage is `Done`, `Failed` or `Cancelled`, and the result is the one returned. Cancelling
/// `cancel` stops every connection at once.
pub async fn measure(
    route: Route,
    server: Server,
    params: Params,
    progress: watch::Sender<Progress>,
    cancel: CancellationToken,
) -> SpeedResult {
    progress.send_replace(Progress {
        stage: Stage::Latency,
        ..Progress::default()
    });
    let outcome = if let Err(error) = params.check() {
        Some(Err(error))
    } else {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => None,
            outcome = run(&route, &server, &params, &progress) => Some(outcome),
        }
    };
    progress.send_modify(|p| finish(p, outcome, cancel.is_cancelled()));
    progress.borrow().result.clone()
}

/// Sets the final stage: `None` means cancelled before the end.
fn finish(p: &mut Progress, outcome: Option<Result<(), SpeedError>>, cancelled: bool) {
    match outcome {
        Some(Ok(())) => {
            p.stage = Stage::Done;
            p.progress = 1.0;
        }
        // A stop can race a network error; the user asked to stop, so it reads as cancelled.
        Some(Err(_)) if cancelled => p.stage = Stage::Cancelled,
        Some(Err(error)) => p.stage = Stage::Failed(error),
        None => p.stage = Stage::Cancelled,
    }
}

async fn run(
    route: &Route,
    server: &Server,
    params: &Params,
    progress: &watch::Sender<Progress>,
) -> Result<(), SpeedError> {
    let latency = latency(route, server, params.latency_samples).await?;
    progress.send_modify(|p| {
        p.result.latency = latency;
        p.stage = Stage::Download;
    });
    measure_direction(route, server, params, progress, false).await?;
    progress.send_modify(|p| p.stage = Stage::Upload);
    measure_direction(route, server, params, progress, true).await?;
    Ok(())
}

/// The median of `samples` requests on one kept-alive connection, in milliseconds.
async fn latency(
    route: &Route,
    server: &Server,
    samples: usize,
) -> Result<Option<f64>, SpeedError> {
    let mut stream: Option<Stream> = None;
    let mut delays = Vec::with_capacity(samples);
    for _ in 0..samples {
        let sample = async {
            if stream.is_none() {
                stream = Some(connect(route, server).await?);
            }
            let open = stream.as_mut().expect("connected above");
            ping(open, server).await
        };
        let delay = tokio::time::timeout(LATENCY_TIMEOUT, sample)
            .await
            .map_err(|_| SpeedError::Timeout)??;
        delays.push(delay.as_secs_f64() * 1000.0);
    }
    delays.sort_by(f64::total_cmp);
    Ok(delays.get(delays.len() / 2).copied())
}

/// Turns byte counts into Mbps the Mac's way: after the warm-up, the rate since the warm-up
/// point; before it, the average since the start. Also the per-tick rate for the curve.
#[derive(Debug)]
pub(crate) struct Sampler {
    warmup: f64,
    warm: Option<(u64, f64)>,
    last: (u64, f64),
}

impl Sampler {
    pub(crate) fn new(warmup: Duration) -> Sampler {
        Sampler {
            warmup: warmup.as_secs_f64(),
            warm: None,
            last: (0, 0.0),
        }
    }

    /// `bytes` moved in `elapsed` seconds; returns the rate and the point for the curve.
    pub(crate) fn sample(&mut self, bytes: u64, elapsed: f64) -> (f64, Option<f64>) {
        let point = (elapsed > self.last.1).then(|| {
            let p = mbps(bytes.saturating_sub(self.last.0), elapsed - self.last.1);
            self.last = (bytes, elapsed);
            p
        });
        if self.warm.is_none() && elapsed >= self.warmup {
            self.warm = Some((bytes, elapsed));
        }
        let rate = match self.warm {
            Some((warm_bytes, warm_time)) if elapsed > warm_time => {
                mbps(bytes.saturating_sub(warm_bytes), elapsed - warm_time)
            }
            _ if elapsed > 0.0 => mbps(bytes, elapsed),
            _ => 0.0,
        };
        (rate, point)
    }
}

fn mbps(bytes: u64, seconds: f64) -> f64 {
    bytes as f64 * 8.0 / seconds / 1_000_000.0
}

/// What the sampling loop does after a tick.
#[derive(Debug, PartialEq)]
pub(crate) enum Next {
    Continue,
    /// Time is up, or every stream finished at 10 s or later.
    Finish,
    /// Every stream finished cleanly before time: the byte cap.
    CapReached,
    Fail(SpeedError),
}

/// Streams that all ended with an error before time lost the connection, as did streams that
/// moved nothing; only streams that ended cleanly reached their byte cap.
pub(crate) fn decide(
    active: usize,
    error: Option<SpeedError>,
    bytes: u64,
    elapsed: f64,
    duration: f64,
) -> Next {
    if active == 0 {
        if let Some(error) = error
            && (bytes == 0 || elapsed < duration)
        {
            return Next::Fail(error);
        }
        return if elapsed < duration {
            Next::CapReached
        } else {
            Next::Finish
        };
    }
    if elapsed >= duration {
        Next::Finish
    } else {
        Next::Continue
    }
}

/// Live streams and the first error among the ones that ended.
type Streams = Arc<Mutex<(usize, Option<SpeedError>)>>;

async fn measure_direction(
    route: &Route,
    server: &Server,
    params: &Params,
    progress: &watch::Sender<Progress>,
    uploading: bool,
) -> Result<f64, SpeedError> {
    let cap = if uploading {
        params.upload_cap
    } else {
        params.download_cap
    };
    let per_stream = cap / params.streams.max(1) as u64;
    let bytes = Arc::new(AtomicU64::new(0));
    let streams: Streams = Arc::new(Mutex::new((params.streams, None)));
    // Dropping the set (on return or cancel) aborts every stream and closes its socket.
    let mut set = JoinSet::new();
    for _ in 0..params.streams {
        let (route, server, bytes, streams) = (
            route.clone(),
            server.clone(),
            bytes.clone(),
            streams.clone(),
        );
        let request_size = params.request_size.max(1);
        set.spawn(async move {
            let outcome = if uploading {
                upload_stream(&route, &server, per_stream, &bytes).await
            } else {
                download_stream(&route, &server, per_stream, request_size, &bytes).await
            };
            let mut state = streams.lock().unwrap_or_else(|e| e.into_inner());
            state.0 -= 1;
            if let Err(error) = outcome {
                state.1.get_or_insert(error);
            }
        });
    }
    let begin = Instant::now();
    let mut direction = Direction::new(params, uploading, progress.borrow().payload_bytes);
    loop {
        tokio::time::sleep(params.tick).await;
        let moved = bytes.load(Ordering::Relaxed);
        let (active, error) = streams.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let elapsed = begin.elapsed().as_secs_f64();
        let mut outcome = None;
        progress.send_modify(|p| outcome = direction.tick(p, moved, active, error, elapsed));
        if let Some(outcome) = outcome {
            return outcome;
        }
    }
}

/// The bookkeeping of one direction's sampling loop, apart from the sockets and the clock.
pub(crate) struct Direction {
    uploading: bool,
    duration: f64,
    /// Payload bytes of the measurement before this direction began.
    before: u64,
    sampler: Sampler,
}

impl Direction {
    pub(crate) fn new(params: &Params, uploading: bool, before: u64) -> Direction {
        Direction {
            uploading,
            duration: params.duration.as_secs_f64(),
            before,
            sampler: Sampler::new(params.warmup),
        }
    }

    /// One tick: `moved` bytes so far, `active` streams left, the first stream error, at
    /// `elapsed` seconds. Updates `p`; returns the direction's outcome once it is over.
    pub(crate) fn tick(
        &mut self,
        p: &mut Progress,
        moved: u64,
        active: usize,
        error: Option<SpeedError>,
        elapsed: f64,
    ) -> Option<Result<f64, SpeedError>> {
        let (rate, point) = self.sampler.sample(moved, elapsed);
        let next = decide(active, error, moved, elapsed, self.duration);
        let shown = elapsed.min(self.duration);
        p.payload_bytes = self.before + moved;
        p.elapsed = shown;
        p.progress = if self.uploading { 0.5 } else { 0.0 } + shown / self.duration / 2.0;
        if self.uploading {
            p.result.upload = Some(rate);
        } else {
            p.result.download = Some(rate);
            p.result.download_series.extend(point);
        }
        match next {
            Next::Continue => None,
            Next::Finish => Some(Ok(rate)),
            Next::CapReached => {
                p.result.notes.push(Note::CapReached {
                    uploading: self.uploading,
                    seconds: elapsed,
                });
                Some(Ok(rate))
            }
            Next::Fail(error) => Some(Err(error)),
        }
    }
}

/// Asks for `total` bytes, at most `request_size` per request, one request after another on
/// the same kept-alive connection; counts body bytes as they arrive.
async fn download_stream(
    route: &Route,
    server: &Server,
    total: u64,
    request_size: u64,
    bytes: &AtomicU64,
) -> Result<(), SpeedError> {
    let mut stream = connect(route, server).await?;
    let mut buf = vec![0u8; READ_BUFFER];
    let mut remaining = total;
    while remaining > 0 {
        let size = request_size.min(remaining);
        remaining -= size;
        stream.write_all(&download_request(server, size)).await?;
        let (head, extra) = read_head(&mut stream).await?;
        head.check()?;
        let mut received = extra.len() as u64;
        bytes.fetch_add(received, Ordering::Relaxed);
        while received < size {
            let n = stream.read(&mut buf).await?;
            if n == 0 {
                return Err(SpeedError::ClosedEarly);
            }
            received += n as u64;
            bytes.fetch_add(n as u64, Ordering::Relaxed);
        }
    }
    Ok(())
}

/// Sends `total` bytes as POST bodies of `UPLOAD_REQUEST`, one request after another on the
/// same kept-alive connection, and counts a request's bytes only once the server answered it.
/// What the socket took has not been sent yet: on the phone route megabytes can wait in
/// buffers past this computer (W2 Task 4 found them in the phone) and drain for seconds or
/// minutes, so counting them would report a burst at the start and then nothing. The socket
/// closes with RST, so whatever is still buffered when the test ends is dropped, not sent.
async fn upload_stream(
    route: &Route,
    server: &Server,
    total: u64,
    bytes: &AtomicU64,
) -> Result<(), SpeedError> {
    let stream = connect(route, server).await?;
    stream.reset_on_close()?;
    let (mut reader, mut writer) = tokio::io::split(stream);
    let block = vec![0x61u8; CHUNK];
    let mut sent = 0u64;
    while sent < total {
        let length = UPLOAD_REQUEST.min(total - sent);
        let send = async {
            writer.write_all(&upload_request(server, length)).await?;
            let mut left = length;
            while left > 0 {
                let n = left.min(CHUNK as u64);
                writer.write_all(&block[..n as usize]).await?;
                left -= n;
            }
            writer.flush().await?;
            Ok::<(), SpeedError>(())
        };
        let reply = read_reply(&mut reader);
        tokio::pin!(send, reply);
        // A refusal can come before the whole body went out; an early 200 still waits for it.
        tokio::select! {
            replied = &mut reply => {
                replied?;
                (&mut send).await?;
            }
            written = &mut send => {
                written?;
                (&mut reply).await?;
            }
        }
        sent += length;
        bytes.fetch_add(length, Ordering::Relaxed);
    }
    Ok(())
}

/// One reply to an upload: a 200 head and its body, which is read and dropped so the
/// connection stays usable for the next request.
async fn read_reply<R: AsyncRead + Unpin>(reader: &mut R) -> Result<(), SpeedError> {
    let (head, extra) = read_head(reader).await?;
    head.check()?;
    let length = head.content_length.ok_or(SpeedError::BadReply)?;
    let mut left = length
        .checked_sub(extra.len() as u64)
        .ok_or(SpeedError::BadReply)?;
    let mut buf = vec![0u8; CHUNK];
    while left > 0 {
        let want = left.min(CHUNK as u64) as usize;
        let n = reader.read(&mut buf[..want]).await?;
        if n == 0 {
            return Err(SpeedError::ClosedEarly);
        }
        left -= n as u64;
    }
    Ok(())
}

/// Which way out was faster, by download.
#[derive(Clone, Debug, PartialEq)]
pub enum Verdict {
    WifiFaster { ratio: f64 },
    PhoneFaster { ratio: f64 },
    Similar,
}

/// Download speeds within 20% of each other are similar; `None` when either is missing or 0.
/// The ratio is rounded to one decimal.
pub fn verdict(phone: Option<f64>, wifi: Option<f64>) -> Option<Verdict> {
    let (phone, wifi) = (phone?, wifi?);
    if phone <= 0.0 || wifi <= 0.0 {
        return None;
    }
    let (high, low) = (phone.max(wifi), phone.min(wifi));
    if high / low <= 1.2 {
        return Some(Verdict::Similar);
    }
    let ratio = (high / low * 10.0).round() / 10.0;
    Some(if phone < wifi {
        Verdict::WifiFaster { ratio }
    } else {
        Verdict::PhoneFaster { ratio }
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::speed::transport::tests::read_request;
    use std::sync::atomic::AtomicUsize;
    use tokio::net::TcpListener;

    #[derive(Clone, Copy, Debug, PartialEq)]
    pub(crate) enum Mode {
        Normal,
        /// Every request gets 429 with `Retry-After: 120`.
        RateLimited,
        /// Downloads of more than 0 bytes stop halfway and the connection closes.
        DropMidBody,
        /// The latency request gets 5 bytes instead of none.
        WrongLength,
        /// Upload bodies are read at once, unpaced; each reply comes this long after its body.
        SlowAck(Duration),
    }

    /// A local stand-in for speed.cloudflare.com, paced to about 13 MB/s per connection so a
    /// one-second test never reaches its caps by accident.
    pub(crate) struct FakeServer {
        port: u16,
        accepted: Arc<AtomicUsize>,
        open: Arc<AtomicUsize>,
        /// Upload requests answered with 200.
        uploads: Arc<AtomicUsize>,
    }

    const PACE: Duration = Duration::from_millis(5);

    struct OpenGuard(Arc<AtomicUsize>);
    impl Drop for OpenGuard {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
        }
    }

    impl FakeServer {
        pub(crate) async fn start(mode: Mode) -> FakeServer {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let accepted = Arc::new(AtomicUsize::new(0));
            let open = Arc::new(AtomicUsize::new(0));
            let uploads = Arc::new(AtomicUsize::new(0));
            let (a, o, u) = (accepted.clone(), open.clone(), uploads.clone());
            tokio::spawn(async move {
                loop {
                    let Ok((stream, _)) = listener.accept().await else {
                        return;
                    };
                    a.fetch_add(1, Ordering::SeqCst);
                    o.fetch_add(1, Ordering::SeqCst);
                    let guard = OpenGuard(o.clone());
                    let uploads = u.clone();
                    tokio::spawn(async move {
                        let _guard = guard;
                        serve(stream, mode, &uploads).await;
                    });
                }
            });
            FakeServer {
                port,
                accepted,
                open,
                uploads,
            }
        }

        pub(crate) fn server(&self) -> Server {
            Server {
                host: "127.0.0.1".into(),
                port: self.port,
                tls: false,
            }
        }

        pub(crate) fn accepted(&self) -> usize {
            self.accepted.load(Ordering::SeqCst)
        }

        pub(crate) fn open(&self) -> usize {
            self.open.load(Ordering::SeqCst)
        }

        pub(crate) fn uploads(&self) -> usize {
            self.uploads.load(Ordering::SeqCst)
        }
    }

    async fn serve(mut stream: tokio::net::TcpStream, mode: Mode, uploads: &AtomicUsize) {
        let block = vec![b'0'; CHUNK];
        let mut buf = vec![0u8; CHUNK];
        loop {
            let (head, body) = read_request(&mut stream).await;
            if head.is_empty() {
                return;
            }
            if mode == Mode::RateLimited {
                let reply = "HTTP/1.1 429 Too Many Requests\r\nRetry-After: 120\r\nContent-Length: 0\r\n\r\n";
                if stream.write_all(reply.as_bytes()).await.is_err() {
                    return;
                }
                continue;
            }
            let line = head.lines().next().unwrap_or_default().to_string();
            if let Some(rest) = line.strip_prefix("GET /__down?bytes=") {
                let size: u64 = rest.split(' ').next().unwrap().parse().unwrap();
                if mode == Mode::WrongLength && size == 0 {
                    let reply = "HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello";
                    let _ = stream.write_all(reply.as_bytes()).await;
                    continue;
                }
                let reply = format!("HTTP/1.1 200 OK\r\nContent-Length: {size}\r\n\r\n");
                if stream.write_all(reply.as_bytes()).await.is_err() {
                    return;
                }
                let send = if mode == Mode::DropMidBody && size > 0 {
                    size / 2
                } else {
                    size
                };
                let mut left = send;
                while left > 0 {
                    let n = left.min(CHUNK as u64) as usize;
                    if stream.write_all(&block[..n]).await.is_err() {
                        return;
                    }
                    left -= n as u64;
                    tokio::time::sleep(PACE).await;
                }
                if send < size {
                    return;
                }
            } else if line.starts_with("POST /__up") {
                let length: u64 = head
                    .lines()
                    .find_map(|l| {
                        l.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|v| v.trim().parse().unwrap())
                    })
                    .unwrap_or(0);
                let mut left = length.saturating_sub(body.len() as u64);
                while left > 0 {
                    let want = left.min(CHUNK as u64) as usize;
                    match stream.read(&mut buf[..want]).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => left -= n as u64,
                    }
                    if !matches!(mode, Mode::SlowAck(_)) {
                        tokio::time::sleep(PACE).await;
                    }
                }
                if let Mode::SlowAck(delay) = mode {
                    tokio::time::sleep(delay).await;
                }
                uploads.fetch_add(1, Ordering::SeqCst);
                let reply = "HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n";
                if stream.write_all(reply.as_bytes()).await.is_err() {
                    return;
                }
            } else {
                return;
            }
        }
    }

    fn small() -> Params {
        Params {
            streams: 2,
            duration: Duration::from_secs(1),
            warmup: Duration::from_millis(200),
            download_cap: 400_000_000,
            upload_cap: 100_000_000,
            request_size: 1_000_000,
            tick: Duration::from_millis(100),
            latency_samples: 3,
        }
    }

    async fn run_against(fake: &FakeServer, params: Params) -> (SpeedResult, Progress) {
        let (tx, _rx) = watch::channel(Progress::default());
        let result = measure(
            Route::Unbound,
            fake.server(),
            params,
            tx.clone(),
            CancellationToken::new(),
        )
        .await;
        let last = tx.borrow().clone();
        (result, last)
    }

    #[tokio::test]
    async fn full_run_reports_latency_both_directions_and_a_series() {
        let fake = FakeServer::start(Mode::Normal).await;
        let (result, last) = run_against(&fake, small()).await;
        assert_eq!(last.stage, Stage::Done);
        assert_eq!(last.progress, 1.0);
        assert_eq!(last.result, result);
        assert!(result.latency.is_some_and(|ms| ms > 0.0), "{result:?}");
        assert!(result.download.is_some_and(|m| m > 0.0), "{result:?}");
        assert!(result.upload.is_some_and(|m| m > 0.0), "{result:?}");
        assert!(!result.download_series.is_empty());
        assert!(result.notes.is_empty(), "{:?}", result.notes);
        assert!(last.payload_bytes > 0);
        // One latency connection plus two streams per direction.
        assert_eq!(fake.accepted(), 5);
    }

    #[tokio::test]
    async fn upload_counts_only_confirmed_bytes() {
        // Bodies are taken at once and each reply comes 1 s later, so the socket takes bytes
        // long before the server confirms them. At no tick may the count run ahead of the
        // answered requests; counting what the socket took puts megabytes there at the first
        // tick, with nothing answered.
        let fake = FakeServer::start(Mode::SlowAck(Duration::from_secs(1))).await;
        let params = Params {
            streams: 1,
            duration: Duration::from_secs(3),
            warmup: Duration::ZERO,
            download_cap: 1,
            upload_cap: 5 * UPLOAD_REQUEST,
            ..small()
        };
        let (tx, mut rx) = watch::channel(Progress::default());
        let task = tokio::spawn(measure(
            Route::Unbound,
            fake.server(),
            params,
            tx,
            CancellationToken::new(),
        ));
        let mut ticks = 0;
        while rx.changed().await.is_ok() {
            let p = rx.borrow_and_update().clone();
            if p.stage == Stage::Upload && p.result.upload.is_some() {
                ticks += 1;
                // The download moved 1 byte.
                let uploaded = p.payload_bytes - 1;
                let answered = fake.uploads() as u64 * UPLOAD_REQUEST;
                assert!(
                    uploaded <= answered,
                    "{uploaded} counted, {answered} answered"
                );
            }
        }
        let result = task.await.unwrap();
        assert!(ticks > 10, "{ticks}");
        // About 1 request a second: the third reply lands at about 3 s, as the last tick does,
        // so at most 3 MB × 8 / 2.9 s.
        let upload = result.upload.unwrap();
        assert!(upload > 0.0 && upload <= 8.5, "{upload}");
    }

    #[tokio::test]
    async fn cap_reached_early_adds_a_note() {
        let fake = FakeServer::start(Mode::Normal).await;
        let params = Params {
            download_cap: 2_000_000,
            ..small()
        };
        let (result, last) = run_against(&fake, params).await;
        assert_eq!(last.stage, Stage::Done);
        assert!(
            matches!(result.notes.as_slice(), [Note::CapReached { uploading: false, seconds }] if *seconds < 1.0),
            "{:?}",
            result.notes
        );
        assert!(result.download.is_some_and(|m| m > 0.0));
    }

    #[tokio::test]
    async fn rate_limit_explains_minutes() {
        let fake = FakeServer::start(Mode::RateLimited).await;
        let (result, last) = run_against(&fake, small()).await;
        assert_eq!(
            last.stage,
            Stage::Failed(SpeedError::RateLimited { minutes: 2 })
        );
        assert_eq!(result.latency, None);
    }

    #[tokio::test]
    async fn drop_mid_test_fails() {
        let fake = FakeServer::start(Mode::DropMidBody).await;
        let (result, last) = run_against(&fake, small()).await;
        assert!(
            matches!(
                last.stage,
                Stage::Failed(SpeedError::ClosedEarly | SpeedError::Io(_))
            ),
            "{:?}",
            last.stage
        );
        assert!(result.latency.is_some(), "latency came before the drop");
        assert!(result.notes.is_empty(), "a drop is not the byte cap");
        assert_eq!(result.upload, None);
    }

    #[tokio::test]
    async fn cancel_stops_promptly() {
        let fake = FakeServer::start(Mode::Normal).await;
        let (tx, mut rx) = watch::channel(Progress::default());
        let cancel = CancellationToken::new();
        let task = tokio::spawn(measure(
            Route::Unbound,
            fake.server(),
            Params {
                duration: Duration::from_secs(30),
                ..small()
            },
            tx,
            cancel.clone(),
        ));
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(fake.open() > 0, "streams are running");
        let asked = Instant::now();
        cancel.cancel();
        tokio::time::timeout(Duration::from_millis(500), task)
            .await
            .expect("measure returns within 500 ms")
            .unwrap();
        assert_eq!(rx.borrow_and_update().stage, Stage::Cancelled);
        while fake.open() > 0 {
            assert!(
                asked.elapsed() < Duration::from_millis(500),
                "{} connections still open",
                fake.open()
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn missing_interface_fails_the_measurement() {
        // SpeedEngineTests.testMissingInterfaceEndsTheStreamsWithAnError.
        let fake = FakeServer::start(Mode::Normal).await;
        let (tx, _rx) = watch::channel(Progress::default());
        measure(
            Route::Interface("cg-no-such0".into()),
            fake.server(),
            small(),
            tx.clone(),
            CancellationToken::new(),
        )
        .await;
        assert_eq!(
            tx.borrow().stage,
            Stage::Failed(SpeedError::NoInterface("cg-no-such0".into()))
        );
        assert_eq!(fake.accepted(), 0, "nothing went out unbound");
    }

    type State = fn(u64) -> (u64, usize, Option<SpeedError>);

    /// Runs one direction's bookkeeping the way the loop does, with the Mac tests' virtual
    /// 0.5 s ticks; `state(tick)` gives (bytes, active, error).
    fn simulate_direction(
        p: &mut Progress,
        uploading: bool,
        state: State,
    ) -> Result<f64, SpeedError> {
        let mut direction = Direction::new(&Params::default(), uploading, p.payload_bytes);
        for tick in 1.. {
            let (bytes, active, error) = state(tick);
            if let Some(outcome) = direction.tick(p, bytes, active, error, tick as f64 * 0.5) {
                return outcome;
            }
        }
        unreachable!()
    }

    /// Download then upload with the same streams, then the final stage, as `measure` does
    /// once latency is in.
    fn simulate(state: State) -> Progress {
        let mut p = Progress::default();
        let outcome = simulate_direction(&mut p, false, state)
            .and_then(|_| simulate_direction(&mut p, true, state))
            .map(|_| ());
        finish(&mut p, Some(outcome), false);
        p
    }

    fn near(value: Option<f64>, expected: f64, tolerance: f64) -> bool {
        value.is_some_and(|v| (v - expected).abs() < tolerance)
    }

    #[test]
    fn steady_rate_after_warmup() {
        // 1,250,000 bytes per 0.5 s tick = 20 Mbps; 20 ticks per direction.
        let p = simulate(|t| (t * 1_250_000, 6, None));
        assert_eq!(p.stage, Stage::Done);
        assert_eq!(p.progress, 1.0);
        assert!(near(p.result.download, 20.0, 0.5), "{:?}", p.result);
        assert!(near(p.result.upload, 20.0, 0.5), "{:?}", p.result);
        assert_eq!(p.result.download_series.len(), 20);
        assert!(
            p.result
                .download_series
                .iter()
                .all(|v| (v - 20.0).abs() < 0.001)
        );
        assert!(p.result.notes.is_empty());
        // Download and upload each moved 20 ticks × 1.25 MB.
        assert_eq!(p.payload_bytes, 2 * 20 * 1_250_000);
    }

    #[test]
    fn warmup_is_left_out() {
        // 0.1 MB per tick for the first 2 s, then 1.25 MB per tick.
        let p = simulate(|t| {
            (
                t.min(4) * 100_000 + t.saturating_sub(4) * 1_250_000,
                6,
                None,
            )
        });
        assert!(near(p.result.download, 20.0, 0.001), "{:?}", p.result);
        assert!(near(p.result.upload, 20.0, 0.001), "{:?}", p.result);
    }

    #[test]
    fn cap_reached_early_adds_a_note_per_direction() {
        let p = simulate(|t| (t * 1_000, if t < 5 { 6 } else { 0 }, None));
        assert_eq!(p.stage, Stage::Done);
        assert_eq!(
            p.result.notes,
            vec![
                Note::CapReached {
                    uploading: false,
                    seconds: 2.5
                },
                Note::CapReached {
                    uploading: true,
                    seconds: 2.5
                },
            ]
        );
        assert_eq!(p.result.download_series.len(), 5);
    }

    #[test]
    fn failure_with_no_bytes_fails() {
        let p = simulate(|_| (0, 0, Some(SpeedError::ClosedEarly)));
        assert_eq!(p.stage, Stage::Failed(SpeedError::ClosedEarly));
        assert_eq!(p.result.upload, None);
    }

    #[test]
    fn drop_after_bytes_fails() {
        // Bytes arrive, then every stream ends with an error at 2.5 s: a lost connection.
        let p = simulate(|t| {
            let error = (t >= 5).then_some(SpeedError::ClosedEarly);
            (t * 1_000, if t < 5 { 6 } else { 0 }, error)
        });
        assert_eq!(p.stage, Stage::Failed(SpeedError::ClosedEarly));
        assert!(p.result.notes.is_empty(), "a drop is not the byte cap");
    }

    #[tokio::test]
    async fn no_streams_fails_instead_of_a_zero_rate_done() {
        let fake = FakeServer::start(Mode::Normal).await;
        for params in [
            Params {
                streams: 0,
                ..small()
            },
            Params {
                download_cap: 1,
                ..small()
            },
        ] {
            let (result, last) = run_against(&fake, params).await;
            assert!(
                matches!(last.stage, Stage::Failed(SpeedError::Io(_))),
                "{:?}",
                last.stage
            );
            assert_eq!((result.download, result.upload), (None, None));
        }
        assert_eq!(fake.accepted(), 0);
    }

    #[test]
    fn mac_parameters_are_the_default() {
        let p = Params::default();
        assert_eq!(
            (p.streams, p.duration, p.warmup, p.tick, p.latency_samples),
            (
                6,
                Duration::from_secs(10),
                Duration::from_secs(2),
                Duration::from_millis(500),
                3
            )
        );
        assert_eq!(
            (p.download_cap, p.upload_cap, p.request_size),
            (1_000_000_000, 300_000_000, 90_000_000)
        );
    }

    #[test]
    fn verdict_bands() {
        assert_eq!(
            verdict(Some(50.0), Some(160.0)),
            Some(Verdict::WifiFaster { ratio: 3.2 })
        );
        assert_eq!(
            verdict(Some(200.0), Some(100.0)),
            Some(Verdict::PhoneFaster { ratio: 2.0 })
        );
        assert_eq!(verdict(Some(100.0), Some(115.0)), Some(Verdict::Similar));
        assert_eq!(verdict(Some(100.0), Some(84.0)), Some(Verdict::Similar));
        assert_eq!(verdict(None, Some(50.0)), None);
        assert_eq!(verdict(Some(0.0), Some(50.0)), None);
    }

    /// Unprivileged SO_BINDTODEVICE on the loopback interface against the local fake: the whole
    /// measurement works through a bound socket, with no real network involved.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn bound_to_loopback_measures_against_the_fake() {
        let fake = FakeServer::start(Mode::Normal).await;
        let (tx, _rx) = watch::channel(Progress::default());
        let result = measure(
            Route::Interface("lo".into()),
            fake.server(),
            small(),
            tx.clone(),
            CancellationToken::new(),
        )
        .await;
        assert_eq!(tx.borrow().stage, Stage::Done, "{result:?}");
        assert!(result.download.is_some_and(|m| m > 0.0), "{result:?}");
        assert!(result.upload.is_some_and(|m| m > 0.0), "{result:?}");
    }

    /// About 25 MB to the real Cloudflare through the machine's physical uplink; run by hand:
    /// `cargo test -p cg-tools -- --ignored cloudflare_small`.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    #[ignore = "uses the real speed.cloudflare.com"]
    async fn cloudflare_small() {
        let uplink = crate::uplink::physical_uplink().expect("a physical default route");
        let params = Params {
            duration: Duration::from_secs(2),
            warmup: Duration::from_millis(500),
            download_cap: 20_000_000,
            upload_cap: 5_000_000,
            request_size: 10_000_000,
            ..Params::default()
        };
        let (tx, _rx) = watch::channel(Progress::default());
        let result = measure(
            Route::Interface(uplink),
            Server::cloudflare(),
            params,
            tx.clone(),
            CancellationToken::new(),
        )
        .await;
        assert_eq!(tx.borrow().stage, Stage::Done, "{result:?}");
        assert!(result.latency.is_some() && result.download.is_some() && result.upload.is_some());
    }

    /// Three tiny requests (no payload) to the real Cloudflare, bound to the uplink: proves
    /// connect, TLS and the bound socket without moving data. Run by hand with `--ignored`.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    #[ignore = "uses the real speed.cloudflare.com"]
    async fn cloudflare_latency_only() {
        let uplink = crate::uplink::physical_uplink().expect("a physical default route");
        let ms = latency(&Route::Interface(uplink), &Server::cloudflare(), 3)
            .await
            .unwrap();
        assert!(ms.is_some_and(|ms| ms > 0.0), "{ms:?}");
    }
}
