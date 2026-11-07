//! The viewer's video hand-off to exactly one native consumer: the network
//! read task pushes, the consumer's own thread pulls on a `Condvar`, never
//! through tokio. The sink is also the viewer's single enforcement point for
//! presentation: only a frame it handed out, of the current generation,
//! received less than `PRESENTED_FRESH` ago and newer than the last one, is
//! accepted and forwarded to the host, as input authorisation and nothing else.
use crate::{
    KeyframeReason, VideoCodec,
    auth::{PRESENTED_FRESH, RECENT_FRAMES},
    viewer::{
        DecoderInfo, DecoderState, InputError, IntervalStats, ViewerShared, recovery::Recovery,
        status::PresentStats,
    },
};
use std::{
    collections::VecDeque,
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

/// Units queued at most; more means the consumer fell far behind.
pub(crate) const SINK_MAX_UNITS: usize = 32;
/// Bytes queued at most.
pub(crate) const SINK_MAX_BYTES: usize = 8 * 1024 * 1024;
/// Longest decoder name kept for the overlay.
const DECODER_NAME_MAX: usize = 64;

/// One encoded access unit (or JPEG picture) as received.
#[derive(Debug)]
pub struct VideoUnit {
    pub seq: u64,
    pub generation: u64,
    pub frame_id: u64,
    pub codec: VideoCodec,
    pub width: u32,
    pub height: u32,
    pub keyframe: bool,
    pub duplicate: bool,
    /// The first unit after any drop, generation change or resume: the decoder
    /// drops what it still holds, restarts at this IDR and rebuilds only if
    /// the size changed.
    pub discontinuity: bool,
    pub pts_us: u64,
    pub bytes: bytes::Bytes,
    pub received_at: Instant,
}

#[derive(Debug)]
pub enum Pull {
    Unit(VideoUnit),
    /// The head needs this many bytes; it stays queued for a larger buffer.
    NeedCapacity(usize),
    Timeout,
    /// The viewer is paused: suspend stall checks and call again.
    Paused,
    /// The sink closed or this consumer was replaced: the consumer must exit.
    Closed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttachError {
    Closed,
    AlreadyAttached,
}
impl std::fmt::Display for AttachError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Closed => "the desktop session's video has ended",
            Self::AlreadyAttached => "another consumer already takes this session's video",
        })
    }
}
impl std::error::Error for AttachError {}

/// A unit the consumer took, remembered to validate its presentation.
#[derive(Clone, Copy)]
struct Handed {
    generation: u64,
    frame_id: u64,
    seq: u64,
    received_at: Instant,
    handed_at: Instant,
}

/// The newest accepted presentation.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Presented {
    pub generation: u64,
    pub frame_id: u64,
    pub presented_at: Instant,
    pub received_at: Instant,
}
impl Presented {
    /// Whether this presentation still authorises input: the session streams a
    /// generation this frame belongs to, and both the frame and its
    /// presentation are recent.
    pub fn fresh(&self, streaming: bool, generation: u64, now: Instant) -> bool {
        streaming
            && self.generation == generation
            && now.saturating_duration_since(self.presented_at) < PRESENTED_FRESH
            && now.saturating_duration_since(self.received_at) < PRESENTED_FRESH
    }
}

/// What the viewer's status reads from the sink.
pub(crate) struct SinkStats {
    pub presented: Option<Presented>,
    pub presented_fps: f32,
    pub present_interval: IntervalStats,
    pub viewer_ms: Option<u32>,
    /// Units the sink discarded without handing them out.
    pub discarded: u64,
    /// Times the queue was dropped to wait for the next IDR.
    pub backlog_drops: u64,
    pub keyframes_requested: u64,
    pub decoder: DecoderInfo,
}

struct SinkState {
    queue: VecDeque<VideoUnit>,
    bytes: usize,
    generation: u64,
    /// Only an IDR can be decoded next.
    wait_key: bool,
    /// The next IDR handed out carries `discontinuity`.
    next_discontinuity: bool,
    handed: VecDeque<Handed>,
    presented: Option<Presented>,
    recovery: Recovery,
    present: PresentStats,
    discarded: u64,
    backlog_drops: u64,
    decoder: DecoderInfo,
}
impl SinkState {
    fn new(epoch: Instant) -> Self {
        Self {
            queue: VecDeque::new(),
            bytes: 0,
            generation: 0,
            wait_key: true,
            next_discontinuity: true,
            handed: VecDeque::with_capacity(RECENT_FRAMES),
            presented: None,
            recovery: Recovery::default(),
            present: PresentStats::new(epoch),
            discarded: 0,
            backlog_drops: 0,
            decoder: DecoderInfo::default(),
        }
    }
    fn clear(&mut self) -> u64 {
        let dropped = self.queue.len() as u64;
        self.queue.clear();
        self.bytes = 0;
        dropped
    }
    /// Everything queued belongs to an older picture chain: start over at an IDR.
    fn restart(&mut self) {
        self.clear();
        self.wait_key = true;
        self.next_discontinuity = true;
    }
    fn enqueue(&mut self, unit: VideoUnit) {
        self.bytes += unit.bytes.len();
        self.queue.push_back(unit);
    }
}

pub struct VideoSink {
    state: parking_lot::Mutex<SinkState>,
    ready: parking_lot::Condvar,
    closed: AtomicBool,
    paused: AtomicBool,
    /// 0 = none, else the attached consumer's token.
    attached: AtomicU64,
    /// Where accepted presentations and keyframe requests go. `Weak`, so a
    /// late native call never keeps a session alive.
    shared: Weak<ViewerShared>,
    /// Session start, for the trace.
    epoch: Instant,
}

/// Consumer tokens are never reused, so a stale consumer can never match.
static NEXT_TOKEN: AtomicU64 = AtomicU64::new(1);

fn micros(epoch: Instant, at: Instant) -> u64 {
    u64::try_from(at.saturating_duration_since(epoch).as_micros()).unwrap_or(u64::MAX)
}

impl VideoSink {
    fn new(shared: Weak<ViewerShared>) -> Self {
        let epoch = Instant::now();
        Self {
            state: parking_lot::Mutex::new(SinkState::new(epoch)),
            ready: parking_lot::Condvar::new(),
            closed: AtomicBool::new(false),
            paused: AtomicBool::new(false),
            attached: AtomicU64::new(0),
            shared,
            epoch,
        }
    }

    pub(crate) fn for_session(shared: Weak<ViewerShared>) -> Arc<Self> {
        Arc::new(Self::new(shared))
    }

    /// The single consumer of this sink: exactly one pops video.
    pub fn attach(self: &Arc<Self>) -> Result<VideoConsumer, AttachError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(AttachError::Closed);
        }
        let token = NEXT_TOKEN.fetch_add(1, Ordering::Relaxed);
        self.attached
            .compare_exchange(0, token, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| AttachError::AlreadyAttached)?;
        Ok(VideoConsumer {
            sink: self.clone(),
            token,
        })
    }

    /// The read task. Never blocks. Units that cannot be decoded next are
    /// dropped here, and the sink asks for an IDR.
    pub(crate) fn push(&self, mut unit: VideoUnit) {
        if self.closed.load(Ordering::Acquire) {
            return;
        }
        let mut state = self.state.lock();
        if unit.generation != state.generation {
            state.restart();
            state.generation = unit.generation;
        }
        if unit.codec == VideoCodec::Jpeg {
            // Every JPEG stands alone: only the newest is worth showing.
            state.discarded += state.clear();
            state.wait_key = false;
            unit.discontinuity = std::mem::take(&mut state.next_discontinuity);
            state.enqueue(unit);
            drop(state);
            self.ready.notify_one();
            return;
        }
        if state.wait_key && !unit.keyframe {
            state.discarded += 1;
            self.request(&mut state, KeyframeReason::Gap);
            return;
        }
        if state.queue.len() >= SINK_MAX_UNITS || state.bytes + unit.bytes.len() > SINK_MAX_BYTES {
            state.discarded += state.clear();
            state.backlog_drops += 1;
            state.restart();
            self.request(&mut state, KeyframeReason::Backlog);
            if !unit.keyframe {
                state.discarded += 1;
                return;
            }
        }
        if unit.keyframe {
            state.wait_key = false;
            unit.discontinuity = std::mem::take(&mut state.next_discontinuity);
        }
        state.enqueue(unit);
        drop(state);
        self.ready.notify_one();
    }

    /// A `State` of a new generation: nothing queued can be shown any more.
    pub(crate) fn set_generation(&self, generation: u64) {
        let mut state = self.state.lock();
        if state.generation != generation {
            state.restart();
            state.generation = generation;
        }
    }

    /// The viewer is hidden: drop the queue and wait for the IDR the host
    /// sends on resume. `next` returns `Paused`, so native stall checks stop.
    pub(crate) fn pause(&self) {
        self.paused.store(true, Ordering::Release);
        self.state.lock().restart();
        self.ready.notify_all();
    }

    pub(crate) fn resume(&self) {
        self.paused.store(false, Ordering::Release);
        self.ready.notify_all();
    }

    /// Drop everything queued and wake a blocked consumer, which then exits.
    pub(crate) fn close(&self) {
        self.closed.store(true, Ordering::Release);
        self.state.lock().clear();
        self.ready.notify_all();
    }

    /// The viewer tick (every 250 ms): an fps cap for the host, when the
    /// presentation rate calls for one. None is derived yet.
    pub(crate) fn tick(&self, _now: Instant, _direct: bool) -> Option<u32> {
        None
    }

    /// The presentation that stamps new input, only while it is fresh.
    pub(crate) fn presented_for_input(&self) -> Result<(u64, u64), InputError> {
        let shared = self.shared.upgrade().ok_or(InputError::Closed)?;
        let (streaming, generation) = {
            let state = shared.state.lock();
            (state.streaming(), state.generation())
        };
        if self.closed.load(Ordering::Acquire) {
            return Err(InputError::Closed);
        }
        match self.state.lock().presented {
            Some(presented) if presented.fresh(streaming, generation, Instant::now()) => {
                Ok((presented.generation, presented.frame_id))
            }
            _ => Err(InputError::NotFresh),
        }
    }

    pub(crate) fn presented(&self) -> Option<Presented> {
        self.state.lock().presented
    }

    /// Ask the host for an IDR, through the limiter.
    pub(crate) fn request_keyframe(&self, reason: KeyframeReason) -> bool {
        if self.closed.load(Ordering::Acquire) {
            return false;
        }
        self.request(&mut self.state.lock(), reason)
    }

    fn request(&self, state: &mut SinkState, reason: KeyframeReason) -> bool {
        let paused = self.paused.load(Ordering::Acquire);
        if !state.recovery.request(reason, Instant::now(), paused) {
            return false;
        }
        // Under the sink lock: the outbound slots only ever follow it.
        if let Some(shared) = self.shared.upgrade() {
            shared.outbound.keyframe(reason);
        }
        true
    }

    pub(crate) fn snapshot(&self, now: Instant) -> SinkStats {
        let paused = self.paused.load(Ordering::Acquire);
        let mut state = self.state.lock();
        let mut decoder = state.decoder.clone();
        if state.wait_key && !paused && state.presented.is_some() {
            decoder.state = DecoderState::Recovering;
        }
        SinkStats {
            presented: state.presented,
            presented_fps: state.present.presented_fps(now),
            present_interval: state.present.intervals(now),
            viewer_ms: state.present.viewer_ms(now),
            discarded: state.discarded,
            backlog_drops: state.backlog_drops,
            keyframes_requested: state.recovery.requested(),
            decoder,
        }
    }

    /// A sink with no session, for FFI and native tests. It validates
    /// presentations and forwards them nowhere.
    #[doc(hidden)]
    pub fn detached_for_test() -> Arc<Self> {
        Arc::new(Self::new(Weak::new()))
    }

    /// Feed a detached sink: the FFI's synthetic source and native tests.
    #[doc(hidden)]
    pub fn push_for_test(&self, unit: VideoUnit) {
        self.push(unit);
    }
}

/// The attached consumer. Dropping it detaches and wakes a blocked `next`.
pub struct VideoConsumer {
    sink: Arc<VideoSink>,
    token: u64,
}
impl VideoConsumer {
    fn current(&self) -> bool {
        !self.sink.closed.load(Ordering::Acquire)
            && self.sink.attached.load(Ordering::Acquire) == self.token
    }

    /// Blocks on the sink's `Condvar` only, never on tokio. While paused it
    /// waits `timeout`, then returns `Paused`. A head larger than `capacity`
    /// returns `NeedCapacity` and stays queued.
    pub fn next(&self, timeout: Duration, capacity: usize) -> Pull {
        let sink = &*self.sink;
        let deadline = Instant::now() + timeout;
        let mut state = sink.state.lock();
        loop {
            if !self.current() {
                return Pull::Closed;
            }
            let paused = sink.paused.load(Ordering::Acquire);
            if !paused && let Some(head) = state.queue.front() {
                if head.bytes.len() > capacity {
                    return Pull::NeedCapacity(head.bytes.len());
                }
                if let Some(unit) = state.queue.pop_front() {
                    state.bytes -= unit.bytes.len();
                    if state.handed.len() == RECENT_FRAMES {
                        state.handed.pop_front();
                    }
                    state.handed.push_back(Handed {
                        generation: unit.generation,
                        frame_id: unit.frame_id,
                        seq: unit.seq,
                        received_at: unit.received_at,
                        handed_at: Instant::now(),
                    });
                    return Pull::Unit(unit);
                }
            }
            if sink.ready.wait_until(&mut state, deadline).timed_out() {
                return if sink.closed.load(Ordering::Acquire) {
                    Pull::Closed
                } else if sink.paused.load(Ordering::Acquire) {
                    Pull::Paused
                } else {
                    Pull::Timeout
                };
            }
        }
    }

    /// The native renderer presented `(generation, frame_id)`. Accepted only
    /// for a frame this sink handed out, of the current generation, received
    /// less than `PRESENTED_FRESH` ago and newer than the last accepted one,
    /// while the viewer is not paused. An accepted presentation goes to the
    /// host as input authorisation; a rejected one goes nowhere.
    pub fn presented(&self, generation: u64, frame_id: u64) -> bool {
        let sink = &*self.sink;
        if !self.current() || sink.paused.load(Ordering::Acquire) {
            return false;
        }
        let now = Instant::now();
        let mut state = sink.state.lock();
        let handed = state
            .handed
            .iter()
            .rev()
            .find(|handed| handed.generation == generation && handed.frame_id == frame_id)
            .copied();
        let Some(handed) = handed.filter(|handed| {
            generation == state.generation
                && now.saturating_duration_since(handed.received_at) < PRESENTED_FRESH
                && state.presented.is_none_or(|presented| {
                    presented.generation != generation || frame_id > presented.frame_id
                })
        }) else {
            tracing::debug!(
                target: "pb_mapper_desktop::frames",
                generation,
                frame_id,
                "desktop_presentation_rejected"
            );
            return false;
        };
        state.presented = Some(Presented {
            generation,
            frame_id,
            presented_at: now,
            received_at: handed.received_at,
        });
        state
            .present
            .record(now, now.saturating_duration_since(handed.received_at));
        tracing::trace!(
            target: "pb_mapper_desktop::frames",
            seq = handed.seq,
            received_us = micros(sink.epoch, handed.received_at),
            handed_us = micros(sink.epoch, handed.handed_at),
            presented_us = micros(sink.epoch, now),
            "desktop_frame_view"
        );
        // Under the sink lock, so input is never stamped with a presentation
        // the outbound slots have not seen yet.
        if let Some(shared) = sink.shared.upgrade() {
            shared.outbound.presented(generation, frame_id);
        }
        true
    }

    /// The decoder needs an independently decodable picture. The request goes
    /// through the viewer's keyframe limiter; false means it was held back.
    pub fn keyframe(&self, reason: KeyframeReason) -> bool {
        self.current() && self.sink.request_keyframe(reason)
    }

    /// The decoder in use, for the overlay. At most 64 characters are kept.
    pub fn report(&self, decoder: &str, hardware: bool) {
        let mut state = self.sink.state.lock();
        state.decoder.name = decoder.chars().take(DECODER_NAME_MAX).collect();
        state.decoder.hardware = Some(hardware);
    }
}
impl Drop for VideoConsumer {
    fn drop(&mut self) {
        let _ =
            self.sink
                .attached
                .compare_exchange(self.token, 0, Ordering::AcqRel, Ordering::Acquire);
        self.sink.ready.notify_all();
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn unit(generation: u64, frame_id: u64, len: usize) -> VideoUnit {
        VideoUnit {
            seq: frame_id,
            generation,
            frame_id,
            codec: VideoCodec::H264,
            width: 16,
            height: 16,
            keyframe: frame_id == 1,
            duplicate: false,
            discontinuity: false,
            pts_us: 0,
            bytes: bytes::Bytes::from(vec![0; len]),
            received_at: Instant::now(),
        }
    }

    fn pull(consumer: &VideoConsumer) -> VideoUnit {
        match consumer.next(Duration::ZERO, 1 << 20) {
            Pull::Unit(unit) => unit,
            other => panic!("expected a unit, got {other:?}"),
        }
    }

    #[test]
    fn single_consumer_need_capacity_keeps_unit_and_close_wakes() {
        let sink = VideoSink::detached_for_test();
        let consumer = sink.attach().unwrap();
        assert_eq!(sink.attach().err(), Some(AttachError::AlreadyAttached));

        sink.push(unit(1, 1, 1000));
        sink.push(unit(1, 2, 10));
        assert!(matches!(
            consumer.next(Duration::ZERO, 999),
            Pull::NeedCapacity(1000)
        ));
        assert!(matches!(
            consumer.next(Duration::ZERO, 1000),
            Pull::Unit(VideoUnit { frame_id: 1, .. })
        ));
        assert!(matches!(
            consumer.next(Duration::ZERO, 1000),
            Pull::Unit(VideoUnit { frame_id: 2, .. })
        ));
        assert!(matches!(
            consumer.next(Duration::from_millis(5), 1000),
            Pull::Timeout
        ));

        // Detaching frees the sink for the next consumer.
        drop(consumer);
        let consumer = sink.attach().unwrap();

        // A consumer blocked in `next` returns promptly once the sink closes.
        let blocked = std::thread::spawn(move || {
            let started = Instant::now();
            let pulled = consumer.next(Duration::from_secs(5), 1000);
            (pulled, started.elapsed())
        });
        std::thread::sleep(Duration::from_millis(50));
        let closed_at = Instant::now();
        sink.close();
        let (pulled, _) = blocked.join().unwrap();
        assert!(matches!(pulled, Pull::Closed));
        assert!(closed_at.elapsed() < Duration::from_millis(100));
        assert_eq!(sink.attach().err(), Some(AttachError::Closed));
    }

    #[test]
    fn presentation_requires_handed_current_fresh_monotonic() {
        let sink = VideoSink::detached_for_test();
        let consumer = sink.attach().unwrap();
        for frame_id in 1..=3 {
            sink.push(unit(1, frame_id, 10));
        }
        let first = pull(&consumer);
        assert!(first.keyframe && first.discontinuity);
        // Never handed out.
        assert!(!consumer.presented(1, 2));
        pull(&consumer);
        pull(&consumer);
        assert!(consumer.presented(1, 2));
        // Not newer than the last accepted presentation.
        assert!(!consumer.presented(1, 2));
        assert!(!consumer.presented(1, 1));
        assert!(consumer.presented(1, 3));

        // Received longer than PRESENTED_FRESH ago.
        let mut old = unit(1, 4, 10);
        old.received_at = Instant::now()
            .checked_sub(PRESENTED_FRESH + Duration::from_millis(1))
            .unwrap();
        sink.push(old);
        pull(&consumer);
        assert!(!consumer.presented(1, 4));

        // An older generation: its frames were handed but are not current.
        sink.push(unit(1, 5, 10));
        pull(&consumer);
        sink.set_generation(2);
        assert!(!consumer.presented(1, 5));

        // Paused: nothing is accepted, not even a frame handed just before.
        sink.push(unit(2, 1, 10));
        pull(&consumer);
        sink.pause();
        assert!(!consumer.presented(2, 1));
        sink.resume();
        assert!(consumer.presented(2, 1));
        assert!(
            sink.presented()
                .is_some_and(|p| (p.generation, p.frame_id) == (2, 1))
        );
    }
}
