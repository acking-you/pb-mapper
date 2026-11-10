//! The figures behind `ViewerStats`: the sink records presentations, the
//! reader records video and the host's `Stats`, the writer's tick rolls the
//! rates up once a second, and status polls read them.
use super::{IntervalStats, ViewerStats};
use crate::protocol::{Adaptation, RateModel};
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

fn millis_u32(duration: Duration) -> u32 {
    u32::try_from(duration.as_millis()).unwrap_or(u32::MAX)
}

fn prune(samples: &mut VecDeque<(Instant, u32)>, now: Instant, window: Duration) {
    while samples
        .front()
        .is_some_and(|(at, _)| now.saturating_duration_since(*at) > window)
    {
        samples.pop_front();
    }
}

/// The median of the samples taken since `now - window`, dropping older ones.
fn windowed_median(
    samples: &mut VecDeque<(Instant, u32)>,
    now: Instant,
    window: Duration,
) -> Option<u32> {
    prune(samples, now, window);
    let mut values: Vec<u32> = samples.iter().map(|(_, value)| *value).collect();
    let middle = values.len() / 2;
    (!values.is_empty()).then(|| *values.select_nth_unstable(middle).1)
}

/// Latency and rate samples are medians over this window.
const LATENCY_WINDOW: Duration = Duration::from_secs(2);
/// Upper bounds of the presentation interval buckets; the last bucket is open.
const INTERVAL_BOUNDS_MS: [u32; 12] = [8, 12, 17, 20, 25, 34, 50, 67, 100, 150, 250, 500];
const INTERVAL_BUCKETS: usize = INTERVAL_BOUNDS_MS.len() + 1;
const INTERVAL_WINDOW_SECONDS: u64 = 60;

#[derive(Clone, Copy, Default)]
struct IntervalSecond {
    second: u64,
    counts: [u32; INTERVAL_BUCKETS],
    max_ms: u32,
}

/// Accepted presentations: rate, intervals and viewer latency. Owned by the
/// sink, recorded under its lock.
pub(crate) struct PresentStats {
    epoch: Instant,
    last: Option<Instant>,
    /// Presentations in the last second.
    recent: VecDeque<Instant>,
    seconds: [IntervalSecond; INTERVAL_WINDOW_SECONDS as usize],
    gaps_100: u64,
    gaps_250: u64,
    /// Network receipt to presentation, in ms.
    latency: VecDeque<(Instant, u32)>,
}
impl PresentStats {
    pub fn new(epoch: Instant) -> Self {
        Self {
            epoch,
            last: None,
            recent: VecDeque::new(),
            seconds: [IntervalSecond::default(); INTERVAL_WINDOW_SECONDS as usize],
            gaps_100: 0,
            gaps_250: 0,
            latency: VecDeque::new(),
        }
    }

    pub fn record(&mut self, now: Instant, latency: Duration) {
        if let Some(last) = self.last.replace(now) {
            let interval = millis_u32(now.saturating_duration_since(last));
            self.gaps_100 += u64::from(interval > 100);
            self.gaps_250 += u64::from(interval > 250);
            let second = now.saturating_duration_since(self.epoch).as_secs();
            let slot = &mut self.seconds[(second % INTERVAL_WINDOW_SECONDS) as usize];
            if slot.second != second {
                *slot = IntervalSecond {
                    second,
                    ..IntervalSecond::default()
                };
            }
            let bucket = INTERVAL_BOUNDS_MS
                .iter()
                .position(|bound| interval < *bound)
                .unwrap_or(INTERVAL_BOUNDS_MS.len());
            slot.counts[bucket] += 1;
            slot.max_ms = slot.max_ms.max(interval);
        }
        self.recent.push_back(now);
        self.prune_recent(now);
        self.latency.push_back((now, millis_u32(latency)));
        prune(&mut self.latency, now, LATENCY_WINDOW);
    }

    fn prune_recent(&mut self, now: Instant) {
        while self
            .recent
            .front()
            .is_some_and(|at| now.saturating_duration_since(*at) >= Duration::from_secs(1))
        {
            self.recent.pop_front();
        }
    }

    pub fn presented_fps(&mut self, now: Instant) -> f32 {
        self.prune_recent(now);
        self.recent.len() as f32
    }

    pub fn viewer_ms(&mut self, now: Instant) -> Option<u32> {
        windowed_median(&mut self.latency, now, LATENCY_WINDOW)
    }

    pub fn intervals(&self, now: Instant) -> IntervalStats {
        let current = now.saturating_duration_since(self.epoch).as_secs();
        let mut counts = [0u64; INTERVAL_BUCKETS];
        let mut max_ms = 0;
        for slot in &self.seconds {
            if current.saturating_sub(slot.second) < INTERVAL_WINDOW_SECONDS {
                for (total, count) in counts.iter_mut().zip(slot.counts) {
                    *total += u64::from(count);
                }
                max_ms = max_ms.max(slot.max_ms);
            }
        }
        let total: u64 = counts.iter().sum();
        let percentile = |fraction: f64| {
            if total == 0 {
                return 0;
            }
            let rank = ((total as f64 * fraction).ceil() as u64).max(1);
            let mut seen = 0;
            for (bucket, count) in counts.iter().enumerate() {
                seen += count;
                if seen >= rank {
                    return INTERVAL_BOUNDS_MS
                        .get(bucket)
                        .map_or(max_ms, |bound| (*bound).min(max_ms));
                }
            }
            max_ms
        };
        IntervalStats {
            p50_ms: percentile(0.50),
            p95_ms: percentile(0.95),
            p99_ms: percentile(0.99),
            max_ms,
            gaps_100: self.gaps_100,
            gaps_250: self.gaps_250,
        }
    }
}

/// The last host `Stats` message, as the viewer reports it.
#[derive(Clone, Copy)]
pub(crate) struct HostStats {
    pub capture_fps: f32,
    pub pace_fps: u32,
    pub target_fps: u32,
    pub fps_cap: u32,
    pub bitrate: u32,
    pub encoder_bitrate: u32,
    pub sent_bitrate: u32,
    pub rtt_ms: Option<u32>,
    pub base_rtt_ms: Option<u32>,
    pub queue_delay_ms: Option<u32>,
    pub ack_queue_ms: u32,
    pub encoder_dwell_ms: Option<u32>,
    pub dropped_frames: u64,
    pub skipped_ticks: u64,
    pub keyframes: u64,
    pub adaptation: Adaptation,
    pub rate_model: RateModel,
}

/// Bytes a session's tunnel carried so far, each way.
#[derive(Clone, Copy)]
pub(crate) struct Traffic {
    pub sent: u64,
    pub received: u64,
}

/// Received video and the host's last `Stats`. The read task records; the
/// write task's tick rolls the rates up once a second.
pub(crate) struct ViewerStatsAcc {
    pub host: Option<HostStats>,
    /// When the last `Stats` came, or the session started.
    host_at: Instant,
    /// Host ticks skipped per second between the last two `Stats`.
    skipped_per_s: f32,
    max_fps: u32,
    window_start: Instant,
    window_frames: u32,
    window_bytes: u64,
    pub received_fps: f32,
    pub received_bitrate: u32,
    /// The tunnel's traffic at the last roll, and when.
    traffic: Option<(Instant, Traffic)>,
    speed_rx_bps: u64,
    speed_tx_bps: u64,
    /// Encoder submit to socket write on the host, in ms.
    host_ms: VecDeque<(Instant, u32)>,
    pub last_video_at: Option<Instant>,
    pub last_video_size: Option<(u32, u32)>,
    poor_since: Option<Instant>,
    good_since: Option<Instant>,
    pub degraded: bool,
}
/// How long a condition must hold before `degraded` changes.
const DEGRADED_HOLD: Duration = Duration::from_secs(3);
impl ViewerStatsAcc {
    pub fn new(now: Instant, max_fps: u32) -> Self {
        Self {
            host: None,
            host_at: now,
            skipped_per_s: 0.0,
            max_fps,
            window_start: now,
            window_frames: 0,
            window_bytes: 0,
            received_fps: 0.0,
            received_bitrate: 0,
            traffic: None,
            speed_rx_bps: 0,
            speed_tx_bps: 0,
            host_ms: VecDeque::new(),
            last_video_at: None,
            last_video_size: None,
            poor_since: None,
            good_since: None,
            degraded: false,
        }
    }

    pub fn on_video(
        &mut self,
        now: Instant,
        bytes: usize,
        duplicate: bool,
        size: (u32, u32),
        host_us: u32,
    ) {
        self.window_frames += u32::from(!duplicate);
        self.window_bytes += bytes as u64;
        self.last_video_at = Some(now);
        self.last_video_size = Some(size);
        self.host_ms.push_back((now, host_us / 1000));
        prune(&mut self.host_ms, now, LATENCY_WINDOW);
    }

    fn host_ms(&mut self, now: Instant) -> Option<u32> {
        windowed_median(&mut self.host_ms, now, LATENCY_WINDOW)
    }

    /// The host's `Stats`, with its skipped ticks turned into a rate since the
    /// previous one, or since the session started. `Stats` carries no host
    /// time, and a link that held one delivers it right before the next, but
    /// the host sends them at least `STATS_INTERVAL` apart: the rate is over
    /// no less, and never above `max_fps`, since a tick skipped is one the
    /// pace had.
    pub fn on_host(&mut self, now: Instant, host: HostStats) {
        let skipped = self.host.map_or(0, |last| last.skipped_ticks);
        let elapsed = now
            .saturating_duration_since(self.host_at)
            .max(crate::session::STATS_INTERVAL)
            .as_secs_f32();
        self.skipped_per_s =
            (host.skipped_ticks.saturating_sub(skipped) as f32 / elapsed).min(self.max_fps as f32);
        self.host = Some(host);
        self.host_at = now;
    }

    /// `host_ms + rtt / 2 + viewer_ms`, once both medians exist.
    fn picture_latency(&self, host_ms: Option<u32>, viewer_ms: Option<u32>) -> Option<u32> {
        let rtt = self.host.and_then(|host| host.rtt_ms).unwrap_or(0);
        Some(host_ms?.saturating_add(rtt / 2).saturating_add(viewer_ms?))
    }

    /// The host's figures, the received rates, the latency medians and
    /// `degraded`.
    pub fn fill(&mut self, now: Instant, viewer_ms: Option<u32>, stats: &mut ViewerStats) {
        if let Some(host) = self.host {
            stats.rtt_ms = host.rtt_ms;
            stats.base_rtt_ms = host.base_rtt_ms;
            stats.queue_delay_ms = host.queue_delay_ms;
            stats.capture_fps = host.capture_fps;
            stats.pace_fps = host.pace_fps;
            stats.target_fps = host.target_fps;
            stats.fps_cap = host.fps_cap;
            stats.skipped_per_s = self.skipped_per_s;
            stats.encoder_dwell_ms = host.encoder_dwell_ms;
            stats.sent_bitrate = host.sent_bitrate;
            stats.ack_queue_ms = Some(host.ack_queue_ms);
            stats.bitrate = host.bitrate;
            stats.encoder_bitrate = host.encoder_bitrate;
            stats.adaptation = host.adaptation;
            stats.rate_model = host.rate_model;
            stats.keyframes = host.keyframes;
            stats.dropped_frames = host.dropped_frames;
        }
        stats.max_fps = self.max_fps;
        let host_ms = self.host_ms(now);
        stats.host_ms = host_ms;
        stats.viewer_ms = viewer_ms;
        stats.picture_latency_ms = self.picture_latency(host_ms, viewer_ms);
        stats.received_fps = self.received_fps;
        stats.received_bitrate = self.received_bitrate;
        stats.speed_rx_bps = self.speed_rx_bps;
        stats.speed_tx_bps = self.speed_tx_bps;
        stats.degraded = self.degraded;
    }

    /// Once a second: the received rates, the speed from the tunnel's
    /// `traffic` (`None` without a tunnel), and `degraded`, which is judged
    /// only while the picture should be moving.
    pub fn roll(
        &mut self,
        now: Instant,
        streaming: bool,
        presented_fps: f32,
        viewer_ms: Option<u32>,
        traffic: Option<Traffic>,
    ) {
        let elapsed = now
            .saturating_duration_since(self.window_start)
            .as_secs_f32();
        if elapsed > 0.0 {
            self.received_fps = self.window_frames as f32 / elapsed;
            self.received_bitrate = (self.window_bytes as f32 * 8.0 / elapsed) as u32;
        }
        match traffic {
            Some(total) => {
                // The first sample is the baseline.
                if let Some((at, last)) = self.traffic.replace((now, total)) {
                    let seconds = now.saturating_duration_since(at).as_secs_f64();
                    if seconds > 0.0 {
                        let rate = |now: u64, last: u64| {
                            (now.saturating_sub(last) as f64 / seconds) as u64
                        };
                        self.speed_rx_bps = rate(total.received, last.received);
                        self.speed_tx_bps = rate(total.sent, last.sent);
                    }
                }
            }
            None if elapsed > 0.0 => {
                self.speed_rx_bps = (self.window_bytes as f64 / f64::from(elapsed)) as u64;
                self.speed_tx_bps = 0;
            }
            None => {}
        }
        self.window_start = now;
        self.window_frames = 0;
        self.window_bytes = 0;
        if !streaming {
            self.poor_since = None;
            self.good_since = None;
            return;
        }
        let host_ms = self.host_ms(now);
        let latency = self.picture_latency(host_ms, viewer_ms);
        let host = self.host;
        let pace = host.map_or(0, |host| host.pace_fps);
        let poor = host
            .and_then(|host| host.queue_delay_ms)
            .is_some_and(|delay| delay >= 150)
            || latency.is_some_and(|latency| latency >= 250)
            || (pace >= 10 && presented_fps < 0.5 * pace as f32);
        let (since, other) = if poor {
            (&mut self.poor_since, &mut self.good_since)
        } else {
            (&mut self.good_since, &mut self.poor_since)
        };
        *other = None;
        if now.saturating_duration_since(*since.get_or_insert(now)) >= DEGRADED_HOLD {
            self.degraded = poor;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Speed is what the tunnel carried each way per second; a session with
    /// no tunnel reports the video it received.
    #[test]
    fn speed_follows_the_tunnel_or_else_the_video() {
        let start = Instant::now();
        let at = |seconds| start + Duration::from_secs(seconds);
        let speed = |acc: &mut ViewerStatsAcc, now| {
            let mut stats = ViewerStats::default();
            acc.fill(now, None, &mut stats);
            (stats.speed_rx_bps, stats.speed_tx_bps)
        };
        let traffic = |sent, received| Some(Traffic { sent, received });

        let mut tunnel = ViewerStatsAcc::new(start, 60);
        tunnel.roll(at(1), true, 0.0, None, traffic(1_000, 50_000));
        assert_eq!(speed(&mut tunnel, at(1)), (0, 0));
        // The tunnel's count already holds the video, and its framing.
        tunnel.on_video(at(2), 20_000, false, (1920, 1080), 0);
        tunnel.roll(at(3), true, 0.0, None, traffic(3_000, 450_000));
        assert_eq!(speed(&mut tunnel, at(3)), (200_000, 1_000));

        let mut test_stream = ViewerStatsAcc::new(start, 60);
        test_stream.on_video(at(1), 300_000, false, (1920, 1080), 0);
        test_stream.roll(at(2), true, 0.0, None, None);
        assert_eq!(speed(&mut test_stream, at(2)), (150_000, 0));
    }
}
