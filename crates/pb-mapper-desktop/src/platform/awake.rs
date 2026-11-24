//! When a host keeps its display and the system awake: from the first
//! capture after an idle spell until [`LINGER`] after the last, so a pause, a
//! close or a lost helper lets them sleep again soon after. Capture records
//! itself with one atomic store ([`Touches::touch`]); a [`Watchdog`] thread,
//! parked while nothing captures, decides when to take and give back the
//! requests through [`Power`]. Pure and OS-free, so every platform tests it,
//! the watchdog's loop included; the Windows host drives it
//! (`windows/power.rs`).

use std::{
    sync::{
        OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

/// How long after its last capture a host stays awake. While a session
/// runs unpaused, the Windows host touches at every capture call, at least
/// once a frame period, which is at most a second (1 fps), and at every
/// pointer read, which goes on while the network or the encoder holds
/// capture back; a pause or a close stops both at once.
pub(crate) const LINGER: Duration = Duration::from_secs(2);

/// What capture writes and the watchdog reads, without a lock: when capture
/// last ran, and whether the watchdog is parked until it runs again.
pub(crate) struct Touches {
    epoch: OnceLock<Instant>,
    /// Milliseconds from `epoch` to the latest touch, plus one; zero: none.
    last: AtomicU64,
    /// The watchdog waits for a touch to wake it, or does not run yet.
    parked: AtomicBool,
}

impl Touches {
    pub(crate) const fn new() -> Self {
        Self {
            epoch: OnceLock::new(),
            last: AtomicU64::new(0),
            parked: AtomicBool::new(true),
        }
    }

    /// Capture ran at `now`. True when the watchdog is parked, or not yet
    /// started, and the caller must wake it: the first touch after an idle
    /// spell, not the ones while the watchdog runs.
    pub(crate) fn touch(&self, now: Instant) -> bool {
        let epoch = *self.epoch.get_or_init(|| now);
        let millis = u64::try_from(now.saturating_duration_since(epoch).as_millis())
            .unwrap_or(u64::MAX)
            .saturating_add(1);
        // Sequentially consistent with `park`: either this load sees the
        // watchdog parked, or its load after parking sees this store.
        self.last.store(millis, Ordering::SeqCst);
        self.parked.load(Ordering::SeqCst)
    }

    /// When capture last ran, if ever.
    fn last(&self) -> Option<Instant> {
        let millis = self.last.load(Ordering::SeqCst).checked_sub(1)?;
        self.epoch.get()?.checked_add(Duration::from_millis(millis))
    }

    /// The watchdog is about to park. True when capture ran within
    /// [`LINGER`] of `now` after all, a touch that saw the watchdog still
    /// running: it must not wait. From here on, every touch asks for a wake.
    fn park(&self, now: Instant) -> bool {
        self.parked.store(true, Ordering::SeqCst);
        live(now, self.last())
    }

    /// The watchdog runs: touches stop asking for a wake. Every look of the
    /// watchdog starts with this; a host that cannot start one calls it
    /// once, so that touches stop asking for one.
    pub(crate) fn run(&self) {
        self.parked.store(false, Ordering::SeqCst);
    }
}

/// Capture ran within [`LINGER`] of `now`: until when it counts.
fn until(now: Instant, last: Option<Instant>) -> Option<Instant> {
    last.and_then(|last| last.checked_add(LINGER))
        .filter(|until| now < *until)
}

fn live(now: Instant, last: Option<Instant>) -> bool {
    until(now, last).is_some()
}

/// What the watchdog does next.
#[derive(Debug, PartialEq, Eq)]
enum Action {
    /// Capture started: take the requests, turn a dark display on, and look
    /// again at `until`.
    Acquire { until: Instant },
    /// Capture goes on: keep them, and look again at `until`.
    Hold { until: Instant },
    /// Capture stopped [`LINGER`] ago: give them back, then park.
    Release,
    /// Nothing held, nothing captures: park.
    Park,
}

/// Whether the watchdog holds the requests.
#[derive(Default)]
struct Keeper {
    held: bool,
}

impl Keeper {
    /// The watchdog looks at `now`; capture last ran at `last`.
    fn step(&mut self, now: Instant, last: Option<Instant>) -> Action {
        let until = until(now, last);
        match (std::mem::replace(&mut self.held, until.is_some()), until) {
            (false, Some(until)) => Action::Acquire { until },
            (true, Some(until)) => Action::Hold { until },
            (true, None) => Action::Release,
            (false, None) => Action::Park,
        }
    }
}

/// What the watchdog holds while capture runs: on Windows, one power
/// request with the display and the system required.
pub(crate) trait Power {
    /// Capture started: take the requests, and turn a dark display on.
    fn acquire(&mut self);
    /// Capture stopped [`LINGER`] ago: give them back.
    fn release(&mut self);
}

/// What the watchdog waits for before it looks again.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Wait {
    /// Until then; touches meanwhile do not end the wait.
    Until(Instant),
    /// Until a touch wakes it. A spurious wake only makes it look again.
    Touch,
}

/// The watchdog's thread, started by the first touch: it holds the
/// requests while capture runs and parks while it does not.
pub(crate) struct Watchdog<'a> {
    touches: &'a Touches,
    keeper: Keeper,
}

impl<'a> Watchdog<'a> {
    pub(crate) fn new(touches: &'a Touches) -> Self {
        Self {
            touches,
            keeper: Keeper::default(),
        }
    }

    /// Looks at `now`, when the thread starts and after every wait: takes or
    /// gives back the requests, and says what to wait for next. From its
    /// first look on, touches ask for no wake until it parks.
    pub(crate) fn look(&mut self, now: Instant, power: &mut impl Power) -> Wait {
        self.touches.run();
        match self.keeper.step(now, self.touches.last()) {
            Action::Acquire { until } => {
                power.acquire();
                Wait::Until(until)
            }
            Action::Hold { until } => Wait::Until(until),
            Action::Release => {
                power.release();
                self.park(now)
            }
            Action::Park => self.park(now),
        }
    }

    /// Parks until the next touch, unless one came in since the look: then
    /// it looks again at once.
    fn park(&self, now: Instant) -> Wait {
        if self.touches.park(now) {
            Wait::Until(now)
        } else {
            Wait::Touch
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECOND: Duration = Duration::from_secs(1);
    /// About 60 fps, in whole milliseconds as touches record them.
    const FRAME: Duration = Duration::from_millis(16);

    /// A session that starts, streams, pauses, resumes and closes: the
    /// requests are taken (and a dark display woken) on the first capture
    /// and again on resume, kept while capture goes on, and given back
    /// [`LINGER`] after the last capture, never sooner.
    #[test]
    fn holds_from_the_first_capture_until_linger_after_the_last() {
        let t0 = Instant::now();
        let at = |seconds: f64| t0 + Duration::from_secs_f64(seconds);
        let mut keeper = Keeper::default();
        assert_eq!(keeper.step(t0, None), Action::Park, "nothing captured yet");

        assert_eq!(
            keeper.step(at(0.0), Some(at(0.0))),
            Action::Acquire { until: at(2.0) }
        );
        // Captures keep coming at 1 fps, the slowest pace: held throughout.
        for second in 1..=5 {
            let now = at(f64::from(second) + 0.9);
            assert_eq!(
                keeper.step(now, Some(at(f64::from(second)))),
                Action::Hold {
                    until: at(f64::from(second) + 2.0)
                },
            );
        }
        // Paused after the capture at 5 s: still held just before 7 s...
        let last = Some(at(5.0));
        assert_eq!(
            keeper.step(at(6.999), last),
            Action::Hold { until: at(7.0) }
        );
        // ...given back at 7 s, then parked while nothing captures.
        assert_eq!(keeper.step(at(7.0), last), Action::Release);
        assert_eq!(keeper.step(at(9.0), last), Action::Park);

        // Resumed: taken again, so a display that went dark meanwhile wakes.
        assert_eq!(
            keeper.step(at(20.0), Some(at(20.0))),
            Action::Acquire { until: at(22.0) }
        );
        // Closed: given back once.
        assert_eq!(keeper.step(at(22.0), Some(at(20.0))), Action::Release);
        assert_eq!(keeper.step(at(30.0), Some(at(20.0))), Action::Park);
    }

    /// The requests as a host holds them.
    #[derive(Default)]
    struct Held<'a> {
        held: bool,
        acquired: u32,
        /// A capture that lands as the watchdog gives the requests back,
        /// after its look and before it parks.
        touch_on_release: Option<(&'a Touches, Instant)>,
    }

    impl Power for Held<'_> {
        fn acquire(&mut self) {
            assert!(!self.held, "taken twice");
            self.held = true;
            self.acquired += 1;
        }
        fn release(&mut self) {
            assert!(self.held, "given back twice");
            self.held = false;
            if let Some((touches, at)) = self.touch_on_release.take() {
                assert!(!touches.touch(at), "still running: no wake");
            }
        }
    }

    /// Runs the watchdog's looks as its thread would, up to `now`: each
    /// timed wait that ends by then.
    fn sleep_to(
        watchdog: &mut Watchdog<'_>,
        power: &mut Held<'_>,
        mut wait: Wait,
        now: Instant,
    ) -> Wait {
        while let Wait::Until(until) = wait
            && until <= now
        {
            wait = watchdog.look(until, power);
        }
        wait
    }

    /// The watchdog as its thread runs it, from a process's first touch:
    /// that touch starts it, those while it runs ask for no wake, from its
    /// very first look on, and the first after it parks wakes it again.
    #[test]
    fn only_the_first_touch_of_a_spell_wakes_the_watchdog() {
        let touches = Touches::new();
        let mut watchdog = Watchdog::new(&touches);
        let mut power = Held::default();
        let t0 = Instant::now();

        assert!(touches.touch(t0), "the first touch starts the watchdog");
        let mut wait = watchdog.look(t0, &mut power);
        assert_eq!(wait, Wait::Until(t0 + LINGER));
        assert!(power.held);

        // Three seconds of captures at about 60 fps: none asks for a wake.
        let last = t0 + FRAME * 180;
        for frame in 1..=180 {
            let now = t0 + FRAME * frame;
            wait = sleep_to(&mut watchdog, &mut power, wait, now);
            assert!(!touches.touch(now), "running: no wake (frame {frame})");
        }
        assert!(power.held);

        // Capture stops: held until LINGER after the last, given back then,
        // and parked.
        wait = sleep_to(
            &mut watchdog,
            &mut power,
            wait,
            last + LINGER - Duration::from_millis(1),
        );
        assert!(power.held, "held until LINGER after the last capture");
        wait = sleep_to(&mut watchdog, &mut power, wait, last + LINGER);
        assert!(!power.held);
        assert_eq!(wait, Wait::Touch);
        // A spurious wake: it parks again, taking nothing.
        let later = last + 5 * SECOND;
        assert_eq!(watchdog.look(later, &mut power), Wait::Touch);

        // Resumed: the first capture wakes it, and it takes them again.
        let resume = later + 5 * SECOND;
        assert!(touches.touch(resume), "parked: wake it");
        assert_eq!(
            watchdog.look(resume, &mut power),
            Wait::Until(resume + LINGER)
        );
        assert_eq!(power.acquired, 2);
        assert!(!touches.touch(resume + FRAME), "running again: no wake");
    }

    /// A capture that lands after the watchdog's look and before it parks
    /// asks for no wake, as the watchdog still runs; parking sees it, so the
    /// watchdog looks again at once instead of waiting for a wake that never
    /// comes, and takes the requests again.
    #[test]
    fn a_touch_is_never_lost_to_parking() {
        let touches = Touches::new();
        let mut watchdog = Watchdog::new(&touches);
        let t0 = Instant::now();
        let stop = t0 + LINGER;
        let mut power = Held {
            touch_on_release: Some((&touches, stop)),
            ..Held::default()
        };

        assert!(touches.touch(t0));
        assert_eq!(watchdog.look(t0, &mut power), Wait::Until(stop));
        assert_eq!(
            watchdog.look(stop, &mut power),
            Wait::Until(stop),
            "given back, but parking saw the touch: look again at once"
        );
        assert!(!power.held);
        assert_eq!(watchdog.look(stop, &mut power), Wait::Until(stop + LINGER));
        assert_eq!(power.acquired, 2);
    }

    /// What a touch adds to each capture call while the watchdog runs: every
    /// touch but the first after an idle spell, which also wakes it. Beside
    /// reading the clock alone. In release:
    /// `cargo test --release -p pb-mapper-desktop --lib touch_cost -- --ignored --nocapture`
    #[test]
    #[ignore = "measures this machine; run in release"]
    fn touch_cost() {
        const CALLS: u32 = 20_000_000;
        let per_call = |run: &dyn Fn()| {
            let started = Instant::now();
            for _ in 0..CALLS {
                run();
            }
            started.elapsed().as_secs_f64() * 1e9 / f64::from(CALLS)
        };
        let touches = Touches::new();
        let now = Instant::now();
        assert!(touches.touch(now));
        // Running, as from the watchdog's first look on.
        let mut watchdog = Watchdog::new(&touches);
        assert_eq!(
            watchdog.look(now, &mut Held::default()),
            Wait::Until(now + LINGER)
        );
        for round in 1..=5 {
            let clock = per_call(&|| {
                std::hint::black_box(Instant::now());
            });
            let touch = per_call(&|| {
                std::hint::black_box(touches.touch(std::hint::black_box(Instant::now())));
            });
            println!("awake_touch_cost round={round} clock_ns={clock:.2} touch_ns={touch:.2}");
        }
    }
}
