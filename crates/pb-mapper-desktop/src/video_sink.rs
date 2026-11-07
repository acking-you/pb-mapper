//! The viewer's video hand-off to exactly one native consumer: the network
//! read task pushes, the consumer's own thread pulls on a `Condvar`, never
//! through tokio. The sink is also the viewer's single enforcement point for
//! presentation: only a frame it handed out, of the current generation,
//! received less than `PRESENTED_FRESH` ago and newer than the last one, is
//! accepted and forwarded to the host, as input authorisation and nothing else.
//!
//! The queue keeps only what can still be decoded and shown in time:
//!
//! - an IDR supersedes everything queued before it, and a JPEG picture any
//!   older one;
//! - while an IDR is awaited (after a drop, a new generation or a pause),
//!   predicted frames are dropped and one is asked for;
//! - when the head waited longer than `BACKLOG_AGE` behind enough units, or
//!   the queue overflows, the consumer fell behind: the queue drops to the
//!   next IDR, one is asked for, and the fps cap learns of it. No such drop
//!   comes within `IDR_GRACE` of handing out an IDR, whose decode may be slow.
//!
//! Keyframe requests go through the viewer's recovery state machine and its
//! limiter, and the viewer tick runs the fps cap and the stall watchdog here.
use crate::{
    KeyframeReason, QualityPreset, VideoCodec,
    auth::{PRESENTED_FRESH, RECENT_FRAMES},
    viewer::{
        DecoderInfo, InputError, IntervalStats, ViewerShared,
        fps_cap::FpsCap,
        recovery::{Recovery, STALL_WATCHDOG},
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
/// A head older than this, with `BACKLOG_MIN_UNITS` queued, is a backlog.
pub(crate) const BACKLOG_AGE: Duration = Duration::from_millis(250);
pub(crate) const BACKLOG_MIN_UNITS: usize = 3;
/// No backlog drop this soon after an IDR was handed out.
pub(crate) const IDR_GRACE: Duration = Duration::from_millis(500);
/// Longest decoder name kept for the overlay.
const DECODER_NAME_MAX: usize = 64;
/// A detached sink has no session limits; this is the largest allowed.
const DETACHED_MAX_FPS: u32 = 120;

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
    /// Times the queue dropped to the next IDR, for age or overflow.
    pub backlog_drops: u64,
    pub keyframes_requested: u64,
    pub decoder: DecoderInfo,
}

/// Why the queue dropped to the next IDR.
#[derive(Clone, Copy, Debug)]
enum Backlog {
    /// The head waited longer than `BACKLOG_AGE`.
    Age,
    /// `SINK_MAX_UNITS` or `SINK_MAX_BYTES`.
    Overflow,
}

struct SinkState {
    queue: VecDeque<VideoUnit>,
    bytes: usize,
    generation: u64,
    /// Only an IDR can be decoded next.
    wait_key: bool,
    /// The next IDR handed out carries `discontinuity`.
    next_discontinuity: bool,
    /// When the consumer last took an IDR.
    idr_pulled_at: Option<Instant>,
    handed: VecDeque<Handed>,
    presented: Option<Presented>,
    recovery: Recovery,
    fps_cap: FpsCap,
    present: PresentStats,
    discarded: u64,
    backlog_drops: u64,
    overflow_drops: u64,
    /// The name and hardware flag the consumer reported; the state comes
    /// from `recovery`.
    decoder: DecoderInfo,
}
impl SinkState {
    fn new(epoch: Instant, max_fps: u32) -> Self {
        Self {
            queue: VecDeque::new(),
            bytes: 0,
            generation: 0,
            wait_key: true,
            next_discontinuity: true,
            idr_pulled_at: None,
            handed: VecDeque::with_capacity(RECENT_FRAMES),
            presented: None,
            recovery: Recovery::default(),
            fps_cap: FpsCap::new(max_fps),
            present: PresentStats::new(epoch),
            discarded: 0,
            backlog_drops: 0,
            overflow_drops: 0,
            decoder: DecoderInfo::default(),
        }
    }
    /// Drops everything queued and returns how many units that was.
    fn clear(&mut self) -> u64 {
        let dropped = self.queue.len() as u64;
        self.queue.clear();
        self.bytes = 0;
        self.discarded += dropped;
        dropped
    }
    /// Everything queued belongs to an older picture chain: start over at an IDR.
    fn restart(&mut self) -> u64 {
        self.wait_key = true;
        self.next_discontinuity = true;
        self.clear()
    }
    fn enqueue(&mut self, unit: VideoUnit) {
        self.bytes += unit.bytes.len();
        self.queue.push_back(unit);
    }
    /// The consumer fell behind: the head waited longer than `BACKLOG_AGE`
    /// with at least `BACKLOG_MIN_UNITS` queued (counting `arriving`), and no
    /// IDR was handed out within `IDR_GRACE`.
    fn backlogged(&self, now: Instant, arriving: usize) -> bool {
        self.idr_pulled_at
            .is_none_or(|at| now.saturating_duration_since(at) >= IDR_GRACE)
            && self.queue.len() + arriving >= BACKLOG_MIN_UNITS
            && self
                .queue
                .front()
                .is_some_and(|head| now.saturating_duration_since(head.received_at) > BACKLOG_AGE)
    }
    /// The consumer still takes frames, but the oldest it took after the last
    /// presented one has waited `STALL_WATCHDOG`, and nothing was presented
    /// for as long.
    fn stalled(&self, now: Instant) -> bool {
        let Some(presented) = self
            .presented
            .filter(|presented| presented.generation == self.generation)
        else {
            return false;
        };
        if now.saturating_duration_since(presented.presented_at) < STALL_WATCHDOG {
            return false;
        }
        let mut waiting = self.handed.iter().rev().take_while(|handed| {
            handed.generation == presented.generation && handed.frame_id > presented.frame_id
        });
        let Some(newest) = waiting.next() else {
            return false;
        };
        let oldest = waiting.last().unwrap_or(newest);
        now.saturating_duration_since(newest.handed_at) < STALL_WATCHDOG
            && now.saturating_duration_since(oldest.handed_at) >= STALL_WATCHDOG
    }
}

pub struct VideoSink {
    state: parking_lot::Mutex<SinkState>,
    ready: parking_lot::Condvar,
    closed: AtomicBool,
    /// Written under the state lock, so a waiting `next` never misses it.
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
    fn new(shared: Weak<ViewerShared>, max_fps: u32) -> Self {
        let epoch = Instant::now();
        Self {
            state: parking_lot::Mutex::new(SinkState::new(epoch, max_fps)),
            ready: parking_lot::Condvar::new(),
            closed: AtomicBool::new(false),
            paused: AtomicBool::new(false),
            attached: AtomicU64::new(0),
            shared,
            epoch,
        }
    }

    /// `max_fps` is the session's `limits.max_fps`, which bounds the fps cap.
    pub(crate) fn for_session(shared: Weak<ViewerShared>, max_fps: u32) -> Arc<Self> {
        Arc::new(Self::new(shared, max_fps))
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

    /// The read task. Never blocks. Units that cannot be decoded and shown in
    /// time are dropped here, and the sink asks for an IDR.
    pub(crate) fn push(&self, unit: VideoUnit) {
        self.push_at(unit, Instant::now());
    }

    fn push_at(&self, mut unit: VideoUnit, now: Instant) {
        if self.closed.load(Ordering::Acquire) {
            return;
        }
        let mut guard = self.state.lock();
        let state = &mut *guard;
        if self.paused.load(Ordering::Acquire) {
            // Sent before the host saw the pause; it resumes with an IDR.
            state.discarded += 1;
            return;
        }
        if unit.generation != state.generation {
            state.restart();
            state.generation = unit.generation;
        }
        if unit.codec == VideoCodec::Jpeg {
            // Every JPEG stands alone: only the newest is worth showing.
            state.clear();
            state.wait_key = false;
            unit.discontinuity = std::mem::take(&mut state.next_discontinuity);
            state.recovery.on_idr_received(now);
        } else if unit.keyframe {
            // Independently decodable: whatever is queued is superseded.
            let superseded = state.clear();
            if superseded > 0 {
                tracing::debug!(
                    target: "pb_mapper_desktop::frames",
                    units = superseded,
                    "desktop_sink_superseded"
                );
            }
            state.wait_key = false;
            unit.discontinuity = std::mem::take(&mut state.next_discontinuity) || superseded > 0;
            state.recovery.on_idr_received(now);
        } else if state.wait_key {
            state.discarded += 1;
            self.request(state, KeyframeReason::Gap, now);
            return;
        } else if state.backlogged(now, 1) {
            state.discarded += 1;
            self.drop_to_idr(state, Backlog::Age, now);
            return;
        } else if state.queue.len() >= SINK_MAX_UNITS
            || state.bytes + unit.bytes.len() > SINK_MAX_BYTES
        {
            state.discarded += 1;
            self.drop_to_idr(state, Backlog::Overflow, now);
            return;
        }
        state.enqueue(unit);
        drop(guard);
        self.ready.notify_one();
    }

    /// The consumer fell behind: drop the queue, wait for the next IDR and
    /// ask for one, and let the fps cap know.
    fn drop_to_idr(&self, state: &mut SinkState, cause: Backlog, now: Instant) {
        let units = state.restart();
        match cause {
            Backlog::Age => state.backlog_drops += 1,
            Backlog::Overflow => state.overflow_drops += 1,
        }
        state.fps_cap.on_backlog_drop();
        tracing::debug!(
            target: "pb_mapper_desktop::frames",
            ?cause,
            units,
            "desktop_sink_backlog_drop"
        );
        self.request(state, KeyframeReason::Backlog, now);
    }

    /// Every host `State`. A new generation makes everything queued
    /// unshowable; recovery follows whether the host is `available`: not
    /// connecting, and its capture not unavailable.
    pub(crate) fn on_state(&self, generation: u64, available: bool) {
        self.on_state_at(generation, available, Instant::now());
    }

    fn on_state_at(&self, generation: u64, available: bool, now: Instant) {
        let mut state = self.state.lock();
        let new_generation = state.generation != generation;
        if new_generation {
            state.restart();
            state.generation = generation;
        }
        state.recovery.on_host_state(available, new_generation, now);
    }

    /// The viewer is hidden: drop the queue and wait for the IDR the host
    /// sends on resume. `next` returns `Paused`, so native stall checks stop;
    /// no keyframe is asked for, and the fps cap and the watchdog hold still.
    pub(crate) fn pause(&self) {
        let mut state = self.state.lock();
        self.paused.store(true, Ordering::Release);
        state.restart();
        state.recovery.pause();
        drop(state);
        self.ready.notify_all();
    }

    pub(crate) fn resume(&self) {
        let mut state = self.state.lock();
        self.paused.store(false, Ordering::Release);
        state.recovery.resume(Instant::now());
        drop(state);
        self.ready.notify_all();
    }

    /// Drop everything queued and wake a blocked consumer, which then exits.
    /// Public because the FFI closes a source the moment its session is torn
    /// down, before the session's own stop has finished.
    pub fn close(&self) {
        let mut state = self.state.lock();
        self.closed.store(true, Ordering::Release);
        state.clear();
        drop(state);
        self.ready.notify_all();
    }

    /// The viewer tick (every 250 ms). Returns the fps the host should be
    /// capped at, when that changes; it is judged only while the session
    /// streams and is not paused. Also re-requests an awaited IDR on its
    /// backoff, and runs the stall watchdog.
    pub(crate) fn tick(
        &self,
        now: Instant,
        streaming: bool,
        direct: bool,
        quality: QualityPreset,
    ) -> Option<u32> {
        if self.closed.load(Ordering::Acquire) {
            return None;
        }
        let paused = self.paused.load(Ordering::Acquire);
        let mut guard = self.state.lock();
        let state = &mut *guard;
        let cap = if streaming && !paused {
            let presented_fps = state.present.presented_fps(now);
            state
                .fps_cap
                .tick(now, presented_fps, state.queue.len(), direct, quality)
        } else {
            state.fps_cap.freeze();
            None
        };
        let stalled = !paused && state.stalled(now);
        if let Some(reason) = state.recovery.tick(now, stalled) {
            self.forward(reason);
        }
        cap
    }

    /// The decoder failed `DECODER_FAILURE_LIMIT` times without showing a
    /// picture: the session's video cannot recover.
    pub(crate) fn decoder_failed(&self) -> bool {
        self.state.lock().recovery.failed()
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

    /// Ask the host for an IDR, through recovery and its limiter.
    pub(crate) fn request_keyframe(&self, reason: KeyframeReason) -> bool {
        if self.closed.load(Ordering::Acquire) {
            return false;
        }
        self.request(&mut self.state.lock(), reason, Instant::now())
    }

    fn request(&self, state: &mut SinkState, reason: KeyframeReason, now: Instant) -> bool {
        if !state.recovery.request(reason, now) {
            return false;
        }
        self.forward(reason);
        true
    }

    /// Under the sink lock: the outbound slots only ever follow it.
    fn forward(&self, reason: KeyframeReason) {
        if let Some(shared) = self.shared.upgrade() {
            shared.outbound.keyframe(reason);
        }
    }

    pub(crate) fn snapshot(&self, now: Instant) -> SinkStats {
        let mut state = self.state.lock();
        let decoder = DecoderInfo {
            state: state.recovery.decoder_state(state.presented.is_some()),
            ..state.decoder.clone()
        };
        SinkStats {
            presented: state.presented,
            presented_fps: state.present.presented_fps(now),
            present_interval: state.present.intervals(now),
            viewer_ms: state.present.viewer_ms(now),
            discarded: state.discarded,
            backlog_drops: state.backlog_drops + state.overflow_drops,
            keyframes_requested: state.recovery.requested(),
            decoder,
        }
    }

    /// Take the head for the consumer, after the backlog check. `None` when
    /// nothing is ready.
    fn take(&self, state: &mut SinkState, now: Instant, capacity: usize) -> Option<Pull> {
        if state.backlogged(now, 0) {
            self.drop_to_idr(state, Backlog::Age, now);
            return None;
        }
        let len = state.queue.front()?.bytes.len();
        if len > capacity {
            return Some(Pull::NeedCapacity(len));
        }
        let unit = state.queue.pop_front()?;
        state.bytes -= len;
        if state.handed.len() == RECENT_FRAMES {
            state.handed.pop_front();
        }
        state.handed.push_back(Handed {
            generation: unit.generation,
            frame_id: unit.frame_id,
            seq: unit.seq,
            received_at: unit.received_at,
            handed_at: now,
        });
        if unit.keyframe || unit.codec == VideoCodec::Jpeg {
            state.idr_pulled_at = Some(now);
            state.recovery.on_idr_handed(unit.generation, unit.frame_id);
        }
        Some(Pull::Unit(unit))
    }

    /// A sink with no session, for FFI and native tests. It validates
    /// presentations and forwards them nowhere.
    #[doc(hidden)]
    pub fn detached_for_test() -> Arc<Self> {
        Arc::new(Self::new(Weak::new(), DETACHED_MAX_FPS))
    }

    /// Feed a detached sink: the FFI's synthetic source and native tests.
    #[doc(hidden)]
    pub fn push_for_test(&self, unit: VideoUnit) {
        self.push(unit);
    }

    /// Pause or resume a detached sink, so a native consumer's `Paused`
    /// handling can run against the FFI's synthetic source.
    #[doc(hidden)]
    pub fn set_paused_for_test(&self, paused: bool) {
        if paused {
            self.pause();
        } else {
            self.resume();
        }
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
            if !sink.paused.load(Ordering::Acquire)
                && let Some(pull) = sink.take(&mut state, Instant::now(), capacity)
            {
                return pull;
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
        self.presented_at(generation, frame_id, Instant::now())
    }

    fn presented_at(&self, generation: u64, frame_id: u64, now: Instant) -> bool {
        let sink = &*self.sink;
        let mut guard = sink.state.lock();
        let state = &mut *guard;
        let handed = state
            .handed
            .iter()
            .rev()
            .find(|handed| handed.generation == generation && handed.frame_id == frame_id)
            .copied();
        let Some(handed) = handed.filter(|handed| {
            self.current()
                && !sink.paused.load(Ordering::Acquire)
                && generation == state.generation
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
        state.recovery.on_presented(generation, frame_id);
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
    /// through the viewer's recovery and its limiter; false means it was held
    /// back. `DecoderError` reports also count toward giving up on the video.
    pub fn keyframe(&self, reason: KeyframeReason) -> bool {
        self.current() && self.sink.request_keyframe(reason)
    }

    /// The decoder in use, for the overlay. At most 64 characters are kept.
    pub fn report(&self, decoder: &str, hardware: bool) {
        let mut state = self.sink.state.lock();
        state.decoder.name = decoder.chars().take(DECODER_NAME_MAX).collect();
        state.decoder.hardware = Some(hardware);
    }

    /// Detach now, even while another thread is blocked in `next`: that call
    /// returns `Closed`, and the sink takes a new consumer at once. Idempotent;
    /// dropping the consumer does the same.
    pub fn detach(&self) {
        let _ =
            self.sink
                .attached
                .compare_exchange(self.token, 0, Ordering::AcqRel, Ordering::Acquire);
        // A `next` holds the lock from its check until it waits, so taking it
        // here means the notification cannot fall between the two.
        drop(self.sink.state.lock());
        self.sink.ready.notify_all();
    }

    /// `next` at `now`, without waiting.
    #[cfg(test)]
    fn pull_at(&self, now: Instant, capacity: usize) -> Option<Pull> {
        let sink = &*self.sink;
        let mut state = sink.state.lock();
        (self.current() && !sink.paused.load(Ordering::Acquire))
            .then(|| sink.take(&mut state, now, capacity))
            .flatten()
    }
}
impl Drop for VideoConsumer {
    fn drop(&mut self) {
        self.detach();
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn video(generation: u64, frame_id: u64, keyframe: bool, received_at: Instant) -> VideoUnit {
        VideoUnit {
            seq: frame_id,
            generation,
            frame_id,
            codec: VideoCodec::H264,
            width: 16,
            height: 16,
            keyframe,
            duplicate: false,
            discontinuity: false,
            pts_us: 0,
            bytes: bytes::Bytes::from(vec![0; 10]),
            received_at,
        }
    }

    fn unit(generation: u64, frame_id: u64, len: usize) -> VideoUnit {
        VideoUnit {
            bytes: bytes::Bytes::from(vec![0; len]),
            ..video(generation, frame_id, frame_id == 1, Instant::now())
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
        sink.on_state(2, true);
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

    #[test]
    fn backlog_drops_to_next_idr_with_grace_and_limiter_backoff() {
        let sink = VideoSink::detached_for_test();
        let consumer = sink.attach().unwrap();
        let start = Instant::now();
        let at = |millis: u64| start + Duration::from_millis(millis);
        // A unit received at `millis`, pushed then.
        let push = |frame_id: u64, keyframe: bool, millis: u64| {
            sink.push_at(video(1, frame_id, keyframe, at(millis)), at(millis));
        };
        let pull_at = |millis: u64| match consumer.pull_at(at(millis), 1 << 20) {
            Some(Pull::Unit(unit)) => Some(unit),
            None => None,
            Some(other) => panic!("expected a unit, got {other:?}"),
        };
        let tick = |millis: u64| sink.tick(at(millis), true, true, QualityPreset::Balanced);
        let requested = || sink.snapshot(start).keyframes_requested;
        let backlog_drops = || sink.snapshot(start).backlog_drops;

        // Streaming, with the first IDR shown.
        sink.on_state_at(1, true, at(0));
        push(1, true, 0);
        assert!(pull_at(0).is_some_and(|unit| unit.discontinuity));
        assert!(consumer.presented_at(1, 1, at(0)));

        // A burst of fresh units is not a backlog, however many.
        for frame_id in 2..=9 {
            push(frame_id, false, 1000);
        }
        for frame_id in 2..=9 {
            assert_eq!(pull_at(1010).map(|unit| unit.frame_id), Some(frame_id));
        }
        assert!(consumer.presented_at(1, 9, at(1010)));

        // Within IDR_GRACE of handing out an IDR, old units wait for the
        // consumer; once it passed, the queue drops to the next IDR at push.
        push(10, true, 2000);
        assert!(pull_at(2000).is_some());
        assert!(consumer.presented_at(1, 10, at(2000)));
        push(11, false, 2000);
        push(12, false, 2000);
        push(13, false, 2300);
        assert_eq!((backlog_drops(), requested()), (0, 0));
        push(14, false, 2600);
        assert_eq!((backlog_drops(), requested()), (1, 1));
        assert!(pull_at(2600).is_none());

        // Until an IDR comes, predicted frames are dropped, and the request
        // is repeated after 0.5, 1, 2, 4 and 4 s. The drop also capped fps.
        push(15, false, 2650);
        assert_eq!(requested(), 1);
        assert_eq!(tick(3099), Some(30));
        for (quiet, due) in [
            (3099, 3100),
            (4099, 4100),
            (6099, 6100),
            (10_099, 10_100),
            (14_099, 14_100),
        ] {
            let before = requested();
            tick(quiet);
            assert_eq!(requested(), before);
            tick(due);
            assert_eq!(requested(), before + 1);
        }

        // An arriving IDR supersedes what is queued and says so.
        push(16, true, 14_200);
        push(17, false, 14_200);
        push(18, false, 14_200);
        push(19, true, 14_210);
        let idr = pull_at(14_210).unwrap();
        assert!(idr.frame_id == 19 && idr.keyframe && idr.discontinuity);
        assert!(pull_at(14_210).is_none());
        assert!(consumer.presented_at(1, 19, at(14_210)));

        // The age rule also runs when the consumer pulls.
        for frame_id in 20..=22 {
            push(frame_id, false, 14_220);
        }
        assert!(pull_at(14_800).is_none());
        assert_eq!(backlog_drops(), 2);
        assert_eq!(requested(), 7);
    }
}
