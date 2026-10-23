//! Coalesced best-effort observations. Reporting never waits on the data path.
use pb_mapper_protocol::direct::{SessionId, SessionState, SessionUpdate};
use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};

#[derive(Default)]
pub(super) struct Latest(Mutex<HashMap<SessionId, (SessionUpdate, Instant)>>);
impl Latest {
    pub fn put(&self, update: SessionUpdate) {
        let mut reports = self.0.lock().unwrap_or_else(|e| e.into_inner());
        reports.retain(|_, (u, at)| {
            u.report.state == SessionState::Active || at.elapsed() < Duration::from_secs(30)
        });
        if reports.len() < 1024 || reports.contains_key(&update.report.id) {
            reports.insert(update.report.id, (update, Instant::now()));
        }
    }
    pub fn snapshot(&self) -> Vec<SessionUpdate> {
        let mut reports = self.0.lock().unwrap_or_else(|e| e.into_inner());
        reports.retain(|_, (u, at)| {
            u.report.state == SessionState::Active || at.elapsed() < Duration::from_secs(30)
        });
        reports.values().map(|(update, _)| update.clone()).collect()
    }
}
