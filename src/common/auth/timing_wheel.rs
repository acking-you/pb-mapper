//! Hierarchical expiry scheduler for temporary credential leases.
//!
//! ```text
//! lease(expires_at) -> level/slot bucket -> one-second actor tick -> expired leases
//!          renew ----> version bump ------^ stale bucket entries are ignored
//!                                       |
//! large clock jump -> bounded bucket scan + rebuild (never second-by-second catch-up)
//! overdue insert --> immediate-due queue --> next advance, without a wheel revolution
//! reset/rotation -> cancel every wheel-owned lease -> clear all buckets
//! ```
//!
//! The wheel's current-owner map holds the strong `Arc<AuthLease>` for each key.
//! Bucket entries are `Weak`, so a renew replaces the previous owner instead of
//! accumulating day-long stale strong references. Request-facing structures also
//! retain only `Weak` references.

use super::*;

const MAX_INCREMENTAL_ADVANCE_SECONDS: u64 = 256;

struct WheelEntry {
    lease: Weak<AuthLease>,
    version: u64,
}

pub(super) struct TimingWheel {
    now: u64,
    owners: HashMap<u64, Arc<AuthLease>>,
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
            owners: HashMap::new(),
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

    pub(super) fn release(&mut self, key_id: u64) {
        self.owners.remove(&key_id);
    }

    pub(super) fn insert_with_version(&mut self, lease: Arc<AuthLease>, version: u64) {
        self.owners.insert(lease.key_id(), lease.clone());
        let expires_at = lease.expires_at();
        let delta = expires_at.saturating_sub(self.now);
        let entry = WheelEntry {
            lease: Arc::downgrade(&lease),
            version,
        };
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
                let Some(lease) = live_lease(&entry) else {
                    continue;
                };
                if lease.expires_at() <= self.now {
                    self.owners.remove(&lease.key_id());
                    due.push(lease);
                } else {
                    self.insert_with_version(lease, entry.version);
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
            let Some(lease) = live_lease(&entry) else {
                continue;
            };
            if lease.expires_at() <= target {
                self.owners.remove(&lease.key_id());
                due.push(lease);
            } else {
                self.insert_with_version(lease, entry.version);
            }
        }
        due
    }

    fn take_immediate_due(&mut self, target: u64) -> Vec<Arc<AuthLease>> {
        let mut due = Vec::new();
        for entry in std::mem::take(&mut self.immediate_due) {
            let Some(lease) = live_lease(&entry) else {
                continue;
            };
            if lease.expires_at() <= target {
                self.owners.remove(&lease.key_id());
                due.push(lease);
            } else {
                self.insert_with_version(lease, entry.version);
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
            if let Some(lease) = live_lease(&entry) {
                self.insert_with_version(lease, entry.version);
            }
        }
    }

    pub(super) fn clear(&mut self, now: u64) {
        let mut entries = std::mem::take(&mut self.immediate_due);
        take_all_entries(&mut self.level0, &mut entries);
        take_all_entries(&mut self.level1, &mut entries);
        take_all_entries(&mut self.level2, &mut entries);
        take_all_entries(&mut self.level3, &mut entries);
        for lease in self.owners.values() {
            lease.cancellation.cancel();
        }
        *self = Self::new(now);
    }
}

fn live_lease(entry: &WheelEntry) -> Option<Arc<AuthLease>> {
    let lease = entry.lease.upgrade()?;
    (entry.version == lease.wheel_version.load(Ordering::Acquire)).then_some(lease)
}

fn empty_buckets(count: usize) -> Vec<Vec<WheelEntry>> {
    std::iter::repeat_with(Vec::new).take(count).collect()
}

fn take_all_entries(buckets: &mut [Vec<WheelEntry>], entries: &mut Vec<WheelEntry>) {
    for bucket in buckets {
        entries.append(bucket);
    }
}
