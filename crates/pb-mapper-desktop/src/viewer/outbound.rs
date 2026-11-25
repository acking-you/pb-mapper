//! What the viewer sends, in priority order. Producers (the read task, the
//! native consumer through the sink, the embedding app) store the latest value
//! of each kind and wake the write task; the write task takes one message per
//! pick, so a flood of one kind never delays a more urgent one:
//!
//! 0. a standby's `Resume`, which promotes it, before anything else;
//! 1. control barriers: revoke, then release, then grant;
//! 2. the latest probe echo;
//! 3. the latest transport receipt (cumulative);
//! 4. input, in order, preceded by the presentation it references;
//! 5. pause or resume;
//! 6. the latest presentation, at most every `PRESENTED_SEND_MIN_INTERVAL`;
//! 7. the latest keyframe request, fps cap, quality, path and display mode;
//! 8. a ping after `PING_IDLE` without any other message.
//!
//! For a host that listed `arrival_time`, a probe echo carries when the
//! reader finished the probe, and a receipt when it finished its newest unit,
//! payload included, on the session's clock;
//! for one that listed `keyframe_seq`, a keyframe request made while
//! predicted frames are thrown away carries the last seq the reader read.
//! Receipts outrank keyframe requests, so by the time one is written the
//! reader may have read on, and the host needs the seq to tell whether the
//! IDR it sent last came after the request.
use super::InputError;
use crate::{
    Input, InputEvent, KeyframeReason, QualityPreset,
    input::SCROLL_UNITS_MAX,
    modes::Mode,
    protocol::{Route, UnlockSecret, ViewerMessage},
};
use std::{
    collections::VecDeque,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::{Duration, Instant},
};

/// Presentations are throttled to this interval, except that one is always
/// written before input that references it.
pub(crate) const PRESENTED_SEND_MIN_INTERVAL: Duration = Duration::from_millis(100);
/// The largest burst any UI path submits between two frames is 288 events of
/// about 100 KB; a full queue therefore means a stalled uplink with input
/// still arriving.
pub(crate) const INPUT_QUEUE: usize = 512;
/// Encoded `Input` JSON queued at most.
pub(crate) const INPUT_QUEUE_BYTES: usize = 256 * 1024;
/// Inputs per `Viewer::input` call.
pub(crate) const INPUT_BATCH_MAX: usize = 16;
/// A ping is written only after this long without any other message.
pub(crate) const PING_IDLE: Duration = Duration::from_secs(1);
const _: () = assert!(
    INPUT_BATCH_MAX <= INPUT_QUEUE,
    "one full batch fits an empty queue"
);
const _: () = assert!(
    crate::lane::LANE_CAPACITY >= INPUT_QUEUE,
    "the host's lane takes a whole viewer queue during one backend call"
);
const _: () = assert!(
    PING_IDLE.as_nanos() < crate::session::HOST_READ_TIMEOUT.as_nanos(),
    "an idle viewer writes before the host reads its silence as a dead transport"
);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Barrier {
    Revoke,
    Release,
    Grant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Lifecycle {
    Pause,
    Resume,
}

/// A change of the host display's mode, or the way back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ModeRequest {
    Set(Mode),
    Restore,
}

// Releases are state, not a queue of gestures. Repeated focus/pointer-cancel
// notifications must not disconnect video. A revoke always reaches the peer
// before a subsequent grant, and every barrier discards obsolete input.
#[derive(Default)]
struct Controls {
    revoke: bool,
    release: bool,
    grant: bool,
    /// Input below this sequence was submitted before the latest barrier,
    /// which makes it obsolete; input submitted since is not.
    obsolete_below: u64,
}
impl Controls {
    fn push(&mut self, barrier: Barrier) {
        match barrier {
            Barrier::Revoke => {
                self.revoke = true;
                self.grant = false;
                self.release = false;
            }
            Barrier::Grant => self.grant = true,
            Barrier::Release => self.release = true,
        }
    }
    fn take(&mut self) -> Option<ViewerMessage> {
        if std::mem::take(&mut self.revoke) {
            Some(ViewerMessage::Control { enabled: false })
        } else if std::mem::take(&mut self.release) {
            Some(ViewerMessage::Release {})
        } else if std::mem::take(&mut self.grant) {
            Some(ViewerMessage::Control { enabled: true })
        } else {
            None
        }
    }
}

#[derive(Default)]
struct Slots {
    /// The session's start, which arrival times count from; `None` unless
    /// the host listed `arrival_time`.
    arrival_epoch: Option<Instant>,
    /// The host listed `keyframe_seq`.
    keyframe_seq: bool,
    /// The newest probe's nonce, and when it was read.
    probe: Option<(u64, Instant)>,
    /// The newest seq read, and when it was read whole.
    receipt: Option<(u64, Instant)>,
    /// The newest seq read, kept once its receipt is written.
    read_seq: u64,
    /// The newest presentation the sink accepted.
    presented: Option<(u64, u64)>,
    /// The last one written, and when.
    presented_sent: Option<(u64, u64, Instant)>,
    lifecycle: Option<Lifecycle>,
    /// The newest request, and the seq it follows while predicted frames
    /// are thrown away.
    keyframe: Option<(KeyframeReason, Option<u64>)>,
    fps_cap: Option<u32>,
    quality: Option<QualityPreset>,
    route: Option<Route>,
    display_mode: Option<ModeRequest>,
    unlock: Option<UnlockSecret>,
}
impl Slots {
    /// Whether `(generation, frame_id)` is newer than the last presentation written.
    fn unsent(&self, (generation, frame_id): (u64, u64)) -> bool {
        self.presented_sent
            .is_none_or(|(sent_generation, sent, _)| {
                (generation, frame_id) > (sent_generation, sent)
            })
    }
}

struct Queued {
    event: InputEvent,
    /// Its encoded `Input` JSON.
    len: usize,
}
#[derive(Default)]
struct InputQueue {
    events: VecDeque<Queued>,
    bytes: usize,
}

/// The write task's next step.
pub(crate) enum Next {
    Send(ViewerMessage),
    /// Nothing is due before this instant unless a producer wakes the writer.
    Idle(Instant),
}

pub(crate) struct Outbound {
    controls: parking_lot::Mutex<Controls>,
    slots: parking_lot::Mutex<Slots>,
    notify: tokio::sync::Notify,
    inputs: parking_lot::Mutex<InputQueue>,
    /// Per viewer, stamped here; the embedding app owns none. A session
    /// that takes another's place goes on from where it stood: the host
    /// counts nothing below it.
    sequence: AtomicU64,
    /// A standby's `Resume`, which promotes it: written before anything else.
    promote: AtomicBool,
}
impl Outbound {
    /// `arrival_epoch`: stamp receipts and probe echoes with arrival times
    /// since then. `keyframe_seq`: name the seq a keyframe request follows.
    pub fn new(arrival_epoch: Option<Instant>, keyframe_seq: bool) -> Self {
        Self {
            controls: parking_lot::Mutex::default(),
            slots: parking_lot::Mutex::new(Slots {
                arrival_epoch,
                keyframe_seq,
                ..Slots::default()
            }),
            notify: tokio::sync::Notify::new(),
            inputs: parking_lot::Mutex::default(),
            sequence: AtomicU64::new(1),
            promote: AtomicBool::new(false),
        }
    }

    /// The session moved to a standby's connection, whose host session
    /// knows nothing this one queued: every message not yet written is
    /// dropped, queued input included, and the next one written is the
    /// `Resume` that promotes it. The terms are its session's.
    pub fn restart(&self, arrival_epoch: Option<Instant>, keyframe_seq: bool) {
        {
            let mut queue = self.inputs.lock();
            queue.events.clear();
            queue.bytes = 0;
        }
        *self.controls.lock() = Controls {
            obsolete_below: self.sequence.load(Ordering::Relaxed),
            ..Controls::default()
        };
        *self.slots.lock() = Slots {
            arrival_epoch,
            keyframe_seq,
            ..Slots::default()
        };
        self.promote.store(true, Ordering::Release);
        self.notify.notify_one();
    }

    /// Wake the writer: its connection changed role.
    pub fn wake(&self) {
        self.notify.notify_one();
    }

    /// Wake every writer waiting here now, storing nothing: one whose
    /// connection was retired then waits elsewhere, and no later wake is
    /// spent on it.
    pub fn wake_all(&self) {
        self.notify.notify_waiters();
    }

    /// Answer here the probe `other` took and has not answered: the host
    /// sends no other until it hears this one.
    pub fn adopt_probe(&self, other: &Outbound) {
        let probe = other.slots.lock().probe.take();
        if let Some(probe) = probe {
            self.update(|slots| slots.probe = Some(probe));
        }
    }

    /// `at` in microseconds since the session began, for a host that reads it.
    fn stamp(slots: &Slots, at: Instant) -> Option<u64> {
        slots.arrival_epoch.map(|epoch| {
            pb_mapper_core::time::saturating_micros(at.saturating_duration_since(epoch))
        })
    }

    pub fn notified(&self) -> tokio::sync::futures::Notified<'_> {
        self.notify.notified()
    }

    fn update(&self, change: impl FnOnce(&mut Slots)) {
        change(&mut self.slots.lock());
        self.notify.notify_one();
    }

    pub fn barrier(&self, barrier: Barrier) {
        self.push_barrier(barrier);
        self.notify.notify_one();
    }
    /// Queue `barrier`, which makes the input submitted so far obsolete. A
    /// submit that this call happens before stamps a later sequence.
    fn push_barrier(&self, barrier: Barrier) {
        let mut controls = self.controls.lock();
        controls.push(barrier);
        controls.obsolete_below = self.sequence.load(Ordering::Relaxed);
    }
    /// The probe `nonce`, read at `at`.
    pub fn probe(&self, nonce: u64, at: Instant) {
        self.update(|slots| slots.probe = Some((nonce, at)));
    }
    /// Cumulative: every video record up to `seq` was read, `seq`'s whole
    /// by `at`.
    pub fn receipt(&self, seq: u64, at: Instant) {
        self.update(|slots| {
            if slots.receipt.is_none_or(|(old, _)| seq > old) {
                slots.receipt = Some((seq, at));
            }
            slots.read_seq = slots.read_seq.max(seq);
        });
    }
    /// Only the sink calls this, for a presentation it validated.
    pub fn presented(&self, generation: u64, frame_id: u64) {
        self.update(|slots| slots.presented = Some((generation, frame_id)));
    }
    pub fn lifecycle(&self, lifecycle: Lifecycle) {
        self.update(|slots| slots.lifecycle = Some(lifecycle));
    }
    /// Only the sink's keyframe limiter calls this. `discarding`: predicted
    /// frames are of no use before the next IDR, so the request names the
    /// last seq read, for a host that reads it. A request that replaces one
    /// not yet written keeps its seq when it names none: the frames are still
    /// of no use.
    pub fn keyframe(&self, reason: KeyframeReason, discarding: bool) {
        self.update(|slots| {
            let after = (slots.keyframe_seq && discarding).then_some(slots.read_seq);
            let kept = slots.keyframe.and_then(|(_, after)| after);
            slots.keyframe = Some((reason, after.or(kept)));
        });
    }
    pub fn fps_cap(&self, fps: u32) {
        self.update(|slots| slots.fps_cap = Some(fps));
    }
    pub fn quality(&self, preset: QualityPreset) {
        self.update(|slots| slots.quality = Some(preset));
    }
    pub fn route(&self, route: Route) {
        self.update(|slots| slots.route = Some(route));
    }
    pub fn display_mode(&self, request: ModeRequest) {
        self.update(|slots| slots.display_mode = Some(request));
    }
    pub fn unlock(&self, password: UnlockSecret) {
        self.update(|slots| slots.unlock = Some(password));
    }

    /// Queue one batch of input, all under the queue lock so sequences follow
    /// queue order. `admit` returns the `(generation, frame_id)` of the fresh
    /// presentation the batch is stamped with, or why there is none. A queue
    /// that would overflow fails closed: control is revoked, `on_congestion`
    /// runs, and nothing of the batch is queued.
    pub fn submit(
        &self,
        inputs: &[Input],
        admit: impl FnOnce() -> Result<(u64, u64), InputError>,
        on_congestion: impl FnOnce(),
    ) -> Result<(), InputError> {
        if inputs.is_empty() || inputs.len() > INPUT_BATCH_MAX {
            return Err(InputError::Invalid(format!(
                "send 1 to {INPUT_BATCH_MAX} inputs per call"
            )));
        }
        let mut queue = self.inputs.lock();
        let (generation, frame_id) = admit()?;
        let stamp = |sequence, input: &Input| InputEvent {
            generation,
            frame_id,
            sequence,
            input: input.clone(),
        };
        // Pointer motion replaces a queued tail move, so it can never fill the
        // queue even while the uplink stalls. The tail is the last queued event,
        // so replacing it reorders nothing.
        if let [input @ Input::Move { .. }] = inputs
            && let Some(tail) = queue.events.back_mut()
            && matches!(tail.event.input, Input::Move { .. })
            && tail.event.generation == generation
        {
            let len = encoded_len(input);
            let old = std::mem::replace(&mut tail.len, len);
            tail.event = stamp(self.sequence.fetch_add(1, Ordering::Relaxed), input);
            queue.bytes = queue.bytes - old + len;
            drop(queue);
            self.notify.notify_one();
            return Ok(());
        }
        // A scroll adds itself to a queued tail scroll of the same generation
        // and continuity while the sum stays within a scroll's bound, so a
        // trackpad's stream cannot fill the queue either. The sum takes a new
        // sequence and the newer stamp, as a replaced move does.
        if let [scroll @ Input::Scroll { .. }] = inputs
            && let Some(tail) = queue.events.back_mut()
            && tail.event.generation == generation
            && let Some(summed) = summed_scroll(&tail.event.input, scroll)
        {
            let sequence = self.sequence.fetch_add(1, Ordering::Relaxed);
            // Read once the sequence is taken: a barrier pushed before then
            // made the tail obsolete, and every event queued ahead of it, so
            // the scroll goes alone rather than carry the tail past it.
            let current = tail.event.sequence >= self.controls.lock().obsolete_below;
            let input = if current { summed } else { scroll.clone() };
            let len = encoded_len(&input);
            let old = std::mem::replace(&mut tail.len, len);
            tail.event = InputEvent {
                generation,
                frame_id,
                sequence,
                input,
            };
            queue.bytes = queue.bytes - old + len;
            drop(queue);
            self.notify.notify_one();
            return Ok(());
        }
        let mut lens = [0; INPUT_BATCH_MAX];
        for (len, input) in lens.iter_mut().zip(inputs) {
            *len = encoded_len(input);
        }
        let bytes: usize = lens.iter().sum();
        if queue.events.len() + inputs.len() > INPUT_QUEUE
            || queue.bytes + bytes > INPUT_QUEUE_BYTES
        {
            self.push_barrier(Barrier::Revoke);
            on_congestion();
            drop(queue);
            self.notify.notify_one();
            return Err(InputError::Congestion);
        }
        let first = self
            .sequence
            .fetch_add(inputs.len() as u64, Ordering::Relaxed);
        for ((sequence, input), len) in (first..).zip(inputs).zip(lens) {
            queue.events.push_back(Queued {
                event: stamp(sequence, input),
                len,
            });
        }
        queue.bytes += bytes;
        drop(queue);
        self.notify.notify_one();
        Ok(())
    }

    /// The next message to write, in priority order, or when something will
    /// next be due. `last_write` is when the writer last wrote anything.
    pub fn next(&self, now: Instant, last_write: Instant) -> Next {
        if self.promote.load(Ordering::Relaxed) && self.promote.swap(false, Ordering::AcqRel) {
            return Next::Send(ViewerMessage::Resume {});
        }
        // Never hold `controls` while taking `inputs`: `submit` takes them in
        // the other order.
        let (barrier, obsolete_below) = {
            let mut controls = self.controls.lock();
            (controls.take(), controls.obsolete_below)
        };
        if let Some(barrier) = barrier {
            // A barrier makes every gesture queued before it obsolete. The
            // queue runs in sequence order: a replaced tail move takes a new
            // sequence.
            let mut queue = self.inputs.lock();
            while let Some(head) = queue.events.front()
                && head.event.sequence < obsolete_below
            {
                let len = head.len;
                queue.events.pop_front();
                queue.bytes -= len;
            }
            return Next::Send(barrier);
        }
        {
            let mut slots = self.slots.lock();
            if let Some((nonce, at)) = slots.probe.take() {
                return Next::Send(ViewerMessage::ProbeEcho {
                    nonce,
                    at_us: Self::stamp(&slots, at),
                });
            }
            if let Some((seq, at)) = slots.receipt.take() {
                return Next::Send(ViewerMessage::Received {
                    seq,
                    at_us: Self::stamp(&slots, at),
                });
            }
        }
        {
            let mut queue = self.inputs.lock();
            if let Some(head) = queue.events.front() {
                let stamp = (head.event.generation, head.event.frame_id);
                let mut slots = self.slots.lock();
                // The host admits input only for a frame it has seen presented,
                // so that presentation goes first, throttle or not. The stamp
                // came from it, so the latest presentation is never older.
                if slots.unsent(stamp)
                    && let Some(presented) = slots.presented.filter(|p| slots.unsent(*p))
                {
                    slots.presented_sent = Some((presented.0, presented.1, now));
                    return Next::Send(presented_message(presented));
                }
                drop(slots);
                if let Some(queued) = queue.events.pop_front() {
                    queue.bytes -= queued.len;
                    return Next::Send(ViewerMessage::Input {
                        event: queued.event,
                    });
                }
            }
        }
        let mut slots = self.slots.lock();
        if let Some(lifecycle) = slots.lifecycle.take() {
            return Next::Send(match lifecycle {
                Lifecycle::Pause => ViewerMessage::Pause {},
                Lifecycle::Resume => ViewerMessage::Resume {},
            });
        }
        if let Some(password) = slots.unlock.take() {
            return Next::Send(ViewerMessage::Unlock { password });
        }
        let ping = last_write + PING_IDLE;
        let mut due = ping;
        if let Some(presented) = slots.presented.filter(|p| slots.unsent(*p)) {
            match slots.presented_sent {
                Some((_, _, at)) if now < at + PRESENTED_SEND_MIN_INTERVAL => {
                    due = due.min(at + PRESENTED_SEND_MIN_INTERVAL);
                }
                _ => {
                    slots.presented_sent = Some((presented.0, presented.1, now));
                    return Next::Send(presented_message(presented));
                }
            }
        }
        if let Some((reason, after_seq)) = slots.keyframe.take() {
            return Next::Send(ViewerMessage::Keyframe { reason, after_seq });
        }
        if let Some(fps) = slots.fps_cap.take() {
            return Next::Send(ViewerMessage::FpsCap { fps });
        }
        if let Some(preset) = slots.quality.take() {
            return Next::Send(ViewerMessage::Quality { preset });
        }
        if let Some(route) = slots.route.take() {
            return Next::Send(ViewerMessage::Path { route });
        }
        if let Some(request) = slots.display_mode.take() {
            return Next::Send(match request {
                ModeRequest::Set(mode) => ViewerMessage::DisplayMode {
                    width: mode.width,
                    height: mode.height,
                },
                ModeRequest::Restore => ViewerMessage::RestoreDisplayMode {},
            });
        }
        if now >= ping {
            return Next::Send(ViewerMessage::Ping {});
        }
        Next::Idle(due)
    }
}

fn presented_message((generation, frame_id): (u64, u64)) -> ViewerMessage {
    ViewerMessage::Presented {
        generation,
        frame_id,
    }
}

/// `tail` and `scroll` as one scroll: both scrolls of the same continuity
/// and direction on each axis whose sum stays within a scroll's bound.
fn summed_scroll(tail: &Input, scroll: &Input) -> Option<Input> {
    let (
        Input::Scroll {
            horizontal: tail_horizontal,
            vertical: tail_vertical,
            continuous: tail_continuous,
        },
        Input::Scroll {
            horizontal,
            vertical,
            continuous,
        },
    ) = (tail, scroll)
    else {
        return None;
    };
    let same_way =
        |queued: i32, more: i32| (queued >= 0 && more >= 0) || (queued <= 0 && more <= 0);
    let sum = |queued: i32, more: i32| {
        same_way(queued, more)
            .then(|| queued.checked_add(more))
            .flatten()
            .filter(|sum| sum.unsigned_abs() <= SCROLL_UNITS_MAX)
    };
    (tail_continuous == continuous).then_some(Input::Scroll {
        horizontal: sum(*tail_horizontal, *horizontal)?,
        vertical: sum(*tail_vertical, *vertical)?,
        continuous: *continuous,
    })
}

/// The length of `input` as JSON, counted without allocating.
fn encoded_len(input: &Input) -> usize {
    struct Count(usize);
    impl std::io::Write for Count {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut count = Count(0);
    // Serialising these plain enums cannot fail; a failure would undercount.
    let _ = serde_json::to_writer(&mut count, input);
    count.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Button, Key};

    fn send(outbound: &Outbound, now: Instant) -> ViewerMessage {
        match outbound.next(now, now) {
            Next::Send(message) => message,
            Next::Idle(_) => panic!("nothing to send"),
        }
    }
    fn idle(outbound: &Outbound, now: Instant) -> bool {
        matches!(outbound.next(now, now), Next::Idle(_))
    }
    fn key(down: bool) -> Input {
        Input::Key {
            key: Key::Backspace,
            down,
        }
    }

    #[test]
    fn repeated_releases_are_bounded_and_revocation_precedes_a_new_grant() {
        let outbound = Outbound::new(None, false);
        let now = Instant::now();
        let fresh = || Ok((1, 1));
        outbound.submit(&[key(true)], fresh, || {}).unwrap();
        for _ in 0..1000 {
            outbound.barrier(Barrier::Release);
        }
        assert!(matches!(send(&outbound, now), ViewerMessage::Release {}));
        // The barrier discarded the queued input, and the releases coalesced.
        assert!(idle(&outbound, now));
        outbound.barrier(Barrier::Grant);
        outbound.barrier(Barrier::Revoke);
        outbound.barrier(Barrier::Grant);
        assert!(matches!(
            send(&outbound, now),
            ViewerMessage::Control { enabled: false }
        ));
        assert!(matches!(
            send(&outbound, now),
            ViewerMessage::Control { enabled: true }
        ));
        assert!(idle(&outbound, now));
    }

    /// A barrier discards the input queued before it, never input that came
    /// while it waited to be written.
    #[test]
    fn a_barrier_keeps_the_input_that_follows_it() {
        let outbound = Outbound::new(None, false);
        let now = Instant::now();
        let fresh = || Ok((1, 1));
        outbound.submit(&[key(true)], fresh, || {}).unwrap();
        outbound.barrier(Barrier::Release);
        outbound.submit(&[key(false)], fresh, || {}).unwrap();
        assert!(matches!(send(&outbound, now), ViewerMessage::Release {}));
        let ViewerMessage::Input { event } = send(&outbound, now) else {
            panic!("the input after the release was discarded");
        };
        assert_eq!(event.sequence, 2);
        assert!(idle(&outbound, now));
    }

    #[test]
    fn presented_precedes_input_that_references_it() {
        let outbound = Outbound::new(None, false);
        let start = Instant::now();
        outbound.presented(1, 1);
        assert!(matches!(
            send(&outbound, start),
            ViewerMessage::Presented {
                generation: 1,
                frame_id: 1
            }
        ));
        // A newer presentation within the throttle waits, and says until when.
        outbound.presented(1, 2);
        let soon = start + Duration::from_millis(10);
        match outbound.next(soon, soon) {
            Next::Idle(due) => assert_eq!(due, start + PRESENTED_SEND_MIN_INTERVAL),
            Next::Send(_) => panic!("the throttle let a presentation through"),
        }
        // Input stamped with frame 2 brings it forward, ahead of the input.
        outbound
            .submit(&[key(true), key(false)], || Ok((1, 2)), || {})
            .unwrap();
        assert!(matches!(
            send(&outbound, soon),
            ViewerMessage::Presented {
                generation: 1,
                frame_id: 2
            }
        ));
        for sequence in [1, 2] {
            let ViewerMessage::Input { event } = send(&outbound, soon) else {
                panic!("expected input");
            };
            assert_eq!((event.generation, event.frame_id), (1, 2));
            assert_eq!(event.sequence, sequence);
        }
        // Input for an already written presentation adds nothing.
        outbound.submit(&[key(true)], || Ok((1, 2)), || {}).unwrap();
        assert!(matches!(send(&outbound, soon), ViewerMessage::Input { .. }));
        assert!(idle(&outbound, soon));
    }

    #[test]
    fn bulk_input_and_moves_never_trip_congestion() {
        let outbound = Outbound::new(None, false);
        let congested = std::cell::Cell::new(false);
        let submit =
            |inputs: &[Input]| outbound.submit(inputs, || Ok((1, 1)), || congested.set(true));
        // An IME replacement, then keyed text under a latched modifier, all
        // while the writer is blocked.
        let backspaces: Vec<_> = (0..16).map(|i| key(i % 2 == 0)).collect();
        for _ in 0..8 {
            submit(&backspaces).unwrap();
        }
        let latched: Vec<_> = (0..10)
            .map(|i| Input::Button {
                button: Button::Left,
                down: i % 2 == 0,
                drag_start: false,
            })
            .collect();
        for _ in 0..16 {
            submit(&latched).unwrap();
        }
        // Pointer motion coalesces into one queued move.
        for x in 0..1000 {
            submit(&[Input::Move { x, y: 1 }]).unwrap();
        }
        assert_eq!(outbound.inputs.lock().events.len(), 288 + 1);
        assert!(!congested.get());
        // A genuine flood fails closed: control is revoked, nothing is queued.
        let accepted = (0..).take_while(|_| submit(&backspaces).is_ok()).count();
        assert!(congested.get());
        assert_eq!(accepted, (INPUT_QUEUE - 289) / 16);
        assert!(matches!(submit(&backspaces), Err(InputError::Congestion)));
        let now = Instant::now();
        assert!(matches!(
            send(&outbound, now),
            ViewerMessage::Control { enabled: false }
        ));
        assert!(idle(&outbound, now));
    }

    /// A stream of scrolls while the uplink stalls queues as one scroll per
    /// continuity, up to a scroll's bound, with the newest stamp and a newer
    /// sequence; anything between two scrolls keeps them apart.
    #[test]
    fn scrolls_add_up_in_the_queue_within_a_scrolls_bound() {
        let outbound = Outbound::new(None, false);
        let now = Instant::now();
        let congested = std::cell::Cell::new(false);
        let scroll = |vertical, continuous| Input::Scroll {
            horizontal: -1,
            vertical,
            continuous,
        };
        let submit = |input: Input, stamp: (u64, u64)| {
            outbound.submit(&[input], || Ok(stamp), || congested.set(true))
        };
        // A trackpad's 1 000 events fill one scroll to its bound; the next
        // starts another, and turning back starts its own rather than erasing
        // the scroll before the turn.
        for frame in 0..1000 {
            submit(scroll(12, true), (1, frame)).unwrap();
        }
        assert_eq!(outbound.inputs.lock().events.len(), 1);
        submit(scroll(12, true), (1, 1000)).unwrap();
        for _ in 0..5 {
            submit(scroll(-12, true), (1, 1000)).unwrap();
        }
        assert_eq!(outbound.inputs.lock().events.len(), 3);
        // A wheel's notch is not a trackpad's travel, a move parts two
        // scrolls, and a scroll of a new generation starts its own.
        submit(scroll(120, false), (1, 1000)).unwrap();
        submit(Input::Move { x: 1, y: 1 }, (1, 1000)).unwrap();
        submit(scroll(120, false), (1, 1000)).unwrap();
        submit(scroll(120, false), (2, 1)).unwrap();
        submit(scroll(120, false), (2, 2)).unwrap();
        assert!(!congested.get());
        let mut sent = Vec::new();
        while let Next::Send(message) = outbound.next(now, now) {
            if let ViewerMessage::Input { event } = message {
                sent.push(event);
            }
        }
        let scrolls: Vec<_> = sent
            .iter()
            .map(|event| match event.input {
                Input::Scroll {
                    horizontal,
                    vertical,
                    continuous,
                } => Some((event.frame_id, horizontal, vertical, continuous)),
                _ => None,
            })
            .collect();
        assert_eq!(
            scrolls,
            [
                Some((999, -1000, 12_000, true)),
                Some((1000, -1, 12, true)),
                Some((1000, -5, -60, true)),
                Some((1000, -1, 120, false)),
                None,
                Some((1000, -1, 120, false)),
                Some((2, -2, 240, false)),
            ]
        );
        assert!(
            sent.windows(2)
                .all(|pair| pair[0].sequence < pair[1].sequence)
        );
    }

    /// A scroll never carries what a barrier made obsolete past it: queued
    /// before the barrier, the tail goes, and only the newer scroll is sent.
    #[test]
    fn a_barrier_keeps_an_obsolete_scroll_out_of_the_next() {
        let outbound = Outbound::new(None, false);
        let now = Instant::now();
        let scroll = |vertical| Input::Scroll {
            horizontal: 0,
            vertical,
            continuous: true,
        };
        outbound
            .submit(&[scroll(50)], || Ok((1, 1)), || {})
            .unwrap();
        outbound.barrier(Barrier::Release);
        outbound.submit(&[scroll(7)], || Ok((1, 2)), || {}).unwrap();
        assert!(matches!(send(&outbound, now), ViewerMessage::Release {}));
        let ViewerMessage::Input { event } = send(&outbound, now) else {
            panic!("the scroll after the release was discarded");
        };
        assert!(matches!(
            event.input,
            Input::Scroll {
                horizontal: 0,
                vertical: 7,
                continuous: true
            }
        ));
        assert!(idle(&outbound, now));
    }

    /// Arrival times and a keyframe request's seq go only to a host that
    /// listed them; to one that did, each receipt carries the time its newest
    /// unit was read, and a request made while frames are thrown away the
    /// newest seq read, even once its receipt went out.
    #[test]
    fn stamps_and_seqs_go_only_to_a_host_that_reads_them() {
        let epoch = Instant::now();
        let at = |ms: u64| epoch + Duration::from_millis(ms);
        let feed = |outbound: &Outbound| {
            outbound.probe(7, at(5));
            outbound.receipt(3, at(10));
            outbound.receipt(4, at(12));
            // A late, older receipt never moves the newest back.
            outbound.receipt(2, at(14));
            outbound.keyframe(KeyframeReason::Gap, true);
        };
        let old = Outbound::new(None, false);
        feed(&old);
        assert!(matches!(
            send(&old, at(20)),
            ViewerMessage::ProbeEcho {
                nonce: 7,
                at_us: None
            }
        ));
        assert!(matches!(
            send(&old, at(20)),
            ViewerMessage::Received {
                seq: 4,
                at_us: None
            }
        ));
        assert!(matches!(
            send(&old, at(20)),
            ViewerMessage::Keyframe {
                reason: KeyframeReason::Gap,
                after_seq: None
            }
        ));

        let new = Outbound::new(Some(epoch), true);
        feed(&new);
        assert!(matches!(
            send(&new, at(20)),
            ViewerMessage::ProbeEcho {
                nonce: 7,
                at_us: Some(5_000)
            }
        ));
        assert!(matches!(
            send(&new, at(20)),
            ViewerMessage::Received {
                seq: 4,
                at_us: Some(12_000)
            }
        ));
        assert!(matches!(
            send(&new, at(20)),
            ViewerMessage::Keyframe {
                reason: KeyframeReason::Gap,
                after_seq: Some(4)
            }
        ));
        // A user's request names no seq, unless it replaced one that did.
        new.keyframe(KeyframeReason::User, false);
        assert!(matches!(
            send(&new, at(30)),
            ViewerMessage::Keyframe {
                reason: KeyframeReason::User,
                after_seq: None
            }
        ));
        new.receipt(5, at(40));
        new.keyframe(KeyframeReason::Stall, true);
        new.keyframe(KeyframeReason::User, false);
        assert!(matches!(
            send(&new, at(50)),
            ViewerMessage::Received { seq: 5, .. }
        ));
        assert!(matches!(
            send(&new, at(50)),
            ViewerMessage::Keyframe {
                reason: KeyframeReason::User,
                after_seq: Some(5)
            }
        ));
        assert!(idle(&new, at(50)));
    }
}
