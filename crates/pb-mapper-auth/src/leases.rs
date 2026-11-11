//! Temporary-key lifetimes, scheduled on the timing wheel.
//!
//! ```text
//!   issue      schedule(expires_at)  ── slots[i] Active, lease live
//!                     |
//!                     v  deadline arrives, or a revoke fires the timer early
//!   retire     lease cancelled, slots[i] Expired (or Revoked), tombstoned_at
//!              recorded, the reap due TOMBSTONE_RETENTION after it
//!                     |  <- a client presenting the dead credential is told
//!                     |     "expired", not the "unknown key" it would get from
//!                     v     an already-recycled row
//!   reap       slots[i] Free (generation kept), cold metadata and any high-slot
//!              row removed
//! ```
//!
//! Both stages are timers, so no queue has to stay in step with the wheel.
//! Every way a key can end runs the same callback: a deadline arriving runs it
//! on schedule, [`Timer::fire`] runs it early for a revoke or a GC, and dropping
//! the wheel runs it for a rotation or shutdown.
//!
//! `stages` maps each key to `Weak` handles on its two current timers, which is
//! what keeps key identity out of the wheel. Renewing moves the callbacks onto
//! fresh timers at the new deadline, sooner or later than the old one: the
//! earlier placements still drain, but the timers they hold have nothing left to
//! run. Because the map holds only `Weak` references, an entry whose timers have
//! both fired keeps nothing alive but two small allocations and a stale key; no
//! callback can reach the map, so `tick` sweeps such entries out every
//! `STAGE_SWEEP_INTERVAL`.
//!
//! The callbacks hold a `Weak<AuthStateInner>`, so they neither keep the state
//! alive nor touch it after a runtime has shut down.
//!
//! Deadlines are wall-clock seconds, and every key ends at its own whichever
//! way the clock steps. Forward, `tick` runs each second up to the new one, so
//! whatever the clock passed ends at once. Backward, the wheel stays where it
//! was: it only turns forward, and everything on it is due after its second,
//! which the clock reaches again first, so those keys still end on time
//! (turning the wheel back would end each of them the step's length early).
//! A deadline set meanwhile that falls before the wheel's second waits in
//! `behind`, which `tick` runs by the wall clock.
//!
//! ```text
//!   the clock steps back from S, the wheel's second, to W
//!
//!   W ─────────────────── S ─────────────────>  wall clock
//!   └ deadlines in (W, S] ┘└ deadlines after S ┘
//!     in `behind`, each run   on the wheel, which turns
//!     as the clock reaches it again once the clock passes S
//! ```

use super::*;

/// How often `tick` forgets the keys whose stages have all run.
const STAGE_SWEEP_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// A key's two scheduled stages. Both are `Weak`, so a stage that has already
/// run keeps nothing alive but its allocation.
#[derive(Default)]
struct Stages {
    retire: Weak<Timer>,
    reap: Weak<Timer>,
}

impl Stages {
    /// Whether either stage can still run.
    fn pending(&self) -> bool {
        self.retire.strong_count() > 0 || self.reap.strong_count() > 0
    }
}

pub(super) struct Leases {
    inner: Weak<AuthStateInner>,
    wheel: TimingWheel,
    /// The wall-clock second the wheel's current tick corresponds to. The wheel
    /// itself only counts ticks, so this is where absolute deadlines are turned
    /// into the relative delays it takes. It never goes back; the clock can.
    now: u64,
    /// Timers due at or before `now`, by their deadline, for a clock that has
    /// stepped back behind it. Empty unless it has.
    behind: BTreeMap<u64, Vec<Arc<Timer>>>,
    /// Each key's stages, so a renew or an early end can reach them without the
    /// wheel knowing what a key is.
    stages: HashMap<KeyId, Stages>,
    /// When `tick` next sweeps `stages` for keys with nothing left to run.
    next_sweep_at: u64,
}

impl Leases {
    /// Rebuilds a loaded state's schedule at wall-clock second `now`: `live`
    /// keys, with the leases the slot table points at, wait for their expiry,
    /// and keys that were already dead wait out the rest of their retention.
    pub(super) fn restored(inner: &Arc<AuthStateInner>, now: u64, live: LiveLeases) -> Self {
        let mut leases = Self {
            inner: Arc::downgrade(inner),
            wheel: new_wheel(),
            now,
            behind: BTreeMap::new(),
            stages: HashMap::new(),
            next_sweep_at: next_sweep(now),
        };
        // Each dead key with its tombstone time where it carries one itself: a
        // row above the table does, in its entry; the table's rows keep theirs
        // in `cold`.
        let mut dead = Vec::new();
        for (index, slot) in inner.slots().iter().enumerate() {
            match slot.state {
                SlotState::Expired | SlotState::Revoked => {
                    dead.push((
                        KeyId::new(slot.generation, SlotIndex::from_index(index)),
                        None,
                    ));
                }
                SlotState::Free | SlotState::Active => {}
            }
        }
        dead.extend(
            inner
                .high()
                .iter()
                .filter(|entry| entry.state != SlotState::Active)
                .map(|entry| (entry.key_id, entry.tombstoned_at)),
        );
        for (key_id, lease) in live {
            leases.watch(key_id, lease, now);
        }
        for (key_id, tombstoned_at) in dead {
            let tombstoned_at = tombstoned_at
                .or_else(|| inner.cold().get(&key_id).map(|cold| cold.tombstoned_at))
                .filter(|at| *at != 0)
                .unwrap_or(now);
            leases.schedule_reap(key_id, retention_ends(tombstoned_at), now);
        }
        leases
    }

    /// Takes over a newly issued key: records its description and schedules both
    /// stages of its teardown. `issued_at` is the wall-clock second its
    /// deadline was counted from.
    pub(super) fn issue(
        &mut self,
        lease: &Arc<AuthLease>,
        issued_at: u64,
        label: Option<String>,
        namespace: Option<u64>,
    ) {
        let Some(inner) = self.inner.upgrade() else {
            return;
        };
        inner.cold_mut().insert(
            lease.key_id(),
            ColdMetadata {
                issued_at,
                label,
                namespace,
                tombstoned_at: 0,
            },
        );
        self.watch(lease.key_id(), lease.clone(), issued_at);
    }

    /// Hands a key's remaining life to a replacement lease, for a renewal at
    /// wall-clock second `now` whose original lease had already been cancelled.
    ///
    /// The stages it replaces are disarmed first. They identify the key rather than
    /// the lease, so at the old deadline they would otherwise end the key this renewal
    /// just extended.
    pub(super) fn adopt(&mut self, lease: &Arc<AuthLease>, now: u64) {
        if let Some(stages) = self.stages.remove(&lease.key_id()) {
            for timer in [stages.retire, stages.reap]
                .iter()
                .filter_map(Weak::upgrade)
            {
                timer.disarm();
            }
        }
        self.watch(lease.key_id(), lease.clone(), now);
    }

    /// Moves a key renewed at wall-clock second `now` to its new expiry, sooner
    /// or later than the old one, and its reap along with it. Returns `false`
    /// for a key with no live stages, as for a high slot.
    ///
    /// See [`Timer::supersede`] for why this cannot just schedule each timer a
    /// second time: a timer fires with its last placement, so a shortened key would
    /// keep running to its old deadline.
    pub(super) fn renew(&mut self, key_id: KeyId, expires_at: u64, now: u64) -> bool {
        let Some(stages) = self.stages.get_mut(&key_id) else {
            return false;
        };
        let (Some(retire), Some(reap)) = (stages.retire.upgrade(), stages.reap.upgrade()) else {
            return false;
        };
        let retire = retire.supersede();
        let reap = reap.supersede();
        stages.retire = Arc::downgrade(&retire);
        stages.reap = Arc::downgrade(&reap);
        self.schedule_at(now, expires_at, retire);
        self.schedule_at(now, retention_ends(expires_at), reap);
        true
    }

    /// Retires a key now rather than at its expiry, leaving its reap on schedule.
    pub(super) fn retire_now(&mut self, key_id: KeyId) {
        if let Some(retire) = self.stage(key_id, |stages| &stages.retire) {
            retire.fire();
        }
    }

    /// Ends a key its revoke at wall-clock second `at` has already marked: the
    /// credential stops working now, and the row is held for a retention window
    /// from `at`, long enough to report *why*, rather than until its old
    /// deadline.
    pub(super) fn revoke(&mut self, key_id: KeyId, at: u64) {
        self.retire_now(key_id);
        if let Some(inner) = self.inner.upgrade() {
            inner
                .cold_mut()
                .entry(key_id)
                .and_modify(|cold| cold.tombstoned_at = at);
        }
        let Some(stages) = self.stages.get_mut(&key_id) else {
            return;
        };
        let Some(reap) = stages.reap.upgrade() else {
            return;
        };
        let reap = reap.supersede();
        stages.reap = Arc::downgrade(&reap);
        self.schedule_at(at, retention_ends(at), reap);
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

    /// Runs every callback whose deadline the wall clock's second `now` has
    /// reached.
    pub(super) fn tick(&mut self, now: u64) {
        // A jump longer than anything a key can be scheduled for means every
        // timer is due, so the schedule is dropped wholesale instead of ticked up
        // to. That keeps a corrected hardware clock from spinning for hours.
        if now.saturating_sub(self.now) > self.wheel.max_delay() {
            self.drop_schedule(now);
            return;
        }
        // These lie at or before the wheel's second, so they come due before
        // anything on it. A clock behind that second turns the wheel no
        // further; see the module docs.
        while let Some(due) = self.behind.first_entry()
            && *due.key() <= now
        {
            drop(due.remove());
        }
        while self.now < now {
            self.now += 1;
            self.wheel.tick();
        }
        if self.now >= self.next_sweep_at {
            // A key that ended on schedule leaves its entry behind: its
            // callbacks cannot reach this map.
            self.stages.retain(|_, stages| stages.pending());
            self.next_sweep_at = next_sweep(self.now);
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

    /// How many keys the schedule still has an entry for.
    #[cfg(test)]
    pub(super) fn tracked_keys(&self) -> usize {
        self.stages.len()
    }

    /// Replaces the whole schedule, running every callback the old one held.
    /// The new one starts at the wall clock's second `now`, which may be
    /// behind the old one's: nothing is on it yet.
    fn drop_schedule(&mut self, now: u64) {
        self.stages.clear();
        self.now = now;
        self.next_sweep_at = next_sweep(now);
        self.wheel = new_wheel();
        drop(std::mem::take(&mut self.behind));
    }

    /// Schedules `timer` for the absolute second `deadline`, the wall clock
    /// reading `now`. A deadline the wheel has yet to reach goes on it, as the
    /// delay from its second; one the clock has already reached releases the
    /// timer at once; and one in between, which only a clock stepped back
    /// behind the wheel leaves, waits in `behind`.
    fn schedule_at(&mut self, now: u64, deadline: u64, timer: Arc<Timer>) {
        if deadline > self.now {
            self.wheel.schedule(deadline - self.now, timer);
        } else if deadline > now {
            self.behind.entry(deadline).or_default().push(timer);
        }
        // Otherwise dropping `timer` here fires it, if this was the last
        // reference.
    }

    /// Upgrades one of a key's stages, forgetting the key once both have run.
    fn stage(
        &mut self,
        key_id: KeyId,
        which: impl Fn(&Stages) -> &Weak<Timer>,
    ) -> Option<Arc<Timer>> {
        let stages = self.stages.get(&key_id)?;
        let timer = which(stages).upgrade();
        if !stages.pending() {
            self.stages.remove(&key_id);
        }
        timer
    }

    /// Schedules both stages of a live key, the wall clock reading `now`:
    /// retirement at its lease's expiry, and the reap a retention window later.
    ///
    /// WHY the reap timer owns the lease rather than the retire timer: the slot
    /// table holds only a `Weak`, so this is the reference that lets a request
    /// during the retention window read *why* the key died instead of finding a
    /// vanished lease. Firing a timer consumes its callback, so an `Arc` held by
    /// the retire stage would be released the moment that stage ran.
    fn watch(&mut self, key_id: KeyId, lease: Arc<AuthLease>, now: u64) {
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
        self.schedule_at(now, expires_at, retire);
        self.schedule_at(now, retention_ends(expires_at), reap);
    }

    /// Schedules only the reap, for a key that is already dead.
    fn schedule_reap(&mut self, key_id: KeyId, deadline: u64, now: u64) {
        let reap = self.reap_timer(key_id, None);
        self.stages.insert(
            key_id,
            Stages {
                reap: Arc::downgrade(&reap),
                ..Stages::default()
            },
        );
        self.schedule_at(now, deadline, reap);
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

fn next_sweep(now: u64) -> u64 {
    now.saturating_add(STAGE_SWEEP_INTERVAL.as_secs())
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
    // Already dead, or the row moved on: a revoke marks the row before it
    // retires the key, and the reap is already scheduled either way.
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
    // A rotation or a stopping actor ends every key through here too, having
    // cancelled it first; only a key that ran out is reported as expired.
    if lease.ran_out() {
        tracing::info!(
            event = "temporary_key_expired",
            auth_stage = "expiry",
            key_id = key_id.as_u64(),
            expires_at = tombstoned_at,
            "temporary key expired and active work was cancelled"
        );
    }
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

/// Buckets per level of the timing wheel.
const WHEEL_RADIX: u64 = 64;
const _: () = assert!(WHEEL_RADIX > 1, "a wheel level needs at least two buckets");

/// The wheel every schedule uses: wide enough for the longest lifetime a key can
/// have, at `WHEEL_RADIX` buckets per level.
fn new_wheel() -> TimingWheel {
    TimingWheel::new(MAX_SCHEDULABLE_DELAY.as_secs(), WHEEL_RADIX)
}
