//! What an idle store writes: how often the five-minute compaction replaces
//! the snapshot, and how many bytes that is.
//!
//! Starts a store at the default capacity holding `KEYS` keys whose lifetimes
//! outlast the run, then asks nothing more of it. Once a second it checks
//! whether `auth.snapshot` has been replaced, and every five minutes, and at
//! the end, it prints the counts so far. On Linux it also prints what the
//! process passed to `write` (`/proc/self/io`); `strace -f -c -e
//! trace=fsync,fdatasync` around it counts the syncs.
//!
//! Run for an hour with
//! `cargo run --release -p pb-mapper-auth --example snapshot_bench -- 3600`.

use std::error::Error;
use std::time::{Duration, Instant, SystemTime};

use pb_mapper_auth::{
    ADMIN_KEY_ID, AuthConfig, AuthRuntime, DEFAULT_MAX_TEMP_KEY_TTL, DEFAULT_TEMP_KEY_CAPACITY,
    LegacyProtocolPolicy,
};
use pb_mapper_core::test_support::{TEST_ADMIN_KEY_BYTES, TempPath};

type BenchResult<T> = Result<T, Box<dyn Error + Send + Sync>>;

/// The keys the store holds.
const KEYS: usize = 100;
/// How often the counts are printed.
const REPORT_EVERY: Duration = Duration::from_secs(5 * 60);

/// `wchar` and `syscw` from `/proc/self/io`: bytes passed to write calls,
/// and how many there were.
#[cfg(target_os = "linux")]
fn written() -> Option<(u64, u64)> {
    let io = std::fs::read_to_string("/proc/self/io").ok()?;
    let field = |name: &str| {
        io.lines()
            .find_map(|line| line.strip_prefix(name)?.trim().parse::<u64>().ok())
    };
    Some((field("wchar:")?, field("syscw:")?))
}

#[cfg(not(target_os = "linux"))]
fn written() -> Option<(u64, u64)> {
    None
}

struct Counts {
    rewrites: u64,
    snapshot_bytes: u64,
    written_at_start: Option<(u64, u64)>,
}

impl Counts {
    fn report(&self, elapsed: Duration) {
        let mut line = format!(
            "case=idle elapsed_s={} rewrites={} snapshot_bytes={}",
            elapsed.as_secs(),
            self.rewrites,
            self.snapshot_bytes
        );
        if let (Some((wchar, syscw)), Some((wchar0, syscw0))) = (written(), self.written_at_start) {
            line.push_str(&format!(
                " write_bytes={} write_calls={}",
                wchar - wchar0,
                syscw - syscw0
            ));
        }
        println!("{line}");
    }
}

#[tokio::main]
async fn main() -> BenchResult<()> {
    let seconds = std::env::args()
        .nth(1)
        .map(|seconds| seconds.parse::<u64>())
        .transpose()?
        .unwrap_or(3600);
    let state_dir = TempPath::new("snapshot-bench");
    let config = AuthConfig {
        state_dir: state_dir.to_path_buf(),
        max_temporary_keys: DEFAULT_TEMP_KEY_CAPACITY,
        max_temporary_key_ttl: DEFAULT_MAX_TEMP_KEY_TTL,
        legacy_protocol: LegacyProtocolPolicy::Deny,
    };
    let runtime = AuthRuntime::start(TEST_ADMIN_KEY_BYTES, config).await?;
    let admin = runtime.authenticate_presented(ADMIN_KEY_ID, &TEST_ADMIN_KEY_BYTES)?;
    for _ in 0..KEYS {
        runtime
            .issue(&admin, DEFAULT_MAX_TEMP_KEY_TTL, None)
            .await?;
    }

    let snapshot = state_dir.join("auth.snapshot");
    let version = || -> Option<(SystemTime, u64)> {
        let metadata = std::fs::metadata(&snapshot).ok()?;
        Some((metadata.modified().ok()?, metadata.len()))
    };
    let mut counts = Counts {
        rewrites: 0,
        snapshot_bytes: 0,
        written_at_start: written(),
    };
    let mut seen = version();
    let start = Instant::now();
    // For telling the idle hours apart in a trace of the run.
    let since_epoch = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH)?;
    println!("# idle from unix_ms={}", since_epoch.as_millis());
    let end = start + Duration::from_secs(seconds);
    let mut next_report = start + REPORT_EVERY;
    let mut second = tokio::time::interval(Duration::from_secs(1));
    while Instant::now() < end {
        second.tick().await;
        let now = version();
        if now != seen {
            counts.rewrites += 1;
            counts.snapshot_bytes += now.map_or(0, |(_, len)| len);
            seen = now;
        }
        if Instant::now() >= next_report {
            counts.report(start.elapsed());
            next_report += REPORT_EVERY;
        }
    }
    counts.report(start.elapsed());

    drop(admin);
    runtime.shutdown_actor().await;
    Ok(())
}
