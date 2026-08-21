//! Temporary-key lifetimes, scheduled on the timing wheel.
//!
//! ```text
//!   issue      schedule(expires_at)  ── slots[i] Active, lease live
//!                     |
//!                     v  deadline arrives, or a revoke fires the timer early
//!   retire     lease cancelled, slots[i] Expired, tombstoned_at recorded,
//!              a reap timer scheduled for +TOMBSTONE_RETENTION
//!                     |  <- a client presenting the dead credential is told
//!                     |     "expired", not the "unknown key" it would get from
//!                     v     an already-recycled row
//!   reap       slots[i] Free (generation kept), cold metadata and any high-slot
//!              row removed
//! ```
//!
//! Both stages are timers, so nothing sweeps and no queue has to stay in step
//! with the wheel. Every way a key can end runs the same callback: a deadline
//! arriving runs it on schedule, [`Timer::fire`] runs it early for a revoke or a
//! GC, and dropping the wheel runs it for a rotation or shutdown.
//!
//! `timers` maps each key to a `Weak` handle on its current timer, which is what
//! keeps key identity out of the wheel. Renewing upgrades the handle and
//! schedules the same timer at the later deadline: the earlier placement still
//! drains, but it is no longer the last reference, so nothing fires. Because the
//! map holds only `Weak` references, an entry whose timer has fired costs nothing
//! but a stale key, cleared by the callback itself.
//!
//! The callbacks hold a `Weak<AuthStateInner>`, so they neither keep the state
//! alive nor touch it after a runtime has shut down.

use super::*;

/// A key's two scheduled stages. Both are `Weak`, so a stage that has already
/// run costs nothing but a stale map key.
#[derive(Default)]
struct Stages {
    retire: Weak<Timer>,
    reap: Weak<Timer>,
}

pub(super) struct Leases {
    inner: Weak<AuthStateInner>,
    wheel: TimingWheel,
    /// The wall-clock second the wheel's current tick corresponds to. The wheel
    /// itself only counts ticks, so this is where absolute deadlines are turned
    /// into the relative delays it takes.
    now: u64,
    /// Each key's stages, so a renew or an early end can reach them without the
    /// wheel knowing what a key is.
    stages: HashMap<KeyId, Stages>,
}

impl Leases {
    /// Rebuilds a loaded state's schedule: live keys wait for their expiry, and
    /// keys that were already dead wait out the rest of their retention.
    pub(super) fn restored(inner: &Arc<AuthStateInner>, now: u64) -> Self {
        let mut leases = Self {
            inner: Arc::downgrade(inner),
            wheel: new_wheel(),
            now,
            stages: HashMap::new(),
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
            leases.watch(key_id, lease);
        }
        for key_id in dead {
            let tombstoned_at = inner
                .cold()
                .get(&key_id)
                .map(|cold| cold.tombstoned_at)
                .filter(|at| *at != 0)
                .unwrap_or(now);
            leases.schedule_reap(key_id, retention_ends(tombstoned_at));
        }
        leases
    }

    /// Takes over a newly issued key: records its description and schedules both
    /// stages of its teardown.
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
        self.watch(lease.key_id(), lease.clone());
    }

    /// Hands a key's remaining life to a replacement lease, for a renewal whose
    /// original lease had already been cancelled.
    pub(super) fn adopt(&mut self, lease: &Arc<AuthLease>) {
        self.watch(lease.key_id(), lease.clone());
    }

    /// Moves a renewed key to its new expiry, and its reap along with it. Returns
    /// `false` for a key with no live stages, as for a high slot.
    ///
    /// Each timer is scheduled a second time rather than moved: its earlier
    /// placement drains on the old deadline but is no longer the last reference,
    /// so it fires nothing.
    pub(super) fn renew(&mut self, key_id: KeyId, expires_at: u64) -> bool {
        let Some(stages) = self.stages.get(&key_id) else {
            return false;
        };
        let (Some(retire), Some(reap)) = (stages.retire.upgrade(), stages.reap.upgrade()) else {
            return false;
        };
        self.schedule_at(expires_at, retire);
        self.schedule_at(retention_ends(expires_at), reap);
        true
    }

    /// Retires a key now rather than at its expiry, leaving its reap on schedule.
    /// This is what a revoke needs: the credential stops working immediately, but
    /// the row is held long enough to report *why*.
    pub(super) fn retire_now(&mut self, key_id: KeyId) {
        if let Some(retire) = self.stage(key_id, |stages| &stages.retire) {
            retire.fire();
        }
    }

    /// Ends a key outright, running both stages. Skips the retention wait, so it
    /// is for a caller that wants the row back now.
    pub(super) fn end(&mut self, key_id: KeyId) {
        self.retire_now(key_id);
        if let Some(reap) = self.stage(key_id, |stages| &stages.reap) {
            reap.fire();
        }
        self.stages.remove(&key_id);
    }

    /// Runs every callback whose deadline has passed.
    pub(super) fn tick(&mut self, now: u64) {
        // A jump longer than anything a key can be scheduled for means every
        // timer is due, so the schedule is dropped wholesale instead of ticked up
        // to. That keeps a corrected hardware clock from spinning for hours.
        //
        // A clock stepping backwards is ignored: buckets are indexed relative to
        // the wheel's `now`, so re-filing against an earlier one would place
        // entries in slots it has already drained.
        if now.saturating_sub(self.now) > self.wheel.max_delay() {
            self.drop_schedule(now);
            return;
        }
        while self.now < now {
            self.now += 1;
            self.wheel.tick();
        }
    }

    /// Ends every key at once, for a root rotation or state reset. Dropping the
    /// wheel releases the last reference to every timer, so no row, lease, or
    /// metadata entry survives it.
    pub(super) fn wipe(&mut self, now: u64) {
        // Rotation is the one reason a callback cannot infer, so it is recorded
        // before the drop; `record_cancel` keeps the first reason.
        if let Some(inner) = self.inner.upgrade() {
            for lease in inner.slots().iter().filter_map(|slot| slot.lease.upgrade()) {
                lease.cancel_rotated();
            }
        }
        self.drop_schedule(now);
    }

    /// Ends every key that is dead or past its deadline, skipping the retention
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

    /// Replaces the whole schedule, running every callback the old one held.
    fn drop_schedule(&mut self, now: u64) {
        self.stages.clear();
        self.now = now;
        self.wheel = new_wheel();
    }

    /// Schedules `timer` for an absolute second, as the delay from now the wheel
    /// works in. A deadline already past releases the timer at once.
    fn schedule_at(&mut self, deadline: u64, timer: Arc<Timer>) {
        self.wheel
            .schedule(deadline.saturating_sub(self.now), timer);
    }

    /// Upgrades one of a key's stages, forgetting the key once both have run.
    fn stage(
        &mut self,
        key_id: KeyId,
        which: impl Fn(&Stages) -> &Weak<Timer>,
    ) -> Option<Arc<Timer>> {
        let stages = self.stages.get(&key_id)?;
        let timer = which(stages).upgrade();
        if stages.retire.strong_count() == 0 && stages.reap.strong_count() == 0 {
            self.stages.remove(&key_id);
        }
        timer
    }

    /// Schedules both stages of a live key: retirement at its lease's expiry, and
    /// the reap a retention window later.
    ///
    /// WHY the reap timer owns the lease rather than the retire timer: the slot
    /// table holds only a `Weak`, so this is the reference that lets a request
    /// during the retention window read *why* the key died instead of finding a
    /// vanished lease. Firing a timer consumes its callback, so an `Arc` held by
    /// the retire stage would be released the moment that stage ran.
    fn watch(&mut self, key_id: KeyId, lease: Arc<AuthLease>) {
        let inner = self.inner.clone();
        let expires_at = lease.expires_at();
        let retire_lease = lease.clone();
        let retire = Timer::new(move || {
            if let Some(inner) = inner.upgrade() {
                retire(&inner, key_id, &retire_lease);
            }
        });
        let reap = self.reap_timer(key_id, Some(lease));
        self.stages.insert(
            key_id,
            Stages {
                retire: Arc::downgrade(&retire),
                reap: Arc::downgrade(&reap),
            },
        );
        self.schedule_at(expires_at, retire);
        self.schedule_at(retention_ends(expires_at), reap);
    }

    /// Schedules only the reap, for a key that is already dead.
    fn schedule_reap(&mut self, key_id: KeyId, deadline: u64) {
        let reap = self.reap_timer(key_id, None);
        self.stages.insert(
            key_id,
            Stages {
                reap: Arc::downgrade(&reap),
                ..Stages::default()
            },
        );
        self.schedule_at(deadline, reap);
    }

    /// Builds the reap stage. `lease` is the key's live lease when there is one,
    /// kept alive by this timer until the row is recycled.
    fn reap_timer(&self, key_id: KeyId, lease: Option<Arc<AuthLease>>) -> Arc<Timer> {
        let inner = self.inner.clone();
        Timer::new(move || {
            drop(lease);
            if let Some(inner) = inner.upgrade() {
                reap(&inner, key_id);
            }
        })
    }
}

fn retention_ends(tombstoned_at: u64) -> u64 {
    tombstoned_at.saturating_add(TOMBSTONE_RETENTION.as_secs())
}

/// Ends a key's active stage: cancels the lease, marks the row dead, records when
/// its retention starts, and schedules the reap that frees the row.
fn retire(inner: &Arc<AuthStateInner>, key_id: KeyId, lease: &Arc<AuthLease>) {
    // WHY expiry is the fallback reason: a key ended for any other reason was
    // already cancelled by the code that knew that reason, and `record_cancel`
    // keeps the first one, so this cannot mislabel it.
    lease.cancel_expired();
    let mut slots = inner.slots_mut();
    let Some(slot) = slots.get_mut(key_id.slot().as_index()) else {
        return;
    };
    // Already dead, or the row moved on: a revoke marked it and recorded its
    // tombstone time, and the reap is already scheduled either way.
    if !slot.holds(key_id) || slot.state != SlotState::Active {
        return;
    }
    slot.state = SlotState::Expired;
    let tombstoned_at = slot.expires_at;
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
}

/// Frees a dead key's row and forgets it.
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

/// The wheel every schedule uses: wide enough for the longest lifetime a key can
/// have, at 64 buckets per level.
fn new_wheel() -> TimingWheel {
    TimingWheel::new(MAX_SCHEDULABLE_DELAY.as_secs(), 64)
}
