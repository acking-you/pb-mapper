//! What the viewer sends, in priority order. Producers (the read task, the
//! native consumer through the sink, the embedding app) store the latest value
//! of each kind and wake the write task; the write task takes one message per
//! pick, so a flood of one kind never delays a more urgent one:
//!
//! 1. control barriers: revoke, then release, then grant;
//! 2. the latest probe echo;
//! 3. the latest transport receipt (cumulative);
//! 4. input, in order, preceded by the presentation it references;
//! 5. pause or resume;
//! 6. the latest presentation, at most every `PRESENTED_SEND_MIN_INTERVAL`;
//! 7. the latest keyframe request, fps cap, quality, path and display mode;
//! 8. a ping after `PING_IDLE` without any other message.
use super::InputError;
use crate::{
    Input, InputEvent, KeyframeReason, QualityPreset,
    modes::Mode,
    protocol::{Route, ViewerMessage},
};
use std::{
    collections::VecDeque,
    sync::atomic::{AtomicU64, Ordering},
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
    probe: Option<u64>,
    receipt: Option<u64>,
    /// The newest presentation the sink accepted.
    presented: Option<(u64, u64)>,
    /// The last one written, and when.
    presented_sent: Option<(u64, u64, Instant)>,
    lifecycle: Option<Lifecycle>,
    keyframe: Option<KeyframeReason>,
    fps_cap: Option<u32>,
    quality: Option<QualityPreset>,
    route: Option<Route>,
    display_mode: Option<ModeRequest>,
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
impl InputQueue {
    fn clear(&mut self) {
        self.events.clear();
        self.bytes = 0;
    }
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
    /// Per session, stamped here; the embedding app owns none.
    sequence: AtomicU64,
}
impl Outbound {
    pub fn new() -> Self {
        Self {
            controls: parking_lot::Mutex::default(),
            slots: parking_lot::Mutex::default(),
            notify: tokio::sync::Notify::new(),
            inputs: parking_lot::Mutex::default(),
            sequence: AtomicU64::new(1),
        }
    }

    pub fn notified(&self) -> tokio::sync::futures::Notified<'_> {
        self.notify.notified()
    }

    fn update(&self, change: impl FnOnce(&mut Slots)) {
        change(&mut self.slots.lock());
        self.notify.notify_one();
    }

    pub fn barrier(&self, barrier: Barrier) {
        self.controls.lock().push(barrier);
        self.notify.notify_one();
    }
    pub fn probe(&self, nonce: u64) {
        self.update(|slots| slots.probe = Some(nonce));
    }
    /// Cumulative: every video record up to `seq` was read.
    pub fn receipt(&self, seq: u64) {
        self.update(|slots| slots.receipt = Some(slots.receipt.map_or(seq, |old| old.max(seq))));
    }
    /// Only the sink calls this, for a presentation it validated.
    pub fn presented(&self, generation: u64, frame_id: u64) {
        self.update(|slots| slots.presented = Some((generation, frame_id)));
    }
    pub fn lifecycle(&self, lifecycle: Lifecycle) {
        self.update(|slots| slots.lifecycle = Some(lifecycle));
    }
    /// Only the sink's keyframe limiter calls this.
    pub fn keyframe(&self, reason: KeyframeReason) {
        self.update(|slots| slots.keyframe = Some(reason));
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
        let mut lens = [0; INPUT_BATCH_MAX];
        for (len, input) in lens.iter_mut().zip(inputs) {
            *len = encoded_len(input);
        }
        let bytes: usize = lens.iter().sum();
        if queue.events.len() + inputs.len() > INPUT_QUEUE
            || queue.bytes + bytes > INPUT_QUEUE_BYTES
        {
            self.controls.lock().push(Barrier::Revoke);
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
        // Never hold `controls` while taking `inputs`: `submit` takes them in
        // the other order.
        let barrier = self.controls.lock().take();
        if let Some(barrier) = barrier {
            // A barrier makes every queued gesture obsolete.
            self.inputs.lock().clear();
            return Next::Send(barrier);
        }
        {
            let mut slots = self.slots.lock();
            if let Some(nonce) = slots.probe.take() {
                return Next::Send(ViewerMessage::ProbeEcho { nonce });
            }
            if let Some(seq) = slots.receipt.take() {
                return Next::Send(ViewerMessage::Received { seq });
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
        if let Some(reason) = slots.keyframe.take() {
            return Next::Send(ViewerMessage::Keyframe { reason });
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
#[allow(clippy::unwrap_used)]
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
        let outbound = Outbound::new();
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

    #[test]
    fn presented_precedes_input_that_references_it() {
        let outbound = Outbound::new();
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
        let outbound = Outbound::new();
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
}
