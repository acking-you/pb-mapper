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

use pb_mapper_auth::KeyId;

/// Distinct (peer, key, reason) triples tracked before the overflow bucket.
pub(super) const MAX_TRACKED: usize = 4096;
/// Each triple emits this many lines per window, then counts the rest.
const EMITS_PER_WINDOW: u8 = 5;
const WINDOW_SECONDS: u64 = 60;
/// A full table drops triples idle this long before falling back to overflow.
const RETAIN_SECONDS: u64 = 2 * WINDOW_SECONDS;
const _: () = assert!(
    RETAIN_SECONDS >= WINDOW_SECONDS,
    "a triple is never dropped while its window still suppresses"
);

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
    pub(super) entries:
        std::collections::HashMap<(std::net::IpAddr, KeyId, String), FailureLogEntry>,
    pub(super) overflow: Option<FailureLogEntry>,
}

impl FailureLogLimiter {
    pub(super) fn record(
        &mut self,
        peer_ip: std::net::IpAddr,
        key_id: KeyId,
        reason: &str,
        now: u64,
    ) -> FailureLogDecision {
        let key = (peer_ip, key_id, reason.to_string());
        if !self.entries.contains_key(&key) && self.entries.len() >= MAX_TRACKED {
            self.entries
                .retain(|_, entry| now.saturating_sub(entry.window_started_at) < RETAIN_SECONDS);
            if self.entries.len() >= MAX_TRACKED {
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
    if now.saturating_sub(entry.window_started_at) >= WINDOW_SECONDS {
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
    if entry.emitted < EMITS_PER_WINDOW {
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
