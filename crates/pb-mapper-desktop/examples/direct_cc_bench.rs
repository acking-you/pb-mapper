//! Cubic against BBR for the desktop host's direct video, open loop: two
//! quinn endpoints with pb-mapper's direct transport, the sender's under the
//! controller each cell names and the receiver's under Cubic, as a viewer's
//! is, through a testkit `lossy_udp` hop that delays, loses and polices
//! each way.
//!
//! The sender stands in for the host: a capture every 1/60 s, a P-frame of
//! the cell's video rate (8 333 bytes at 4 Mbit/s) and a 150 KB IDR every
//! 2 s, admitted under the host's credit window (`credit.rs`: 2 × the rate
//! × (base RTT + 100 ms), 3 to 64 frames, 64 KiB to 4 MiB, one IDR over the
//! byte window) and released by receipts the receiver echoes for every
//! frame it has read. A frame the window holds stalls the capture, whose
//! ticks are skipped until it leaves, as the host's are. There is no rate
//! control above the window: what the window and the controller do alone.
//!
//! Per cell: frame latency (capture to the last byte read, so time held for
//! credit counts) p50/p95/p99 for P-frames and for IDRs after the first, the
//! first IDR alone, the rate delivered while the cell ran, the ticks
//! skipped, and the sender's `lost_packets`, `congestion_events` and window
//! from quinn's path stats, the figures pb-mapper's `LinkStats` reads.
//!
//! ```text
//! cargo run --release -p pb-mapper-desktop --example direct_cc_bench -- \
//!     [--seconds 30] [--rtt 20,60,120] [--loss 0,0.1,1] [--policer 0,5] \
//!     [--rate 4,8] [--cc cubic,bbr] [--jobs 1]
//! ```
//!
//! `--loss` is in percent and lost each way, `--rtt` in milliseconds (half
//! each way), `--policer` and `--rate` in Mbit/s (a policer of 0 is none).
//! `--jobs` runs that many cells at once.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use pb_mapper_client::sdk::{CongestionControl, direct_transport_config};
use pb_mapper_testkit::{LossyShape, lossy_udp, lossy_udp_socket};
use quinn::rustls;
use tokio::sync::{Semaphore, mpsc};
use tokio::time::{Instant, MissedTickBehavior};

const FPS: u32 = 60;
const IDR_BYTES: usize = 150_000;
const IDR_EVERY: Duration = Duration::from_secs(2);
/// The host's credit window (`credit.rs`).
const WINDOW_MARGIN: Duration = Duration::from_millis(100);
const FRAME_WINDOW: (usize, usize) = (3, 64);
const BYTE_WINDOW: (usize, usize) = (64 * 1024, 4 * 1024 * 1024);
/// How long a cell waits after its last capture for the frames in flight.
const DRAIN: Duration = Duration::from_secs(10);
/// Seq, capture time in µs since the cell began, length, IDR.
const HEADER: usize = 8 + 8 + 4 + 1;
const SERVER_NAME: &str = "localhost";

#[derive(Clone, Copy, Debug)]
struct Cell {
    congestion: CongestionControl,
    rtt_ms: u64,
    loss_pct: f64,
    policer_mbps: f64,
    rate_mbps: f64,
}

#[derive(Default)]
struct Outcome {
    p_frames: Vec<f64>,
    idrs: Vec<f64>,
    first_idr: Option<f64>,
    /// Frames captured but never read by the end of the drain.
    undelivered: u64,
    delivered_mbps: f64,
    offered_mbps: f64,
    skipped: u64,
    lost_packets: u64,
    congestion_events: u64,
    sent_packets: u64,
    cwnd: u64,
    hop_lost: u64,
    hop_policed: u64,
}

/// One frame as the receiver read it.
struct Read {
    seq: u64,
    captured_us: u64,
    len: usize,
    idr: bool,
    at: Instant,
}

/// A frame captured and not yet written.
#[derive(Clone, Copy)]
struct Frame {
    seq: u64,
    captured: Instant,
    len: usize,
    idr: bool,
}

/// The host's credit ledger, as far as admission goes.
#[derive(Default)]
struct Ledger {
    in_flight: VecDeque<(u64, usize, bool)>,
    bytes: usize,
    idr_inflight: Option<u64>,
}

impl Ledger {
    fn window(rtt: Duration, rate_bps: f64) -> (usize, usize) {
        let t = (rtt + WINDOW_MARGIN).as_secs_f64();
        let frames = (f64::from(FPS) * t).ceil() as usize;
        let bytes = (2.0 * rate_bps * t / 8.0) as usize;
        (
            frames.clamp(FRAME_WINDOW.0, FRAME_WINDOW.1),
            bytes.clamp(BYTE_WINDOW.0, BYTE_WINDOW.1),
        )
    }

    /// `CreditLedger::fits`.
    fn fits(&self, frame: &Frame, (frames, bytes): (usize, usize)) -> bool {
        self.in_flight.is_empty()
            || (frame.idr
                && self.idr_inflight.is_none()
                && self.in_flight.len() < frames + 1
                && self.bytes <= bytes)
            || (self.in_flight.len() < frames && self.bytes + frame.len <= bytes)
    }

    fn book(&mut self, frame: &Frame) {
        self.in_flight.push_back((frame.seq, frame.len, frame.idr));
        self.bytes += frame.len;
        if frame.idr && self.idr_inflight.is_none() {
            self.idr_inflight = Some(frame.seq);
        }
    }

    fn receipt(&mut self, seq: u64) {
        while let Some(&(sent, len, _)) = self.in_flight.front() {
            if sent > seq {
                break;
            }
            self.in_flight.pop_front();
            self.bytes -= len;
            if self.idr_inflight.is_some_and(|idr| idr <= sent) {
                self.idr_inflight = None;
            }
        }
    }
}

fn tls() -> (rustls::ServerConfig, rustls::ClientConfig) {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let (rcgen::CertifiedKey { cert, .. }, key) =
        rcgen::generate_simple_self_signed(vec![SERVER_NAME.into()]).unwrap();
    let cert = rustls::pki_types::CertificateDer::from(cert.der().to_vec());
    let server = rustls::ServerConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![cert.clone()], key)
        .unwrap();
    let mut roots = rustls::RootCertStore::empty();
    roots.add(cert).unwrap();
    let client = rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    (server, client)
}

fn endpoint(server: Option<quinn::ServerConfig>) -> quinn::Endpoint {
    let socket = lossy_udp_socket(SocketAddr::from(([127, 0, 0, 1], 0))).unwrap();
    // As every direct endpoint: no QUIC-bit greasing.
    let mut config = quinn::EndpointConfig::default();
    config.grease_quic_bit(false);
    quinn::Endpoint::new(config, server, socket, Arc::new(quinn::TokioRuntime)).unwrap()
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    let rank = ((p / 100.0) * sorted.len() as f64).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

async fn run(cell: Cell, seconds: u64) -> Outcome {
    let (server_tls, client_tls) = tls();
    let mut server_config = quinn::ServerConfig::with_crypto(Arc::new(
        quinn::crypto::rustls::QuicServerConfig::try_from(server_tls).unwrap(),
    ));
    server_config.transport_config(Arc::new(direct_transport_config(cell.congestion).unwrap()));
    let mut client_config = quinn::ClientConfig::new(Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(client_tls).unwrap(),
    ));
    client_config.transport_config(Arc::new(
        direct_transport_config(CongestionControl::Cubic).unwrap(),
    ));
    let host = endpoint(Some(server_config));
    let viewer = endpoint(None);
    let shape = LossyShape::new(
        0.0,
        Duration::from_millis(cell.rtt_ms) / 2,
        (cell.policer_mbps * 1e6) as u64,
    );
    let hop = lossy_udp(host.local_addr().unwrap(), shape.clone()).await;
    let connecting = viewer
        .connect_with(client_config, hop.addr(), SERVER_NAME)
        .unwrap();
    let (to_host, from_viewer) =
        tokio::join!(connecting, async { host.accept().await.unwrap().await });
    let (to_host, from_viewer) = (to_host.unwrap(), from_viewer.unwrap());
    // The viewer opens the one stream, as a direct consumer does.
    let (mut viewer_send, mut viewer_recv) = to_host.open_bi().await.unwrap();
    viewer_send.write_all(&[0]).await.unwrap();
    let (mut host_send, mut host_recv) = from_viewer.accept_bi().await.unwrap();
    let mut hello = [0u8; 1];
    host_recv.read_exact(&mut hello).await.unwrap();
    shape.set_loss(cell.loss_pct / 100.0);

    let start = Instant::now();
    let end = start + Duration::from_secs(seconds);
    let rate_bps = cell.rate_mbps * 1e6;
    let p_bytes = (rate_bps / 8.0 / f64::from(FPS)).round() as usize;
    let window = Ledger::window(Duration::from_millis(cell.rtt_ms), rate_bps);

    // The viewer: read each frame, receipt it, note when it was read.
    let (reads_tx, mut reads) = mpsc::unbounded_channel::<Read>();
    let reader = tokio::spawn(async move {
        let mut header = [0u8; HEADER];
        let mut payload = vec![0u8; IDR_BYTES.max(1 << 20)];
        while viewer_recv.read_exact(&mut header).await.is_ok() {
            let seq = u64::from_be_bytes(header[0..8].try_into().unwrap());
            let captured_us = u64::from_be_bytes(header[8..16].try_into().unwrap());
            let len = u32::from_be_bytes(header[16..20].try_into().unwrap()) as usize;
            let idr = header[20] == 1;
            if payload.len() < len {
                payload.resize(len, 0);
            }
            if viewer_recv.read_exact(&mut payload[..len]).await.is_err() {
                break;
            }
            let at = Instant::now();
            if viewer_send.write_all(&seq.to_be_bytes()).await.is_err() {
                break;
            }
            let _ = reads_tx.send(Read {
                seq,
                captured_us,
                len,
                idr,
                at,
            });
        }
    });
    // The host's receipts, read apart so a write never waits on them.
    let (receipts_tx, mut receipts) = mpsc::unbounded_channel::<u64>();
    let receipt_reader = tokio::spawn(async move {
        let mut seq = [0u8; 8];
        while host_recv.read_exact(&mut seq).await.is_ok() {
            if receipts_tx.send(u64::from_be_bytes(seq)).is_err() {
                break;
            }
        }
    });

    // The host: capture, admit under credit, write.
    let mut ledger = Ledger::default();
    let mut ticker = tokio::time::interval_at(start, Duration::from_secs(1) / FPS);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut pending: Option<Frame> = None;
    let mut next_idr = start;
    let mut seq = 0u64;
    let mut skipped = 0u64;
    let mut offered = 0usize;
    let mut captured = 0u64;
    let mut buffer = vec![0u8; HEADER + IDR_BYTES.max(p_bytes)];
    let write = |frame: Frame, buffer: &mut Vec<u8>| {
        buffer.clear();
        buffer.extend_from_slice(&frame.seq.to_be_bytes());
        let captured_us = frame.captured.saturating_duration_since(start).as_micros() as u64;
        buffer.extend_from_slice(&captured_us.to_be_bytes());
        buffer.extend_from_slice(&(frame.len as u32).to_be_bytes());
        buffer.push(u8::from(frame.idr));
        buffer.resize(HEADER + frame.len, 0);
    };
    loop {
        tokio::select! {
            biased;
            Some(receipt) = receipts.recv() => {
                ledger.receipt(receipt);
                if let Some(frame) = pending.filter(|frame| ledger.fits(frame, window)) {
                    pending = None;
                    ledger.book(&frame);
                    write(frame, &mut buffer);
                    host_send.write_all(&buffer).await.unwrap();
                }
            }
            now = ticker.tick() => {
                if now >= end {
                    break;
                }
                if pending.is_some() {
                    skipped += 1;
                    continue;
                }
                let idr = now >= next_idr;
                if idr {
                    next_idr += IDR_EVERY;
                }
                seq += 1;
                captured += 1;
                let frame = Frame {
                    seq,
                    captured: now,
                    len: if idr { IDR_BYTES } else { p_bytes },
                    idr,
                };
                offered += frame.len;
                if ledger.fits(&frame, window) {
                    ledger.book(&frame);
                    write(frame, &mut buffer);
                    host_send.write_all(&buffer).await.unwrap();
                } else {
                    pending = Some(frame);
                }
            }
        }
    }
    // A frame still held when the captures stop is let go with them; the
    // rest drain, receipts or not.
    let written = seq - u64::from(pending.is_some());
    let mut outcome = Outcome {
        skipped,
        ..Outcome::default()
    };
    let mut delivered = 0usize;
    let mut read = 0u64;
    let deadline = Instant::now() + DRAIN;
    while read < written {
        let Ok(Some(frame)) = tokio::time::timeout_at(deadline, reads.recv()).await else {
            break;
        };
        // Receipts keep the ledger moving while the window drains.
        while let Ok(receipt) = receipts.try_recv() {
            ledger.receipt(receipt);
        }
        read += 1;
        if frame.at <= end {
            delivered += frame.len;
        }
        let latency = frame
            .at
            .saturating_duration_since(start + Duration::from_micros(frame.captured_us))
            .as_secs_f64()
            * 1000.0;
        match (frame.idr, frame.seq) {
            (true, 1) => outcome.first_idr = Some(latency),
            (true, _) => outcome.idrs.push(latency),
            (false, _) => outcome.p_frames.push(latency),
        }
    }
    outcome.undelivered = captured - read;
    outcome.delivered_mbps = delivered as f64 * 8.0 / seconds as f64 / 1e6;
    outcome.offered_mbps = offered as f64 * 8.0 / seconds as f64 / 1e6;
    let path = from_viewer.stats().path;
    outcome.lost_packets = path.lost_packets;
    outcome.congestion_events = path.congestion_events;
    outcome.sent_packets = path.sent_packets;
    outcome.cwnd = path.cwnd;
    let [up, down] = hop.counts();
    outcome.hop_lost = up.lost + down.lost;
    outcome.hop_policed = up.policed + down.policed;
    reader.abort();
    receipt_reader.abort();
    from_viewer.close(0u32.into(), b"done");
    to_host.close(0u32.into(), b"done");
    host.close(0u32.into(), b"done");
    viewer.close(0u32.into(), b"done");
    outcome
}

fn list<T: std::str::FromStr>(value: &str) -> Vec<T>
where
    T::Err: std::fmt::Debug,
{
    value.split(',').map(|item| item.parse().unwrap()).collect()
}

fn line(cell: &Cell, outcome: &mut Outcome) -> String {
    outcome.p_frames.sort_by(f64::total_cmp);
    outcome.idrs.sort_by(f64::total_cmp);
    let (p, i) = (&outcome.p_frames, &outcome.idrs);
    format!(
        "cc={} rtt_ms={} loss_pct={} policer_mbps={} rate_mbps={} p_frames={} p_p50={:.1} p_p95={:.1} p_p99={:.1} idrs={} idr_p50={:.1} idr_p95={:.1} idr_p99={:.1} first_idr_ms={:.1} offered_mbps={:.2} delivered_mbps={:.2} skipped={} undelivered={} lost_packets={} congestion_events={} sent_packets={} cwnd={} hop_lost={} hop_policed={}",
        cell.congestion.as_str(),
        cell.rtt_ms,
        cell.loss_pct,
        cell.policer_mbps,
        cell.rate_mbps,
        p.len(),
        percentile(p, 50.0),
        percentile(p, 95.0),
        percentile(p, 99.0),
        i.len(),
        percentile(i, 50.0),
        percentile(i, 95.0),
        percentile(i, 99.0),
        outcome.first_idr.unwrap_or(f64::NAN),
        outcome.offered_mbps,
        outcome.delivered_mbps,
        outcome.skipped,
        outcome.undelivered,
        outcome.lost_packets,
        outcome.congestion_events,
        outcome.sent_packets,
        outcome.cwnd,
        outcome.hop_lost,
        outcome.hop_policed,
    )
}

#[tokio::main]
async fn main() {
    let mut seconds = 30u64;
    let mut rtts = vec![20u64, 60, 120];
    let mut losses = vec![0.0f64, 0.1, 1.0];
    let mut policers = vec![0.0f64, 5.0];
    let mut rates = vec![4.0f64, 8.0];
    let mut controllers = vec![CongestionControl::Cubic, CongestionControl::Bbr];
    let mut jobs = 1usize;
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let value = args.next().expect("every flag takes a value");
        match flag.as_str() {
            "--seconds" => seconds = value.parse().unwrap(),
            "--rtt" => rtts = list(&value),
            "--loss" => losses = list(&value),
            "--policer" => policers = list(&value),
            "--rate" => rates = list(&value),
            "--cc" => {
                controllers = value
                    .split(',')
                    .map(|name| match name {
                        "cubic" => CongestionControl::Cubic,
                        "bbr" => CongestionControl::Bbr,
                        other => panic!("unknown controller {other}"),
                    })
                    .collect();
            }
            "--jobs" => jobs = value.parse().unwrap(),
            other => panic!("unknown flag {other}"),
        }
    }
    let mut cells = Vec::new();
    for &rtt_ms in &rtts {
        for &loss_pct in &losses {
            for &policer_mbps in &policers {
                for &rate_mbps in &rates {
                    for &congestion in &controllers {
                        cells.push(Cell {
                            congestion,
                            rtt_ms,
                            loss_pct,
                            policer_mbps,
                            rate_mbps,
                        });
                    }
                }
            }
        }
    }
    println!(
        "direct_cc_bench: {} cells of {seconds} s, {jobs} at once",
        cells.len()
    );
    let slots = Arc::new(Semaphore::new(jobs));
    let mut running = Vec::new();
    for cell in cells {
        let slot = slots.clone().acquire_owned().await.unwrap();
        running.push(tokio::spawn(async move {
            let mut outcome = run(cell, seconds).await;
            drop(slot);
            let line = line(&cell, &mut outcome);
            println!("{line}");
            line
        }));
    }
    for task in running {
        task.await.unwrap();
    }
}
