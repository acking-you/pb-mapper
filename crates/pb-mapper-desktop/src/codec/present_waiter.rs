//! The thread that captures for a push source, the Windows host's Desktop
//! Duplication, while thread C wants a picture.
//!
//! Thread C arms it when its tick is due and nothing new was taken. Armed,
//! it looks once for a present that already came, then waits for the next,
//! at most its source's `WAIT` a time; it converts a new picture at once
//! into the target thread C handed it, publishes it, disarms itself and
//! wakes thread C. A look that finds nothing new marks it idle: a repeat
//! now repeats what the display still shows, and thread C, when it waits
//! for that answer, is woken for it. Unarmed, it waits for thread C while
//! the operating system folds the display's presents together. A failure is
//! published, and ends it. Dropping it stops and joins it, at most
//! `STOP_WAIT`.
//!
//! A picture that waited `FRESH_FOR` or longer before thread C takes it
//! (thread C skipped ticks, every viewer paused, or a repeat went first) may
//! have a newer present behind it that nobody looked for: thread C holds it
//! back and arms the thread to look past it first. An armed thread that
//! thread C has not asked again within its source's `ASKED_WITHIN` stops
//! looking, as when its caller paused without saying so.
//!
//! Neutral, and tested everywhere with a stub source.

use crate::{
    DRAIN_POLL, Result, Waker,
    codec::pipeline::{FIRST_SUBMIT_WAIT, LIVENESS_INTERVAL},
    error,
};
use parking_lot::{Condvar, Mutex};
use std::{
    sync::{Arc, mpsc},
    thread::JoinHandle,
    time::{Duration, Instant},
};

/// How long dropping a waiter waits for its thread to end: past one of its
/// source's waits and a conversion. A thread still busy then is left behind
/// rather than holding thread C.
const STOP_WAIT: Duration = Duration::from_millis(100);

/// A published picture this old or older when thread C takes it is looked
/// past first: it waited beyond the wake its publish gave thread C, and
/// the thread, disarmed as it published, has not looked for what the
/// display presented since. Thread C's poll interval while the desktop was
/// active, which a present waited at most for when thread C polled the
/// source itself.
pub(crate) const FRESH_FOR: Duration = DRAIN_POLL;

/// How long an armed thread that thread C has not asked again goes on
/// looking: past the slowest pace's period (1 s), at which a privileged
/// helper's host asks, and past the liveness repeat, by which thread C
/// asks in process, so that only a caller that stopped asking (every viewer
/// paused, or the host went) trips it.
pub(crate) const ASKED_WITHIN: Duration = Duration::from_secs(2);
const _: () = assert!(
    ASKED_WITHIN.as_millis() > 1_000 && ASKED_WITHIN.as_millis() > LIVENESS_INTERVAL.as_millis(),
    "a caller that asks at its pace, or repeats, keeps the thread armed"
);

/// What a capture thread captures from.
pub(crate) trait PresentSource: Send + 'static {
    /// What a picture is converted into, which thread C hands over before
    /// it arms the thread.
    type Target: Send + 'static;
    /// A converted picture, which holds its target.
    type Picture: Send + 'static;
    /// The longest one look waits for a present: what bounds how long the
    /// thread takes to see that it was stopped or disarmed.
    const WAIT: Duration;
    /// How long the thread, armed, goes on looking without thread C asking
    /// again (`PresentWaiter::arm`).
    const ASKED_WITHIN: Duration = ASKED_WITHIN;
    /// On the capture thread, before anything else.
    fn start(&mut self) -> Result<()>;
    /// Wait at most `wait` for a new picture, and convert it into the
    /// target `take` hands out; `take` gives none only to a thread armed
    /// without one, which is a failure. `Ok(None)`: no new picture came,
    /// or only something else changed (the pointer).
    fn next(
        &mut self,
        wait: Duration,
        take: impl FnOnce() -> Option<Self::Target>,
    ) -> Result<Option<Self::Picture>>;
    /// `newer` replaces `older`, a picture thread C held back and never
    /// sent: what `older` covered (its presents) counts for `newer`, and
    /// `older`'s target goes.
    fn fold(older: Self::Picture, newer: &mut Self::Picture);
}

/// What one [`PresentWaiter::arm`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Arm {
    /// Armed: the thread publishes the next picture and wakes thread C.
    Armed,
    /// No target to hand over (its pool is empty): left unarmed.
    Empty,
    /// A picture or a failure waits to be taken: left unarmed.
    Published,
}

/// What the capture thread tells thread C, read at once.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Status {
    /// A picture or a failure waits to be taken.
    pub(crate) published: bool,
    /// Since when the thread is armed, while it is, or since it began
    /// converting a present it found: what thread C, waiting for its
    /// answer, waits at most `look` after.
    pub(crate) armed: Option<Instant>,
    /// Armed, and a look since found no new picture: a repeat now repeats
    /// what the display still shows.
    pub(crate) idle: bool,
    /// Thread C holds back a picture that waited, while the thread looks
    /// past it (`PresentWaiter::next_picture`).
    pub(crate) held: bool,
}
impl Status {
    /// Whether thread C, waiting for the armed thread's answer, has it or
    /// has waited long enough: a look found nothing new, or `look` passed
    /// since the arming (or since the thread began converting a present).
    pub(crate) fn answered(&self, now: Instant, look: Duration) -> bool {
        self.idle || self.armed.is_some_and(|at| now >= at + look)
    }
}

struct State<T, P> {
    stop: bool,
    /// Since when thread C wants a picture, while it does; then since the
    /// thread began converting the present it found.
    armed: Option<Instant>,
    /// When thread C last armed the thread, or found it armed.
    asked: Instant,
    /// Counts the armings, so that the thread tells a new one from the one
    /// it looked for.
    arming: u64,
    idle: bool,
    /// Thread C waits for the look's answer: wake it once it is in.
    verdict: bool,
    /// The target of the next picture: handed over by thread C, and never
    /// taken back, so that a thread that is told to stop looking while it
    /// holds a new picture still has one to convert it into.
    target: Option<T>,
    /// The picture published, and when.
    picture: Option<(P, Instant)>,
    failure: Option<std::io::Error>,
    /// The thread ended, after a failure.
    ended: bool,
    /// How often the thread waited unarmed.
    #[cfg(test)]
    parks: u64,
}
impl<T, P> State<T, P> {
    fn disarm(&mut self) {
        self.armed = None;
        self.idle = false;
        self.verdict = false;
    }

    fn published(&self) -> bool {
        self.picture.is_some() || self.failure.is_some() || self.ended
    }

    fn status(&self, held: bool) -> Status {
        Status {
            published: self.published(),
            armed: self.armed,
            idle: self.idle,
            held,
        }
    }
}

struct Shared<T, P> {
    state: Mutex<State<T, P>>,
    /// Ends an unarmed thread's wait: an arming, or the stop.
    armed: Condvar,
}
impl<T, P> Shared<T, P> {
    /// The target for a present the thread found: from now on a due repeat
    /// waits for the picture it becomes, at most `look` past now.
    fn take_target(&self) -> Option<T> {
        let mut state = self.state.lock();
        state.idle = false;
        if state.armed.is_some() {
            state.armed = Some(Instant::now());
        }
        state.target.take()
    }

    /// A new picture: the newer replaces one not taken yet, as it shows
    /// the display as it is now.
    fn publish(&self, picture: P, waker: &Waker) {
        {
            let mut state = self.state.lock();
            state.picture = Some((picture, Instant::now()));
            state.disarm();
        }
        waker.wake();
    }

    /// The look for `arming` found nothing new.
    fn looked(&self, arming: u64, waker: &Waker) {
        let wake = {
            let mut state = self.state.lock();
            if state.armed.is_none() || state.arming != arming || state.idle {
                return;
            }
            state.idle = true;
            std::mem::take(&mut state.verdict)
        };
        if wake {
            waker.wake();
        }
    }

    fn fail(&self, failure: std::io::Error, waker: &Waker) {
        {
            let mut state = self.state.lock();
            state.failure = Some(failure);
            state.ended = true;
            state.disarm();
        }
        waker.wake();
    }
}

/// A capture thread and its source, which thread C arms.
pub(crate) struct PresentWaiter<S: PresentSource> {
    shared: Arc<Shared<S::Target, S::Picture>>,
    thread: Option<JoinHandle<()>>,
    /// Disconnects once the thread is done, its source dropped on it.
    ended: mpsc::Receiver<()>,
    /// Thread C's: a published picture that waited, held back while the
    /// thread looks past it.
    held: Option<S::Picture>,
}
impl<S: PresentSource> PresentWaiter<S> {
    /// Start `source`'s thread, `name`d, which wakes `waker` for what it
    /// publishes. It starts unarmed.
    pub(crate) fn spawn(name: &str, source: S, waker: Waker) -> Result<Self> {
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                stop: false,
                armed: None,
                asked: Instant::now(),
                arming: 0,
                idle: false,
                verdict: false,
                target: None,
                picture: None,
                failure: None,
                ended: false,
                #[cfg(test)]
                parks: 0,
            }),
            armed: Condvar::new(),
        });
        let (exit, ended) = mpsc::sync_channel::<()>(0);
        let thread = {
            let shared = shared.clone();
            std::thread::Builder::new()
                .name(name.into())
                .spawn(move || {
                    run(source, &shared, &waker);
                    drop(exit);
                })?
        };
        Ok(Self {
            shared,
            thread: Some(thread),
            ended,
            held: None,
        })
    }

    /// The picture thread C submits now, as of `now`, if any.
    ///
    /// The one the thread published, unless it waited `FRESH_FOR` or
    /// longer: then it is held back (`Status::held`), and thread C arms the
    /// thread to look past it, asking for the answer. The look decides: a
    /// newer picture, which this returns with the held one folded into it;
    /// or, once the look found nothing newer, or `look` passed since the
    /// arming without an answer, the held one.
    pub(crate) fn next_picture(&mut self, now: Instant, look: Duration) -> Option<S::Picture> {
        let (published, status) = {
            let mut state = self.shared.state.lock();
            let status = state.status(self.held.is_some());
            (state.picture.take(), status)
        };
        if let Some((mut picture, at)) = published {
            // A picture published while one is held answers the look past
            // it: published later, it shows what came after.
            if let Some(older) = self.held.take() {
                S::fold(older, &mut picture);
                return Some(picture);
            }
            if now.saturating_duration_since(at) < FRESH_FOR {
                return Some(picture);
            }
            self.held = Some(picture);
            return None;
        }
        if status.held && status.answered(now, look) {
            return self.held.take();
        }
        None
    }

    /// The picture held back, whatever the look: for a thread that cannot
    /// be armed to look past it.
    pub(crate) fn take_held(&mut self) -> Option<S::Picture> {
        self.held.take()
    }

    /// The failure that ended the thread, once; then that it ended.
    pub(crate) fn failed(&self) -> Result<()> {
        let mut state = self.shared.state.lock();
        if let Some(failure) = state.failure.take() {
            return Err(failure);
        }
        if state.ended {
            return Err(error("the capture thread ended after a failure"));
        }
        Ok(())
    }

    /// Arm the thread for the next picture, unless one waits to be taken,
    /// handing it a target first when it has none and is not converting a
    /// present into the one it had: `target` makes one, or none when its
    /// pool is empty. With `verdict`, thread C waits for the thread's look:
    /// it is woken once that found nothing new.
    pub(crate) fn arm(
        &self,
        verdict: bool,
        target: impl FnOnce() -> Result<Option<S::Target>>,
    ) -> Result<Arm> {
        {
            let state = self.shared.state.lock();
            if state.published() {
                return Ok(Arm::Published);
            }
            // Armed without a target, the thread is converting the present
            // it found into the one it had, and publishes that next.
            if state.target.is_some() || state.armed.is_some() {
                return Ok(self.armed(state, verdict));
            }
        }
        // Outside the lock: only thread C hands targets over, so none
        // arrives meanwhile.
        let Some(target) = target()? else {
            return Ok(Arm::Empty);
        };
        let mut state = self.shared.state.lock();
        state.target = Some(target);
        if state.published() {
            return Ok(Arm::Published);
        }
        Ok(self.armed(state, verdict))
    }

    fn armed(
        &self,
        mut state: parking_lot::MutexGuard<'_, State<S::Target, S::Picture>>,
        verdict: bool,
    ) -> Arm {
        let now = Instant::now();
        state.asked = now;
        let newly = state.armed.is_none();
        if newly {
            state.armed = Some(now);
            state.arming += 1;
            state.idle = false;
        }
        state.verdict |= verdict;
        drop(state);
        if newly {
            self.shared.armed.notify_one();
        }
        Arm::Armed
    }

    /// Thread C took the tick otherwise (a repeat, a full encoder, every
    /// viewer paused): the next present waits for the next arming. A look
    /// under way still publishes what it finds.
    pub(crate) fn disarm(&self) {
        self.shared.state.lock().disarm();
    }

    pub(crate) fn status(&self) -> Status {
        self.shared.state.lock().status(self.held.is_some())
    }
}
impl<S: PresentSource> Drop for PresentWaiter<S> {
    /// Stop the thread and join it, at most `STOP_WAIT`; one still busy
    /// then is left behind, with its source.
    fn drop(&mut self) {
        let started = Instant::now();
        self.shared.state.lock().stop = true;
        self.shared.armed.notify_one();
        let Some(thread) = self.thread.take() else {
            return;
        };
        match self.ended.recv_timeout(STOP_WAIT) {
            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                let _ = thread.join();
                tracing::debug!(
                    target: "pb_mapper_desktop::session",
                    event = "desktop_capture_thread_joined",
                    join_us = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX),
                );
            }
            Err(mpsc::RecvTimeoutError::Timeout) => tracing::warn!(
                target: "pb_mapper_desktop::session",
                event = "desktop_capture_thread_stuck",
                "the capture thread did not stop within 100 ms; it is left behind",
            ),
        }
    }
}

/// The capture thread: start the source, then, while armed, look for each
/// new picture and publish it, until stopped or a failure. Armed and not
/// asked again within `S::ASKED_WITHIN`, it disarms itself.
fn run<S: PresentSource>(mut source: S, shared: &Shared<S::Target, S::Picture>, waker: &Waker) {
    if let Err(failure) = source.start() {
        shared.fail(failure, waker);
        return;
    }
    // The arming last looked for, and the last one found idle.
    let (mut seen, mut idle) = (0, 0);
    loop {
        let first = {
            let mut state = shared.state.lock();
            loop {
                if state.stop {
                    return;
                }
                if state.armed.is_some() {
                    if Instant::now() < state.asked + S::ASKED_WITHIN {
                        break;
                    }
                    // Thread C stopped asking: its caller paused, or went,
                    // without saying so, as a privileged helper's host does.
                    state.disarm();
                }
                #[cfg(test)]
                {
                    state.parks += 1;
                }
                shared.armed.wait(&mut state);
            }
            let first = state.arming != seen;
            seen = state.arming;
            first
        };
        // A new arming first takes what came meanwhile, without waiting.
        let wait = if first { Duration::ZERO } else { S::WAIT };
        match source.next(wait, || shared.take_target()) {
            Ok(Some(picture)) => shared.publish(picture, waker),
            Ok(None) if idle != seen => {
                idle = seen;
                shared.looked(seen, waker);
            }
            Ok(None) => {}
            Err(failure) => {
                shared.fail(failure, waker);
                return;
            }
        }
    }
}

/// What thread C knows of a source whose pictures a [`PresentWaiter`]
/// captures, to decide when it needs that source again.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Idle {
    /// The last submit found the encoder without an input request then;
    /// and whether a request has since come that wakes thread C.
    pub(crate) waiting: Option<(Instant, bool)>,
    pub(crate) status: Status,
    pub(crate) created: Instant,
    /// The last submit of a picture, new or repeated; `None` before the
    /// first.
    pub(crate) last_submit: Option<Instant>,
    /// A keyframe was asked for and not sent yet.
    pub(crate) keyframe: bool,
    /// How long after arming the thread thread C waits for its look before
    /// it repeats anyway, or sends what it held back.
    pub(crate) look: Duration,
}
impl Idle {
    /// Whether the last picture is due again at `now`: a keyframe was asked
    /// for, or `LIVENESS_INTERVAL` passed since the last submit.
    pub(crate) fn repeat_due(&self, now: Instant) -> bool {
        self.last_submit
            .is_some_and(|at| self.keyframe || now >= at + LIVENESS_INTERVAL)
    }

    /// Whether thread C waits for the armed thread's look: it holds a
    /// picture back, or a repeat is due.
    pub(crate) fn wants_answer(&self, now: Instant) -> bool {
        self.status.held || self.repeat_due(now)
    }

    /// Whether a due repeat goes now: nothing is held back, which goes
    /// instead, and the thread looked and found nothing new, or has not
    /// said so within `look` of its arming. Otherwise a new picture may
    /// already wait in the operating system, which the repeat would hold
    /// back a tick.
    pub(crate) fn repeat_now(&self, now: Instant) -> bool {
        !self.status.held && self.repeat_due(now) && self.status.answered(now, self.look)
    }

    /// When thread C needs the source again, if not at its next tick
    /// (`None`), as of `now`, with a keyframe due at `keyframe_due`.
    ///
    /// - An encoder that had not asked for input: its request wakes thread
    ///   C, which retries `DRAIN_POLL` after that submit meanwhile.
    /// - A picture or failure published, or the thread unarmed (the tick
    ///   arms it): the tick.
    /// - Armed with a picture held back or a repeat due: the tick once the
    ///   thread found nothing new, its wake or `look` after the arming
    ///   before.
    /// - Armed before the first picture: the wake, or the first picture's
    ///   deadline.
    /// - Otherwise the wake, or the next repeat: liveness, or the keyframe
    ///   due.
    pub(crate) fn until(&self, now: Instant, keyframe_due: Option<Instant>) -> Option<Instant> {
        if let Some((asked, requested)) = self.waiting {
            return Some(if requested { asked } else { asked + DRAIN_POLL });
        }
        if self.status.published {
            return None;
        }
        let armed = self.status.armed?;
        if self.wants_answer(now) {
            return (!self.status.answered(now, self.look)).then_some(armed + self.look);
        }
        let Some(last) = self.last_submit else {
            return Some(self.created + FIRST_SUBMIT_WAIT);
        };
        let liveness = last + LIVENESS_INTERVAL;
        Some(keyframe_due.map_or(liveness, |due| due.min(liveness)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scheduler::{Waiter, Wake};
    use std::{
        sync::atomic::{AtomicBool, Ordering},
        thread::ThreadId,
    };

    /// What the stub display does next.
    enum Event {
        Present(u32),
        /// A present whose conversion takes this long.
        Slow(u32, Duration),
        Pointer,
        Fail,
    }

    /// What the stub saw: the thread it started and was dropped on, and
    /// each look's wait, logged as the look begins.
    #[derive(Default)]
    struct Log {
        started: Option<ThreadId>,
        looks: Vec<Duration>,
        dropped: Option<ThreadId>,
    }

    /// A display whose presents a test sends, a look waiting at most `MS`
    /// for each; a present is converted by writing its number into the
    /// target, and a held picture folds into the next by going before it.
    struct Stub<const MS: u64> {
        events: mpsc::Receiver<Event>,
        log: Arc<Mutex<Log>>,
        fail_start: bool,
    }
    impl<const MS: u64> PresentSource for Stub<MS> {
        type Target = Vec<u32>;
        type Picture = Vec<u32>;
        const WAIT: Duration = Duration::from_millis(MS);
        const ASKED_WITHIN: Duration = Duration::from_millis(MS * 8);
        fn start(&mut self) -> Result<()> {
            self.log.lock().started = Some(std::thread::current().id());
            if self.fail_start {
                return Err(error("no desktop"));
            }
            Ok(())
        }
        fn next(
            &mut self,
            wait: Duration,
            take: impl FnOnce() -> Option<Vec<u32>>,
        ) -> Result<Option<Vec<u32>>> {
            self.log.lock().looks.push(wait);
            let (present, converting) = match self.events.recv_timeout(wait) {
                Ok(Event::Present(n)) => (n, Duration::ZERO),
                Ok(Event::Slow(n, converting)) => (n, converting),
                Ok(Event::Pointer) | Err(mpsc::RecvTimeoutError::Timeout) => return Ok(None),
                Ok(Event::Fail) => return Err(error("access lost")),
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    std::thread::sleep(wait);
                    return Ok(None);
                }
            };
            let mut target = take().ok_or_else(|| error("armed without a target"))?;
            std::thread::sleep(converting);
            target.push(present);
            Ok(Some(target))
        }
        fn fold(older: Vec<u32>, newer: &mut Vec<u32>) {
            newer.splice(0..0, older);
        }
    }
    impl<const MS: u64> Drop for Stub<MS> {
        fn drop(&mut self) {
            self.log.lock().dropped = Some(std::thread::current().id());
        }
    }

    struct Rig<const MS: u64> {
        waiter: PresentWaiter<Stub<MS>>,
        display: mpsc::Sender<Event>,
        log: Arc<Mutex<Log>>,
        woken: Waiter,
        /// Before anything was published: thread C taking a picture as of
        /// then takes it the moment its publish woke it.
        start: Instant,
    }
    impl<const MS: u64> Rig<MS> {
        fn new(fail_start: bool) -> Self {
            let start = Instant::now();
            let (display, events) = mpsc::channel();
            let log = Arc::new(Mutex::new(Log::default()));
            let wake = Wake::new();
            let woken = Waiter::new(&wake).unwrap();
            let stub = Stub {
                events,
                log: log.clone(),
                fail_start,
            };
            let waiter = PresentWaiter::spawn("present-waiter-test", stub, Waker(wake)).unwrap();
            Self {
                waiter,
                display,
                log,
                woken,
                start,
            }
        }
        fn looks(&self) -> usize {
            self.log.lock().looks.len()
        }
        fn parks(&self) -> u64 {
            self.waiter.shared.state.lock().parks
        }
        /// What thread C takes as its publish wakes it.
        fn take(&mut self) -> Option<Vec<u32>> {
            self.waiter.next_picture(self.start, LOOK)
        }
    }
    const LOOK: Duration = Duration::from_secs(1);
    fn soon() -> Instant {
        Instant::now() + Duration::from_secs(5)
    }
    /// Wait for `done`, which another thread makes true.
    fn until(done: impl Fn() -> bool) {
        let deadline = soon();
        while !done() {
            assert!(Instant::now() < deadline, "timed out");
            std::thread::yield_now();
        }
    }
    /// A target, counting how many were made.
    fn target(made: &mut u32) -> impl FnOnce() -> Result<Option<Vec<u32>>> + '_ {
        move || {
            *made += 1;
            Ok(Some(Vec::new()))
        }
    }

    /// Unarmed, the thread never looks, and a present waits; armed, its
    /// first look takes that present at once, converts it into the target
    /// thread C handed over, publishes it, disarms and wakes thread C. Each
    /// picture takes a new target; none is made while one waits, nor while
    /// a picture waits to be taken, and an empty pool leaves it unarmed.
    #[test]
    fn an_armed_thread_publishes_the_next_present_and_wakes_thread_c() {
        let mut rig = Rig::<16>::new(false);
        rig.display.send(Event::Present(1)).unwrap();
        until(|| rig.parks() > 0);
        assert_eq!(rig.looks(), 0, "unarmed, it never looks");
        assert!(rig.take().is_none());
        assert_eq!(rig.waiter.status(), Status::default());

        let mut made = 0;
        assert_eq!(
            rig.waiter.arm(false, target(&mut made)).unwrap(),
            Arm::Armed
        );
        assert!(rig.woken.wait(soon()).unwrap());
        let status = rig.waiter.status();
        assert!(status.published && status.armed.is_none());
        assert_eq!(rig.log.lock().looks, [Duration::ZERO]);
        assert_eq!(
            rig.waiter.arm(false, target(&mut made)).unwrap(),
            Arm::Published
        );
        assert_eq!(made, 1);
        assert_eq!(rig.take(), Some(vec![1]));
        assert!(!rig.waiter.status().published);
        rig.waiter.failed().unwrap();

        assert_eq!(
            rig.waiter.arm(false, target(&mut made)).unwrap(),
            Arm::Armed
        );
        assert_eq!(
            rig.waiter.arm(false, target(&mut made)).unwrap(),
            Arm::Armed
        );
        assert_eq!(made, 2);
        rig.display.send(Event::Present(2)).unwrap();
        assert!(rig.woken.wait(soon()).unwrap());
        assert_eq!(rig.take(), Some(vec![2]));
        assert_eq!(rig.waiter.arm(false, || Ok(None)).unwrap(), Arm::Empty);
        assert_eq!(rig.waiter.status().armed, None);
    }

    /// A look that finds nothing new, or only the pointer, marks the thread
    /// idle and publishes nothing, and wakes thread C only when asked to.
    /// Disarmed, the thread ends the look under way and looks no more; a
    /// new arming looks at once again.
    #[test]
    fn a_look_that_finds_nothing_marks_the_thread_idle_and_answers_only_when_asked() {
        let mut rig = Rig::<5_000>::new(false);
        let mut made = 0;
        rig.display.send(Event::Pointer).unwrap();
        until(|| rig.parks() > 0);
        rig.waiter.arm(false, target(&mut made)).unwrap();
        until(|| rig.waiter.status().idle);
        let status = rig.waiter.status();
        assert!(!status.published && status.armed.is_some());
        assert!(!rig.woken.wait(Instant::now()).unwrap(), "nobody asked");
        // Armed on, it waits a whole look at a time.
        until(|| rig.looks() == 2);
        assert_eq!(
            rig.log.lock().looks,
            [Duration::ZERO, Duration::from_secs(5)]
        );

        rig.waiter.disarm();
        assert_eq!(rig.waiter.status(), Status::default());
        rig.display.send(Event::Pointer).unwrap();
        until(|| rig.parks() > 1);
        assert_eq!(rig.looks(), 2, "disarmed, it looks no more");

        // Asked for the answer, the new arming's first look wakes thread C.
        rig.waiter.arm(true, target(&mut made)).unwrap();
        assert!(rig.woken.wait(soon()).unwrap());
        assert!(rig.waiter.status().idle);
        assert_eq!(rig.log.lock().looks.get(2), Some(&Duration::ZERO));
        assert_eq!(made, 1, "the target waits for the next picture");
        rig.display.send(Event::Present(7)).unwrap();
        assert!(rig.woken.wait(soon()).unwrap());
        assert_eq!(rig.take(), Some(vec![7]));
    }

    /// A present found by the look under way when thread C disarmed the
    /// thread is still converted and published rather than lost: the
    /// display shows it, and no later look would find it again.
    #[test]
    fn a_present_found_after_a_disarm_is_still_published() {
        let mut rig = Rig::<5_000>::new(false);
        let mut made = 0;
        rig.waiter.arm(false, target(&mut made)).unwrap();
        until(|| rig.looks() == 2);
        rig.waiter.disarm();
        rig.display.send(Event::Present(3)).unwrap();
        assert!(rig.woken.wait(soon()).unwrap());
        assert_eq!(rig.take(), Some(vec![3]));
        assert_eq!(made, 1);
    }

    /// A picture that waited `FRESH_FOR` before thread C takes it (skipped
    /// ticks, a pause) is held back while the thread, armed again, looks
    /// past it: with nothing newer it goes once that look answers, and a
    /// newer present goes instead, carrying the held picture's presents.
    #[test]
    fn a_picture_that_waited_goes_only_once_the_thread_looked_past_it() {
        /// Arm the thread, and take the present it publishes `FRESH_FOR`
        /// after the wake.
        fn stale(rig: &mut Rig<5_000>, made: &mut u32, present: u32) {
            rig.waiter.arm(false, target(made)).unwrap();
            rig.display.send(Event::Present(present)).unwrap();
            assert!(rig.woken.wait(soon()).unwrap());
            let late = Instant::now() + FRESH_FOR;
            assert_eq!(rig.waiter.next_picture(late, LOOK), None);
            let status = rig.waiter.status();
            assert!(status.held && !status.published && status.armed.is_none());
            // Unarmed, nothing answers.
            assert_eq!(rig.waiter.next_picture(late + LOOK, LOOK), None);
        }
        let mut rig = Rig::<5_000>::new(false);
        let mut made = 0;

        // Nothing newer: the look's answer wakes thread C, which takes the
        // held picture, then nothing.
        stale(&mut rig, &mut made, 1);
        rig.waiter.arm(true, target(&mut made)).unwrap();
        assert!(rig.woken.wait(soon()).unwrap());
        let status = rig.waiter.status();
        assert!(status.held && status.idle && !status.published);
        assert_eq!(rig.take(), Some(vec![1]));
        assert_eq!(rig.take(), None);
        assert!(!rig.waiter.status().held);

        // A newer present came meanwhile: the look takes it at once, and it
        // goes in the held one's place, however late it is taken.
        rig.waiter.disarm();
        stale(&mut rig, &mut made, 2);
        rig.display.send(Event::Present(3)).unwrap();
        rig.waiter.arm(true, target(&mut made)).unwrap();
        assert!(rig.woken.wait(soon()).unwrap());
        let late = Instant::now() + FRESH_FOR;
        assert_eq!(rig.waiter.next_picture(late, LOOK), Some(vec![2, 3]));
        assert_eq!(rig.waiter.status(), Status::default());

        // A held picture the thread cannot look past goes as it is.
        stale(&mut rig, &mut made, 4);
        assert_eq!(rig.waiter.take_held(), Some(vec![4]));
        assert!(!rig.waiter.status().held);
    }

    /// A present the thread found and is converting keeps a due repeat
    /// waiting: the thread is no longer idle, and `look` counts from the
    /// conversion's start, not from the arming long before. No second
    /// target is handed to a thread converting into its own.
    #[test]
    fn a_due_repeat_waits_for_the_present_the_thread_is_converting() {
        let mut rig = Rig::<5_000>::new(false);
        let mut made = 0;
        rig.waiter.arm(false, target(&mut made)).unwrap();
        until(|| rig.waiter.status().idle);
        let armed = rig.waiter.status().armed.unwrap();
        until(|| rig.looks() == 2);
        let look = Duration::from_millis(200);
        let converting = Instant::now();
        rig.display
            .send(Event::Slow(5, Duration::from_millis(500)))
            .unwrap();
        until(|| !rig.waiter.status().idle);
        let status = rig.waiter.status();
        let since = status.armed.unwrap();
        assert!(since > armed && since >= converting, "{status:?}");
        assert!(!status.answered(since, look));
        let repeat = Idle {
            waiting: None,
            status,
            created: armed,
            last_submit: Some(armed),
            keyframe: true,
            look,
        };
        assert!(repeat.repeat_due(since) && !repeat.repeat_now(since));
        assert_eq!(repeat.until(since, None), Some(since + look));
        // Asked again meanwhile, it is armed, with no new target.
        assert_eq!(rig.waiter.arm(true, || Ok(None)).unwrap(), Arm::Armed);
        assert!(rig.woken.wait(soon()).unwrap());
        assert_eq!(rig.take(), Some(vec![5]));
        assert_eq!(made, 1);
    }

    /// An armed thread that thread C does not ask again within its
    /// source's `ASKED_WITHIN` stops looking, as a paused host behind the
    /// helper never says it paused; the next arming looks at once again.
    #[test]
    fn an_armed_thread_not_asked_again_stops_looking() {
        let rig = Rig::<16>::new(false);
        let mut made = 0;
        until(|| rig.parks() > 0);
        rig.waiter.arm(false, target(&mut made)).unwrap();
        until(|| rig.looks() >= 2);
        until(|| rig.parks() > 1);
        assert_eq!(rig.waiter.status(), Status::default());
        let looks = rig.looks();
        assert!(
            looks as u128 <= 2 + Stub::<16>::ASKED_WITHIN.as_millis() / 16,
            "{looks}"
        );
        rig.waiter.arm(false, target(&mut made)).unwrap();
        until(|| rig.looks() > looks);
        assert_eq!(rig.log.lock().looks.get(looks), Some(&Duration::ZERO));
        assert_eq!(made, 1);
    }

    /// A failure is published and wakes thread C, ends the thread, and
    /// fails every later call; a source that cannot start fails at once,
    /// unarmed.
    #[test]
    fn a_failure_is_published_once_and_every_later_call_fails() {
        let rig = Rig::<16>::new(false);
        let mut made = 0;
        rig.waiter.arm(false, target(&mut made)).unwrap();
        rig.display.send(Event::Fail).unwrap();
        assert!(rig.woken.wait(soon()).unwrap());
        assert!(rig.waiter.status().published);
        assert_eq!(rig.waiter.failed().unwrap_err().to_string(), "access lost");
        assert!(rig.waiter.failed().is_err());
        assert_eq!(
            rig.waiter.arm(false, target(&mut made)).unwrap(),
            Arm::Published
        );
        until(|| rig.log.lock().dropped.is_some());

        let unstarted = Rig::<16>::new(true);
        assert!(unstarted.woken.wait(soon()).unwrap());
        assert_eq!(
            unstarted.waiter.failed().unwrap_err().to_string(),
            "no desktop"
        );
        assert_eq!(unstarted.looks(), 0);
    }

    /// Dropping the waiter stops its thread, armed or not, and joins it:
    /// its source is dropped on that thread before the drop returns.
    #[test]
    fn dropping_the_waiter_stops_and_joins_its_thread() {
        for armed in [false, true] {
            let rig = Rig::<16>::new(false);
            until(|| rig.parks() > 0);
            if armed {
                let mut made = 0;
                rig.waiter.arm(false, target(&mut made)).unwrap();
                until(|| rig.looks() >= 2);
            }
            drop(rig.waiter);
            let log = rig.log.lock();
            let ran = log.started.unwrap();
            assert_eq!(log.dropped, Some(ran), "joined, its source dropped on it");
            assert_ne!(ran, std::thread::current().id());
        }
    }

    /// A target, a picture, or a picture held back, left with the waiter
    /// goes with it.
    #[test]
    fn what_waits_with_the_waiter_goes_with_it() {
        struct Counted(Arc<AtomicBool>);
        impl Drop for Counted {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        /// Converts a present at its first look when `publish`, and finds
        /// nothing otherwise.
        struct Holder {
            publish: bool,
        }
        impl PresentSource for Holder {
            type Target = Counted;
            type Picture = Counted;
            const WAIT: Duration = Duration::from_millis(1);
            fn start(&mut self) -> Result<()> {
                Ok(())
            }
            fn next(
                &mut self,
                wait: Duration,
                take: impl FnOnce() -> Option<Counted>,
            ) -> Result<Option<Counted>> {
                if std::mem::take(&mut self.publish) {
                    return Ok(take());
                }
                std::thread::sleep(wait);
                Ok(None)
            }
            fn fold(_: Counted, _: &mut Counted) {}
        }
        for (publish, hold) in [(true, false), (true, true), (false, false)] {
            let wake = Wake::new();
            let woken = Waiter::new(&wake).unwrap();
            let mut waiter =
                PresentWaiter::spawn("present-waiter-test", Holder { publish }, Waker(wake))
                    .unwrap();
            let dropped = Arc::new(AtomicBool::new(false));
            // Asked for the look's answer: woken for the picture, or for
            // the look that found nothing with the target waiting.
            let arm = waiter.arm(true, || Ok(Some(Counted(dropped.clone()))));
            assert_eq!(arm.unwrap(), Arm::Armed);
            assert!(woken.wait(soon()).unwrap());
            assert_eq!(waiter.status().published, publish);
            if hold {
                let late = Instant::now() + FRESH_FOR;
                assert!(waiter.next_picture(late, LOOK).is_none());
                assert!(waiter.status().held);
            }
            assert!(!dropped.load(Ordering::SeqCst));
            drop(waiter);
            assert!(
                dropped.load(Ordering::SeqCst),
                "publish={publish} hold={hold}"
            );
        }
    }

    /// Thread C's rule for when it needs the source: never `None` while
    /// nothing can be done at its tick, so it never spins, and never later
    /// than the wake, the liveness repeat or a keyframe it waits for.
    #[test]
    fn thread_c_comes_back_at_the_tick_only_when_there_is_something_to_do() {
        let t0 = Instant::now();
        let ms = Duration::from_millis;
        let look = ms(32);
        let armed = Status {
            armed: Some(t0 + ms(330)),
            ..Status::default()
        };
        let base = Idle {
            waiting: None,
            status: armed,
            created: t0,
            last_submit: Some(t0 + ms(90)),
            keyframe: false,
            look,
        };
        let at = |idle: Idle, now: Duration| idle.until(t0 + now, None);

        // An encoder without an input request: a poll until it asks, then
        // at once.
        let waiting = |requested| Idle {
            waiting: Some((t0 + ms(120), requested)),
            ..base
        };
        assert_eq!(at(waiting(false), ms(120)), Some(t0 + ms(120) + DRAIN_POLL));
        assert_eq!(at(waiting(true), ms(121)), Some(t0 + ms(120)));
        // Something to take, or an unarmed thread to arm: the tick.
        let published = Status {
            published: true,
            ..armed
        };
        let with = |status| Idle { status, ..base };
        assert_eq!(at(with(published), ms(120)), None);
        assert_eq!(at(with(Status::default()), ms(120)), None);
        // Armed before the first picture: the first picture's deadline.
        let first = Idle {
            last_submit: None,
            keyframe: true,
            ..base
        };
        assert_eq!(at(first, ms(120)), Some(t0 + FIRST_SUBMIT_WAIT));
        assert!(!first.repeat_now(t0 + ms(5_000)));
        // Armed, nothing due: liveness, or a keyframe due sooner.
        assert_eq!(at(base, ms(120)), Some(t0 + ms(90) + LIVENESS_INTERVAL));
        assert_eq!(
            base.until(t0 + ms(120), Some(t0 + ms(150))),
            Some(t0 + ms(150))
        );
        assert!(!base.repeat_due(t0 + ms(120)));
        // A repeat due: the look's answer first, `look` after the arming at
        // the latest.
        let due = ms(90) + LIVENESS_INTERVAL;
        assert!(base.repeat_due(t0 + due) && !base.repeat_now(t0 + due));
        assert_eq!(at(base, due), Some(t0 + ms(330) + look));
        assert!(base.repeat_now(t0 + ms(330) + look));
        assert_eq!(at(base, ms(330) + look), None);
        let idle = Status {
            idle: true,
            ..armed
        };
        assert!(with(idle).repeat_now(t0 + due));
        assert_eq!(at(with(idle), due), None);
        // A keyframe asked for is a repeat due now.
        let keyframe = Idle {
            keyframe: true,
            ..base
        };
        assert_eq!(at(keyframe, ms(120)), Some(t0 + ms(330) + look));
        let answered = Idle {
            status: idle,
            ..keyframe
        };
        assert_eq!(at(answered, ms(120)), None);
        // A picture held back waits for the look's answer as a repeat
        // does, before the first picture too, and goes instead of one.
        let held = Status {
            held: true,
            ..armed
        };
        for last_submit in [None, Some(t0 + ms(90))] {
            let rule = Idle {
                status: held,
                last_submit,
                ..keyframe
            };
            assert!(rule.wants_answer(t0 + ms(120)));
            assert_eq!(at(rule, ms(120)), Some(t0 + ms(330) + look));
            assert_eq!(at(rule, ms(330) + look), None);
            assert!(!rule.repeat_now(t0 + ms(330) + look));
        }
        assert_eq!(at(with(Status { idle: true, ..held }), ms(120)), None);
        // Whatever the state, `None` leaves something to do at the tick,
        // and an instant already past never comes without it.
        let statuses = [Status::default(), armed, published, idle]
            .map(|status| [false, true].map(|held| Status { held, ..status }));
        for status in statuses.into_iter().flatten() {
            for last_submit in [None, Some(t0 + ms(90))] {
                for keyframe in [false, true] {
                    let rule = Idle {
                        status,
                        last_submit,
                        keyframe,
                        ..base
                    };
                    for now in [ms(95), ms(120), due, ms(330) + look, ms(400)] {
                        let now = t0 + now;
                        let something = status.published
                            || status.armed.is_none()
                            || rule.repeat_now(now)
                            || (status.held && status.answered(now, look));
                        match rule.until(now, None) {
                            None => assert!(something, "{rule:?} at {now:?}"),
                            Some(until) => assert!(until > now || something, "{rule:?}"),
                        }
                    }
                }
            }
        }
    }
}
