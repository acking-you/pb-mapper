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
//! identities, or has to be told one was superseded: to move a deadline, schedule
//! the same timer again — the earlier chain still drains, but it is no longer the
//! last reference, so it fires nothing.

use super::*;

/// A callback that runs once: when its delay elapses, or when it is cancelled,
/// whichever comes first.
pub(super) struct Timer {
    /// `None` once the callback has run, so any route still holding this timer is
    /// inert and a cancelled timer cannot fire twice.
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
    Deliver(#[allow(dead_code)] Arc<Timer>),
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
        let (level, slot, remaining) = (0..self.levels.len())
            .rev()
            .find_map(|level| self.entry_leg(level, self.ticks + delay))
            .expect("a delay within max_delay reaches some level");
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

    fn file(&mut self, level: usize, slot: usize, link: Link) {
        if let Some(bucket) = self.levels[level].get_mut(slot) {
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
mod tests {
    use super::*;

    /// A wheel plus the tick each scheduled timer actually fired on.
    struct Harness {
        wheel: TimingWheel,
        fired: Arc<Mutex<Vec<(u32, u64)>>>,
        ticks: Arc<AtomicU64>,
    }

    impl Harness {
        fn new(max_delay: u64, radix: u64) -> Self {
            Self {
                wheel: TimingWheel::new(max_delay, radix),
                fired: Arc::new(Mutex::new(Vec::new())),
                ticks: Arc::new(AtomicU64::new(0)),
            }
        }

        fn timer(&self, id: u32) -> Arc<Timer> {
            let fired = self.fired.clone();
            let ticks = self.ticks.clone();
            Timer::new(move || {
                fired.lock().push((id, ticks.load(Ordering::Acquire)));
            })
        }

        fn schedule(&mut self, id: u32, delay: u64) {
            let timer = self.timer(id);
            self.wheel.schedule(delay, timer);
        }

        fn tick(&mut self) {
            self.ticks.fetch_add(1, Ordering::AcqRel);
            self.wheel.tick();
        }

        fn fired_at(&self, id: u32) -> Option<u64> {
            self.fired
                .lock()
                .iter()
                .find(|(fired, _)| *fired == id)
                .map(|(_, at)| *at)
        }
    }

    /// Every delay, from every starting offset, must fire on exactly the tick it
    /// asked for. This is the wheel's whole contract, and a radix decomposition is
    /// easy to get wrong by one bucket, so it is checked exhaustively rather than
    /// sampled.
    #[test]
    fn every_delay_fires_on_its_exact_tick() {
        let radix = 4;
        let max_delay = radix * radix * radix;
        for offset in 0..2 * radix * radix {
            let mut harness = Harness::new(max_delay, radix);
            for _ in 0..offset {
                harness.tick();
            }
            for delay in 1..=max_delay {
                harness.schedule(delay as u32, delay);
            }
            for _ in 0..max_delay {
                harness.tick();
            }
            for delay in 1..=max_delay {
                assert_eq!(
                    harness.fired_at(delay as u32),
                    Some(offset + delay),
                    "radix {radix}, offset {offset}, delay {delay}"
                );
            }
        }
    }

    /// The same contract at the shape the wheel actually runs with.
    #[test]
    fn every_short_delay_fires_on_its_exact_tick_at_radix_64() {
        let radix = 64;
        let mut harness = Harness::new(radix * radix * radix * radix, radix);
        for _ in 0..100 {
            harness.tick();
        }
        let delays = (1..=200).chain([radix - 1, radix, radix + 1, radix * radix, 4095, 4096]);
        for delay in delays.clone() {
            harness.schedule(delay as u32, delay);
        }
        for _ in 0..5000 {
            harness.tick();
        }
        for delay in delays {
            assert_eq!(
                harness.fired_at(delay as u32),
                Some(100 + delay),
                "delay {delay}"
            );
        }
    }

    #[test]
    fn level_count_covers_the_requested_delay() {
        assert_eq!(TimingWheel::new(64, 64).max_delay(), 64);
        assert_eq!(TimingWheel::new(65, 64).max_delay(), 4096);
        assert_eq!(TimingWheel::new(4096, 64).max_delay(), 4096);
        assert_eq!(TimingWheel::new(4097, 64).max_delay(), 262_144);
    }

    #[test]
    fn a_delay_the_wheel_cannot_place_fires_at_once() {
        let mut harness = Harness::new(64, 64);
        harness.schedule(1, 65);
        assert_eq!(harness.fired_at(1), Some(0));
        harness.schedule(2, 0);
        assert_eq!(harness.fired_at(2), Some(0));
    }

    #[test]
    fn rescheduling_the_same_timer_defers_it_to_the_later_route() {
        let mut harness = Harness::new(4096, 64);
        let timer = harness.timer(1);
        harness.wheel.schedule(5, timer.clone());
        harness.wheel.schedule(20, timer);

        for _ in 0..5 {
            harness.tick();
        }
        assert_eq!(
            harness.fired_at(1),
            None,
            "the earlier route must not fire the timer"
        );
        for _ in 5..20 {
            harness.tick();
        }
        assert_eq!(harness.fired_at(1), Some(20));
    }

    #[test]
    fn firing_early_makes_the_scheduled_route_inert() {
        let mut harness = Harness::new(4096, 64);
        let timer = harness.timer(1);
        harness.wheel.schedule(10, timer.clone());

        timer.fire();
        assert_eq!(harness.fired_at(1), Some(0));
        for _ in 0..10 {
            harness.tick();
        }
        assert_eq!(harness.fired.lock().len(), 1);
    }

    #[test]
    fn dropping_the_wheel_fires_everything_it_holds() {
        let mut harness = Harness::new(262_144, 64);
        harness.schedule(1, 5);
        harness.schedule(2, 200_000);

        let fired = harness.fired.clone();
        drop(harness);
        let ids = fired
            .lock()
            .iter()
            .map(|(id, _)| *id)
            .collect::<HashSet<_>>();
        assert_eq!(ids, HashSet::from([1, 2]));
    }
}
