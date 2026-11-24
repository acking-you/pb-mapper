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
    /// Since when the thread is armed, while it is.
    pub(crate) armed: Option<Instant>,
    /// Armed, and a look since found no new picture: a repeat now repeats
    /// what the display still shows.
    pub(crate) idle: bool,
}

struct State<T, P> {
    stop: bool,
    /// Since when thread C wants a picture, while it does.
    armed: Option<Instant>,
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
    picture: Option<P>,
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
}

struct Shared<T, P> {
    state: Mutex<State<T, P>>,
    /// Ends an unarmed thread's wait: an arming, or the stop.
    armed: Condvar,
}
impl<T, P> Shared<T, P> {
    /// A new picture: the newer replaces one not taken yet, as it shows
    /// the display as it is now.
    fn publish(&self, picture: P, waker: &Waker) {
        {
            let mut state = self.state.lock();
            state.picture = Some(picture);
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
}
impl<S: PresentSource> PresentWaiter<S> {
    /// Start `source`'s thread, `name`d, which wakes `waker` for what it
    /// publishes. It starts unarmed.
    pub(crate) fn spawn(name: &str, source: S, waker: Waker) -> Result<Self> {
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                stop: false,
                armed: None,
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
        })
    }

    /// The picture the thread published, if one waits.
    pub(crate) fn take(&self) -> Option<S::Picture> {
        self.shared.state.lock().picture.take()
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
    /// handing it a target first when it has none: `target` makes one, or
    /// none when its pool is empty. With `verdict`, thread C waits for the
    /// thread's look: it is woken once that found nothing new.
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
            if state.target.is_some() {
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
        let newly = state.armed.is_none();
        if newly {
            state.armed = Some(Instant::now());
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

    /// Thread C took the tick otherwise (a repeat, backpressure): the next
    /// present waits for the next arming. A look under way still publishes
    /// what it finds.
    pub(crate) fn disarm(&self) {
        self.shared.state.lock().disarm();
    }

    pub(crate) fn status(&self) -> Status {
        let state = self.shared.state.lock();
        Status {
            published: state.published(),
            armed: state.armed,
            idle: state.idle,
        }
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
/// new picture and publish it, until stopped or a failure.
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
                    break;
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
        match source.next(wait, || shared.state.lock().target.take()) {
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
    /// it repeats anyway.
    pub(crate) look: Duration,
}
impl Idle {
    /// Whether the last picture is due again at `now`: a keyframe was asked
    /// for, or `LIVENESS_INTERVAL` passed since the last submit.
    pub(crate) fn repeat_due(&self, now: Instant) -> bool {
        self.last_submit
            .is_some_and(|at| self.keyframe || now >= at + LIVENESS_INTERVAL)
    }

    /// Whether a due repeat goes now: the thread looked and found nothing
    /// new, or has not said so within `look` of its arming. Otherwise a new
    /// picture may already wait in the operating system, which the repeat
    /// would hold back a tick.
    pub(crate) fn repeat_now(&self, now: Instant) -> bool {
        self.repeat_due(now)
            && (self.status.idle || self.status.armed.is_some_and(|at| now >= at + self.look))
    }

    /// When thread C needs the source again, if not at its next tick
    /// (`None`), as of `now`, with a keyframe due at `keyframe_due`.
    ///
    /// - An encoder that had not asked for input: its request wakes thread
    ///   C, which retries `DRAIN_POLL` after that submit meanwhile.
    /// - A picture or failure published, or the thread unarmed (the tick
    ///   arms it): the tick.
    /// - Armed before the first picture: the wake, or the first picture's
    ///   deadline.
    /// - Armed with a repeat due: the tick once the thread found nothing
    ///   new, its wake or `look` after the arming before.
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
        let Some(last) = self.last_submit else {
            return Some(self.created + FIRST_SUBMIT_WAIT);
        };
        if self.repeat_due(now) {
            return (!self.repeat_now(now)).then_some(armed + self.look);
        }
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
    /// target.
    struct Stub<const MS: u64> {
        events: mpsc::Receiver<Event>,
        log: Arc<Mutex<Log>>,
        fail_start: bool,
    }
    impl<const MS: u64> PresentSource for Stub<MS> {
        type Target = Vec<u32>;
        type Picture = Vec<u32>;
        const WAIT: Duration = Duration::from_millis(MS);
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
            match self.events.recv_timeout(wait) {
                Ok(Event::Present(n)) => {
                    let mut target = take().ok_or_else(|| error("armed without a target"))?;
                    target.push(n);
                    Ok(Some(target))
                }
                Ok(Event::Pointer) | Err(mpsc::RecvTimeoutError::Timeout) => Ok(None),
                Ok(Event::Fail) => Err(error("access lost")),
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    std::thread::sleep(wait);
                    Ok(None)
                }
            }
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
    }
    impl<const MS: u64> Rig<MS> {
        fn new(fail_start: bool) -> Self {
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
            }
        }
        fn looks(&self) -> usize {
            self.log.lock().looks.len()
        }
        fn parks(&self) -> u64 {
            self.waiter.shared.state.lock().parks
        }
    }
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
        let rig = Rig::<16>::new(false);
        rig.display.send(Event::Present(1)).unwrap();
        until(|| rig.parks() > 0);
        assert_eq!(rig.looks(), 0, "unarmed, it never looks");
        assert!(rig.waiter.take().is_none());
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
        assert_eq!(rig.waiter.take(), Some(vec![1]));
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
        assert_eq!(rig.waiter.take(), Some(vec![2]));
        assert_eq!(rig.waiter.arm(false, || Ok(None)).unwrap(), Arm::Empty);
        assert_eq!(rig.waiter.status().armed, None);
    }

    /// A look that finds nothing new, or only the pointer, marks the thread
    /// idle and publishes nothing, and wakes thread C only when asked to.
    /// Disarmed, the thread ends the look under way and looks no more; a
    /// new arming looks at once again.
    #[test]
    fn a_look_that_finds_nothing_marks_the_thread_idle_and_answers_only_when_asked() {
        let rig = Rig::<5_000>::new(false);
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
        assert_eq!(rig.waiter.take(), Some(vec![7]));
    }

    /// A present found by the look under way when thread C disarmed the
    /// thread is still converted and published rather than lost: the
    /// display shows it, and no later look would find it again.
    #[test]
    fn a_present_found_after_a_disarm_is_still_published() {
        let rig = Rig::<5_000>::new(false);
        let mut made = 0;
        rig.waiter.arm(false, target(&mut made)).unwrap();
        until(|| rig.looks() == 2);
        rig.waiter.disarm();
        rig.display.send(Event::Present(3)).unwrap();
        assert!(rig.woken.wait(soon()).unwrap());
        assert_eq!(rig.waiter.take(), Some(vec![3]));
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

    /// A target or a picture left with the waiter goes with it.
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
        }
        for publish in [true, false] {
            let wake = Wake::new();
            let woken = Waiter::new(&wake).unwrap();
            let waiter =
                PresentWaiter::spawn("present-waiter-test", Holder { publish }, Waker(wake))
                    .unwrap();
            let dropped = Arc::new(AtomicBool::new(false));
            // Asked for the look's answer: woken for the picture, or for
            // the look that found nothing with the target waiting.
            let arm = waiter.arm(true, || Ok(Some(Counted(dropped.clone()))));
            assert_eq!(arm.unwrap(), Arm::Armed);
            assert!(woken.wait(soon()).unwrap());
            assert_eq!(waiter.status().published, publish);
            assert!(!dropped.load(Ordering::SeqCst));
            drop(waiter);
            assert!(dropped.load(Ordering::SeqCst), "publish={publish}");
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
        // Whatever the state, `None` leaves something to do at the tick,
        // and an instant already past never comes without it.
        for status in [Status::default(), armed, published, idle] {
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
                        let something =
                            status.published || status.armed.is_none() || rule.repeat_now(now);
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
