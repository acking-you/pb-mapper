//! Release-mode protocol-v2 first-flight admission as the relay runs it:
//! `ServerSecurity::read_initial` on first flights of fresh sessions, each
//! admitted against the relay's durable replay log in its state directory.
//!
//! - `c<n>`: `n` first flights arrive at once and keep arriving, as `n`
//!   connections opening together would: `n` tasks on a multi-thread runtime
//!   read `HANDSHAKES` first flights between them. Prints admissions a second,
//!   the latency of `read_initial` (median, 99th percentile, largest) and how
//!   many failed.
//! - `compact<k>` (with `--compaction`): a log holding `k` live admissions, a
//!   minute after the relay opened it, so that the next admission compacts it;
//!   then `c64`'s load. Prints the same, the largest latency being the wait
//!   behind the compaction.
//!
//! Run with
//! `cargo run --release -p pb-mapper-protocol --example handshake_bench [-- [--compaction] [<dir>]]`;
//! the state directories go under `<dir>` (default: the system temporary
//! directory) and are removed after each case.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use pb_mapper_auth::{AuthConfig, AuthRuntime, LegacyProtocolPolicy};
use pb_mapper_core::checksum::Credential;
use pb_mapper_core::test_support::TEST_ADMIN_KEY_BYTES;
use pb_mapper_protocol::secure::{ClientHeaderSession, ServerSecurity};

const CONCURRENCY: [usize; 4] = [1, 8, 64, 256];
/// First flights per case, warm-up excluded.
const HANDSHAKES: usize = 4_000;
const WARMUP: usize = 64;
/// About what a subscribe's first request carries.
const PAYLOAD_LEN: usize = 128;
/// Live admissions in the compaction case's log: most of the 240,000 the
/// default budget admits in the ten-minute replay window, leaving room for
/// the case's own.
const COMPACTION_RECORDS: usize = 200_000;
/// What the relay waits between compactions, and a little more.
const COMPACTION_WAIT: Duration = Duration::from_secs(61);

/// A first flight of a session of its own, so with a salt of its own.
async fn first_flight(payload: &[u8]) -> Vec<u8> {
    let session = ClientHeaderSession::new_v2(&Credential::Admin(TEST_ADMIN_KEY_BYTES)).unwrap();
    let mut wire = Vec::new();
    session.write_initial(&mut wire, payload).await.unwrap();
    wire
}

async fn security(state_dir: &Path) -> ServerSecurity {
    let auth = AuthRuntime::start(
        TEST_ADMIN_KEY_BYTES,
        AuthConfig {
            state_dir: state_dir.to_path_buf(),
            max_temporary_keys: 8,
            max_temporary_key_ttl: Duration::from_secs(3600),
            legacy_protocol: LegacyProtocolPolicy::Deny,
        },
    )
    .await
    .unwrap();
    ServerSecurity::new(auth)
}

/// `count` first flights read by `concurrency` tasks at once: admissions a
/// second, each one's latency in microseconds, sorted, and the failures.
async fn admit(
    security: &ServerSecurity,
    concurrency: usize,
    count: usize,
) -> (f64, Vec<f64>, usize) {
    let payload = vec![0x5a_u8; PAYLOAD_LEN];
    let mut flights = Vec::with_capacity(count);
    for _ in 0..count {
        flights.push(first_flight(&payload).await);
    }
    let flights = Arc::new(flights);
    let next = Arc::new(AtomicUsize::new(0));
    let start = Instant::now();
    let tasks = (0..concurrency)
        .map(|_| {
            let (security, flights, next) = (security.clone(), flights.clone(), next.clone());
            tokio::spawn(async move {
                let mut latencies = Vec::new();
                let mut failed = 0;
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(flight) = flights.get(index) else {
                        break;
                    };
                    let began = Instant::now();
                    let result = security
                        .read_initial(&mut std::io::Cursor::new(flight.as_slice()))
                        .await;
                    latencies.push(began.elapsed().as_secs_f64() * 1e6);
                    match result {
                        Ok(initial) => {
                            black_box(initial.payload);
                        }
                        Err(_) => failed += 1,
                    }
                }
                (latencies, failed)
            })
        })
        .collect::<Vec<_>>();
    let mut latencies = Vec::with_capacity(count);
    let mut failed = 0;
    for task in tasks {
        let (mine, my_failed) = task.await.unwrap();
        latencies.extend(mine);
        failed += my_failed;
    }
    let per_second = count as f64 / start.elapsed().as_secs_f64();
    latencies.sort_by(f64::total_cmp);
    (per_second, latencies, failed)
}

fn percentile(sorted: &[f64], fraction: f64) -> f64 {
    let index = ((sorted.len() - 1) as f64 * fraction).round() as usize;
    sorted[index]
}

fn report(case: &str, (per_second, latencies, failed): (f64, Vec<f64>, usize)) {
    println!(
        "case={case} per_s={per_second:.0} p50_us={:.1} p99_us={:.1} max_us={:.1} failed={failed}",
        percentile(&latencies, 0.5),
        percentile(&latencies, 0.99),
        latencies.last().copied().unwrap_or_default(),
    );
}

/// A fresh state directory under `root`, removed when dropped.
struct StateDir(PathBuf);

impl StateDir {
    fn new(root: &Path, case: &str) -> Self {
        let dir = root.join(format!(
            "pb-mapper-handshake-bench-{}-{case}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for StateDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A replay log of `records` admissions made just now, as a relay leaves it.
fn live_log(path: &Path, records: usize) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let mut log = Vec::with_capacity(records * 40);
    for index in 0..records {
        // Distinct fingerprints, spread over the filter as SHA-256 outputs are.
        let digest = ring::digest::digest(&ring::digest::SHA256, &(index as u64).to_be_bytes());
        log.extend_from_slice(digest.as_ref());
        log.extend_from_slice(&now.to_be_bytes());
    }
    std::fs::write(path, log).unwrap();
}

#[tokio::main]
async fn main() {
    let mut compaction = false;
    let mut root = std::env::temp_dir();
    for arg in std::env::args().skip(1) {
        if arg == "--compaction" {
            compaction = true;
        } else {
            root = PathBuf::from(arg);
        }
    }
    for concurrency in CONCURRENCY {
        let case = format!("c{concurrency}");
        let dir = StateDir::new(&root, &case);
        let security = security(&dir.0).await;
        admit(&security, 1, WARMUP).await;
        report(&case, admit(&security, concurrency, HANDSHAKES).await);
    }
    if compaction {
        let case = format!("compact{COMPACTION_RECORDS}");
        let dir = StateDir::new(&root, &case);
        live_log(&dir.0.join("connection.replay"), COMPACTION_RECORDS);
        let security = security(&dir.0).await;
        admit(&security, 1, WARMUP).await;
        tokio::time::sleep(COMPACTION_WAIT).await;
        report(&case, admit(&security, 64, HANDSHAKES).await);
    }
}
