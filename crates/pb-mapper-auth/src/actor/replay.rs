//! The administrator replay window: which mutations were already admitted.
use super::super::*;

/// The fingerprints of administrator mutations admitted within
/// `ADMIN_REPLAY_RETENTION`, so a captured request cannot be admitted twice.
///
/// The set answers "seen before?"; the queue holds the same records in admission
/// order, which is the order they expire in and what a snapshot persists. Both
/// change only through these methods, so they cannot drift apart.
pub(crate) struct AdminReplays {
    seen: HashSet<[u8; 32]>,
    order: VecDeque<AdminReplayRecord>,
}

impl AdminReplays {
    /// The window a start resumes from: the persisted records still within
    /// retention, newest last, trimmed to `ADMIN_REPLAY_CAPACITY`.
    pub(crate) fn restored(records: &[AdminReplayRecord], now: u64) -> Self {
        let mut order = records
            .iter()
            .filter(|record| record.within_retention(now))
            .cloned()
            .collect::<VecDeque<_>>();
        while order.len() > ADMIN_REPLAY_CAPACITY {
            order.pop_front();
        }
        let seen = order.iter().map(|record| record.fingerprint).collect();
        Self { seen, order }
    }

    /// Forgets every record that has left the retention window. Returns
    /// whether there was one.
    pub(crate) fn prune(&mut self, now: u64) -> bool {
        let Self { seen, order } = self;
        let before = order.len();
        order.retain(|record| {
            let keep = record.within_retention(now);
            if !keep {
                seen.remove(&record.fingerprint);
            }
            keep
        });
        order.len() < before
    }

    /// Whether a mutation with `fingerprint` is inside the window.
    pub(crate) fn admitted(&self, fingerprint: &[u8; 32]) -> bool {
        self.seen.contains(fingerprint)
    }

    /// Admits a mutation, durably, unless it was admitted before, the window is
    /// full, or its timestamp is too far from the relay's clock.
    pub(crate) fn claim(
        &mut self,
        inner: &AuthStateInner,
        config: &AuthConfig,
        fingerprint: [u8; 32],
        client_timestamp: u64,
    ) -> Result<(), AuthFailure> {
        let now = unix_seconds();
        self.prune(now);
        if self.admitted(&fingerprint) {
            return Err(AuthFailure::new(
                codes::ADMIN_REQUEST_REPLAYED,
                "administrator mutation was already admitted",
                false,
            ));
        }
        if self.seen.len() >= ADMIN_REPLAY_CAPACITY {
            return Err(AuthFailure::new(
                codes::ADMIN_REPLAY_CAPACITY_EXHAUSTED,
                "administrator mutation replay window is full; retry after older claims expire",
                true,
            ));
        }
        if now.abs_diff(client_timestamp) > ADMIN_REPLAY_RETENTION.as_secs() / 2 {
            return Err(AuthFailure::new(
                codes::ADMIN_REQUEST_TIMESTAMP_INVALID,
                "administrator mutation timestamp is outside the accepted window",
                false,
            ));
        }
        let record = AdminReplayRecord {
            fingerprint,
            client_timestamp,
            accepted_at: now,
        };
        append_record(config, inner, &WalRecord::AdminReplay(record.clone()))?;
        self.seen.insert(fingerprint);
        self.order.push_back(record);
        Ok(())
    }

    /// Empties the window, as a successful root rotation does.
    pub(crate) fn clear(&mut self) {
        self.seen.clear();
        self.order.clear();
    }

    /// The admitted records in admission order, as a snapshot persists them.
    pub(crate) fn records(&self) -> &VecDeque<AdminReplayRecord> {
        &self.order
    }
}
