//! Temporary-key lifetimes, expressed as one self-advancing cleanup callback per
//! key.
//!
//! ```text
//!   issue     schedule(expires_at) ── slots[i] Active, lease live
//!                    |
//!                    v  phase 1: deadline reached, or the entry is cancelled
//!   retire    lease cancelled, slots[i] Expired, tombstoned_at recorded
//!                    |
//!                    v  returns expires_at + TOMBSTONE_RETENTION
//!             (waiting)  <- a client presenting the dead credential is told
//!                    |      "expired", not the "unknown key" it would get from
//!                    v      an already-recycled row
//!   reap      phase 2: slots[i] Free (generation kept), cold metadata and any
//!                     high-slot row removed. Nothing left; the entry is done.
//! ```
//!
//! One entry covers a key's whole life, so there is no tombstone queue and no
//! sweep to keep in step with the wheel. Every way a key can end runs the same
//! two phases: reaching a deadline runs them on schedule, and cancelling the
//! entry — for a revoke, a GC, a root rotation, or the wheel being dropped — runs
//! whichever phases remain immediately. No call site performs cleanup, which is
//! what keeps a forgotten call from stranding a lease past its row's reuse or
//! leaking a metadata entry per issued key.
//!
//! The callbacks hold a `Weak<AuthStateInner>`, so they neither keep the state
//! alive nor touch it after a runtime has shut down.

use super::*;

/// Whether a new entry tears down the one it replaces, or takes over its work.
enum Schedule {
    /// Any entry already held is torn down first.
    Fresh,
    /// The previous entry is discarded without running its phases, because this
    /// one now owes them.
    Supersede,
}

pub(super) struct Leases {
    inner: Weak<AuthStateInner>,
    wheel: TimingWheel,
}

impl Leases {
    /// Rebuilds a loaded state's schedule: live keys wait for their expiry, and
    /// keys that were already dead wait out the rest of their retention.
    pub(super) fn restored(inner: &Arc<AuthStateInner>, now: u64) -> Self {
        let mut leases = Self {
            inner: Arc::downgrade(inner),
            wheel: TimingWheel::new(now),
        };
        let mut live = Vec::new();
        let mut dead = Vec::new();
        for (index, slot) in inner.slots().iter().enumerate() {
            let key_id = KeyId::new(slot.generation, SlotIndex::from_index(index));
            match slot.state {
                SlotState::Active => live.extend(slot.lease.upgrade().map(|l| (key_id, l))),
                SlotState::Expired | SlotState::Revoked => dead.push(key_id),
                SlotState::Free => {}
            }
        }
        dead.extend(
            inner
                .high()
                .iter()
                .filter(|entry| entry.state != SlotState::Active)
                .map(|entry| entry.key_id),
        );
        for (key_id, lease) in live {
            leases.watch(key_id, lease, Schedule::Fresh);
        }
        for key_id in dead {
            let tombstoned_at = inner
                .cold()
                .get(&key_id)
                .map(|cold| cold.tombstoned_at)
                .filter(|at| *at != 0)
                .unwrap_or(now);
            leases.entomb(key_id, tombstoned_at);
        }
        leases
    }

    /// Takes over a newly issued key: records its description and schedules the
    /// retirement its expiry, or any earlier cancellation, will run.
    pub(super) fn issue(&mut self, lease: &Arc<AuthLease>, issued_at: u64, label: Option<String>) {
        let Some(inner) = self.inner.upgrade() else {
            return;
        };
        inner.cold_mut().insert(
            lease.key_id(),
            ColdMetadata {
                issued_at,
                label,
                tombstoned_at: 0,
            },
        );
        self.watch(lease.key_id(), lease.clone(), Schedule::Fresh);
    }

    /// Hands a key's remaining life to a replacement lease, for a renewal whose
    /// original lease had already been cancelled. The row stays alive, so the
    /// entry being replaced must not run its teardown.
    pub(super) fn adopt(&mut self, lease: &Arc<AuthLease>) {
        self.watch(lease.key_id(), lease.clone(), Schedule::Supersede);
    }

    /// Moves a renewed key to its new expiry. Returns `false` for a key the
    /// wheel is not watching, as for a high slot.
    pub(super) fn renew(&mut self, key_id: KeyId, expires_at: u64) -> bool {
        self.wheel.reschedule(key_id, expires_at)
    }

    /// Retires a key now rather than at its expiry, leaving its tombstone to run
    /// on schedule. This is what a revoke needs: the credential stops working
    /// immediately, but the row is still held long enough to report *why*.
    pub(super) fn retire_now(&mut self, key_id: KeyId) {
        self.wheel.advance_one_phase(key_id);
    }

    /// Ends a key outright, running whichever of its phases remain: an active
    /// key is retired and reaped, and a tombstoned one is reaped. Skips the
    /// retention wait, so it is for a caller that wants the row back now.
    pub(super) fn end(&mut self, key_id: KeyId) {
        self.wheel.cancel(key_id);
    }

    /// Runs every phase whose deadline has passed.
    pub(super) fn tick(&mut self, now: u64) {
        self.wheel.advance(now);
    }

    /// Ends every key at once, for a root rotation or state reset. Dropping the
    /// wheel runs all remaining phases, so no row, lease, or metadata entry
    /// survives it.
    pub(super) fn wipe(&mut self, now: u64) {
        // Rotation is the one reason a phase cannot infer, so it is recorded
        // before the drop; `record_cancel` keeps the first reason.
        if let Some(inner) = self.inner.upgrade() {
            for lease in inner.slots().iter().filter_map(|slot| slot.lease.upgrade()) {
                lease.cancel_rotated();
            }
        }
        self.wheel = TimingWheel::new(now);
    }

    /// Ends every key that is dead or past its deadline, skipping the tombstone
    /// wait. Returns how many keys were ended.
    pub(super) fn collect_garbage(&mut self, now: u64) -> u64 {
        let Some(inner) = self.inner.upgrade() else {
            return 0;
        };
        let mut due = inner
            .slots()
            .iter()
            .enumerate()
            .filter(|(_, slot)| slot.is_collectable(now))
            .map(|(index, slot)| KeyId::new(slot.generation, SlotIndex::from_index(index)))
            .collect::<Vec<_>>();
        due.extend(
            inner
                .high()
                .iter()
                .filter(|entry| entry.state != SlotState::Active || entry.expires_at <= now)
                .map(|entry| entry.key_id),
        );
        for key_id in &due {
            self.end(*key_id);
        }
        due.len() as u64
    }

    /// Schedules a live key's two phases, starting at its lease's expiry.
    ///
    /// The entry owns the strong `Arc<AuthLease>`, so the lease lives exactly as
    /// long as the wheel is watching it: request-facing structures hold only
    /// `Weak` references, and dropping the entry is what ends the lease.
    fn watch(&mut self, key_id: KeyId, lease: Arc<AuthLease>, schedule: Schedule) {
        let inner = self.inner.clone();
        let deadline = lease.expires_at();
        // WHY the closure keeps the lease across both phases: the slot table
        // holds only a `Weak`, so this is the reference that lets a request
        // during the tombstone read *why* the key died instead of finding a
        // vanished lease. It is released when the entry itself is dropped, after
        // the reap.
        let mut retired = false;
        let phase = move || {
            let inner = inner.upgrade()?;
            if std::mem::replace(&mut retired, true) {
                reap(&inner, key_id);
                return None;
            }
            Some(retire(&inner, key_id, Some(&lease)))
        };
        match schedule {
            Schedule::Fresh => self.wheel.schedule(key_id, deadline, phase),
            Schedule::Supersede => self.wheel.supersede(key_id, deadline, phase),
        }
    }

    /// Schedules only the reap phase, for a key that is already dead.
    fn entomb(&mut self, key_id: KeyId, tombstoned_at: u64) {
        let inner = self.inner.clone();
        self.wheel
            .schedule(key_id, retention_ends(tombstoned_at), move || {
                reap(&inner.upgrade()?, key_id);
                None
            });
    }
}

fn retention_ends(tombstoned_at: u64) -> u64 {
    tombstoned_at.saturating_add(TOMBSTONE_RETENTION.as_secs())
}

/// Phase 1: cancels the lease, marks the row dead, and records when its
/// retention starts. Returns the deadline of the reap that follows.
fn retire(inner: &Arc<AuthStateInner>, key_id: KeyId, lease: Option<&Arc<AuthLease>>) -> u64 {
    if let Some(lease) = lease {
        // WHY expiry is the fallback reason: a key ended for any other reason was
        // already cancelled by the code that knew that reason, and `record_cancel`
        // keeps the first one, so this cannot mislabel it.
        lease.cancel_expired();
    }
    let mut slots = inner.slots_mut();
    let tombstoned_at = match slots.get_mut(key_id.slot().as_index()) {
        Some(slot) if slot.holds(key_id) && slot.state == SlotState::Active => {
            slot.state = SlotState::Expired;
            slot.expires_at
        }
        // Already marked dead by a revoke, or the row moved on. Its retention
        // still has to be honoured, timed from whenever it was marked.
        _ => {
            drop(slots);
            let mut cold = inner.cold_mut();
            let tombstoned_at = match cold.get_mut(&key_id) {
                Some(cold) if cold.tombstoned_at != 0 => cold.tombstoned_at,
                Some(cold) => {
                    cold.tombstoned_at = unix_seconds();
                    cold.tombstoned_at
                }
                None => unix_seconds(),
            };
            return retention_ends(tombstoned_at);
        }
    };
    drop(slots);
    inner
        .cold_mut()
        .entry(key_id)
        .and_modify(|cold| cold.tombstoned_at = tombstoned_at);
    tracing::info!(
        event = "temporary_key_expired",
        auth_stage = "expiry",
        key_id = key_id.as_u64(),
        expires_at = tombstoned_at,
        "temporary key expired and active work was cancelled"
    );
    retention_ends(tombstoned_at)
}

/// Phase 2: frees the row and forgets the key.
fn reap(inner: &Arc<AuthStateInner>, key_id: KeyId) {
    let mut slots = inner.slots_mut();
    match slots.get_mut(key_id.slot().as_index()) {
        Some(slot) if slot.holds(key_id) => {
            slot.retire();
            drop(slots);
        }
        Some(_) => return,
        // Above the addressable table: the retained row is dropped outright,
        // since only its generation has to survive.
        None => {
            drop(slots);
            inner.high_mut().retain(|entry| entry.key_id != key_id);
        }
    }
    inner.cold_mut().remove(&key_id);
}
