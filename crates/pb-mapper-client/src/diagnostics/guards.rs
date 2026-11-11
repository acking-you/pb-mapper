//! What a tunnel's workers hold while something is under way: a session, a
//! direct attempt, a direct link, or a cleanup that holds the event stream
//! open. Each reports to the tunnel's [`Diagnostics`] when it drops.

use super::Diagnostics;
#[cfg(feature = "p2p")]
use super::{DirectStage, FallbackReason, LinkKind, TunnelEventKind, ms};
use pb_mapper_protocol::forward::ByteMeter;
use std::sync::Arc;
#[cfg(feature = "p2p")]
use tokio::time::{Duration, Instant};

/// Holds a tunnel's event stream open past its stop; see
/// [`Diagnostics::defer_close`].
#[cfg(feature = "port-mapping")]
pub(crate) struct DeferClose(pub(super) Diagnostics);

#[cfg(feature = "port-mapping")]
impl Drop for DeferClose {
    fn drop(&mut self) {
        let mut state = self.0.state();
        state.cleanups = state.cleanups.saturating_sub(1);
        state.close_if_drained();
    }
}

/// One direct attempt in flight. Dropped without an outcome (an error after
/// commit, a cancelled setup), it tells subscribers the attempt was abandoned,
/// so that one following its stages does not wait on it forever.
#[cfg(feature = "p2p")]
pub(crate) struct DirectAttemptGuard {
    pub(super) diagnostics: Diagnostics,
    pub(super) stage: DirectStage,
    pub(super) started: Instant,
    pub(super) settled: bool,
}

#[cfg(feature = "p2p")]
impl DirectAttemptGuard {
    pub(crate) fn stage(&mut self, stage: DirectStage) {
        self.stage = stage;
        let mut state = self.diagnostics.state();
        state.attempt = Some((stage, self.started));
        state.record(TunnelEventKind::DirectStageChanged { stage });
    }
    /// Time since the attempt began.
    pub(crate) fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }
    /// The attempt went direct, on a path of `kind`, after `setup`.
    pub(crate) fn established(mut self, setup: Duration, kind: LinkKind) {
        self.settled = true;
        let mut state = self.diagnostics.state();
        state.direct.get_or_insert_default().last_kind = Some(kind);
        state.record(TunnelEventKind::DirectEstablished {
            setup_ms: ms(setup),
        });
    }
    /// Fall back to the relay for `reason`, whose label
    /// [`PathDiagnostics::last_fallback`] keeps.
    ///
    /// [`PathDiagnostics::last_fallback`]: super::PathDiagnostics::last_fallback
    pub(crate) fn fell_back(mut self, reason: FallbackReason) {
        self.settled = true;
        let mut state = self.diagnostics.state();
        let p = state.direct.get_or_insert_default();
        p.fallbacks += 1;
        p.last_fallback = Some(reason.as_str().into());
        state.record(TunnelEventKind::DirectFallback {
            stage: self.stage,
            reason,
        });
    }
}

#[cfg(feature = "p2p")]
impl Drop for DirectAttemptGuard {
    fn drop(&mut self) {
        let mut state = self.diagnostics.state();
        state.in_flight = state.in_flight.saturating_sub(1);
        if state.in_flight == 0 {
            state.attempt = None;
        }
        if !self.settled {
            state.record(TunnelEventKind::DirectAbandoned { stage: self.stage });
        }
    }
}

/// A direct session's link, forgotten, and its connection let go, when this
/// drops with the session.
#[cfg(feature = "p2p")]
pub(crate) struct LinkGuard {
    pub(super) diagnostics: Diagnostics,
    pub(super) id: u64,
}

#[cfg(feature = "p2p")]
impl Drop for LinkGuard {
    fn drop(&mut self) {
        self.diagnostics
            .state()
            .links
            .retain(|live| live.id != self.id);
    }
}

/// A session open on a path, from [`Diagnostics::session_opened`]: its
/// forwarding counts into [`Self::meter`], and dropping it closes the
/// session with the bytes counted, however the forwarding ended.
pub(crate) struct SessionGuard {
    pub(super) diagnostics: Diagnostics,
    pub(super) id: u64,
    pub(super) meter: Arc<ByteMeter>,
}

impl SessionGuard {
    /// Where the session's forwarding counts its bytes.
    pub(crate) fn meter(&self) -> Arc<ByteMeter> {
        self.meter.clone()
    }
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        self.diagnostics.session_closed(self.id);
    }
}
