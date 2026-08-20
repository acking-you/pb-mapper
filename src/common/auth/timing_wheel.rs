//! Hierarchical timer wheel whose entries are self-advancing cleanup callbacks.
//!
//! ```text
//! schedule(deadline, callback) -> one level/slot bucket
//!   deadline reached -> callback runs -> reschedules itself, or is done
//!   dropped early    -> callback runs to completion right there
//!
//! one-second tick -> drain level 0's current slot
//!                 -> every 64th tick, cascade level 1 into finer levels, and so on
//! ```
//!
//! The wheel knows nothing about what it schedules. An entry owns a callback that
//! performs one phase of some cleanup and returns when it next wants to run, or
//! `None` when finished, so a multi-stage teardown is written once at the point
//! the entry is created. Dropping an entry early runs every phase it has left, in
//! order, immediately — which is what lets cancelling one entry stand in for a
//! whole cleanup routine, and lets dropping the wheel tear down everything it
//! owns without a caller walking any of it.
//!
//! Each entry lives in exactly one bucket, and `positions` records which, so
//! rescheduling or cancelling one is a map lookup rather than a search. A tick
//! touches one slot per level that turns over, never the whole wheel, so cost
//! tracks the entries that actually cascade or fire.

use super::*;

/// Bits of a deadline that one level's slot field covers, so a level holds
/// `1 << 6` slots and each is 64 times coarser than the level below it.
const SLOT_BITS: u32 = 6;
const SLOTS: usize = 1 << SLOT_BITS;
const SLOT_MASK: u64 = SLOTS as u64 - 1;
/// Six 64-slot levels span `64^6` seconds, which keeps every deadline a
/// configured TTL can produce inside a level whose range really contains it.
const NUM_LEVELS: usize = 6;
const TOP_LEVEL: Level = NUM_LEVELS as Level - 1;

/// Which level of the hierarchy a bucket belongs to: `0..NUM_LEVELS`.
type Level = u8;

/// Which bucket within one level: `0..SLOTS`.
type Slot = u8;

/// One phase of a scheduled teardown: does its work and returns the deadline of
/// the phase after it, or `None` once nothing remains.
type Phase = Box<dyn FnMut() -> Option<u64> + Send>;

struct Entry {
    deadline: u64,
    phase: Phase,
    /// Set once a phase has returned `None`, so a completed entry's drop does
    /// not call into its callback again.
    finished: bool,
}

impl Entry {
    /// Runs the next phase and reports the deadline it wants, if any.
    fn fire(&mut self) -> Option<u64> {
        if self.finished {
            return None;
        }
        let next = (self.phase)();
        self.finished = next.is_none();
        next
    }
}

impl Drop for Entry {
    /// An entry let go of before its deadline still owes every phase it has
    /// left, so they all run here. This is why cancelling an entry and letting
    /// it expire have the same effect, only sooner.
    fn drop(&mut self) {
        while self.fire().is_some() {}
    }
}

/// Where a key's entry currently sits, so a reschedule or cancel does not have
/// to search the wheel.
#[derive(Clone, Copy)]
enum Position {
    /// Scheduled for a deadline the wheel had already passed.
    Overdue,
    Wheel {
        level: Level,
        slot: Slot,
    },
}

type Bucket = HashMap<KeyId, Entry>;

pub(super) struct TimingWheel {
    now: u64,
    positions: HashMap<KeyId, Position>,
    /// Entries whose deadline was already past when they were filed. Level 0's
    /// slot for `now` was drained this tick, so filing them there would delay
    /// them by a full revolution.
    overdue: Bucket,
    levels: [Vec<Bucket>; NUM_LEVELS],
}

impl TimingWheel {
    pub(super) fn new(now: u64) -> Self {
        Self {
            now,
            positions: HashMap::new(),
            overdue: Bucket::new(),
            levels: std::array::from_fn(|_| {
                std::iter::repeat_with(Bucket::new).take(SLOTS).collect()
            }),
        }
    }

    /// Schedules `phase` to run once `deadline` has passed. Any entry already
    /// held for `key_id` is dropped, which runs the phases it had left.
    pub(super) fn schedule(
        &mut self,
        key_id: KeyId,
        deadline: u64,
        phase: impl FnMut() -> Option<u64> + Send + 'static,
    ) {
        self.cancel(key_id);
        self.place(
            key_id,
            Entry {
                deadline,
                phase: Box::new(phase),
                finished: false,
            },
        );
    }

    /// Replaces the entry for `key_id`, discarding the previous one *without*
    /// running its remaining phases. Use this only when the new entry takes over
    /// the same cleanup, so nothing the old one owed is lost; otherwise
    /// [`Self::schedule`] is what you want.
    pub(super) fn supersede(
        &mut self,
        key_id: KeyId,
        deadline: u64,
        phase: impl FnMut() -> Option<u64> + Send + 'static,
    ) {
        if let Some(mut previous) = self.detach(key_id) {
            previous.finished = true;
        }
        self.place(
            key_id,
            Entry {
                deadline,
                phase: Box::new(phase),
                finished: false,
            },
        );
    }

    /// Moves an entry to a new deadline without running anything. Returns
    /// `false` when the wheel holds no entry for `key_id`.
    pub(super) fn reschedule(&mut self, key_id: KeyId, deadline: u64) -> bool {
        let Some(mut entry) = self.detach(key_id) else {
            return false;
        };
        entry.deadline = deadline;
        self.place(key_id, entry);
        true
    }

    /// Runs everything the entry for `key_id` still owes, now rather than at its
    /// deadline. A no-op when the wheel holds no entry for it.
    pub(super) fn cancel(&mut self, key_id: KeyId) {
        drop(self.detach(key_id));
    }

    /// Runs only the entry's next phase, then waits for the deadline that phase
    /// asked for. Use this where a stage has arrived early but the stages after
    /// it must still keep their own timing — a revoke ends a key's active phase
    /// without skipping the retention that follows it.
    pub(super) fn advance_one_phase(&mut self, key_id: KeyId) -> bool {
        let Some(mut entry) = self.detach(key_id) else {
            return false;
        };
        match entry.fire() {
            Some(next) => {
                entry.deadline = next;
                self.place(key_id, entry);
            }
            // Finished, so the drop below has nothing left to run.
            None => drop(entry),
        }
        true
    }

    /// Runs the wheel up to `target`, firing every entry whose deadline has
    /// passed and re-filing the phases they schedule next.
    pub(super) fn advance(&mut self, target: u64) {
        let overdue = std::mem::take(&mut self.overdue);
        self.settle(overdue, target);
        // A jump longer than the longest lifetime the config can produce means
        // every entry is already past its deadline, so the whole wheel can be
        // drained in one pass. Ticking through it instead would spin for hours
        // when a bad hardware clock is corrected forward by years.
        if target.saturating_sub(self.now) > MAX_SCHEDULABLE_DELAY.as_secs() {
            self.now = target;
            // A phase can schedule a successor that is also already overdue, so
            // keep draining until a pass leaves nothing due.
            loop {
                let due = self
                    .levels
                    .iter_mut()
                    .flat_map(|level| level.iter_mut())
                    .fold(Bucket::new(), |mut due, bucket| {
                        due.extend(std::mem::take(bucket));
                        due
                    });
                let overdue = std::mem::take(&mut self.overdue);
                if due.is_empty() && overdue.is_empty() {
                    return;
                }
                self.settle(due, target);
                self.settle(overdue, target);
            }
        }
        while self.now < target {
            self.now += 1;
            // Coarse to fine, so an entry cascading several levels down still
            // reaches level 0 in time to be drained by this same tick.
            for level in (1..=TOP_LEVEL).rev() {
                // A level turns over once every `slot_range` seconds, exactly
                // when `now` has no bits left below that level's slot field.
                if self.now & (slot_range(level) - 1) != 0 {
                    continue;
                }
                let entries = self.take_bucket(level, self.now);
                self.settle(entries, self.now);
            }
            let entries = self.take_bucket(0, self.now);
            self.settle(entries, self.now);
        }
        // A clock stepping backwards must not rewind the wheel: buckets are
        // indexed relative to `now`, so re-indexing against an earlier `now`
        // would file entries into slots the wheel has already drained.
        self.now = self.now.max(target);
    }

    /// Fires the entries due at `deadline` and re-files both the phases they
    /// schedule next and the entries that are not due yet.
    fn settle(&mut self, entries: Bucket, deadline: u64) {
        for (key_id, mut entry) in entries {
            if entry.deadline > deadline {
                self.place(key_id, entry);
                continue;
            }
            match entry.fire() {
                Some(next) => {
                    entry.deadline = next;
                    self.place(key_id, entry);
                }
                None => {
                    self.positions.remove(&key_id);
                }
            }
        }
    }

    fn place(&mut self, key_id: KeyId, entry: Entry) {
        let position = if entry.deadline <= self.now {
            self.overdue.insert(key_id, entry);
            Position::Overdue
        } else {
            let level = level_for(self.now, entry.deadline);
            let slot = slot_for(level, entry.deadline);
            self.bucket(level, slot).insert(key_id, entry);
            Position::Wheel { level, slot }
        };
        self.positions.insert(key_id, position);
    }

    fn detach(&mut self, key_id: KeyId) -> Option<Entry> {
        match self.positions.remove(&key_id)? {
            Position::Overdue => self.overdue.remove(&key_id),
            Position::Wheel { level, slot } => self.bucket(level, slot).remove(&key_id),
        }
    }

    fn bucket(&mut self, level: Level, slot: Slot) -> &mut Bucket {
        &mut self.levels[level as usize][slot as usize]
    }

    /// Empties the bucket that `when` falls in at `level`.
    fn take_bucket(&mut self, level: Level, when: u64) -> Bucket {
        let slot = slot_for(level, when);
        std::mem::take(self.bucket(level, slot))
    }

    #[cfg(test)]
    pub(super) fn holds(&self, key_id: KeyId) -> bool {
        self.positions.contains_key(&key_id)
    }
}

/// Seconds covered by one of `level`'s slots.
fn slot_range(level: Level) -> u64 {
    1 << (SLOT_BITS * level as u32)
}

fn slot_for(level: Level, when: u64) -> Slot {
    ((when >> (SLOT_BITS * level as u32)) & SLOT_MASK) as Slot
}

/// Finest level able to hold `deadline`: the one whose slot field covers the
/// highest bit in which `now` and `deadline` differ. A deadline past the top
/// level is clamped into it and cannot fire early, because a drained entry is
/// only fired once its own deadline has passed.
fn level_for(now: u64, deadline: u64) -> Level {
    let significant = 63 - ((now ^ deadline) | SLOT_MASK).leading_zeros();
    ((significant / SLOT_BITS) as Level).min(TOP_LEVEL)
}
