//! A UDP hop that loses, delays and polices datagrams, adjustable while
//! traffic runs: a lossy long path for QUIC, in one process.
//!
//! Each direction is one ordered queue, as airhand's `lossy_quic_pair` has
//! it: datagrams leave in the order they came, each `delay` after it came,
//! so a late timer releases a batch late rather than reordering it. Every
//! datagram is copied; this is test support and never sits on a data path.

use std::net::{SocketAddr, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio::time::Instant;

/// The socket buffers every socket of a lossy path asks for: one IDR's
/// burst of datagrams must not overflow them, or a path meant to be
/// lossless loses too. Platforms may grant less.
pub const LOSSY_SOCKET_BUFFER: usize = 4 << 20;

/// How much a policer lets through at once: this long at its rate.
pub const POLICER_BURST: Duration = Duration::from_millis(50);

/// The fewest bytes a policer's bucket holds, whatever its rate: two
/// full-size datagrams.
const POLICER_BURST_MIN: f64 = 3000.0;

/// Datagrams one direction holds; past it, one is dropped, as a full
/// router queue drops.
const QUEUE: usize = 1 << 16;

/// How a [`lossy_udp`] hop treats each direction alike, changeable while
/// it runs.
#[derive(Default)]
pub struct LossyShape {
    /// Datagrams dropped at random, in parts per million.
    pub loss_ppm: AtomicU32,
    /// How long each datagram is held, one way, in microseconds.
    pub delay_us: AtomicU64,
    /// The policer's rate in bits per second; 0 is none. Its token bucket
    /// holds [`POLICER_BURST`] of the rate, and a datagram the bucket cannot
    /// pay for is dropped, not queued.
    pub rate_bps: AtomicU64,
}

impl LossyShape {
    /// `loss` as a fraction (0.01 is 1%), `delay` one way, and the
    /// policer's `rate_bps` (0 for none).
    pub fn new(loss: f64, delay: Duration, rate_bps: u64) -> Arc<Self> {
        let shape = Self::default();
        shape.set_loss(loss);
        shape
            .delay_us
            .store(delay.as_micros() as u64, Ordering::Relaxed);
        shape.rate_bps.store(rate_bps, Ordering::Relaxed);
        Arc::new(shape)
    }

    /// Lose `loss` of the datagrams from now on, as a fraction.
    pub fn set_loss(&self, loss: f64) {
        let ppm = (loss.clamp(0.0, 1.0) * 1e6).round() as u32;
        self.loss_ppm.store(ppm, Ordering::Relaxed);
    }
}

/// What one direction of a [`lossy_udp`] hop did with its datagrams.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LossyCounts {
    pub forwarded: u64,
    /// Dropped at random.
    pub lost: u64,
    /// Dropped by the policer.
    pub policed: u64,
    /// Dropped with the queue full.
    pub overflowed: u64,
}

#[derive(Default)]
struct Counters {
    forwarded: AtomicU64,
    lost: AtomicU64,
    policed: AtomicU64,
    overflowed: AtomicU64,
}

impl Counters {
    fn read(&self) -> LossyCounts {
        LossyCounts {
            forwarded: self.forwarded.load(Ordering::Relaxed),
            lost: self.lost.load(Ordering::Relaxed),
            policed: self.policed.load(Ordering::Relaxed),
            overflowed: self.overflowed.load(Ordering::Relaxed),
        }
    }
}

/// A running [`lossy_udp`] hop. Dropping it stops the hop.
pub struct LossyUdp {
    addr: SocketAddr,
    /// Toward the target, and back toward whoever sent to the hop.
    counters: [Arc<Counters>; 2],
    _tasks: JoinSet<()>,
}

impl LossyUdp {
    /// Where to send in place of the target.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// What the hop did toward the target, and back.
    pub fn counts(&self) -> [LossyCounts; 2] {
        [self.counters[0].read(), self.counters[1].read()]
    }
}

/// A UDP socket bound to `addr`, nonblocking, asking for
/// [`LOSSY_SOCKET_BUFFER`] each way: what each end of a lossy path binds.
pub fn lossy_udp_socket(addr: SocketAddr) -> std::io::Result<UdpSocket> {
    let socket = socket2::Socket::new(
        socket2::Domain::for_address(addr),
        socket2::Type::DGRAM,
        Some(socket2::Protocol::UDP),
    )?;
    // Best effort: a platform that grants less still forwards.
    let _ = socket.set_recv_buffer_size(LOSSY_SOCKET_BUFFER);
    let _ = socket.set_send_buffer_size(LOSSY_SOCKET_BUFFER);
    socket.set_nonblocking(true)?;
    socket.bind(&addr.into())?;
    Ok(socket.into())
}

/// A UDP hop on loopback in front of `target`, shaped by `shape` each way
/// while it runs. Datagrams from `target` go to whoever last sent to the
/// hop. The random losses come from a fixed seed, so a run loses the same
/// datagrams as the last at the same loss.
pub async fn lossy_udp(target: SocketAddr, shape: Arc<LossyShape>) -> LossyUdp {
    let socket = Arc::new(
        tokio::net::UdpSocket::from_std(
            lossy_udp_socket(SocketAddr::from(([127, 0, 0, 1], 0))).unwrap(),
        )
        .unwrap(),
    );
    let addr = socket.local_addr().unwrap();
    let counters: [Arc<Counters>; 2] = [Arc::default(), Arc::default()];
    let mut tasks = JoinSet::new();
    let (to_target, up) = mpsc::channel(QUEUE);
    let (to_client, down) = mpsc::channel(QUEUE);
    tasks.spawn(release(socket.clone(), up));
    tasks.spawn(release(socket.clone(), down));
    let directions = [
        Direction::new(to_target, counters[0].clone(), 0x2545_f491_4f6c_dd1d),
        Direction::new(to_client, counters[1].clone(), 0x9e37_79b9_7f4a_7c15),
    ];
    tasks.spawn(receive(socket, target, shape, directions));
    LossyUdp {
        addr,
        counters,
        _tasks: tasks,
    }
}

/// A datagram, when it may leave and where to.
type Held = (Instant, Vec<u8>, SocketAddr);

/// One direction's loss, policer and queue.
struct Direction {
    queue: mpsc::Sender<Held>,
    counters: Arc<Counters>,
    seed: u64,
    /// The policer's bytes in hand, and when they were last topped up.
    tokens: f64,
    filled: Instant,
}

impl Direction {
    fn new(queue: mpsc::Sender<Held>, counters: Arc<Counters>, seed: u64) -> Self {
        Self {
            queue,
            counters,
            seed,
            tokens: f64::MAX,
            filled: Instant::now(),
        }
    }

    /// The next draw of the fixed-seed xorshift, in [0, 1e6).
    fn draw_ppm(&mut self) -> u32 {
        self.seed ^= self.seed << 13;
        self.seed ^= self.seed >> 7;
        self.seed ^= self.seed << 17;
        ((self.seed >> 11) % 1_000_000) as u32
    }

    /// Whether the policer at `rate_bps` lets `len` bytes through at `now`.
    fn police(&mut self, rate_bps: u64, len: usize, now: Instant) -> bool {
        if rate_bps == 0 {
            return true;
        }
        let rate = rate_bps as f64 / 8.0;
        let depth = (rate * POLICER_BURST.as_secs_f64()).max(POLICER_BURST_MIN);
        let elapsed = now.saturating_duration_since(self.filled).as_secs_f64();
        self.tokens = (self.tokens.min(depth) + elapsed * rate).min(depth);
        self.filled = now;
        if self.tokens < len as f64 {
            return false;
        }
        self.tokens -= len as f64;
        true
    }

    fn admit(&mut self, shape: &LossyShape, datagram: &[u8], to: SocketAddr, now: Instant) {
        let loss = shape.loss_ppm.load(Ordering::Relaxed);
        // Draw for every datagram, lost or not, so a seed's sequence of
        // losses does not depend on the policer.
        if self.draw_ppm() < loss {
            self.counters.lost.fetch_add(1, Ordering::Relaxed);
            return;
        }
        if !self.police(shape.rate_bps.load(Ordering::Relaxed), datagram.len(), now) {
            self.counters.policed.fetch_add(1, Ordering::Relaxed);
            return;
        }
        let delay = Duration::from_micros(shape.delay_us.load(Ordering::Relaxed));
        match self.queue.try_send((now + delay, datagram.to_vec(), to)) {
            Ok(()) => {
                self.counters.forwarded.fetch_add(1, Ordering::Relaxed);
            }
            Err(_) => {
                self.counters.overflowed.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

async fn receive(
    socket: Arc<tokio::net::UdpSocket>,
    target: SocketAddr,
    shape: Arc<LossyShape>,
    directions: [Direction; 2],
) {
    let [mut up, mut down] = directions;
    let mut client = None;
    let mut buffer = vec![0u8; 65536];
    while let Ok((len, from)) = socket.recv_from(&mut buffer).await {
        let now = Instant::now();
        let datagram = &buffer[..len];
        if from == target {
            if let Some(to) = client {
                down.admit(&shape, datagram, to, now);
            }
        } else {
            client = Some(from);
            up.admit(&shape, datagram, target, now);
        }
    }
}

/// Send each held datagram when it may leave, in the order they came.
async fn release(socket: Arc<tokio::net::UdpSocket>, mut queue: mpsc::Receiver<Held>) {
    while let Some((at, datagram, to)) = queue.recv().await {
        tokio::time::sleep_until(at).await;
        let _ = socket.send_to(&datagram, to).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::poll::poll_until;

    /// A target that echoes every datagram, and a client socket.
    async fn echo_and_client() -> (SocketAddr, tokio::net::UdpSocket, JoinSet<()>) {
        let bind = || {
            tokio::net::UdpSocket::from_std(
                lossy_udp_socket(SocketAddr::from(([127, 0, 0, 1], 0))).unwrap(),
            )
            .unwrap()
        };
        let target = bind();
        let addr = target.local_addr().unwrap();
        let mut tasks = JoinSet::new();
        tasks.spawn(async move {
            let mut buffer = vec![0u8; 2048];
            while let Ok((len, from)) = target.recv_from(&mut buffer).await {
                let _ = target.send_to(&buffer[..len], from).await;
            }
        });
        (addr, bind(), tasks)
    }

    /// Wait until `hop` has counted `total` datagrams toward its target.
    async fn counted(hop: &LossyUdp, total: u64) {
        poll_until(Duration::from_secs(5), Duration::from_millis(5), || async {
            let [up, _] = hop.counts();
            let seen = up.forwarded + up.lost + up.policed + up.overflowed;
            if seen == total {
                Ok(())
            } else {
                Err(format!("{seen} of {total} counted: {up:?}"))
            }
        })
        .await
        .unwrap();
    }

    /// Without loss, every datagram goes there and back in order, each held
    /// its delay both ways.
    #[tokio::test]
    async fn delays_each_way_in_order() {
        let (target, client, _echo) = echo_and_client().await;
        let delay = Duration::from_millis(15);
        let hop = lossy_udp(target, LossyShape::new(0.0, delay, 0)).await;
        let started = Instant::now();
        for sequence in 0u32..50 {
            client
                .send_to(&sequence.to_be_bytes(), hop.addr())
                .await
                .unwrap();
        }
        let mut buffer = [0u8; 4];
        for sequence in 0u32..50 {
            let (len, from) =
                tokio::time::timeout(Duration::from_secs(5), client.recv_from(&mut buffer))
                    .await
                    .unwrap()
                    .unwrap();
            assert_eq!((len, from), (4, hop.addr()));
            assert_eq!(u32::from_be_bytes(buffer), sequence, "in the order sent");
        }
        assert!(started.elapsed() >= 2 * delay, "held both ways");
        let forwarded = LossyCounts {
            forwarded: 50,
            ..LossyCounts::default()
        };
        assert_eq!(hop.counts(), [forwarded, forwarded]);
    }

    /// Random loss takes about its share, from a fixed seed: the same
    /// datagrams every run.
    #[tokio::test]
    async fn loses_its_share_from_a_fixed_seed() {
        let mut runs = Vec::new();
        for _ in 0..2 {
            let (target, client, _echo) = echo_and_client().await;
            let hop = lossy_udp(target, LossyShape::new(0.25, Duration::ZERO, 0)).await;
            for sequence in 0u32..400 {
                client
                    .send_to(&sequence.to_be_bytes(), hop.addr())
                    .await
                    .unwrap();
            }
            counted(&hop, 400).await;
            runs.push(hop.counts()[0]);
        }
        let lost = runs[0].lost;
        assert!((60..140).contains(&lost), "about a quarter of 400: {lost}");
        assert_eq!(runs[0], runs[1], "the same losses from the same seed");
    }

    /// A policer passes its bucket at once and drops the rest of a burst;
    /// the bucket refills at its rate, never past its depth.
    #[test]
    fn a_policer_passes_its_bucket_and_refills_at_its_rate() {
        let (queue, _held) = mpsc::channel(1);
        let start = Instant::now();
        let mut direction = Direction::new(queue, Arc::default(), 1);
        // 50 ms of 1.6 Mbit/s is 10 000 bytes: ten datagrams of 1000.
        let rate = 1_600_000;
        let passed = |direction: &mut Direction, at: Instant| {
            (0..30)
                .take_while(|_| direction.police(rate, 1000, at))
                .count()
        };
        assert_eq!(passed(&mut direction, start), 10);
        // 25 ms refills 5000 bytes.
        assert_eq!(passed(&mut direction, start + Duration::from_millis(25)), 5);
        // A long pause refills the bucket, not more.
        assert_eq!(passed(&mut direction, start + Duration::from_secs(10)), 10);
        // No policer passes everything.
        assert!((0..1000).all(|_| direction.police(0, 1500, start)));
    }

    /// Through the hop, a burst past the policer's bucket is cut to it.
    #[tokio::test]
    async fn polices_a_burst_through_the_hop() {
        let (target, client, _echo) = echo_and_client().await;
        let hop = lossy_udp(target, LossyShape::new(0.0, Duration::ZERO, 1_600_000)).await;
        let datagram = [0u8; 1000];
        for _ in 0..30 {
            client.send_to(&datagram, hop.addr()).await.unwrap();
        }
        counted(&hop, 30).await;
        let [up, _] = hop.counts();
        // The bucket's ten, and what it refilled while the burst came.
        assert!((10..=12).contains(&up.forwarded), "{up:?}");
        assert_eq!(up.forwarded + up.policed, 30);
    }
}
