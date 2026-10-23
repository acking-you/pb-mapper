//! Coalesced best-effort observations. Reporting never waits on the data path.
use pb_mapper_protocol::direct::{SessionId, SessionState, SessionUpdate};
use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};

const CAPACITY: usize = 1024;
/// How long a closed session is still reported, so the coordinator sees it end.
const CLOSED_RETENTION: Duration = Duration::from_secs(30);

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
