//! Release-mode memory that idle tunnels hold after a burst of traffic.
//!
//! A relay, and the publishers and consumers of `TUNNELS` tunnels, all in one
//! process on the allocator the `pb-mapper` binary uses. Per case (carrier
//! and codec), in a process of its own: each tunnel echoes `BURST` bytes
//! through one connection, written and read `CHUNK` at a time, all tunnels at
//! once, and the connections then stay open and idle. Every forwarding hop is
//! in this process: each tunnel's consumer end, its two relay legs and its
//! publisher end.
//!
//! Prints per case the process's memory with the tunnels set up and idle
//! (`idle_*`), and a second after the burst (`after_*`): resident (`rss`) and,
//! on Windows, committed private memory (`private`; on Linux the resident
//! anonymous memory), in MiB; and the burst's echo throughput over all
//! tunnels (`burst_mib_s`).
//!
//! Run with `cargo run --release -p pb-mapper-cli --example buffer_retention_bench`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::{Duration, Instant};

use better_mimalloc_rs::MiMalloc;
use pb_mapper_client::sdk::{
    Client, ConnectOptions, ConnectRequest, PathPolicy, RegisterOptions, RegisterRequest,
    RelayTransport, Transport,
};
use pb_mapper_testkit::{READY_TIMEOUT, Relay, admin_credential};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::task::AbortOnDropHandle;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

const TUNNELS: usize = 8;
const BURST: usize = 32 << 20;
/// What the bench writes and reads at a time.
const CHUNK: usize = 64 << 10;
/// How long after the burst memory is read, so that what the allocator
/// returns on its own has been returned.
const SETTLE: Duration = Duration::from_secs(1);

const CASES: [(RelayTransport, bool); 4] = [
    (RelayTransport::Tcp, false),
    (RelayTransport::Tcp, true),
    (RelayTransport::Quic, false),
    (RelayTransport::Quic, true),
];

/// Resident and private memory of this process, in bytes.
#[cfg(windows)]
fn memory() -> (u64, u64) {
    use std::ffi::c_void;

    #[repr(C)]
    #[derive(Default)]
    struct Counters {
        cb: u32,
        page_fault_count: u32,
        peak_working_set_size: usize,
        working_set_size: usize,
        quota_peak_paged_pool_usage: usize,
        quota_paged_pool_usage: usize,
        quota_peak_non_paged_pool_usage: usize,
        quota_non_paged_pool_usage: usize,
        pagefile_usage: usize,
        peak_pagefile_usage: usize,
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentProcess() -> *mut c_void;
        fn K32GetProcessMemoryInfo(process: *mut c_void, counters: *mut Counters, cb: u32) -> i32;
    }
    let mut counters = Counters {
        cb: size_of::<Counters>() as u32,
        ..Default::default()
    };
    // SAFETY: the pseudo-handle of the current process needs no closing, and
    // the call writes at most `cb` bytes into `counters`, which has them.
    let succeeded =
        unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, counters.cb) };
    assert_ne!(succeeded, 0, "K32GetProcessMemoryInfo failed");
    (
        counters.working_set_size as u64,
        counters.pagefile_usage as u64,
    )
}

/// Resident and resident anonymous memory of this process, in bytes.
#[cfg(not(windows))]
fn memory() -> (u64, u64) {
    let status = std::fs::read_to_string("/proc/self/status").unwrap();
    let field = |name: &str| {
        status
            .lines()
            .find_map(|line| line.strip_prefix(name))
            .map(|value| value.trim().trim_end_matches(" kB").parse::<u64>().unwrap() * 1024)
            .unwrap()
    };
    (field("VmRSS:"), field("RssAnon:"))
}

/// The burst, from one small buffer, so that what the process holds after it
/// is the tunnels' and not the bench's.
async fn send_burst(write: &mut (impl AsyncWriteExt + Unpin)) -> std::io::Result<()> {
    let chunk = [0x5a; CHUNK];
    for _ in 0..BURST / CHUNK {
        write.write_all(&chunk).await?;
    }
    Ok(())
}

/// The echoed burst, into one small buffer, checked as it comes.
async fn receive_burst(read: &mut (impl AsyncReadExt + Unpin)) -> std::io::Result<()> {
    let mut chunk = [0; CHUNK];
    let mut left = BURST;
    while left > 0 {
        let n = read.read(&mut chunk[..CHUNK.min(left)]).await?;
        assert!(n > 0, "the echo ended early");
        assert!(chunk[..n].iter().all(|byte| *byte == 0x5a));
        left -= n;
    }
    Ok(())
}

fn mib(bytes: u64) -> f64 {
    bytes as f64 / f64::from(1 << 20)
}

async fn measure(carrier: RelayTransport, codec: bool) {
    let relay = Relay::start_with_quic("buffer-retention-bench").await;
    let client = || {
        Client::from_credential(relay.addr().to_string(), admin_credential(), false, None)
            .with_relay_transport(carrier)
            .unwrap()
    };
    let (publisher, consumer) = (client(), client());
    let mut kept = Vec::new();
    let mut sockets = Vec::new();
    for tunnel in 0..TUNNELS {
        let key = format!("burst-{tunnel}");
        let backend = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let backend_addr = backend.local_addr().unwrap();
        let echo = AbortOnDropHandle::new(tokio::spawn(async move {
            let (mut socket, _) = backend.accept().await.unwrap();
            socket.set_nodelay(true).unwrap();
            let (mut read, mut write) = socket.split();
            let _ = tokio::io::copy(&mut read, &mut write).await;
        }));
        let registration = publisher
            .register_with_options(
                RegisterRequest {
                    key: key.clone(),
                    local_addr: backend_addr.to_string(),
                    transport: Transport::Tcp,
                    codec,
                    force_namespace: false,
                },
                RegisterOptions {
                    allow_direct: false,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        registration
            .wait_ready_timeout(READY_TIMEOUT)
            .await
            .unwrap();
        let local = pb_mapper_testkit::reserve_addr(pb_mapper_testkit::Transport::Tcp).await;
        let connection = consumer
            .connect_with_options(
                ConnectRequest {
                    key,
                    local_addr: local.to_string(),
                    transport: Transport::Tcp,
                },
                ConnectOptions {
                    path: PathPolicy::RelayOnly,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        connection.wait_ready_timeout(READY_TIMEOUT).await.unwrap();
        let mut socket = TcpStream::connect(local).await.unwrap();
        socket.set_nodelay(true).unwrap();
        socket.write_all(b"warm").await.unwrap();
        let mut warm = [0; 4];
        socket.read_exact(&mut warm).await.unwrap();
        assert_eq!(&warm, b"warm");
        sockets.push(socket);
        kept.push((echo, registration, connection));
    }
    tokio::time::sleep(SETTLE).await;
    let (idle_rss, idle_private) = memory();

    let start = Instant::now();
    let echoes = sockets
        .into_iter()
        .map(|mut socket| {
            tokio::spawn(async move {
                let (mut read, mut write) = socket.split();
                tokio::try_join!(send_burst(&mut write), receive_burst(&mut read)).unwrap();
                socket
            })
        })
        .collect::<Vec<_>>();
    let mut sockets = Vec::new();
    for echo in echoes {
        sockets.push(
            tokio::time::timeout(Duration::from_secs(120), echo)
                .await
                .unwrap()
                .unwrap(),
        );
    }
    let burst = start.elapsed();
    tokio::time::sleep(SETTLE).await;
    let (after_rss, after_private) = memory();
    println!(
        "case={carrier:?}-codec={codec} idle_rss={:.1} idle_private={:.1} after_rss={:.1} after_private={:.1} burst_mib_s={:.1}",
        mib(idle_rss),
        mib(idle_private),
        mib(after_rss),
        mib(after_private),
        (TUNNELS * BURST) as f64 / f64::from(1 << 20) / burst.as_secs_f64(),
    );
    drop(sockets);
    for (_echo, registration, connection) in kept {
        connection.stop().await.unwrap();
        registration.stop().await.unwrap();
    }
}

fn main() {
    MiMalloc::init();
    let mut args = std::env::args().skip(1);
    if let (Some(flag), Some(case)) = (args.next(), args.next())
        && flag == "--case"
    {
        let (carrier, codec) = CASES[case.parse::<usize>().unwrap()];
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(measure(carrier, codec));
        return;
    }
    // Each case in a fresh process, so that none starts from what another
    // left the allocator holding.
    let cases = if cfg!(feature = "relay-quic") {
        CASES.len()
    } else {
        2
    };
    for case in 0..cases {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--case", &case.to_string()])
            .output()
            .unwrap();
        assert!(output.status.success(), "case {case} failed");
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            if line.starts_with("case=") {
                println!("{line}");
            }
        }
    }
}
