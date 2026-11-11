//! Release-mode cost to the relay of subscribes that wait for a replacement
//! registration.
//!
//! Each case registers one service per hundred subscribes, each through a
//! legacy control connection that reads its stream requests and answers none,
//! connects `n` subscribers and then sends their subscribes at once, spread
//! over the services. Each waits the relay's first acknowledgement budget
//! (300 ms), suspects its registration and then waits for a replacement until
//! its recovery window ends. None comes, and each subscribe is refused.
//! Meanwhile a probe asks the relay for its registered services every 5 ms:
//! the routing manager takes each probe's connection and answers its request.
//! The relay logs as the `pb-mapper` binary does by default, pretty at
//! `info`, here to a file.
//!
//! Prints per case, from the subscribes to the last refusal: `wait_ms`, how
//! long that took; `cpu_ms`, the CPU time the whole process used (Windows
//! charges it by the clock tick, which this bursty load defeats: read it on
//! Linux); `probe_p50_us`, `probe_p99_us` and `probe_max_us`, the probe's
//! round trips; `log_lines`, what the relay logged; `asks`, how many times
//! the routing manager found a subscribe nothing to route to, once per ask of
//! a subscribe that then waits; `refused`, of `n`; and `waited`, those
//! refused after a second or more, which waited for a replacement. The `idle`
//! case probes for as long with nothing waiting.
//!
//! Run with `cargo run --release -p pb-mapper-cli --example resubscribe_bench`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs::File;
use std::hint::black_box;
use std::io::Write;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use pb_mapper_auth::{AuthConfig, AuthRuntime, LegacyProtocolPolicy};
use pb_mapper_core::checksum::set_process_msg_header_key;
use pb_mapper_core::test_support::{TEST_ADMIN_KEY, TEST_ADMIN_KEY_BYTES};
use pb_mapper_protocol::command::{
    MessageSerializer, PbConnRequest, PbConnResponse, PbConnStatusReq,
};
use pb_mapper_protocol::{
    MessageReader, MessageWriter, get_header_msg_reader, get_header_msg_writer,
};
use pb_mapper_server::{RelayServerOptions, run_server_on_listener_with_options};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Barrier;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::{EnvFilter, Layer, fmt};

/// Subscribes sent at once, per case.
const SUBSCRIBES: [usize; 3] = [100, 400, 1000];
const SUBSCRIBES_PER_SERVICE: usize = 100;
const PROBE_EVERY: Duration = Duration::from_millis(5);
/// How long the `idle` case probes: about as long as a wait lasts.
const IDLE_PROBING: Duration = Duration::from_millis(2_500);

/// The relay's log file, counting the lines written to it.
#[derive(Clone)]
struct LogFile {
    file: Arc<File>,
    lines: Arc<AtomicU64>,
}

impl Write for LogFile {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let written = (&*self.file).write(buf)?;
        let lines = buf[..written].iter().filter(|byte| **byte == b'\n').count();
        self.lines.fetch_add(lines as u64, Ordering::Relaxed);
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        (&*self.file).flush()
    }
}

/// Counts the routing manager's answers to a subscribe that found no
/// registration to route to, logged as `subscribe_no_usable_server_conn` or
/// `subscribe_key_missing`: one per ask of a subscribe that then waits.
struct FruitlessAsks(Arc<AtomicU64>);

impl<S: tracing::Subscriber> Layer<S> for FruitlessAsks {
    fn on_event(&self, event: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        struct Name(bool);
        impl tracing::field::Visit for Name {
            fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
                if field.name() == "event"
                    && matches!(
                        value,
                        "subscribe_no_usable_server_conn" | "subscribe_key_missing"
                    )
                {
                    self.0 = true;
                }
            }

            fn record_debug(&mut self, _: &tracing::field::Field, _: &dyn std::fmt::Debug) {}
        }
        let mut name = Name(false);
        event.record(&mut name);
        if name.0 {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// What the relay logged: lines, and the manager's fruitless asks.
struct Logged {
    lines: Arc<AtomicU64>,
    asks: Arc<AtomicU64>,
}

impl Logged {
    fn now(&self) -> (u64, u64) {
        (
            self.lines.load(Ordering::Relaxed),
            self.asks.load(Ordering::Relaxed),
        )
    }
}

/// The CPU time this process has used, user and kernel.
#[cfg(windows)]
fn cpu_time() -> Duration {
    use std::ffi::c_void;

    #[repr(C)]
    #[derive(Default)]
    struct FileTime {
        low: u32,
        high: u32,
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentProcess() -> *mut c_void;
        fn GetProcessTimes(
            process: *mut c_void,
            creation: *mut FileTime,
            exit: *mut FileTime,
            kernel: *mut FileTime,
            user: *mut FileTime,
        ) -> i32;
    }
    let [mut creation, mut exit, mut kernel, mut user]: [FileTime; 4] = Default::default();
    // SAFETY: the pseudo-handle of the current process needs no closing, and
    // `GetProcessTimes` writes only the four times it is given.
    let succeeded = unsafe {
        GetProcessTimes(
            GetCurrentProcess(),
            &mut creation,
            &mut exit,
            &mut kernel,
            &mut user,
        )
    };
    assert_ne!(succeeded, 0, "GetProcessTimes failed");
    let hundreds_of_ns = |time: FileTime| (u64::from(time.high) << 32) | u64::from(time.low);
    Duration::from_nanos((hundreds_of_ns(kernel) + hundreds_of_ns(user)) * 100)
}

/// The CPU time this process has used, user and system, from
/// `/proc/self/stat` in its clock ticks of 10 ms.
#[cfg(not(windows))]
fn cpu_time() -> Duration {
    let stat = std::fs::read_to_string("/proc/self/stat").unwrap();
    let after_name = &stat[stat.rfind(')').unwrap() + 2..];
    let fields = after_name.split(' ').collect::<Vec<_>>();
    let ticks = fields[11].parse::<u64>().unwrap() + fields[12].parse::<u64>().unwrap();
    Duration::from_millis(ticks * 10)
}

struct Relay {
    addr: SocketAddr,
    shutdown: CancellationToken,
    task: tokio::task::JoinHandle<()>,
    state_dir: PathBuf,
}

impl Relay {
    async fn start(case: &str) -> Self {
        let state_dir = std::env::temp_dir().join(format!(
            "pb-mapper-resubscribe-bench-{}-{case}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&state_dir);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let auth = AuthRuntime::start(
            TEST_ADMIN_KEY_BYTES,
            AuthConfig {
                state_dir: state_dir.clone(),
                max_temporary_keys: 8,
                max_temporary_key_ttl: Duration::from_secs(3600),
                legacy_protocol: LegacyProtocolPolicy::Allow,
            },
        )
        .await
        .unwrap();
        let shutdown = CancellationToken::new();
        let task = tokio::spawn({
            let shutdown = shutdown.clone();
            async move {
                run_server_on_listener_with_options(
                    listener,
                    shutdown,
                    None,
                    false,
                    auth,
                    RelayServerOptions::default(),
                )
                .await
                .unwrap();
            }
        });
        Self {
            addr,
            shutdown,
            task,
            state_dir,
        }
    }

    async fn stop(self) {
        self.shutdown.cancel();
        self.task.await.unwrap();
        let _ = std::fs::remove_dir_all(&self.state_dir);
    }
}

/// A registration of `key` that reads its stream requests and answers none,
/// until the relay retires it. Returns once it is registered.
async fn silent_provider(addr: SocketAddr, key: String) -> tokio::task::JoinHandle<()> {
    let control = TcpStream::connect(addr).await.unwrap();
    let (mut read, mut write) = control.into_split();
    let request = PbConnRequest::Register {
        direct: None,
        need_codec: false,
        is_datagram: false,
        key,
        protocol_version: None,
        client_instance_id: None,
        heartbeat_interval_ms: None,
        heartbeat_tolerance_ms: None,
    }
    .encode()
    .unwrap();
    get_header_msg_writer(&mut write)
        .unwrap()
        .write_msg(&request)
        .await
        .unwrap();
    let (registered, waiting) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        let _write = write;
        let mut reader = get_header_msg_reader(&mut read).unwrap();
        let response = reader.read_msg().await.unwrap();
        assert!(matches!(
            PbConnResponse::decode(response).unwrap(),
            PbConnResponse::Register(_)
        ));
        registered.send(()).unwrap();
        while reader.read_msg().await.is_ok() {}
    });
    waiting.await.unwrap();
    task
}

/// A subscriber of `key`: it connects, sends its subscribe once every
/// subscriber of the case is `ready`, and answers whether the relay refused
/// it as a subscribe no registration can serve, and whether that answer took a
/// second or more, as it does for one that waited for a replacement.
async fn subscribe(addr: SocketAddr, key: String, ready: Arc<Barrier>) -> (bool, bool) {
    let mut client = TcpStream::connect(addr).await.unwrap();
    let request = PbConnRequest::Subcribe {
        data_protocol: None,
        key,
    }
    .encode()
    .unwrap();
    ready.wait().await;
    let sent = Instant::now();
    get_header_msg_writer(&mut client)
        .unwrap()
        .write_msg(&request)
        .await
        .unwrap();
    let mut reader = get_header_msg_reader(&mut client).unwrap();
    let refused = match timeout(Duration::from_secs(10), reader.read_msg()).await {
        Ok(Ok(answer)) => matches!(
            PbConnResponse::decode(answer),
            Ok(PbConnResponse::Error(refusal)) if refusal.code == "service_not_available"
        ),
        _ => false,
    };
    (refused, sent.elapsed() >= Duration::from_secs(1))
}

/// One status request on a connection of its own, through the routing
/// manager twice: its accept and its answer.
async fn probe(addr: SocketAddr) -> Duration {
    let start = Instant::now();
    let mut stream = TcpStream::connect(addr).await.unwrap();
    let request = PbConnRequest::Status(PbConnStatusReq::Keys)
        .encode()
        .unwrap();
    get_header_msg_writer(&mut stream)
        .unwrap()
        .write_msg(&request)
        .await
        .unwrap();
    let mut reader = get_header_msg_reader(&mut stream).unwrap();
    black_box(reader.read_msg().await.unwrap());
    start.elapsed()
}

fn percentile(sorted: &[Duration], fraction: f64) -> f64 {
    let index = ((sorted.len() - 1) as f64 * fraction).round() as usize;
    sorted[index].as_secs_f64() * 1e6
}

async fn measure(case: &str, subscribes_sent: usize, logged: &Logged) {
    let relay = Relay::start(case).await;
    let services = subscribes_sent.div_ceil(SUBSCRIBES_PER_SERVICE).max(1);
    let mut providers = Vec::new();
    for service in 0..services {
        providers.push(silent_provider(relay.addr, format!("service-{service}")).await);
    }
    // The relay's own start and the registrations are not measured.
    for _ in 0..20 {
        probe(relay.addr).await;
    }
    // Connected before the clock starts, so that the subscribes reach the
    // relay together, inside one acknowledgement budget.
    let ready = Arc::new(Barrier::new(subscribes_sent + 1));
    let mut subscribes = tokio::task::JoinSet::new();
    for index in 0..subscribes_sent {
        subscribes.spawn(subscribe(
            relay.addr,
            format!("service-{}", index % services),
            ready.clone(),
        ));
    }
    ready.wait().await;
    let (cpu_before, logged_before, start) = (cpu_time(), logged.now(), Instant::now());
    let mut probes = Vec::new();
    let (mut refused, mut waited) = (0, 0);
    loop {
        while let Some(answer) = subscribes.try_join_next() {
            let (was_refused, did_wait) = answer.unwrap();
            refused += usize::from(was_refused);
            waited += usize::from(did_wait);
        }
        let done = if subscribes_sent == 0 {
            start.elapsed() >= IDLE_PROBING
        } else {
            subscribes.is_empty()
        };
        if done {
            break;
        }
        probes.push(probe(relay.addr).await);
        tokio::time::sleep(PROBE_EVERY).await;
    }
    let wait = start.elapsed();
    let cpu = cpu_time() - cpu_before;
    let (lines, asks) = logged.now();
    let (lines, asks) = (lines - logged_before.0, asks - logged_before.1);
    probes.sort();
    println!(
        "case={case} wait_ms={:.0} cpu_ms={:.0} probe_p50_us={:.1} probe_p99_us={:.1} probe_max_us={:.1} log_lines={lines} asks={asks} refused={refused} waited={waited}",
        wait.as_secs_f64() * 1e3,
        cpu.as_secs_f64() * 1e3,
        percentile(&probes, 0.5),
        percentile(&probes, 0.99),
        probes.last().unwrap().as_secs_f64() * 1e6,
    );
    for provider in providers {
        provider.abort();
    }
    relay.stop().await;
}

fn main() {
    // Every case's subscribes arrive inside one acknowledgement budget: let
    // the namespace's burst take all of them.
    // SAFETY: no other thread exists yet.
    unsafe { std::env::set_var("PB_MAPPER_NEW_STREAMS_BURST", "4096") };
    let log_path = std::env::temp_dir().join(format!(
        "pb-mapper-resubscribe-bench-{}.log",
        std::process::id()
    ));
    let log = LogFile {
        file: Arc::new(File::create(&log_path).unwrap()),
        lines: Arc::new(AtomicU64::new(0)),
    };
    let logged = Logged {
        lines: log.lines.clone(),
        asks: Arc::new(AtomicU64::new(0)),
    };
    let subscriber = tracing_subscriber::registry()
        .with(
            fmt::layer()
                .pretty()
                .with_writer(move || log.clone())
                .with_filter(
                    EnvFilter::builder()
                        .with_default_directive(tracing::level_filters::LevelFilter::INFO.into())
                        .from_env_lossy(),
                ),
        )
        .with(
            FruitlessAsks(logged.asks.clone())
                .with_filter(tracing::level_filters::LevelFilter::WARN),
        );
    tracing::subscriber::set_global_default(subscriber).unwrap();
    set_process_msg_header_key(Some(TEST_ADMIN_KEY)).unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        measure("idle", 0, &logged).await;
        for subscribes in SUBSCRIBES {
            measure(&format!("wait{subscribes}"), subscribes, &logged).await;
        }
    });
    drop(runtime);
    let _ = std::fs::remove_file(log_path);
}
