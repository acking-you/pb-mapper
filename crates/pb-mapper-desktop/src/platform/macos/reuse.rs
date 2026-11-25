//! How long capture and the pointer take a passing desktop check as still
//! true. Around every capture and every pointer read, thread C checks that
//! this login still owns the screen, unlocked, and that the bound display
//! still has the context's geometry: on a Mac each is a WindowServer round
//! trip, the session dictionary the costly one. A check that passed within
//! [`REUSE`] answers those instead, so a 60 FPS session reads WindowServer
//! about once a frame rather than four times. A failure is never reused,
//! and forgets what passed: the next check reads again, as after the
//! backend's own display mode change or restore. Every check that reads (the
//! injection boundary's, for each input, and the context check's) renews
//! what the others reuse.
//!
//! Pure, with the time passed in: built and tested on every platform.

use crate::Result;
use std::time::{Duration, Instant};

/// How long a passing check answers for capture and the pointer: about one
/// frame at 60 FPS, and one pointer sample.
pub(crate) const REUSE: Duration = Duration::from_millis(16);

/// The last passing session check (`S`), and the last geometry read (`G`),
/// each with when it was taken.
pub(crate) struct Reuse<G, S = ()> {
    session: Option<(Instant, S)>,
    geometry: Option<(Instant, G)>,
}

impl<G, S> Default for Reuse<G, S> {
    fn default() -> Self {
        Self {
            session: None,
            geometry: None,
        }
    }
}

impl<G: Copy, S: Copy> Reuse<G, S> {
    /// The session check at `now`. With `recent`, one that passed within
    /// [`REUSE`] answers, and `check` does not run; otherwise it does, and a
    /// pass is kept from `now`.
    pub(crate) fn session(
        &mut self,
        now: Instant,
        recent: bool,
        check: impl FnOnce() -> Result<S>,
    ) -> Result<S> {
        if recent
            && let Some((at, value)) = self.session
            && within(at, now)
        {
            return Ok(value);
        }
        let checked = check();
        match &checked {
            Ok(value) => self.session = Some((now, *value)),
            Err(_) => self.forget(),
        }
        checked
    }

    /// The geometry `read` gives at `now`, kept from then.
    pub(crate) fn geometry(&mut self, now: Instant, read: impl FnOnce() -> Result<G>) -> Result<G> {
        let read = read();
        match &read {
            Ok(geometry) => self.geometry = Some((now, *geometry)),
            Err(_) => self.forget(),
        }
        read
    }

    /// Whether the geometry `fits` at `now`. With `recent`, one read within
    /// [`REUSE`] that fits answers, and `read` does not run. Only a fit is
    /// reused: one that no longer fits is read again, as is any without
    /// `recent`.
    pub(crate) fn matches(
        &mut self,
        now: Instant,
        recent: bool,
        read: impl FnOnce() -> Result<G>,
        fits: impl Fn(&G) -> bool,
    ) -> Result<bool> {
        if recent
            && let Some((at, geometry)) = self.geometry
            && within(at, now)
            && fits(&geometry)
        {
            return Ok(true);
        }
        Ok(fits(&self.geometry(now, read)?))
    }

    /// Forget only the cached geometry (as when the lock state flips while
    /// the fresh session check itself just passed).
    pub(crate) fn forget_geometry(&mut self) {
        self.geometry = None;
    }

    /// Nothing is reused until a check passes again.
    pub(crate) fn forget(&mut self) {
        self.session = None;
        self.geometry = None;
    }
}

/// Whether a check taken `at` still answers at `now`.
fn within(at: Instant, now: Instant) -> bool {
    now.checked_duration_since(at)
        .is_some_and(|age| age < REUSE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::Cell, io};

    /// How often each check ran.
    #[derive(Default)]
    struct Calls {
        session: Cell<u32>,
        geometry: Cell<u32>,
    }
    impl Calls {
        fn session(&self, passes: bool) -> Result<()> {
            self.session.set(self.session.get() + 1);
            if passes {
                Ok(())
            } else {
                Err(io::Error::other("the screen is locked"))
            }
        }
        fn geometry(&self, read: Option<u32>) -> Result<u32> {
            self.geometry.set(self.geometry.get() + 1);
            read.ok_or_else(|| io::Error::other("display lost"))
        }
        fn counts(&self) -> (u32, u32) {
            (self.session.get(), self.geometry.get())
        }
    }

    /// One verify as capture and the pointer make it, or the injection
    /// boundary (`recent = false`): the session, then whether the display
    /// is still 7.
    fn verify(
        reuse: &mut Reuse<u32>,
        calls: &Calls,
        now: Instant,
        recent: bool,
        read: Option<u32>,
    ) -> Result<bool> {
        reuse.session(now, recent, || calls.session(true))?;
        reuse.matches(
            now,
            recent,
            || calls.geometry(read),
            |display| *display == 7,
        )
    }

    /// Capture and the pointer read WindowServer once in 16 ms however often
    /// they verify; the first verify reads, and so does the first after the
    /// 16 ms.
    #[test]
    fn a_passing_check_answers_capture_and_the_pointer_for_16_ms() {
        let at = Instant::now();
        let (mut reuse, calls) = (Reuse::default(), Calls::default());
        for offset in [0, 1, 8, 15] {
            let now = at + Duration::from_millis(offset);
            assert!(verify(&mut reuse, &calls, now, true, Some(7)).unwrap());
            assert!(verify(&mut reuse, &calls, now, true, Some(7)).unwrap());
        }
        let edge = at + REUSE - Duration::from_nanos(1);
        assert!(verify(&mut reuse, &calls, edge, true, Some(7)).unwrap());
        assert_eq!(calls.counts(), (1, 1));
        assert!(verify(&mut reuse, &calls, at + REUSE, true, Some(7)).unwrap());
        assert_eq!(calls.counts(), (2, 2));
        // A clock that steps back reuses nothing.
        assert!(verify(&mut reuse, &calls, at, true, Some(7)).unwrap());
        assert_eq!(calls.counts(), (3, 3));
    }

    /// The injection boundary reads every time, even right after a pass, and
    /// what it reads renews the reuse of capture and the pointer.
    #[test]
    fn input_always_reads_and_renews_the_reuse() {
        let at = Instant::now();
        let (mut reuse, calls) = (Reuse::default(), Calls::default());
        for offset in 0..4 {
            let now = at + Duration::from_millis(offset);
            assert!(verify(&mut reuse, &calls, now, false, Some(7)).unwrap());
        }
        assert_eq!(calls.counts(), (4, 4));
        // The last read, at 3 ms, answers until 19 ms.
        let late = at + Duration::from_millis(18);
        assert!(verify(&mut reuse, &calls, late, true, Some(7)).unwrap());
        assert_eq!(calls.counts(), (4, 4));
        // The context check reads the geometry fresh, and renews it alone.
        let fresh = at + Duration::from_millis(17);
        assert_eq!(
            reuse.geometry(fresh, || calls.geometry(Some(7))).unwrap(),
            7
        );
        let later = at + Duration::from_millis(30);
        reuse.session(later, true, || calls.session(true)).unwrap();
        assert!(
            reuse
                .matches(later, true, || calls.geometry(Some(7)), |d| *d == 7)
                .unwrap()
        );
        assert_eq!(calls.counts(), (5, 5));
    }

    /// A failed session check is never reused, and forgets the pass and the
    /// geometry before it: each check after it reads, until one passes.
    #[test]
    fn a_failure_is_never_reused_and_forgets_the_last_pass() {
        let at = Instant::now();
        let (mut reuse, calls) = (Reuse::default(), Calls::default());
        assert!(verify(&mut reuse, &calls, at, true, Some(7)).unwrap());
        // The injection boundary finds the screen locked a millisecond later.
        let locked = at + Duration::from_millis(1);
        assert!(
            reuse
                .session(locked, false, || calls.session(false))
                .is_err()
        );
        for offset in [2, 3] {
            let now = at + Duration::from_millis(offset);
            assert!(reuse.session(now, true, || calls.session(false)).is_err());
        }
        assert_eq!(calls.counts(), (4, 1));
        // Unlocked: the geometry the lock forgot is read again too.
        let unlocked = at + Duration::from_millis(4);
        assert!(verify(&mut reuse, &calls, unlocked, true, Some(7)).unwrap());
        assert_eq!(calls.counts(), (5, 2));

        // A geometry read that fails forgets the session too.
        let lost = at + Duration::from_millis(5);
        assert!(verify(&mut reuse, &calls, lost, false, None).is_err());
        assert_eq!(calls.counts(), (6, 3));
        let back = at + Duration::from_millis(6);
        assert!(verify(&mut reuse, &calls, back, true, Some(7)).unwrap());
        assert_eq!(calls.counts(), (7, 4));
    }

    /// After a forget, as the backend's own display mode change makes one,
    /// the next check of each kind reads, even within 16 ms of a pass.
    #[test]
    fn a_forget_leaves_nothing_to_reuse() {
        let at = Instant::now();
        let (mut reuse, calls) = (Reuse::default(), Calls::default());
        assert!(verify(&mut reuse, &calls, at, false, Some(7)).unwrap());
        reuse.forget();
        let soon = at + Duration::from_millis(1);
        assert!(verify(&mut reuse, &calls, soon, true, Some(7)).unwrap());
        assert_eq!(calls.counts(), (2, 2));
        let later = at + Duration::from_millis(2);
        assert!(verify(&mut reuse, &calls, later, true, Some(7)).unwrap());
        assert_eq!(calls.counts(), (2, 2));
    }

    /// Only a fit is reused: a display that no longer fits the context is
    /// read again, once, and a fresh read that fits answers from then on.
    #[test]
    fn a_geometry_that_no_longer_fits_is_read_again() {
        let at = Instant::now();
        let (mut reuse, calls) = (Reuse::default(), Calls::default());
        assert!(!verify(&mut reuse, &calls, at, true, Some(8)).unwrap());
        let soon = at + Duration::from_millis(1);
        assert!(verify(&mut reuse, &calls, soon, true, Some(7)).unwrap());
        assert_eq!(calls.counts(), (1, 2));
        let later = at + Duration::from_millis(2);
        assert!(verify(&mut reuse, &calls, later, true, Some(8)).unwrap());
        assert_eq!(calls.counts(), (1, 2));
    }
}
