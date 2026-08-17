//! Cardinality-bounded suppression for repeated authentication failure logs.
//!
//! ```text
//! (peer IP, key id, reason) -> per-window counter -> emit first / suppress repeats
//!                too many distinct keys ---------> shared overflow bucket
//! ```
//!
//! This limits log amplification from the public relay port without changing protocol
//! decisions: every authentication failure is still rejected, only duplicate logging
//! is coalesced.

#[derive(Clone, Copy, Debug)]
pub struct FailureLogDecision {
    pub emit: bool,
    pub suppressed: u64,
}

pub(super) struct FailureLogEntry {
    window_started_at: u64,
    emitted: u8,
    suppressed: u64,
}

#[derive(Default)]
pub(super) struct FailureLogLimiter {
    pub(super) entries: std::collections::HashMap<(std::net::IpAddr, u64, String), FailureLogEntry>,
    pub(super) overflow: Option<FailureLogEntry>,
}

impl FailureLogLimiter {
    pub(super) fn record(
        &mut self,
        peer_ip: std::net::IpAddr,
        key_id: u64,
        reason: &str,
        now: u64,
    ) -> FailureLogDecision {
        let key = (peer_ip, key_id, reason.to_string());
        if !self.entries.contains_key(&key) && self.entries.len() >= 4096 {
            self.entries
                .retain(|_, entry| now.saturating_sub(entry.window_started_at) < 120);
            if self.entries.len() >= 4096 {
                let entry = self.overflow.get_or_insert(FailureLogEntry {
                    window_started_at: now,
                    emitted: 0,
                    suppressed: 0,
                });
                return record_failure_entry(entry, now);
            }
        }
        let entry = self.entries.entry(key).or_insert(FailureLogEntry {
            window_started_at: now,
            emitted: 0,
            suppressed: 0,
        });
        record_failure_entry(entry, now)
    }
}

fn record_failure_entry(entry: &mut FailureLogEntry, now: u64) -> FailureLogDecision {
    if now.saturating_sub(entry.window_started_at) >= 60 {
        let suppressed = entry.suppressed;
        *entry = FailureLogEntry {
            window_started_at: now,
            emitted: 1,
            suppressed: 0,
        };
        return FailureLogDecision {
            emit: true,
            suppressed,
        };
    }
    if entry.emitted < 5 {
        entry.emitted += 1;
        FailureLogDecision {
            emit: true,
            suppressed: 0,
        }
    } else {
        entry.suppressed = entry.suppressed.saturating_add(1);
        FailureLogDecision {
            emit: false,
            suppressed: 0,
        }
    }
}
