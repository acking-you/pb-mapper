//! Coalesced best-effort observations. Reporting never waits on the data path.
use super::{consumer::Consumer, provider::ProviderReports};
use crate::diagnostics::{LinkGuard, SessionGuard};
use pb_mapper_protocol::direct::{
    LinkSummary, SessionGrant, SessionId, SessionPath, SessionReport, SessionSecret, SessionState,
    SessionSummary, SessionUpdate,
};
use std::{
    collections::HashMap,
    future::Future,
    io,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const CAPACITY: usize = 1024;
/// How long a closed session is still reported, so the coordinator sees it end.
const CLOSED_RETENTION: Duration = Duration::from_secs(30);
/// How often a live session reports itself, and so the longest a consumer's
/// reporter waits between attempts.
pub(super) const REPORT_INTERVAL: Duration = Duration::from_secs(15);

#[derive(Default)]
pub(super) struct Latest(Mutex<HashMap<SessionId, (SessionUpdate, Instant)>>);
impl Latest {
    /// O(1) for an existing session; a periodic tick of every live session
    /// must not scan the whole table.
    pub fn put(&self, update: SessionUpdate) {
        let mut reports = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if reports.len() >= CAPACITY && !reports.contains_key(&update.report.id) {
            prune(&mut reports);
            if reports.len() >= CAPACITY {
                let oldest_closed = reports
                    .iter()
                    .filter(|(_, (u, _))| u.report.state == SessionState::Closed)
                    .min_by_key(|(_, (_, at))| *at)
                    .map(|(id, _)| *id);
                let Some(id) = oldest_closed else {
                    return;
                };
                reports.remove(&id);
            }
        }
        reports.insert(update.report.id, (update, Instant::now()));
    }
    pub fn get(&self, id: &SessionId) -> Option<SessionUpdate> {
        let reports = self.0.lock().unwrap_or_else(|e| e.into_inner());
        reports.get(id).map(|(update, _)| update.clone())
    }
    pub fn snapshot(&self) -> Vec<SessionUpdate> {
        let mut reports = self.0.lock().unwrap_or_else(|e| e.into_inner());
        prune(&mut reports);
        reports.values().map(|(update, _)| update.clone()).collect()
    }
}
fn prune(reports: &mut HashMap<SessionId, (SessionUpdate, Instant)>) {
    reports.retain(|_, (u, at)| {
        u.report.state == SessionState::Active || at.elapsed() < CLOSED_RETENTION
    });
}

/// Where a session's reports go: a consumer's reporter, which the session
/// keeps running until its last report is queued, or a provider's routes to
/// its coordinator.
pub(super) enum Sink {
    Consumer(Arc<Consumer>),
    Provider(Arc<ProviderReports>),
}

/// One open session's reports, on either end: active when it opens and
/// every [`REPORT_INTERVAL`] while it forwards, closed when it is dropped.
/// Dropping it then closes the session in diagnostics and lets its link go,
/// in that order.
pub(super) struct SessionReporter {
    sink: Sink,
    session: SessionSummary,
    secret: SessionSecret,
    path: SessionPath,
    link: Option<LinkSummary>,
    seq: u64,
    /// Closes the session in diagnostics after the final report.
    _session: SessionGuard,
    _link: Option<LinkGuard>,
}

impl SessionReporter {
    /// Report `grant`'s session open, on `path` and `link`, with the
    /// diagnostics guards it holds until it closes.
    pub fn start(
        sink: Sink,
        grant: &SessionGrant,
        path: SessionPath,
        link: Option<LinkSummary>,
        session: SessionGuard,
        link_guard: Option<LinkGuard>,
    ) -> Self {
        let mut reporter = Self {
            sink,
            session: grant.into(),
            secret: grant.secret,
            path,
            link,
            seq: 0,
            _session: session,
            _link: link_guard,
        };
        reporter.update(SessionState::Active);
        reporter
    }

    /// Run `forwarding` to its end, reporting the session active every
    /// [`REPORT_INTERVAL`] meanwhile.
    pub async fn during(
        &mut self,
        forwarding: impl Future<Output = io::Result<()>>,
    ) -> io::Result<()> {
        tokio::pin!(forwarding);
        let mut tick = tokio::time::interval(REPORT_INTERVAL);
        tick.tick().await;
        loop {
            tokio::select! {
                result = &mut forwarding => return result,
                _ = tick.tick() => self.update(SessionState::Active),
            }
        }
    }

    fn update(&mut self, state: SessionState) {
        self.seq += 1;
        let update = SessionUpdate {
            session: self.session.clone(),
            secret: self.secret,
            report: SessionReport {
                id: self.session.id,
                seq: self.seq,
                path: self.path,
                state,
                reason: None,
                link: self.link,
            },
        };
        match &self.sink {
            Sink::Consumer(owner) => {
                owner.reports.put(update);
                owner.report_wake.notify_one();
            }
            Sink::Provider(reports) => reports.put(update),
        }
    }
}

impl Drop for SessionReporter {
    fn drop(&mut self) {
        self.update(SessionState::Closed);
    }
}
