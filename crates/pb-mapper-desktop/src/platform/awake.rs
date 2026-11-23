//! When a host keeps its display and the system awake: from the first
//! capture after an idle spell until [`LINGER`] after the last, so a pause, a
//! close or a lost helper lets them sleep again soon after. Capture records
//! itself with one atomic store ([`Touches::touch`]); a watchdog thread,
//! parked while nothing captures, decides with [`Keeper::step`]. Pure and
//! OS-free, so every platform tests it; the Windows host drives it
//! (`windows/power.rs`).

use std::{
    sync::{
        OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

/// How long after its last capture a host stays awake. While a session
/// streams, thread C asks for a capture at least once a frame period, which
/// is at most a second (1 fps), unless the network or the encoder holds it
/// back; a pause or a close stops capture at once.
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
    /// started, and the caller must wake it.
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
    pub(crate) fn last(&self) -> Option<Instant> {
        let millis = self.last.load(Ordering::SeqCst).checked_sub(1)?;
        self.epoch.get()?.checked_add(Duration::from_millis(millis))
    }

    /// The watchdog is about to park. True when capture ran within
    /// [`LINGER`] of `now` after all, a touch that saw the watchdog still
    /// running: it must not wait, and runs again instead ([`Touches::run`]).
    /// From here on, every touch asks for a wake.
    pub(crate) fn park(&self, now: Instant) -> bool {
        self.parked.store(true, Ordering::SeqCst);
        live(now, self.last())
    }

    /// The watchdog runs: touches stop asking for a wake.
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
pub(crate) enum Action {
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
pub(crate) struct Keeper {
    held: bool,
}

impl Keeper {
    /// The watchdog looks at `now`; capture last ran at `last`.
    pub(crate) fn step(&mut self, now: Instant, last: Option<Instant>) -> Action {
        let until = until(now, last);
        match (std::mem::replace(&mut self.held, until.is_some()), until) {
            (false, Some(until)) => Action::Acquire { until },
            (true, Some(until)) => Action::Hold { until },
            (true, None) => Action::Release,
            (false, None) => Action::Park,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECOND: Duration = Duration::from_secs(1);

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

    /// The first touch asks for the watchdog, touches while it runs cost no
    /// wake, and a touch that lands between the watchdog's last look and
    /// its parking is not lost: parking sees it.
    #[test]
    fn a_touch_is_never_lost_to_parking() {
        let touches = Touches::new();
        let t0 = Instant::now();
        assert_eq!(touches.last(), None);
        assert!(touches.touch(t0), "not started: wake it");
        assert_eq!(touches.last(), Some(t0));

        touches.run();
        assert!(!touches.touch(t0 + SECOND), "running: no wake");
        assert_eq!(touches.last(), Some(t0 + SECOND));

        // The watchdog decides to release at 3 s and parks: nothing new.
        let mut keeper = Keeper { held: true };
        assert_eq!(
            keeper.step(t0 + 3 * SECOND, touches.last()),
            Action::Release
        );
        assert!(!touches.park(t0 + 3 * SECOND));
        assert!(touches.touch(t0 + 4 * SECOND), "parked: wake it");

        // A touch between its look and parking: it does not wait.
        touches.run();
        assert_eq!(keeper.step(t0 + 7 * SECOND, touches.last()), Action::Park);
        assert!(!touches.touch(t0 + 7 * SECOND), "still running: no wake");
        assert!(touches.park(t0 + 7 * SECOND), "parking sees the touch");
        touches.run();
        assert_eq!(
            keeper.step(t0 + 7 * SECOND, touches.last()),
            Action::Acquire {
                until: t0 + 9 * SECOND
            }
        );
    }

    /// What a touch adds to each capture call while the watchdog runs (the
    /// common case: no wake), beside reading the clock alone. In release:
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
        touches.run();
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
