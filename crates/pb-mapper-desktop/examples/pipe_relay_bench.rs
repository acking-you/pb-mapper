//! The Windows helper's unit path, in one process: a worker answers each
//! request with a JSON record and a unit, a service relays both pipes as the
//! helper's service does, and the app times each request until it holds the
//! unit, as `RemoteBackend::call` waits for it. Each role runs on its own
//! thread and current-thread runtime, as each runs in its own process. It
//! uses the helper's framing (`src/oop/ipc.rs`) and, by default, its quotas and
//! relay buffers (`src/platform/windows/helper/transfer.rs`).
//!
//! ```text
//! cargo run --release -p pb-mapper-desktop --example pipe_relay_bench -- [flags]
//! ```
//!
//! - `--writes one|records|parts`: how the worker writes an answer and the
//!   app a request. `one`, what ships: both records of an answer in one
//!   vectored write. `records`: one vectored write per record. `parts`, as
//!   before: a write for each length and each body, and a flush after each
//!   record.
//! - `--quota <bytes>`: both pipes' in and out quotas.
//! - `--relay-to-app <bytes>`, `--relay-to-worker <bytes>`: the relay's
//!   buffers.
//! - `--sizes 20000,170000,1000000,3700000`: the units, in bytes.
//! - `--records <n>`: timed requests per unit size (200), after 20 untimed.
//!
//! Each line gives the time from the request's write until the app holds the
//! whole unit (median, p95, mean), and the calls per request that moved bytes:
//! the worker's writes, the relay's reads from the worker and writes to the
//! app, and the app's reads. On a named pipe each write the worker or the
//! relay makes is one overlapped `WriteFile`; a read takes what one completed
//! `ReadFile` delivered, or part of it. To compare mio 1.2.4, set its entry in
//! `Cargo.lock` back, run, and restore the file.
//!
//! Elsewhere it runs over TCP on loopback, to check the harness only: those
//! figures say nothing of named pipes, and `--quota` does nothing there.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use serde::{Deserialize, Serialize};
use std::{
    io,
    pin::Pin,
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    task::{Context, Poll},
    time::{Duration, Instant},
};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, ReadBuf};

#[path = "../src/oop/ipc.rs"]
mod ipc;
#[path = "../src/platform/windows/helper/transfer.rs"]
mod transfer;

/// What the included framing names in its own crate.
type Result<T> = io::Result<T>;
fn error(e: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> io::Error {
    io::Error::other(e)
}

const WARM_UP: usize = 20;

/// How the worker writes an answer and the app a request.
#[derive(Clone, Copy)]
enum Writes {
    One,
    Records,
    Parts,
}
impl Writes {
    fn name(self) -> &'static str {
        match self {
            Self::One => "one",
            Self::Records => "records",
            Self::Parts => "parts",
        }
    }
}

struct Config {
    writes: Writes,
    quota: u32,
    relay_to_app: usize,
    relay_to_worker: usize,
    sizes: Vec<usize>,
    records: usize,
}

impl Config {
    fn parse() -> std::result::Result<Self, String> {
        let mut config = Self {
            writes: Writes::One,
            quota: transfer::PIPE_QUOTA,
            relay_to_app: transfer::RELAY_TO_APP,
            relay_to_worker: transfer::RELAY_TO_WORKER,
            sizes: vec![20_000, 170_000, 1_000_000, 3_700_000],
            records: 200,
        };
        let mut args = std::env::args().skip(1);
        while let Some(flag) = args.next() {
            let value = args.next().ok_or_else(|| format!("{flag} needs a value"))?;
            let number = |value: &str| {
                value
                    .parse::<usize>()
                    .map_err(|_| format!("{flag} takes a number, not {value}"))
            };
            match flag.as_str() {
                "--writes" => {
                    config.writes = match value.as_str() {
                        "one" => Writes::One,
                        "records" => Writes::Records,
                        "parts" => Writes::Parts,
                        _ => return Err(format!("--writes is one, records or parts, not {value}")),
                    }
                }
                "--quota" => {
                    config.quota = u32::try_from(number(&value)?)
                        .map_err(|_| format!("--quota {value} is too large"))?;
                }
                "--relay-to-app" => config.relay_to_app = number(&value)?,
                "--relay-to-worker" => config.relay_to_worker = number(&value)?,
                "--sizes" => {
                    config.sizes = value
                        .split(',')
                        .map(number)
                        .collect::<std::result::Result<_, _>>()?;
                }
                "--records" => config.records = number(&value)?,
                _ => return Err(format!("unknown flag {flag}")),
            }
        }
        if config.sizes.iter().any(|&size| size > ipc::MAX_FRAME) {
            return Err(format!("a unit is at most {} bytes", ipc::MAX_FRAME));
        }
        if config.sizes.is_empty() || config.records == 0 {
            return Err("at least one size and one record".into());
        }
        if config.relay_to_app == 0 || config.relay_to_worker == 0 {
            return Err("a relay buffer holds at least a byte".into());
        }
        Ok(config)
    }
}

/// The app's request.
#[derive(Serialize, Deserialize)]
struct Ask {
    sequence: u64,
    size: usize,
    close: bool,
}

/// The worker's answer, about the size of a capture's.
#[derive(Serialize, Deserialize)]
struct Answer {
    sequence: u64,
    size: usize,
    keyframe: bool,
    context: String,
}

/// Unit byte `index`, which the app checks.
fn pattern(index: usize) -> u8 {
    (index % 251) as u8
}

/// Calls that moved bytes, for the counted ends.
static WORKER_WRITES: AtomicU64 = AtomicU64::new(0);
static RELAY_READS: AtomicU64 = AtomicU64::new(0);
static RELAY_WRITES: AtomicU64 = AtomicU64::new(0);
static APP_READS: AtomicU64 = AtomicU64::new(0);
/// The request direction's, which no line reports.
static UNCOUNTED: AtomicU64 = AtomicU64::new(0);

/// A transport end that counts its reads and writes that moved bytes.
struct Counted<T> {
    io: T,
    reads: &'static AtomicU64,
    writes: &'static AtomicU64,
}
impl<T> Counted<T> {
    fn new(io: T, reads: &'static AtomicU64, writes: &'static AtomicU64) -> Self {
        Self { io, reads, writes }
    }
}
impl<T: AsyncRead + Unpin> AsyncRead for Counted<T> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let before = buf.filled().len();
        let polled = Pin::new(&mut self.io).poll_read(cx, buf);
        if matches!(polled, Poll::Ready(Ok(()))) && buf.filled().len() > before {
            self.reads.fetch_add(1, Ordering::Relaxed);
        }
        polled
    }
}
impl<T: AsyncWrite + Unpin> Counted<T> {
    fn count(&self, polled: &Poll<io::Result<usize>>) {
        if matches!(polled, Poll::Ready(Ok(written)) if *written > 0) {
            self.writes.fetch_add(1, Ordering::Relaxed);
        }
    }
}
impl<T: AsyncWrite + Unpin> AsyncWrite for Counted<T> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let polled = Pin::new(&mut self.io).poll_write(cx, buf);
        self.count(&polled);
        polled
    }
    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        let polled = Pin::new(&mut self.io).poll_write_vectored(cx, bufs);
        self.count(&polled);
        polled
    }
    fn is_write_vectored(&self) -> bool {
        self.io.is_write_vectored()
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.io).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.io).poll_shutdown(cx)
    }
}

/// The counters at one moment.
#[derive(Clone, Copy)]
struct Counts([u64; 4]);
impl Counts {
    fn now() -> Self {
        Self(
            [&WORKER_WRITES, &RELAY_READS, &RELAY_WRITES, &APP_READS]
                .map(|counter| counter.load(Ordering::Relaxed)),
        )
    }
    /// Per request, since `start`.
    fn per(self, start: Self, requests: usize) -> [f64; 4] {
        let mut per = [0.0; 4];
        for ((slot, now), start) in per.iter_mut().zip(self.0).zip(start.0) {
            *slot = (now - start) as f64 / requests as f64;
        }
        per
    }
}

/// Records written as `--writes` says.
async fn send<W: AsyncWrite + Unpin, T: Serialize>(
    io: &mut W,
    writes: Writes,
    value: &T,
    bytes: Option<&[u8]>,
) -> io::Result<()> {
    match (writes, bytes) {
        (Writes::One, Some(bytes)) => ipc::write_record(io, value, bytes).await,
        (Writes::One | Writes::Records, None) => ipc::write_json(io, value).await,
        (Writes::Records, Some(bytes)) => {
            ipc::write_json(io, value).await?;
            ipc::write_bytes(io, bytes, ipc::MAX_FRAME).await
        }
        (Writes::Parts, bytes) => {
            let json = serde_json::to_vec(value).map_err(io::Error::other)?;
            for record in std::iter::once(&json[..]).chain(bytes) {
                let len = u32::try_from(record.len()).map_err(io::Error::other)?;
                io.write_u32(len).await?;
                io.write_all(record).await?;
                io.flush().await?;
            }
            Ok(())
        }
    }
}

fn runtime() -> io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
}

/// The service: both pipes' server, relaying between them until both close.
fn service(
    quota: u32,
    relay_to_app: usize,
    relay_to_worker: usize,
    ready: mpsc::Sender<(transport::Address, transport::Address)>,
) -> io::Result<()> {
    runtime()?.block_on(async move {
        let app = transport::listen("app", quota)?;
        let worker = transport::listen("worker", quota)?;
        ready
            .send((app.address()?, worker.address()?))
            .map_err(io::Error::other)?;
        let caller = app.accept().await?;
        let private = worker.accept().await?;
        let mut caller = Counted::new(caller, &UNCOUNTED, &RELAY_WRITES);
        let mut private = Counted::new(private, &RELAY_READS, &UNCOUNTED);
        tokio::io::copy_bidirectional_with_sizes(
            &mut caller,
            &mut private,
            relay_to_worker,
            relay_to_app,
        )
        .await?;
        Ok(())
    })
}

/// The worker: answers each request with a unit of the size asked, until
/// asked to close.
fn worker(address: transport::Address, writes: Writes, largest: usize) -> io::Result<()> {
    runtime()?.block_on(async move {
        let pipe = transport::connect(&address).await?;
        let mut pipe = Counted::new(pipe, &UNCOUNTED, &WORKER_WRITES);
        let unit: Vec<u8> = (0..largest).map(pattern).collect();
        let mut answer = Answer {
            sequence: 0,
            size: 0,
            keyframe: true,
            // A capture answer's desktop context runs to about this long.
            context: "x".repeat(160),
        };
        loop {
            let ask: Ask = ipc::read_json(&mut pipe).await?;
            let bytes = unit
                .get(..ask.size)
                .ok_or_else(|| error("a unit larger than any asked for"))?;
            (answer.sequence, answer.size) = (ask.sequence, ask.size);
            send(&mut pipe, writes, &answer, Some(bytes)).await?;
            if ask.close {
                return Ok(());
            }
        }
    })
}

/// One request and its answer, as the app makes it.
async fn exchange<P: AsyncRead + AsyncWrite + Unpin>(
    pipe: &mut P,
    writes: Writes,
    sequence: u64,
    size: usize,
    close: bool,
) -> io::Result<()> {
    let ask = Ask {
        sequence,
        size,
        close,
    };
    send(pipe, writes, &ask, None).await?;
    let answer: Answer = ipc::read_json(pipe).await?;
    let unit = ipc::read_bytes(pipe, ipc::MAX_FRAME).await?;
    let whole = unit.len() == size
        && unit
            .last()
            .is_none_or(|&last| last == pattern(size.saturating_sub(1)));
    if answer.sequence != sequence || answer.size != size || !whole {
        return Err(error("the answer is not the one asked for"));
    }
    Ok(())
}

fn median_p95_mean(times: &mut [Duration]) -> (f64, f64, f64) {
    times.sort_unstable();
    let micros = |time: Duration| time.as_secs_f64() * 1e6;
    let at = |fraction: f64| {
        let index = ((times.len() - 1) as f64 * fraction).round() as usize;
        micros(times[index])
    };
    let mean = micros(times.iter().sum::<Duration>()) / times.len() as f64;
    (at(0.5), at(0.95), mean)
}

fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let config = Config::parse()?;
    let largest = config.sizes.iter().copied().max().unwrap_or(0);
    let (ready, addresses) = mpsc::channel();
    let (quota, to_app, to_worker) = (config.quota, config.relay_to_app, config.relay_to_worker);
    let service = std::thread::spawn(move || service(quota, to_app, to_worker, ready));
    let Ok((app_address, worker_address)) = addresses.recv() else {
        service.join().expect("the service thread")?;
        return Err("the service did not start".into());
    };
    let writes = config.writes;
    let worker = std::thread::spawn(move || worker(worker_address, writes, largest));
    runtime()?.block_on(async {
        let pipe = transport::connect(&app_address).await?;
        let mut pipe = Counted::new(pipe, &APP_READS, &UNCOUNTED);
        let mut sequence = 0;
        for &size in &config.sizes {
            for _ in 0..WARM_UP {
                sequence += 1;
                exchange(&mut pipe, writes, sequence, size, false).await?;
            }
            let start = Counts::now();
            let mut times = Vec::with_capacity(config.records);
            for _ in 0..config.records {
                sequence += 1;
                let began = Instant::now();
                exchange(&mut pipe, writes, sequence, size, false).await?;
                times.push(began.elapsed());
            }
            let [worker_writes, relay_reads, relay_writes, app_reads] =
                Counts::now().per(start, config.records);
            let (median, p95, mean) = median_p95_mean(&mut times);
            println!(
                "transport={} writes={} quota={} relay_to_app={} relay_to_worker={} \
                 unit_bytes={size} records={} median_us={median:.1} p95_us={p95:.1} \
                 mean_us={mean:.1} mb_per_s={:.1} worker_writes={worker_writes:.2} \
                 relay_reads={relay_reads:.2} relay_writes={relay_writes:.2} \
                 app_reads={app_reads:.2}",
                transport::KIND,
                config.writes.name(),
                config.quota,
                config.relay_to_app,
                config.relay_to_worker,
                config.records,
                size as f64 / median,
            );
        }
        exchange(&mut pipe, writes, sequence + 1, 0, true).await
    })?;
    worker.join().expect("the worker thread")?;
    service.join().expect("the service thread")?;
    Ok(())
}

/// Named pipes as the helper creates them: byte mode, local clients only,
/// at most 8 instances, with the quota in each direction.
#[cfg(windows)]
mod transport {
    use std::io;
    use tokio::net::windows::named_pipe::{
        ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions,
    };

    pub const KIND: &str = "named-pipe";
    pub type Address = String;

    pub struct Listener {
        server: NamedPipeServer,
        name: String,
    }
    pub fn listen(role: &str, quota: u32) -> io::Result<Listener> {
        let name = format!(
            r"\\.\pipe\pb-mapper.pipe-relay-bench.{}.{role}",
            std::process::id()
        );
        let server = ServerOptions::new()
            .first_pipe_instance(true)
            .reject_remote_clients(true)
            .max_instances(8)
            .in_buffer_size(quota)
            .out_buffer_size(quota)
            .create(&name)?;
        Ok(Listener { server, name })
    }
    impl Listener {
        pub fn address(&self) -> io::Result<Address> {
            Ok(self.name.clone())
        }
        pub async fn accept(self) -> io::Result<NamedPipeServer> {
            self.server.connect().await?;
            Ok(self.server)
        }
    }
    pub async fn connect(address: &Address) -> io::Result<NamedPipeClient> {
        ClientOptions::new().open(address)
    }
}

/// TCP on loopback, without Nagle's delay: only to check the harness where
/// there are no named pipes.
#[cfg(not(windows))]
mod transport {
    use std::{
        io,
        net::{Ipv4Addr, SocketAddr},
    };
    use tokio::net::{TcpListener, TcpStream};

    pub const KIND: &str = "tcp-loopback";
    pub type Address = SocketAddr;

    pub struct Listener(TcpListener);
    pub fn listen(_role: &str, _quota: u32) -> io::Result<Listener> {
        let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        listener.set_nonblocking(true)?;
        Ok(Listener(TcpListener::from_std(listener)?))
    }
    impl Listener {
        pub fn address(&self) -> io::Result<Address> {
            self.0.local_addr()
        }
        pub async fn accept(self) -> io::Result<TcpStream> {
            let (stream, _) = self.0.accept().await?;
            stream.set_nodelay(true)?;
            Ok(stream)
        }
    }
    pub async fn connect(address: &Address) -> io::Result<TcpStream> {
        let stream = TcpStream::connect(address).await?;
        stream.set_nodelay(true)?;
        Ok(stream)
    }
}
