//! Hierarchical timer wheel. It schedules opaque timers and knows nothing about
//! what they mean.
//!
//! ```text
//! schedule(deadline, timer) -> the bucket covering that deadline
//!
//! tick() -> now += 1
//!        -> drain level 0's slot for `now`; every 64th tick level 1's, and so on
//!        -> an entry whose deadline has arrived is dropped; the rest are re-filed
//!           into the finer level that now covers them
//! ```
//!
//! The wheel holds the only strong references to its timers, so a timer runs when
//! the last entry referring to it is dropped. That is what keeps the wheel
//! indifferent to its users: it never looks a timer up, compares identities, or
//! has to be told that one was superseded.
//!
//! Rescheduling exploits that directly. Holding a `Weak<Timer>`, a caller inserts
//! the same timer again at a later deadline; the earlier entry still drains on its
//! own schedule, but dropping it no longer brings the count to zero, so the timer
//! waits for the last entry to go. Cancelling is the mirror image: [`Timer::fire`]
//! runs the callback early and leaves the remaining entries inert.

use super::*;

/// Bits of a deadline that one level's slot field covers, so a level holds
/// `1 << 6` slots and each is 64 times coarser than the level below it.
const SLOT_BITS: u32 = 6;
const SLOTS: usize = 1 << SLOT_BITS;
const SLOT_MASK: u64 = SLOTS as u64 - 1;
/// Six 64-slot levels span `64^6` seconds, so any deadline a caller can ask for
/// lands in a level whose range really contains it.
const NUM_LEVELS: usize = 6;
const TOP_LEVEL: Level = NUM_LEVELS as Level - 1;

/// Which level of the hierarchy a bucket belongs to: `0..NUM_LEVELS`.
type Level = u8;

/// Which bucket within one level: `0..SLOTS`.
type Slot = u8;

/// A callback that runs once: when its deadline arrives, or when it is cancelled,
/// whichever comes first.
pub(super) struct Timer {
    /// `None` once the callback has run, so any remaining wheel entries for this
    /// timer are inert and a cancelled timer cannot fire twice.
    callback: std::sync::Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl Timer {
    pub(super) fn new(callback: impl FnOnce() + Send + 'static) -> Arc<Self> {
        Arc::new(Self {
            callback: std::sync::Mutex::new(Some(Box::new(callback))),
        })
    }

    /// Runs the callback unless it has run already. Called by the wheel when a
    /// deadline arrives, and by a caller cancelling ahead of that.
    pub(super) fn fire(&self) {
        let callback = recover_lock(self.callback.lock()).take();
        if let Some(callback) = callback {
            callback();
        }
    }
}

impl Drop for Timer {
    /// Releasing the last reference is what fires a timer, so dropping the wheel
    /// tears down everything it was holding.
    fn drop(&mut self) {
        self.fire();
    }
}

/// One placement of a timer. The deadline lives here rather than in the `Timer`,
/// so a timer rescheduled later leaves its earlier placements draining harmlessly
/// instead of dragging them forward.
struct Entry {
    deadline: u64,
    /// Never read: holding the reference *is* the entry's job, and releasing it
    /// is what can fire the timer.
    #[allow(dead_code)]
    timer: Arc<Timer>,
}

pub(super) struct TimingWheel {
    now: u64,
    levels: [Vec<Vec<Entry>>; NUM_LEVELS],
}

impl TimingWheel {
    pub(super) fn new(now: u64) -> Self {
        Self {
            now,
            levels: std::array::from_fn(|_| std::iter::repeat_with(Vec::new).take(SLOTS).collect()),
        }
    }

    pub(super) fn now(&self) -> u64 {
        self.now
    }

    /// Holds `timer` until `deadline`. Scheduling a timer the wheel already holds
    /// adds a placement rather than replacing one, which is how a caller moves a
    /// deadline outward without the wheel having to find the old entry.
    pub(super) fn schedule(&mut self, deadline: u64, timer: Arc<Timer>) {
        self.place(Entry { deadline, timer });
    }

    /// Advances one second and drains whatever that turnover exposes.
    pub(super) fn tick(&mut self) {
        self.now += 1;
        // Coarse to fine, so a timer cascading several levels down still reaches
        // level 0 in time to be drained by this same tick.
        for level in (1..=TOP_LEVEL).rev() {
            // A level turns over once every `slot_range` seconds, exactly when
            // `now` has no bits left below that level's slot field.
            if self.now & (slot_range(level) - 1) != 0 {
                continue;
            }
            let entries = self.take_bucket(level, self.now);
            self.refile(entries);
        }
        let entries = self.take_bucket(0, self.now);
        self.refile(entries);
    }

    fn refile(&mut self, entries: Vec<Entry>) {
        for entry in entries {
            self.place(entry);
        }
    }

    fn place(&mut self, entry: Entry) {
        // An arrived deadline means this placement is done: returning drops the
        // entry, which fires the timer if this was its last reference.
        if entry.deadline <= self.now {
            return;
        }
        let level = level_for(self.now, entry.deadline);
        let slot = slot_for(level, entry.deadline);
        self.levels[level as usize][slot as usize].push(entry);
    }

    /// Empties the bucket that `when` falls in at `level`.
    fn take_bucket(&mut self, level: Level, when: u64) -> Vec<Entry> {
        let slot = slot_for(level, when);
        std::mem::take(&mut self.levels[level as usize][slot as usize])
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
/// level is clamped into it and cannot fire early, because an entry is dropped
/// only once its own deadline has arrived.
fn level_for(now: u64, deadline: u64) -> Level {
    let significant = 63 - ((now ^ deadline) | SLOT_MASK).leading_zeros();
    ((significant / SLOT_BITS) as Level).min(TOP_LEVEL)
}
