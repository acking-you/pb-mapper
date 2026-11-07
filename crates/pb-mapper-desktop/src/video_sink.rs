//! The viewer's video hand-off to exactly one native consumer: the
//! network read task pushes, the consumer's own thread pulls on a `Condvar`,
//! never through tokio. This is the skeleton, a plain FIFO; the v2 viewer adds
//! the backlog and overflow policy, presentation validation, the keyframe
//! limiter and recovery, the fps cap, and the forwarding to its session.
use crate::{KeyframeReason, VideoCodec};
use std::{
    collections::VecDeque,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

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
    /// restarts at this IDR and rebuilds only if the size changed.
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

#[derive(Default)]
struct SinkState {
    queue: VecDeque<VideoUnit>,
    generation: u64,
    decoder: Option<(String, bool)>,
}

pub struct VideoSink {
    state: parking_lot::Mutex<SinkState>,
    ready: parking_lot::Condvar,
    closed: AtomicBool,
    paused: AtomicBool,
    /// 0 = none, else the attached consumer's token.
    attached: AtomicU64,
}

/// Consumer tokens are never reused, so a stale consumer can never match.
static NEXT_TOKEN: AtomicU64 = AtomicU64::new(1);

impl VideoSink {
    fn new() -> Self {
        Self {
            state: parking_lot::Mutex::new(SinkState::default()),
            ready: parking_lot::Condvar::new(),
            closed: AtomicBool::new(false),
            paused: AtomicBool::new(false),
            attached: AtomicU64::new(0),
        }
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

    /// The read task. Never blocks.
    pub(crate) fn push(&self, unit: VideoUnit) {
        if self.closed.load(Ordering::Acquire) {
            return;
        }
        self.state.lock().queue.push_back(unit);
        self.ready.notify_one();
    }

    pub(crate) fn set_generation(&self, generation: u64) {
        self.state.lock().generation = generation;
    }

    pub(crate) fn pause(&self) {
        self.paused.store(true, Ordering::Release);
        self.ready.notify_all();
    }

    pub(crate) fn resume(&self) {
        self.paused.store(false, Ordering::Release);
        self.ready.notify_all();
    }

    /// Drop everything queued and wake a blocked consumer, which then exits.
    pub(crate) fn close(&self) {
        self.closed.store(true, Ordering::Release);
        self.state.lock().queue.clear();
        self.ready.notify_all();
    }

    /// A sink with no session, for FFI and native tests.
    #[doc(hidden)]
    pub fn detached_for_test() -> Arc<Self> {
        Arc::new(Self::new())
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
    /// Blocks on the sink's `Condvar` only, never on tokio. While paused it
    /// waits `timeout`, then returns `Paused`. A head larger than `capacity`
    /// returns `NeedCapacity` and stays queued.
    pub fn next(&self, timeout: Duration, capacity: usize) -> Pull {
        let sink = &*self.sink;
        let deadline = Instant::now() + timeout;
        let mut state = sink.state.lock();
        loop {
            if sink.closed.load(Ordering::Acquire)
                || sink.attached.load(Ordering::Acquire) != self.token
            {
                return Pull::Closed;
            }
            let paused = sink.paused.load(Ordering::Acquire);
            if !paused && let Some(head) = state.queue.front() {
                if head.bytes.len() > capacity {
                    return Pull::NeedCapacity(head.bytes.len());
                }
                if let Some(unit) = state.queue.pop_front() {
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

    /// The native renderer presented `(generation, frame_id)`. The viewer validates
    /// it and forwards it to the host as input authorisation; this skeleton
    /// has no session to forward to, so it accepts nothing.
    pub fn presented(&self, _generation: u64, _frame_id: u64) -> bool {
        false
    }

    /// The decoder needs an independently decodable picture. The viewer sends it
    /// through the keyframe limiter; this skeleton queues nothing.
    pub fn keyframe(&self, _reason: KeyframeReason) -> bool {
        false
    }

    /// The decoder in use, for the overlay. At most 64 characters are kept.
    pub fn report(&self, decoder: &str, hardware: bool) {
        let name = decoder.chars().take(64).collect();
        self.sink.state.lock().decoder = Some((name, hardware));
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

    fn unit(frame_id: u64, len: usize) -> VideoUnit {
        VideoUnit {
            seq: frame_id,
            generation: 1,
            frame_id,
            codec: VideoCodec::H264,
            width: 16,
            height: 16,
            keyframe: frame_id == 1,
            duplicate: false,
            discontinuity: frame_id == 1,
            pts_us: 0,
            bytes: bytes::Bytes::from(vec![0; len]),
            received_at: Instant::now(),
        }
    }

    #[test]
    fn single_consumer_need_capacity_keeps_unit_and_close_wakes() {
        let sink = VideoSink::detached_for_test();
        let consumer = sink.attach().unwrap();
        assert_eq!(sink.attach().err(), Some(AttachError::AlreadyAttached));

        sink.push(unit(1, 1000));
        sink.push(unit(2, 10));
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
}
