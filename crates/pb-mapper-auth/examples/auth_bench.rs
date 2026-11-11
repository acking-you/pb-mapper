//! Release-mode cost of what a relay asks of the authentication state.
//!
//! For each new connection: deriving a key from its id, and authenticating the
//! key a peer presented, for a temporary key and, as a control, the
//! administrator.
//!
//! For an administrator's mutation (a renewal: one durable WAL append), a
//! compaction (a GC, which writes the snapshot the five-minute compaction
//! writes, after a sweep of the slot table) and a start: how long each takes,
//! and how long it holds up the other tasks on the caller's runtime, on a
//! current-thread runtime and on a multi-thread one. Busy probe tasks, one per
//! worker, yield in a loop and record the longest gap between two of their
//! turns; an operation's hold is the longest gap that ended while it ran or
//! just after.
//!
//! Run with `cargo run --release -p pb-mapper-auth --example auth_bench`.

use std::error::Error;
use std::future::Future;
use std::hint::black_box;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use pb_mapper_auth::{
    ADMIN_KEY_ID, AuthConfig, AuthContext, AuthRuntime, DEFAULT_TEMP_KEY_CAPACITY, KeyId,
    LegacyProtocolPolicy, MAX_TEMP_KEY_CAPACITY,
};
use pb_mapper_core::test_support::{TEST_ADMIN_KEY_BYTES, TempPath};
use tokio::runtime::{Builder, Runtime};
use tokio::task::JoinHandle;

type BenchResult<T> = Result<T, Box<dyn Error + Send + Sync>>;

const CALLS: usize = 200_000;
const SAMPLES: usize = 9;
const HOUR: Duration = Duration::from_secs(3600);
/// Renewals timed on each runtime, after a tenth as many untimed.
const RENEWALS: usize = 200;
/// Compactions and starts timed per capacity on each runtime.
const ROUNDS: usize = 20;
/// The keys a compacted or restarted store holds.
const KEYS: usize = 100;
/// The multi-thread runtime's workers.
const WORKERS: usize = 4;

/// Nanoseconds per call of `call`, the median of `SAMPLES` runs.
fn time(mut call: impl FnMut() -> BenchResult<()>) -> BenchResult<f64> {
    for _ in 0..CALLS / 10 {
        call()?;
    }
    let mut samples = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        let start = Instant::now();
        for _ in 0..CALLS {
            call()?;
        }
        samples.push(start.elapsed().as_secs_f64() * 1e9 / CALLS as f64);
    }
    samples.sort_by(f64::total_cmp);
    Ok(samples[SAMPLES / 2])
}

fn config(state_dir: &Path, capacity: usize) -> AuthConfig {
    AuthConfig {
        state_dir: state_dir.to_path_buf(),
        max_temporary_keys: capacity,
        max_temporary_key_ttl: HOUR,
        legacy_protocol: LegacyProtocolPolicy::Deny,
    }
}

async fn start(state_dir: &Path, capacity: usize) -> BenchResult<(AuthRuntime, AuthContext)> {
    let runtime = AuthRuntime::start(TEST_ADMIN_KEY_BYTES, config(state_dir, capacity)).await?;
    let admin = runtime.authenticate_presented(ADMIN_KEY_ID, &TEST_ADMIN_KEY_BYTES)?;
    Ok((runtime, admin))
}

/// The per-connection checks, which take no lock the actor holds for long.
async fn hot_path() -> BenchResult<()> {
    let state_dir = TempPath::new("auth-bench");
    let (runtime, admin) = start(&state_dir, 16).await?;
    let issued = runtime.issue(&admin, HOUR, None).await?;
    let key_id: KeyId = issued.metadata.key_id;
    let presented = runtime.derive_key(key_id)?;

    let derive = time(|| {
        black_box(runtime.derive_key(black_box(key_id))?);
        Ok(())
    })?;
    let temporary = time(|| {
        black_box(runtime.authenticate_presented(black_box(key_id), black_box(&presented))?);
        Ok(())
    })?;
    let administrator =
        time(|| {
            black_box(runtime.authenticate_presented(
                black_box(ADMIN_KEY_ID),
                black_box(&TEST_ADMIN_KEY_BYTES),
            )?);
            Ok(())
        })?;
    println!("case=derive_temporary_key ns={derive:.1}");
    println!("case=authenticate_temporary ns={temporary:.1}");
    println!("case=authenticate_administrator ns={administrator:.1}");

    drop(admin);
    runtime.shutdown_actor().await;
    Ok(())
}

/// Busy tasks that record the longest wait between two of their turns.
struct Probes {
    /// Nanoseconds, since the last reset.
    longest_gap: Arc<AtomicU64>,
    turns: Vec<Arc<AtomicU64>>,
    stop: Arc<AtomicBool>,
    tasks: Vec<JoinHandle<()>>,
}

impl Probes {
    fn start(count: usize) -> Self {
        let longest_gap = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let turns = (0..count)
            .map(|_| Arc::new(AtomicU64::new(0)))
            .collect::<Vec<_>>();
        let tasks = turns
            .iter()
            .map(|turns| {
                let (longest_gap, turns, stop) = (longest_gap.clone(), turns.clone(), stop.clone());
                tokio::spawn(async move {
                    let mut last = Instant::now();
                    while !stop.load(Ordering::Relaxed) {
                        tokio::task::yield_now().await;
                        let now = Instant::now();
                        let gap = u64::try_from((now - last).as_nanos()).unwrap_or(u64::MAX);
                        longest_gap.fetch_max(gap, Ordering::Relaxed);
                        turns.fetch_add(1, Ordering::Relaxed);
                        last = now;
                    }
                })
            })
            .collect();
        Self {
            longest_gap,
            turns,
            stop,
            tasks,
        }
    }

    fn turns(&self) -> Vec<u64> {
        self.turns
            .iter()
            .map(|turns| turns.load(Ordering::Relaxed))
            .collect()
    }

    /// Waits until every probe has taken two more turns, which closes any gap
    /// open now.
    async fn settle(&self) {
        let before = self.turns();
        while self
            .turns()
            .iter()
            .zip(&before)
            .any(|(now, then)| *now < then + 2)
        {
            tokio::task::yield_now().await;
        }
    }

    /// Runs `operation`, returning its output, how long it took, and the
    /// longest a probe waited meanwhile.
    async fn measure<T>(&self, operation: impl Future<Output = T>) -> (T, Timing) {
        self.settle().await;
        self.longest_gap.store(0, Ordering::Relaxed);
        let start = Instant::now();
        let output = operation.await;
        let latency = start.elapsed();
        self.settle().await;
        let hold = Duration::from_nanos(self.longest_gap.load(Ordering::Relaxed));
        (output, Timing { latency, hold })
    }

    async fn stop(self) {
        self.stop.store(true, Ordering::Relaxed);
        for task in self.tasks {
            let _ = task.await;
        }
    }
}

struct Timing {
    latency: Duration,
    hold: Duration,
}

#[derive(Default)]
struct Timings {
    latency: Vec<Duration>,
    hold: Vec<Duration>,
}

impl Timings {
    fn push(&mut self, Timing { latency, hold }: Timing) {
        self.latency.push(latency);
        self.hold.push(hold);
    }

    fn report(mut self, case: &str) {
        let micros = |samples: &mut Vec<Duration>, quantile: f64| {
            samples.sort();
            let index = ((samples.len() - 1) as f64 * quantile).round() as usize;
            samples[index].as_secs_f64() * 1e6
        };
        println!(
            "case={case} ops={} latency_p50_us={:.1} latency_p99_us={:.1} hold_p50_us={:.1} hold_p99_us={:.1} hold_max_us={:.1}",
            self.latency.len(),
            micros(&mut self.latency, 0.5),
            micros(&mut self.latency, 0.99),
            micros(&mut self.hold, 0.5),
            micros(&mut self.hold, 0.99),
            micros(&mut self.hold, 1.0),
        );
    }
}

/// The administrator's operations, measured on the runtime this runs on.
async fn persistence(flavor: &'static str, probes: usize) -> BenchResult<()> {
    let probes = Probes::start(probes);

    let state_dir = TempPath::new("auth-bench-renew");
    let (runtime, admin) = start(&state_dir, DEFAULT_TEMP_KEY_CAPACITY).await?;
    let key_id = runtime.issue(&admin, HOUR, None).await?.metadata.key_id;
    let mut timings = Timings::default();
    for round in 0..RENEWALS + RENEWALS / 10 {
        let (renewed, timing) = probes.measure(runtime.renew(&admin, key_id, HOUR)).await;
        black_box(renewed?);
        if round >= RENEWALS / 10 {
            timings.push(timing);
        }
    }
    timings.report(&format!("renew_{flavor}"));
    // A command with no file work: what reaching the actor costs.
    let mut timings = Timings::default();
    for round in 0..RENEWALS + RENEWALS / 10 {
        let (status, timing) = probes.measure(runtime.status(&admin)).await;
        black_box(status?);
        if round >= RENEWALS / 10 {
            timings.push(timing);
        }
    }
    timings.report(&format!("status_{flavor}"));
    drop(admin);
    runtime.shutdown_actor().await;

    for capacity in [DEFAULT_TEMP_KEY_CAPACITY, MAX_TEMP_KEY_CAPACITY] {
        let state_dir = TempPath::new("auth-bench-store");
        let (runtime, admin) = start(&state_dir, capacity).await?;
        for _ in 0..KEYS {
            runtime.issue(&admin, HOUR, None).await?;
        }
        let mut timings = Timings::default();
        for _ in 0..ROUNDS {
            let (removed, timing) = probes.measure(runtime.gc(&admin)).await;
            black_box(removed?);
            timings.push(timing);
        }
        timings.report(&format!("gc{capacity}_{flavor}"));
        drop(admin);
        runtime.shutdown_actor().await;
        // The handle holds the directory's lock too.
        drop(runtime);

        let mut timings = Timings::default();
        for _ in 0..ROUNDS {
            let (started, timing) = probes
                .measure(AuthRuntime::start(
                    TEST_ADMIN_KEY_BYTES,
                    config(&state_dir, capacity),
                ))
                .await;
            timings.push(timing);
            started?.shutdown_actor().await;
        }
        timings.report(&format!("start{capacity}_{flavor}"));
    }

    probes.stop().await;
    Ok(())
}

fn current_thread() -> std::io::Result<Runtime> {
    Builder::new_current_thread().enable_all().build()
}

fn multi_thread() -> std::io::Result<Runtime> {
    Builder::new_multi_thread()
        .worker_threads(WORKERS)
        .enable_all()
        .build()
}

fn main() -> BenchResult<()> {
    current_thread()?.block_on(hot_path())?;
    for (flavor, runtime, probes) in [
        ("current", current_thread()?, 1),
        ("multi", multi_thread()?, WORKERS),
    ] {
        // Driven from a task, as a relay's administrator connection drives it.
        runtime.block_on(async { tokio::spawn(persistence(flavor, probes)).await })??;
    }
    Ok(())
}
