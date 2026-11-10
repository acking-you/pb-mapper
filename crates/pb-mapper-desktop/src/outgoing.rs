//! The hand-off from thread C to B-send: encoded units waiting for send credit.
//! The submit gate keeps it below two units plus the encoder's in-flight ones,
//! so an overflow is a bug path; it never discards an IDR it was handed.
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
    pub bytes: bytes::Bytes,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Push {
    Queued,
    /// The queue was cleared; the pushed unit survives only if it is an IDR.
    /// The caller requests a host resync.
    Overflow {
        dropped: usize,
    },
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
    q: parking_lot::Mutex<VecDeque<HostUnit>>,
    notify: tokio::sync::Notify,
    stats: ProducedCounters,
}
impl Outgoing {
    pub fn new() -> Self {
        Self {
            q: parking_lot::Mutex::new(VecDeque::with_capacity(OUTGOING_CAPACITY)),
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
        let result = {
            let mut q = self.q.lock();
            let queued: usize = q.iter().map(|unit| unit.bytes.len()).sum();
            if q.len() >= OUTGOING_CAPACITY || queued + unit.bytes.len() > OUTGOING_BYTES {
                let mut dropped = q.len();
                q.clear();
                if unit.keyframe {
                    q.push_back(unit);
                } else {
                    dropped += 1;
                }
                Push::Overflow { dropped }
            } else {
                q.push_back(unit);
                Push::Queued
            }
        };
        self.notify.notify_one();
        result
    }

    pub fn peek_meta(&self) -> Option<UnitMeta> {
        self.q.lock().front().map(|unit| UnitMeta {
            generation: unit.generation,
            frame_id: unit.frame_id,
            keyframe: unit.keyframe,
            len: unit.bytes.len(),
            ready_at: unit.ready_at,
        })
    }

    #[cfg(test)]
    pub fn pop(&self) -> Option<HostUnit> {
        self.q.lock().pop_front()
    }

    /// Pop the head only if it is still the unit `peek_meta` described: C may
    /// clear or retain the queue between the peek and the pop.
    pub fn pop_if(&self, frame_id: u64) -> Option<HostUnit> {
        let mut q = self.q.lock();
        if q.front()?.frame_id == frame_id {
            q.pop_front()
        } else {
            None
        }
    }

    /// Includes the head B-send is evaluating.
    pub fn len(&self) -> usize {
        self.q.lock().len()
    }

    pub fn clear(&self) -> usize {
        let mut q = self.q.lock();
        let dropped = q.len();
        q.clear();
        dropped
    }

    /// Drop units older than generation `generation` (C, on invalidate).
    pub fn retain_generation(&self, generation: u64) -> usize {
        let mut q = self.q.lock();
        let before = q.len();
        q.retain(|unit| unit.generation >= generation);
        before - q.len()
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
            bytes: bytes::Bytes::from(vec![0; len]),
        }
    }

    #[tokio::test]
    async fn overflow_keeps_only_an_idr_and_invalidate_drops_old_generations() {
        let outgoing = Outgoing::new();
        let full = OUTGOING_CAPACITY as u64;
        for frame in 1..=full {
            assert_eq!(outgoing.push(unit(1, frame, frame == 1, 10)), Push::Queued);
        }
        // A push that was not awaited yet still wakes B-send.
        outgoing.notified().await;
        assert_eq!(
            outgoing.push(unit(1, full + 1, false, 10)),
            Push::Overflow {
                dropped: OUTGOING_CAPACITY + 1
            }
        );
        assert_eq!(outgoing.len(), 0);
        assert_eq!(
            outgoing.push(unit(1, full + 2, true, OUTGOING_BYTES)),
            Push::Queued
        );
        assert_eq!(
            outgoing.push(unit(1, full + 3, true, 1)),
            Push::Overflow { dropped: 1 }
        );
        let head = outgoing.peek_meta().unwrap();
        assert!(head.keyframe && head.len == 1);

        outgoing.push(unit(2, full + 4, true, 10));
        assert_eq!(outgoing.retain_generation(2), 1);
        assert_eq!(outgoing.pop().map(|unit| unit.frame_id), Some(full + 4));
        assert_eq!(outgoing.clear(), 0);
        let produced = outgoing.produced();
        assert_eq!((produced.frames, produced.keyframes), (full + 4, 4));
        assert_eq!(
            (produced.non_idr_frames, produced.non_idr_bytes),
            (full, 10 * full)
        );
    }
}
