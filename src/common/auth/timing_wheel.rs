//! Hierarchical expiry scheduler for temporary credential leases.
//!
//! ```text
//! lease(expires_at) -> level/slot bucket -> one-second actor tick -> expired leases
//!          renew ----> version bump ------^ stale bucket entries are ignored
//!                                       |
//! large clock jump -> bounded bucket scan + rebuild (never second-by-second catch-up)
//! overdue insert --> immediate-due queue --> next advance, without a wheel revolution
//! ```
//!
//! The wheel owns strong `Arc<AuthLease>` references. Request-facing structures retain
//! only `Weak` references, so expiry, revoke, reset, and root rotation have one clear
//! cancellation owner without keeping dead credentials alive indefinitely.

use super::*;

const MAX_INCREMENTAL_ADVANCE_SECONDS: u64 = 256;

struct WheelEntry {
    lease: Arc<AuthLease>,
    version: u64,
}

pub(super) struct TimingWheel {
    now: u64,
    immediate_due: Vec<WheelEntry>,
    level0: Vec<Vec<WheelEntry>>,
    level1: Vec<Vec<WheelEntry>>,
    level2: Vec<Vec<WheelEntry>>,
    level3: Vec<Vec<WheelEntry>>,
}

impl TimingWheel {
    pub(super) fn new(now: u64) -> Self {
        Self {
            now,
            immediate_due: Vec::new(),
            level0: empty_buckets(256),
            level1: empty_buckets(64),
            level2: empty_buckets(64),
            level3: empty_buckets(64),
        }
    }

    pub(super) fn insert(&mut self, lease: Arc<AuthLease>) {
        let version = lease.wheel_version.load(Ordering::Acquire);
        self.insert_with_version(lease, version);
    }

    pub(super) fn insert_with_version(&mut self, lease: Arc<AuthLease>, version: u64) {
        let expires_at = lease.expires_at();
        let delta = expires_at.saturating_sub(self.now);
        let entry = WheelEntry { lease, version };
        if expires_at <= self.now {
            self.immediate_due.push(entry);
        } else if delta < 1 << 8 {
            self.level0[(expires_at & 0xff) as usize].push(entry);
        } else if delta < 1 << 14 {
            self.level1[((expires_at >> 8) & 0x3f) as usize].push(entry);
        } else if delta < 1 << 20 {
            self.level2[((expires_at >> 14) & 0x3f) as usize].push(entry);
        } else {
            self.level3[((expires_at >> 20) & 0x3f) as usize].push(entry);
        }
    }

    pub(super) fn advance(&mut self, target: u64) -> Vec<Arc<AuthLease>> {
        if target.saturating_sub(self.now) > MAX_INCREMENTAL_ADVANCE_SECONDS {
            return self.fast_forward(target);
        }

        let mut due = self.take_immediate_due(target);
        while self.now < target {
            self.now = self.now.saturating_add(1);
            if self.now & 0xff == 0 {
                self.cascade(1);
                if (self.now >> 8) & 0x3f == 0 {
                    self.cascade(2);
                    if (self.now >> 14) & 0x3f == 0 {
                        self.cascade(3);
                    }
                }
            }
            due.extend(self.take_immediate_due(self.now));
            let index = (self.now & 0xff) as usize;
            for entry in std::mem::take(&mut self.level0[index]) {
                if entry.version == entry.lease.wheel_version.load(Ordering::Acquire) {
                    if entry.lease.expires_at() <= self.now {
                        due.push(entry.lease);
                    } else {
                        self.insert(entry.lease);
                    }
                }
            }
        }
        due
    }

    fn fast_forward(&mut self, target: u64) -> Vec<Arc<AuthLease>> {
        self.now = target;
        let mut entries = std::mem::take(&mut self.immediate_due);
        take_all_entries(&mut self.level0, &mut entries);
        take_all_entries(&mut self.level1, &mut entries);
        take_all_entries(&mut self.level2, &mut entries);
        take_all_entries(&mut self.level3, &mut entries);

        let mut due = Vec::new();
        for entry in entries {
            if entry.version != entry.lease.wheel_version.load(Ordering::Acquire) {
                continue;
            }
            if entry.lease.expires_at() <= target {
                due.push(entry.lease);
            } else {
                self.insert_with_version(entry.lease, entry.version);
            }
        }
        due
    }

    fn take_immediate_due(&mut self, target: u64) -> Vec<Arc<AuthLease>> {
        let mut due = Vec::new();
        for entry in std::mem::take(&mut self.immediate_due) {
            if entry.version != entry.lease.wheel_version.load(Ordering::Acquire) {
                continue;
            }
            if entry.lease.expires_at() <= target {
                due.push(entry.lease);
            } else {
                self.insert_with_version(entry.lease, entry.version);
            }
        }
        due
    }

    fn cascade(&mut self, level: u8) {
        let entries = match level {
            1 => {
                let index = ((self.now >> 8) & 0x3f) as usize;
                std::mem::take(&mut self.level1[index])
            }
            2 => {
                let index = ((self.now >> 14) & 0x3f) as usize;
                std::mem::take(&mut self.level2[index])
            }
            3 => {
                let index = ((self.now >> 20) & 0x3f) as usize;
                std::mem::take(&mut self.level3[index])
            }
            _ => Vec::new(),
        };
        for entry in entries {
            if entry.version == entry.lease.wheel_version.load(Ordering::Acquire) {
                self.insert_with_version(entry.lease, entry.version);
            }
        }
    }

    pub(super) fn clear(&mut self, now: u64) {
        *self = Self::new(now);
    }
}

fn empty_buckets(count: usize) -> Vec<Vec<WheelEntry>> {
    std::iter::repeat_with(Vec::new).take(count).collect()
}

fn take_all_entries(buckets: &mut [Vec<WheelEntry>], entries: &mut Vec<WheelEntry>) {
    for bucket in buckets {
        entries.append(bucket);
    }
}
