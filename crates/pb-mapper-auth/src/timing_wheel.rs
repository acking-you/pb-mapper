//! Hierarchical timer wheel: rotating bucket queues indexed by relative delay.
//!
//! Levels are digit positions in base `radix`, and how many exist is derived from
//! the longest delay the wheel must support. Scheduling decomposes the delay into
//! those digits and builds one nested link per digit, coarsest outermost:
//!
//! ```text
//! radix = 64,  schedule(delay = 1*64² + 5*64 + 3)
//!
//!   level 2  [ ][A][ ]…      A pops after 1 rotation of 64² ticks; dropping it
//!   level 1  [ ]…[B][ ]…       files B, which pops 5 rotations of 64 ticks later
//!   level 0  [ ][ ][C]…          and files C, which pops 3 ticks later and fires
//! ```
//!
//! `A` holds `B` holds `C` holds the timer, so the chain *is* the route: no per
//! timer list of future placements, and nothing to look up or recompute. A bucket
//! is just `Vec<Link>`, and dropping a link is what files the next one.
//!
//! ```text
//! tick() -> ticks += 1
//!        -> level 0 always rotates; level i rotates when ticks % radix^i == 0
//!        -> rotate = pop_front, push_back an empty bucket; dropping the popped
//!           bucket files each link's successor, or fires the timer if the link
//!           was the innermost
//! ```
//!
//! So a tick moves one bucket per level that turns over and performs no
//! arithmetic per entry: the queues rotate, which keeps a bucket's index equal to
//! its distance from now.
//!
//! Only the wheel holds strong references to a timer, so it runs when the last
//! chain holding it is dropped. The wheel never looks a timer up, compares
//! identities, or has to be told one was superseded: to move a deadline, hand the
//! callback to a new timer with [`Timer::supersede`] and schedule that. The earlier
//! chain still drains, but the timer at its end has nothing left to run.

use super::*;

/// A callback that runs once: when its delay elapses, or when it is cancelled,
/// whichever comes first.
pub(super) struct Timer {
    /// `None` once the callback has run or moved on, so any route still holding this
    /// timer is inert and a cancelled timer cannot fire twice.
    ///
    /// WHY a lock for state a single task owns: running a `FnOnce` moves it out,
    /// which needs `&mut`, but a timer is reached through a shared handle so that
    /// two routes can hold one. `Arc<T>: Send` — which `tokio::spawn` requires of
    /// the actor this runs in — implies `T: Sync`, and shared mutability that is
    /// `Sync` needs a lock; `Cell` would be cheaper but is not `Sync`. It is never
    /// contended, and the path that fires almost every timer skips it: `Drop` has
    /// `&mut self`, so it reaches the callback directly.
    callback: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl Timer {
    pub(super) fn new(callback: impl FnOnce() + Send + 'static) -> Arc<Self> {
        Arc::new(Self {
            callback: Mutex::new(Some(Box::new(callback))),
        })
    }

    /// Runs the callback unless it has run already, for a caller cancelling ahead
    /// of the deadline.
    pub(super) fn fire(&self) {
        let callback = self.callback.lock().take();
        run(callback);
    }

    /// Discards the callback unrun, for a timer whose work no longer applies.
    pub(super) fn disarm(&self) {
        let callback = self.callback.lock().take();
        drop(callback);
    }

    /// Moves the callback to a new timer, leaving this one with nothing to run.
    ///
    /// This is how a deadline moves in either direction. Scheduling the same timer
    /// twice could only postpone it: it runs when its last placement drains, so the
    /// old, later placement would hold a shortened deadline back until it came due.
    pub(super) fn supersede(&self) -> Arc<Self> {
        Arc::new(Self {
            callback: Mutex::new(self.callback.lock().take()),
        })
    }
}

impl Drop for Timer {
    /// Releasing the last reference is what fires a timer, so dropping the wheel
    /// runs everything it was holding. Owning `&mut self` here is what lets the
    /// usual path take the callback without locking.
    fn drop(&mut self) {
        let callback = self.callback.get_mut().take();
        run(callback);
    }
}

fn run(callback: Option<Box<dyn FnOnce() + Send>>) {
    if let Some(callback) = callback {
        callback();
    }
}

/// One leg of a timer's route through the levels.
///
/// A delay spanning several digits cannot be filed in one bucket, so the route is
/// a chain: each [`Link::Relay`] waits in one bucket and, once that bucket comes
/// off the front, hands the leg nested inside it to the wheel, which files it in
/// the next, finer bucket. Only the outermost leg is ever in a bucket, and only
/// [`Link::Deliver`] holds the timer, so the chain unwinding one bucket at a time
/// *is* the timer descending the levels. That is what leaves a tick with nothing
/// to compute.
///
/// ```text
///   delay = 1*64² + 5*64 + 3
///   Relay{L2,slot 1} -> Relay{L1,slot 5} -> Relay{L0,slot 3} -> Deliver(timer)
///   ^ filed now         ^ filed when the   ^ …and so on         ^ dropping this
///                         one before it                           fires the timer
///                         comes off
/// ```
enum Link {
    /// The end of a route. Never read: holding the reference *is* this leg's job,
    /// and releasing it is what fires the timer.
    Deliver(Arc<Timer>),
    /// Files `next` into `level`'s `slot` when this leg comes off the front.
    ///
    /// `Box`, not `Arc`: exactly one bucket owns a route at a time, so a leg needs
    /// no reference count of its own — only the timer at the end is shared.
    Relay {
        level: u8,
        slot: u16,
        next: Box<Link>,
    },
}

/// A bucket's worth of routes. Dropping one without draining it releases the
/// timers at the end of every route it holds, which is how dropping the wheel
/// fires everything.
type Bucket = Vec<Link>;

pub(super) struct TimingWheel {
    /// Ticks elapsed since construction. Buckets are indexed relative to it, so
    /// advancing re-indexes nothing.
    ticks: u64,
    radix: u64,
    levels: Vec<VecDeque<Bucket>>,
}

impl TimingWheel {
    /// Builds the smallest wheel that can place `max_delay` ticks, adding a level
    /// at a time until the levels together span it.
    pub(super) fn new(max_delay: u64, radix: u64) -> Self {
        assert!(radix > 1, "a level needs at least two buckets");
        let mut levels = 1_usize;
        let mut span = radix;
        while span < max_delay {
            levels += 1;
            span = span.saturating_mul(radix);
        }
        Self {
            ticks: 0,
            radix,
            levels: (0..levels)
                .map(|_| {
                    std::iter::repeat_with(Bucket::new)
                        .take(radix as usize)
                        .collect()
                })
                .collect(),
        }
    }

    /// Longest delay this wheel can place exactly.
    pub(super) fn max_delay(&self) -> u64 {
        self.period(self.levels.len())
    }

    /// Holds `timer` for `delay` ticks. A delay of zero, or one past
    /// [`Self::max_delay`], releases the timer at once rather than misplacing it.
    ///
    /// Scheduling a timer the wheel already holds builds a second route rather
    /// than replacing the first, which is how a caller moves a deadline without
    /// the wheel having to find the old one.
    pub(super) fn schedule(&mut self, delay: u64, timer: Arc<Timer>) {
        if delay == 0 || delay > self.max_delay() {
            // Dropping `timer` here fires it if this was the last reference.
            return;
        }
        let deliver = Link::Deliver(timer);
        // The coarsest reachable level absorbs however far the current tick sits
        // into its rotation, so its bucket comes off on a rotation boundary. Every
        // finer level is at zero offset there, which makes the delay still
        // remaining a plain base-`radix` decomposition from that point down.
        // The guard above bounds `delay`, so some level always takes it.
        // Unreachable, and treated like the out-of-range case: dropping the
        // timer fires it, which is the safe direction for a credential deadline.
        let Some((level, slot, remaining)) = (0..self.levels.len())
            .rev()
            .find_map(|level| self.entry_leg(level, self.ticks + delay))
        else {
            return;
        };
        let route = match remaining {
            0 => deliver,
            remaining => self.route(remaining, deliver),
        };
        self.file(level, slot, route);
    }

    /// Advances one tick, rotating every level that turns over.
    pub(super) fn tick(&mut self) {
        self.ticks += 1;
        for level in 0..self.levels.len() {
            // A level turns over every `radix^level` ticks. Once one does not, no
            // coarser one can either, since its period divides theirs.
            if !self.ticks.is_multiple_of(self.period(level)) {
                break;
            }
            let bucket = self.rotate(level);
            for link in bucket {
                match link {
                    // Out of legs: dropping it releases the timer, firing it if
                    // this was the last route holding it.
                    Link::Deliver(timer) => drop(timer),
                    Link::Relay { level, slot, next } => {
                        self.file(level as usize, slot as usize, *next)
                    }
                }
            }
        }
    }

    /// Takes `level`'s front bucket off and puts an empty one on the back, so
    /// bucket indices stay relative to the current tick.
    fn rotate(&mut self, level: usize) -> Bucket {
        let queue = &mut self.levels[level];
        let bucket = queue.pop_front().unwrap_or_default();
        queue.push_back(Bucket::new());
        bucket
    }

    /// Files `link` in `level`'s bucket `slot`. Out of range, the link is dropped,
    /// which fires its timer: the safe direction for a credential deadline.
    fn file(&mut self, level: usize, slot: usize, link: Link) {
        if let Some(bucket) = self
            .levels
            .get_mut(level)
            .and_then(|queue| queue.get_mut(slot))
        {
            bucket.push(link);
        }
    }

    /// The route to file for a timer due `remaining` ticks after a rotation
    /// boundary, built by recursing into the finer levels so each leg owns the
    /// part of the route it hands on. `remaining` must be non-zero.
    fn route(&self, remaining: u64, inner: Link) -> Link {
        let (level, slot, rest) = self.next_leg(remaining);
        let next = match rest {
            0 => inner,
            rest => self.route(rest, inner),
        };
        Link::Relay {
            level: level as u8,
            slot: slot as u16,
            next: Box::new(next),
        }
    }

    /// The route's first leg if it starts at `level`: which bucket holds a timer
    /// due at tick `target`, and how much delay that leaves for the legs after it.
    /// `None` when this level's next rotation already overshoots `target`, or when
    /// `target` is more than one revolution away.
    fn entry_leg(&self, level: usize, target: u64) -> Option<(usize, usize, u64)> {
        let period = self.period(level);
        let next_rotation = self.ticks - self.ticks % period + period;
        let ahead = target.checked_sub(next_rotation)?;
        let slot = ahead / period;
        (slot < self.radix).then_some((level, slot as usize, ahead % period))
    }

    /// The next leg for a timer due `remaining` ticks after a rotation boundary:
    /// the coarsest level whose rotation still fits. From a boundary that level's
    /// front bucket comes off one period out, so bucket `j` comes off after
    /// `j + 1` of them.
    fn next_leg(&self, remaining: u64) -> (usize, usize, u64) {
        let level = (0..self.levels.len())
            .rev()
            .find(|level| self.period(*level) <= remaining)
            .unwrap_or(0);
        let period = self.period(level);
        (level, (remaining / period - 1) as usize, remaining % period)
    }

    /// Ticks spanned by `level` and every level below it: `radix^level`.
    fn period(&self, level: usize) -> u64 {
        self.radix.saturating_pow(level as u32)
    }
}

#[cfg(test)]
mod tests;
