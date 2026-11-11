//! What happens to a tunnel, as [`TunnelEvent`]s: the labels they carry, the
//! history a tunnel keeps of them, and the stream that delivers each one.

use super::{AddressFamily, LanEvidence, LinkKind, LinkVia, PortMappingStatus, ms};
use pb_mapper_protocol::direct::{DirectTransport, SessionPath};
use serde::Serialize;
use std::collections::VecDeque;
use tokio::sync::broadcast;
use tokio::time::Instant;

/// The phase most recently entered by a tunnel worker.
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryPhase {
    Starting,
    Capacity,
    Resolving,
    Connecting,
    Handshake,
    Ready,
    Backoff,
    Stopped,
}

impl RecoveryPhase {
    /// Stable diagnostic label, matching the serialized representation.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Capacity => "capacity",
            Self::Resolving => "resolving",
            Self::Connecting => "connecting",
            Self::Handshake => "handshake",
            Self::Ready => "ready",
            Self::Backoff => "backoff",
            Self::Stopped => "stopped",
        }
    }
}

/// Why a recovery attempt failed, without credentials or application data.
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryFailure {
    Dns,
    Transport,
    Timeout,
    ServiceUnavailable,
    Rejected,
    NetworkChanged,
}

impl RecoveryFailure {
    /// Stable diagnostic label, matching the serialized representation.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Dns => "dns",
            Self::Transport => "transport",
            Self::Timeout => "timeout",
            Self::ServiceUnavailable => "service_unavailable",
            Self::Rejected => "rejected",
            Self::NetworkChanged => "network_changed",
        }
    }
}

/// Where a direct attempt stands. Every stage before [`Self::Committing`] can
/// still fall back to the relay; a fallback names the stage it left.
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DirectStage {
    /// Asking the relay whether the publisher offers direct, and opening the
    /// control link.
    Negotiating,
    /// Learning this endpoint's public address and requesting a session grant.
    Discovering,
    /// Dialing the publisher through both NATs.
    Punching,
    /// The path is up; the publisher is opening its local service.
    Committing,
}

impl DirectStage {
    /// Stable diagnostic label, matching the serialized representation.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Negotiating => "negotiating",
            Self::Discovering => "discovering",
            Self::Punching => "punching",
            Self::Committing => "committing",
        }
    }
}

/// Why a direct attempt gave way to the relay. Serialized, like
/// [`Self::as_str`], as the label [`PathDiagnostics::last_fallback`] has
/// always carried.
///
/// [`PathDiagnostics::last_fallback`]: super::PathDiagnostics::last_fallback
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[non_exhaustive]
pub enum FallbackReason {
    /// The relay or the publisher does not offer direct, or offers another
    /// version of it.
    #[serde(rename = "direct_not_offered")]
    NotOffered,
    /// Asking for direct, opening its control link, discovering this end's
    /// addresses or the grant failed.
    #[serde(rename = "direct_negotiation_failed")]
    NegotiationFailed,
    /// Those took longer than their budget of 3 s.
    #[serde(rename = "direct_negotiation_timeout")]
    NegotiationTimeout,
    /// No path through both NATs was set up in time.
    #[serde(rename = "direct_timeout_or_unreachable")]
    Unreachable,
}

impl FallbackReason {
    /// Stable diagnostic label, matching the serialized representation.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotOffered => "direct_not_offered",
            Self::NegotiationFailed => "direct_negotiation_failed",
            Self::NegotiationTimeout => "direct_negotiation_timeout",
            Self::Unreachable => "direct_timeout_or_unreachable",
        }
    }
}

/// Why a direct setup that asked for TCP within a local network did not run
/// over it, and took QUIC or the relay instead.
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum LanTcpMiss {
    /// The publisher offered none: it, or the relay, predates TCP; it does
    /// not serve it; or it shares no network with this end as it sees it.
    NotOffered,
    /// It offered addresses, none on a network of this end's.
    NoSharedLan,
    /// Its address refused the connection, or could not be reached.
    Refused,
    /// Nothing answered in time: a firewall dropping it, typically.
    TimedOut,
    /// TCP connected, and TLS failed.
    Handshake,
}

impl LanTcpMiss {
    /// Stable diagnostic label, matching the serialized representation.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotOffered => "not_offered",
            Self::NoSharedLan => "no_shared_lan",
            Self::Refused => "refused",
            Self::TimedOut => "timed_out",
            Self::Handshake => "handshake",
        }
    }
}

/// What happened, in a [`TunnelEvent`].
///
/// The first six are outcomes, kept in [`TunnelDiagnostics::events`] as well
/// as delivered to [`TunnelEvents`]. The rest are steps along the way,
/// delivered live only: kept, they would crowd the outcomes out of a history
/// of 16.
///
/// Non-exhaustive: kinds are added as the SDK observes more, so a `match`
/// needs a wildcard arm.
///
/// [`TunnelDiagnostics::events`]: super::TunnelDiagnostics::events
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum TunnelEventKind {
    /// The relay answered for the first time, or again after failures.
    RelayReady { latency_ms: u64 },
    /// A relay attempt failed; the next one is `retry_in_ms` away (zero when
    /// the worker retries at once or the failure is final).
    RelayFailed {
        failure: RecoveryFailure,
        retry_in_ms: u64,
    },
    /// The local network changed; recovery restarts at once.
    NetworkChanged,
    /// A session took the direct path.
    DirectEstablished { setup_ms: u64 },
    /// A session gave up on direct at `stage`, for `reason`, and used the
    /// relay.
    DirectFallback {
        stage: DirectStage,
        reason: FallbackReason,
    },
    /// The tunnel stopped, and whatever it was cleaning up has reported. The
    /// last event of every stream.
    Stopped,
    /// A relay attempt entered `phase`. Once the tunnel is ready, its routine
    /// re-checks are not announced: the next change is `backoff`, when one
    /// fails.
    PhaseChanged { phase: RecoveryPhase },
    /// A direct attempt entered `stage`.
    DirectStageChanged { stage: DirectStage },
    /// A direct attempt ended at `stage` with neither a direct session nor a
    /// fallback: its setup was cancelled, or it failed after committing, when
    /// the relay can no longer take its place.
    DirectAbandoned { stage: DirectStage },
    /// Session number `session` of this tunnel opened on `path`; a direct
    /// one across `link`.
    SessionOpened {
        session: u64,
        path: SessionPath,
        link: Option<LinkKind>,
    },
    /// Session `session` closed, after `duration_ms`, having carried these
    /// application bytes: sent from this end's local side, and received.
    SessionClosed {
        session: u64,
        path: SessionPath,
        link: Option<LinkKind>,
        duration_ms: u64,
        sent_bytes: u64,
        received_bytes: u64,
    },
    /// The gateway mapping of the tunnel's direct endpoint changed.
    PortMappingChanged { mapping: Box<PortMappingStatus> },
    /// A direct setup found the peer on one of this end's local networks,
    /// before any packet. With preferred, this end's LAN-first is on: the
    /// peer's LAN addresses are probed first.
    LanDetected {
        evidence: LanEvidence,
        preferred: bool,
    },
    /// A direct session committed to a path: across `link`, over `family`,
    /// to an address learned `via`, carried by `transport`.
    PathSelected {
        link: LinkKind,
        family: AddressFamily,
        via: LinkVia,
        transport: DirectTransport,
    },
    /// A direct setup that found the peer on a local network, by
    /// `evidence`, did not end there: it took a path across `took`, or the
    /// relay when `None`.
    LanMissed {
        evidence: LanEvidence,
        took: Option<LinkKind>,
    },
    /// A direct setup that asked for TCP within a local network did not get
    /// it, for `reason`: it took `took`, or the relay when `None`.
    LanTcpMissed {
        reason: LanTcpMiss,
        took: Option<DirectTransport>,
    },
}

impl TunnelEventKind {
    /// Whether a repeat of `self` folds into the entry for `other`.
    pub(super) fn folds_into(&self, other: &Self) -> bool {
        match self {
            Self::RelayFailed { failure, .. } => {
                matches!(other, Self::RelayFailed { failure: last, .. } if last == failure)
            }
            Self::DirectFallback { stage, reason } => matches!(
                other,
                Self::DirectFallback {
                    stage: last_stage,
                    reason: last_reason,
                } if (last_stage, last_reason) == (stage, reason)
            ),
            Self::RelayReady { .. } => matches!(other, Self::RelayReady { .. }),
            Self::NetworkChanged => matches!(other, Self::NetworkChanged),
            Self::DirectEstablished { .. } => matches!(other, Self::DirectEstablished { .. }),
            Self::Stopped
            | Self::PhaseChanged { .. }
            | Self::DirectStageChanged { .. }
            | Self::DirectAbandoned { .. }
            | Self::SessionOpened { .. }
            | Self::SessionClosed { .. }
            | Self::PortMappingChanged { .. }
            | Self::LanDetected { .. }
            | Self::PathSelected { .. }
            | Self::LanMissed { .. }
            | Self::LanTcpMissed { .. } => false,
        }
    }

    /// Whether this is a step delivered live only, never kept in the history.
    pub(super) fn is_live_only(&self) -> bool {
        match self {
            Self::PhaseChanged { .. }
            | Self::DirectStageChanged { .. }
            | Self::DirectAbandoned { .. }
            | Self::SessionOpened { .. }
            | Self::SessionClosed { .. }
            | Self::PortMappingChanged { .. }
            | Self::LanDetected { .. }
            | Self::PathSelected { .. }
            | Self::LanMissed { .. }
            | Self::LanTcpMissed { .. } => true,
            Self::RelayReady { .. }
            | Self::RelayFailed { .. }
            | Self::NetworkChanged
            | Self::DirectEstablished { .. }
            | Self::DirectFallback { .. }
            | Self::Stopped => false,
        }
    }
}

/// One event of a tunnel. In [`TunnelDiagnostics::events`], oldest first,
/// consecutive repeats fold into one entry, so a long outage cannot push out
/// what came before it; [`TunnelEvents`] delivers every occurrence.
///
/// [`TunnelDiagnostics::events`]: super::TunnelDiagnostics::events
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct TunnelEvent {
    /// Increases with every event, a fold included, so a reader can tell
    /// what it has not seen yet. A stream and the history share it.
    pub seq: u64,
    /// Time since the latest occurrence.
    pub age_ms: u64,
    /// Occurrences this entry stands for.
    pub repeats: u32,
    /// The latest occurrence.
    pub kind: TunnelEventKind,
}

/// How many entries a tunnel's history keeps.
pub(super) const EVENT_CAPACITY: usize = 16;

/// How many events a subscriber may fall behind before it misses some.
pub(super) const STREAM_CAPACITY: usize = 128;

/// A tunnel's events as they happen, after the history it has kept: from
/// `subscribe_events` on a [`Registration`](crate::sdk::Registration) or a
/// [`Connection`](crate::sdk::Connection).
///
/// Every subscriber reads at its own pace from its own queue. One that falls
/// more than 128 events behind is told how many it missed, and goes on from
/// there. A subscription does not keep its tunnel alive: once the tunnel
/// stops and its cleanup has reported, the stream ends.
pub struct TunnelEvents {
    pub(super) replay: VecDeque<Event>,
    pub(super) receiver: broadcast::Receiver<Event>,
}

impl TunnelEvents {
    /// The next event, or `None` once the stream has ended. An error says
    /// events were missed; the tunnel's `diagnostics()` is the state to
    /// reconcile with, and the stream goes on. Cancel-safe.
    pub async fn recv(&mut self) -> Option<Result<TunnelEvent, EventsLagged>> {
        // Aged as it is read, however long it waited in the queue.
        if let Some(event) = self.replay.pop_front() {
            return Some(Ok(event.into_public(Instant::now())));
        }
        match self.receiver.recv().await {
            Ok(event) => Some(Ok(event.into_public(Instant::now()))),
            Err(broadcast::error::RecvError::Lagged(skipped)) => {
                Some(Err(EventsLagged { skipped }))
            }
            Err(broadcast::error::RecvError::Closed) => None,
        }
    }
}

/// A [`TunnelEvents`] subscriber fell behind and missed `skipped` events.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EventsLagged {
    pub skipped: u64,
}

impl std::fmt::Display for EventsLagged {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "missed {} tunnel events", self.skipped)
    }
}

impl std::error::Error for EventsLagged {}

#[derive(Clone)]
pub(super) struct Event {
    pub(super) seq: u64,
    pub(super) at: Instant,
    pub(super) repeats: u32,
    pub(super) kind: TunnelEventKind,
}

impl Event {
    /// What a reader is shown of this event, read at `now`.
    pub(super) fn public(&self, now: Instant) -> TunnelEvent {
        self.clone().into_public(now)
    }

    /// [`Self::public`], from an event the reader owns: its kind moves
    /// rather than being copied.
    pub(super) fn into_public(self, now: Instant) -> TunnelEvent {
        let Self {
            seq,
            at,
            repeats,
            kind,
        } = self;
        TunnelEvent {
            seq,
            age_ms: ms(now.saturating_duration_since(at)),
            repeats,
            kind,
        }
    }
}
