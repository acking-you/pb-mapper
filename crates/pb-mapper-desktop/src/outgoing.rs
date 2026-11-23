//! The hand-off from thread C to B-send: encoded units waiting for send credit.
//! A slow viewer keeps a decodable prefix, and after a gap resumes at a
//! keyframe. Any keyframe supersedes the units still waiting: none has a seq
//! yet, and nothing after a keyframe refers to anything before it.
use crate::{
    VideoCodec,
    worker::{ENCODER_DEPTH, TRANSPORT_GATE},
};
use std::{
    collections::VecDeque,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

pub(crate) const OUTGOING_CAPACITY: usize = TRANSPORT_GATE + ENCODER_DEPTH;
pub(crate) const OUTGOING_BYTES: usize = 8 * 1024 * 1024;
const _: () = assert!(
    OUTGOING_BYTES >= crate::wire::MAX_PAYLOAD,
    "the largest unit the wire carries must fit an empty hand-off"
);
/// A head older than this is cleared and replaced by a requested IDR. Time it
/// waits behind an IDR in flight does not count (`CreditLedger::aging_since`).
pub(crate) const OUTGOING_MAX_AGE: Duration = Duration::from_secs(1);

/// Encoded picture bytes: kept directly as `Vec<u8>` when a single viewer is
/// active (zero promotion allocation), or refcounted across multiple viewers.
pub(crate) enum FrameBytes {
    Owned(Vec<u8>),
    Shared(bytes::Bytes),
}
impl std::ops::Deref for FrameBytes {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        match self {
            Self::Owned(bytes) => bytes,
            Self::Shared(bytes) => bytes,
        }
    }
}

pub(crate) struct HostUnit {
    pub generation: u64,
    pub frame_id: u64,
    pub codec: VideoCodec,
    pub width: u32,
    pub height: u32,
    pub keyframe: bool,
    pub duplicate: bool,
    pub pts_us: u64,
    pub encode_us: u32,
    pub ready_at: Instant,
    pub bytes: FrameBytes,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Push {
    pub dropped: usize,
    pub request_idr: bool,
}

struct Queue {
    units: VecDeque<HostUnit>,
    needs_keyframe: bool,
}

/// What B-send needs to admit the head without taking it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct UnitMeta {
    pub generation: u64,
    pub frame_id: u64,
    pub keyframe: bool,
    pub len: usize,
    pub ready_at: Instant,
}

/// Cumulative production, diffed by the QoS controller per window.
#[derive(Default)]
pub(crate) struct ProducedCounters {
    frames: AtomicU64,
    unique: AtomicU64,
    keyframes: AtomicU64,
    non_idr_frames: AtomicU64,
    non_idr_bytes: AtomicU64,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ProducedSnapshot {
    pub frames: u64,
    pub unique: u64,
    pub keyframes: u64,
    pub non_idr_frames: u64,
    pub non_idr_bytes: u64,
}

pub(crate) struct Outgoing {
    q: parking_lot::Mutex<Queue>,
    notify: tokio::sync::Notify,
    stats: ProducedCounters,
}
impl Outgoing {
    pub fn new() -> Self {
        Self {
            q: parking_lot::Mutex::new(Queue {
                units: VecDeque::with_capacity(OUTGOING_CAPACITY),
                needs_keyframe: true,
            }),
            notify: tokio::sync::Notify::new(),
            stats: ProducedCounters::default(),
        }
    }

    pub fn push(&self, unit: HostUnit) -> Push {
        let stats = &self.stats;
        stats.frames.fetch_add(1, Ordering::Relaxed);
        if !unit.duplicate {
            stats.unique.fetch_add(1, Ordering::Relaxed);
        }
        if unit.keyframe {
            stats.keyframes.fetch_add(1, Ordering::Relaxed);
        } else {
            stats.non_idr_frames.fetch_add(1, Ordering::Relaxed);
            stats
                .non_idr_bytes
                .fetch_add(unit.bytes.len() as u64, Ordering::Relaxed);
        }
        let mut q = self.q.lock();
        let full = q.units.len() >= OUTGOING_CAPACITY
            || q.units.iter().map(|unit| unit.bytes.len()).sum::<usize>() + unit.bytes.len()
                > OUTGOING_BYTES;
        if !unit.keyframe && (full || q.needs_keyframe) {
            q.needs_keyframe = true;
            // Avoid forcing the shared encoder to produce large IDRs for a
            // viewer that still has no room. Other viewers keep their cadence.
            return Push {
                dropped: 1,
                request_idr: q.units.len() < TRANSPORT_GATE,
            };
        }
        let mut result = Push::default();
        // A keyframe supersedes the units still waiting, full or not, so the
        // newer independent picture goes next. None of them has a seq yet,
        // since B-send books one at the write, so the seq stays contiguous;
        // and a keyframe is an H.264 IDR, an H.265 IRAP or a JPEG picture,
        // with no frame reordered, so nothing after it refers to anything
        // older. A full hand-off gets here only with a keyframe.
        if unit.keyframe {
            result.dropped = q.units.len();
            q.units.clear();
        }
        q.needs_keyframe = false;
        q.units.push_back(unit);
        drop(q);
        self.notify.notify_one();
        result
    }

    pub fn peek_meta(&self) -> Option<UnitMeta> {
        self.q.lock().units.front().map(|unit| UnitMeta {
            generation: unit.generation,
            frame_id: unit.frame_id,
            keyframe: unit.keyframe,
            len: unit.bytes.len(),
            ready_at: unit.ready_at,
        })
    }

    #[cfg(test)]
    pub fn pop(&self) -> Option<HostUnit> {
        self.q.lock().units.pop_front()
    }

    /// Pop the head only if it is still the unit `peek_meta` described: C may
    /// clear or retain the queue between the peek and the pop.
    pub fn pop_if(&self, frame_id: u64) -> Option<HostUnit> {
        let mut q = self.q.lock();
        if q.units.front()?.frame_id == frame_id {
            q.units.pop_front()
        } else {
            None
        }
    }

    /// Includes the head B-send is evaluating.
    pub fn len(&self) -> usize {
        self.q.lock().units.len()
    }

    pub fn clear(&self) -> usize {
        let mut q = self.q.lock();
        let dropped = q.units.len();
        q.units.clear();
        q.needs_keyframe = true;
        dropped
    }

    /// Drop units older than generation `generation` (C, on invalidate).
    pub fn retain_generation(&self, generation: u64) -> usize {
        let mut q = self.q.lock();
        let before = q.units.len();
        q.units.retain(|unit| unit.generation >= generation);
        q.needs_keyframe |= q.units.is_empty();
        before - q.units.len()
    }

    /// Resolves after a push, including one that happened before this call.
    pub async fn notified(&self) {
        self.notify.notified().await;
    }

    pub fn produced(&self) -> ProducedSnapshot {
        let stats = &self.stats;
        ProducedSnapshot {
            frames: stats.frames.load(Ordering::Relaxed),
            unique: stats.unique.load(Ordering::Relaxed),
            keyframes: stats.keyframes.load(Ordering::Relaxed),
            non_idr_frames: stats.non_idr_frames.load(Ordering::Relaxed),
            non_idr_bytes: stats.non_idr_bytes.load(Ordering::Relaxed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(generation: u64, frame_id: u64, keyframe: bool, len: usize) -> HostUnit {
        HostUnit {
            generation,
            frame_id,
            codec: VideoCodec::H264,
            width: 16,
            height: 16,
            keyframe,
            duplicate: false,
            pts_us: frame_id,
            encode_us: 0,
            ready_at: Instant::now(),
            bytes: FrameBytes::Owned(vec![0; len]),
        }
    }

    #[tokio::test]
    async fn overflow_keeps_only_an_idr_and_invalidate_drops_old_generations() {
        let outgoing = Outgoing::new();
        let full = OUTGOING_CAPACITY as u64;
        for frame in 1..=full {
            assert_eq!(
                outgoing.push(unit(1, frame, frame == 1, 10)),
                Push::default()
            );
        }
        outgoing.notified().await;
        let blocked = outgoing.push(unit(1, full + 1, false, 10));
        assert_eq!(blocked.dropped, 1);
        assert!(!blocked.request_idr);
        for frame in 1..=full {
            assert_eq!(outgoing.pop().unwrap().frame_id, frame);
        }
        let gap = outgoing.push(unit(1, full + 2, false, 10));
        assert!(gap.request_idr);
        assert!(outgoing.peek_meta().is_none());
        assert_eq!(
            outgoing.push(unit(1, full + 3, true, OUTGOING_BYTES)),
            Push::default()
        );
        assert_eq!(outgoing.push(unit(1, full + 4, true, 1)).dropped, 1);
        assert!(outgoing.peek_meta().unwrap().keyframe);
        // C drops the old generation before it pushes the new one's IDR.
        assert_eq!(outgoing.retain_generation(2), 1);
        assert_eq!(outgoing.push(unit(2, full + 5, true, 10)), Push::default());
        assert_eq!(outgoing.pop().unwrap().frame_id, full + 5);
        assert_eq!(outgoing.clear(), 0);
        let produced = outgoing.produced();
        assert_eq!((produced.frames, produced.keyframes), (full + 5, 4));
    }

    /// A keyframe pushed behind units still waiting for credit replaces them,
    /// and they count as dropped; what follows it queues behind it as usual.
    #[test]
    fn a_keyframe_supersedes_what_waits_ahead_of_it() {
        let outgoing = Outgoing::new();
        assert_eq!(outgoing.push(unit(1, 1, true, 10)), Push::default());
        assert_eq!(outgoing.pop().unwrap().frame_id, 1);
        for frame in [2, 3] {
            assert_eq!(outgoing.push(unit(1, frame, false, 10)), Push::default());
        }
        assert_eq!(
            outgoing.push(unit(1, 4, true, 10)),
            Push {
                dropped: 2,
                request_idr: false,
            }
        );
        let head = outgoing.peek_meta().unwrap();
        assert_eq!((head.frame_id, head.keyframe), (4, true));
        assert_eq!(outgoing.len(), 1);
        // Production still counts every unit encoded, superseded or not.
        let produced = outgoing.produced();
        assert_eq!((produced.frames, produced.keyframes), (4, 2));
        assert_eq!(outgoing.push(unit(1, 5, false, 10)), Push::default());
        assert_eq!(outgoing.len(), 2);

        // Every JPEG picture is a keyframe: only the newest waits.
        let jpeg = |frame_id| HostUnit {
            codec: VideoCodec::Jpeg,
            ..unit(1, frame_id, true, 10)
        };
        let outgoing = Outgoing::new();
        assert_eq!(outgoing.push(jpeg(1)), Push::default());
        assert_eq!(outgoing.push(jpeg(2)).dropped, 1);
        assert_eq!(outgoing.len(), 1);
        assert_eq!(outgoing.pop().unwrap().frame_id, 2);
    }
}
