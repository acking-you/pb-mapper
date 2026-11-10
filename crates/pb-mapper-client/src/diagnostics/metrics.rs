//! What a running tunnel keeps measuring — its sessions, the bytes they
//! carry on each path, the relay's health, the direct link — as one compact
//! [`TunnelMetrics`], read with `metrics()` or followed with
//! `subscribe_metrics()`.
//!
//! [`TunnelDiagnostics`](super::TunnelDiagnostics) is the full snapshot; this
//! is the part of it that changes all the time, cheap enough to sample every
//! second. Both are read from the same state, so they always agree.

use super::{Diagnostics, LanEvidence, LinkStats, LinkVia, RecoveryPhase};
use crate::relay::RelayTransport;
pub use pb_mapper_protocol::direct::{AddressFamily, DirectTransport, LinkKind};
use pb_mapper_protocol::forward::ForwardTotals;
use serde::Serialize;
use std::net::{IpAddr, SocketAddr};
use tokio::sync::watch;
use tokio::time::{Duration, Instant};

/// Application bytes: what the local service and the peer exchanged, after
/// decryption and without framing. `sent` went from this end's local side
/// to the remote one, `received` came back.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct ByteCount {
    pub sent: u64,
    pub received: u64,
}

impl ByteCount {
    pub(super) fn add(self, other: ForwardTotals) -> Self {
        Self {
            sent: self.sent.saturating_add(other.sent),
            received: self.received.saturating_add(other.received),
        }
    }

    fn plus(self, other: Self) -> Self {
        self.add(ForwardTotals {
            sent: other.sent,
            received: other.received,
        })
    }
}

/// A tunnel's application bytes by path, over its life: those of its
/// sessions that closed, and so far of those open now. `direct` includes
/// `lan`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct PathTraffic {
    pub relay: ByteCount,
    pub direct: ByteCount,
    /// Direct sessions within one local network.
    pub lan: ByteCount,
}

/// Where a session's bytes are counted: each session in exactly one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Bucket {
    Relay,
    DirectLan,
    /// A direct path across the internet, a private network, or one of a
    /// kind this end does not know.
    DirectOther,
}

impl Bucket {
    pub(super) const ALL: usize = 3;

    pub(super) fn of(
        path: pb_mapper_protocol::direct::SessionPath,
        link: Option<LinkKind>,
    ) -> Self {
        match (path, link) {
            (pb_mapper_protocol::direct::SessionPath::Relay, _) => Self::Relay,
            (_, Some(LinkKind::Lan)) => Self::DirectLan,
            _ => Self::DirectOther,
        }
    }

    pub(super) fn index(self) -> usize {
        self as usize
    }

    /// The buckets, summed into the paths they make up.
    pub(super) fn traffic(buckets: [ByteCount; Self::ALL]) -> PathTraffic {
        let [relay, lan, other] = buckets;
        PathTraffic {
            relay,
            direct: lan.plus(other),
            lan,
        }
    }
}

/// A tunnel's sessions on each path: open now, and since it started.
/// Each TCP connection or UDP flow through it is one session.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct SessionCounts {
    pub active_relay: u64,
    pub active_direct: u64,
    /// Of the direct sessions, those within one local network.
    pub active_lan: u64,
    pub relay: u64,
    pub direct: u64,
    pub lan: u64,
}

/// How this tunnel's relay is doing, as its own attempts and probes see it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct RelayHealth {
    /// The carrier, fixed when the tunnel started.
    pub transport: RelayTransport,
    pub phase: RecoveryPhase,
    pub consecutive_failures: u64,
    /// The smoothed round trip of the tunnel's control exchanges with the
    /// relay: a share's heartbeat, a connection's service probe. It includes
    /// the relay's own handling, so it is the control path's round trip, not
    /// the network's alone; `None` until one is answered.
    pub rtt_ms: Option<u64>,
    /// Time since the relay last answered, even with a refusal.
    pub last_success_age_ms: Option<u64>,
}

/// The newest direct session open now: where its path runs, how it was
/// found, and QUIC's view of it as of this sample.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct LinkSample {
    pub kind: LinkKind,
    pub family: AddressFamily,
    pub via: LinkVia,
    pub transport: DirectTransport,
    pub peer: SocketAddr,
    pub local_ip: Option<IpAddr>,
    /// What suggested a shared network before the setup, if anything.
    pub same_lan: Option<LanEvidence>,
    /// How long its setup took.
    pub setup_ms: u64,
    pub stats: LinkStats,
}

/// Direct setups that found the peer on one of this end's local networks,
/// and those of them that ended on another path or on the relay.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct LanCounts {
    pub detected: u64,
    pub missed: u64,
}

/// A running tunnel's figures, sampled at once: see the module docs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct TunnelMetrics {
    /// Time since the tunnel started.
    pub uptime_ms: u64,
    /// The `seq` of the tunnel's latest [`TunnelEvent`](super::TunnelEvent),
    /// so a sample can be lined up with its event stream.
    pub event_seq: u64,
    pub sessions: SessionCounts,
    pub traffic: PathTraffic,
    pub relay: RelayHealth,
    pub link: Option<LinkSample>,
    pub lan: LanCounts,
    /// How long new sessions still go straight to the relay after a direct
    /// attempt fell back; see [`Self::cooldown_skips`].
    pub direct_cooldown_ms: Option<u64>,
    /// Sessions that went straight to the relay without trying direct,
    /// because a recent attempt had fallen back.
    pub cooldown_skips: u64,
    /// Whether direct setups try a shared local network first; see
    /// [TunnelDiagnostics::lan_first](super::TunnelDiagnostics::lan_first).
    pub lan_first: bool,
}

/// The closest two samples of a stream may be.
const MIN_GAP: Duration = Duration::from_millis(100);
/// The longest a stream's period may be.
const MAX_EVERY: Duration = Duration::from_secs(60 * 60);

/// The period a stream asked to sample `every` samples at: 100 ms to an hour.
pub(crate) fn sample_period(every: Duration) -> Duration {
    every.clamp(MIN_GAP, MAX_EVERY)
}

/// A running tunnel's [`TunnelMetrics`], sampled as time passes and as it
/// changes: from `subscribe_metrics` on a
/// [`Registration`](crate::sdk::Registration) or a
/// [`Connection`](crate::sdk::Connection).
///
/// [`Self::recv`] returns a sample at once, then one every period and one
/// whenever a session opens or closes or the tunnel's state otherwise
/// changes, never two within 100 ms. Nothing samples in the background and
/// nothing queues: each sample is taken when it is returned, so a reader
/// that falls behind gets fewer, fresher samples. Once the tunnel has stopped
/// and its cleanup has reported, it returns one last sample, then `None`. It
/// holds no connection open and does not keep its tunnel running.
pub struct TunnelMetricsStream {
    diagnostics: Diagnostics,
    transport: RelayTransport,
    /// Gone once the tunnel has stopped.
    revision: Option<watch::Receiver<u64>>,
    every: Duration,
    /// When the latest sample was taken: the period and the gap run from
    /// it, whatever made it due.
    last: Option<Instant>,
    next: Next,
}

/// What [`TunnelMetricsStream::recv`] returns next.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Next {
    /// A sample at once: the first.
    First,
    /// A sample once the period passes or the tunnel changes.
    Wait,
    /// A sample as soon as the gap allows.
    Due,
    /// Nothing: the last sample was taken after the tunnel stopped.
    End,
}

impl TunnelMetricsStream {
    pub(crate) fn new(
        diagnostics: Diagnostics,
        transport: RelayTransport,
        every: Duration,
    ) -> Self {
        // No timer is made here: this may be called outside a runtime.
        let revision = diagnostics.revision();
        Self {
            diagnostics,
            transport,
            revision,
            every: sample_period(every),
            last: None,
            next: Next::First,
        }
    }

    /// The next sample, or `None` once the stream has ended. Cancel-safe: a
    /// sample made due is returned by the next call.
    pub async fn recv(&mut self) -> Option<TunnelMetrics> {
        loop {
            match self.next {
                Next::First => return Some(self.sample()),
                Next::Wait => {
                    let Some(revision) = &mut self.revision else {
                        self.next = Next::Due;
                        continue;
                    };
                    let period = self.last.map_or_else(Instant::now, |at| at + self.every);
                    // The tunnel's end is a change too: it drops the sender.
                    tokio::select! {
                        () = tokio::time::sleep_until(period) => {}
                        changed = revision.changed() => {
                            if changed.is_err() {
                                self.revision = None;
                            }
                        }
                    }
                    self.next = Next::Due;
                }
                Next::Due => {
                    if let Some(at) = self.last.map(|at| at + MIN_GAP) {
                        tokio::time::sleep_until(at).await;
                    }
                    return Some(self.sample());
                }
                Next::End => return None,
            }
        }
    }

    /// Sample now, and decide what comes next: nothing more once the tunnel
    /// has stopped, so the first sample taken after that is the last.
    fn sample(&mut self) -> TunnelMetrics {
        let now = Instant::now();
        self.last = Some(now);
        if let Some(revision) = &mut self.revision {
            revision.borrow_and_update();
        }
        let (metrics, stopped) = {
            let state = self.diagnostics.state();
            (state.metrics(self.transport, now), state.revision.is_none())
        };
        self.next = if stopped { Next::End } else { Next::Wait };
        metrics
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::TunnelEventKind;
    use pb_mapper_protocol::direct::SessionPath;

    /// Node opens a stream from its JavaScript thread, outside any runtime.
    #[test]
    fn a_stream_is_opened_without_a_runtime() {
        let diagnostics = Diagnostics::default();
        let _stream = diagnostics.subscribe_metrics(RelayTransport::Tcp, Duration::from_secs(1));
    }

    #[tokio::test(start_paused = true)]
    async fn sessions_and_their_bytes_count_on_their_path_until_the_stream_ends() {
        let diagnostics = Diagnostics::default();
        let mut events = diagnostics.subscribe();
        let mut stream = TunnelMetricsStream::new(
            diagnostics.clone(),
            RelayTransport::Tcp,
            Duration::from_secs(5),
        );
        let first = stream.recv().await.unwrap();
        assert_eq!(first.sessions, SessionCounts::default());

        let lan = diagnostics.session_opened(
            SessionPath::Direct,
            Some(pb_mapper_protocol::direct::LinkSummary {
                kind: LinkKind::Lan,
                family: AddressFamily::Ipv4,
                transport: DirectTransport::Quic,
            }),
        );
        let relay = diagnostics.session_opened(SessionPath::Relay, None);
        lan.meter().add_sent(100);
        relay.meter().add_received(7);
        // A change is sampled well before the period, bytes as they pass.
        let opened = stream.recv().await.unwrap();
        assert!(opened.uptime_ms < 1_000);
        assert_eq!(
            (opened.sessions.active_lan, opened.sessions.active_relay),
            (1, 1)
        );
        assert_eq!(
            (opened.traffic.direct.sent, opened.traffic.lan.sent),
            (100, 100)
        );
        assert_eq!(opened.traffic.relay.received, 7);

        drop(relay);
        drop(lan);
        let closed = stream.recv().await.unwrap();
        assert_eq!(
            (closed.sessions.active_direct, closed.sessions.direct),
            (0, 1)
        );
        assert_eq!(
            closed.traffic, opened.traffic,
            "a closed session's bytes stay"
        );
        let closes: Vec<_> = std::iter::from_fn(|| events.receiver.try_recv().ok())
            .filter_map(|event| match event.kind {
                TunnelEventKind::SessionClosed {
                    session,
                    sent_bytes,
                    received_bytes,
                    ..
                } => Some((session, sent_bytes, received_bytes)),
                _ => None,
            })
            .collect();
        assert_eq!(closes, [(2, 0, 7), (1, 100, 0)]);

        diagnostics.stop();
        let last = stream.recv().await.unwrap();
        assert_eq!(last.relay.phase, RecoveryPhase::Stopped);
        assert!(stream.recv().await.is_none());
    }
}
