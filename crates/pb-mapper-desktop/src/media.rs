//! A bounded access-unit queue. Dropping a predictive frame invalidates the
//! remaining GOP: discard it and request an IDR instead of decoding corrupt refs.
//! The viewer now hands video to a `VideoSink`; only the v1 host's congestion
//! control (its frame bound, and this queue in its tests) still uses this
//! module, which the G1 cleanup deletes.
#![cfg_attr(not(test), allow(dead_code))]
use crate::{CursorPosition, Frame, SessionStatus, VideoCodec};
use parking_lot::Mutex;
use std::{
    collections::VecDeque,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
};

pub(crate) const MAX_QUEUED_FRAMES: usize = 8;
pub(crate) const MAX_QUEUED_BYTES: usize = 8 * 1024 * 1024;

#[derive(Default)]
struct Pending {
    frames: VecDeque<Frame>,
    status: Option<SessionStatus>,
    cursor: Option<CursorPosition>,
    bytes: usize,
    generation: u64,
    wait_keyframe: bool,
    requested: bool,
}
#[derive(Default)]
pub(crate) struct Queue {
    pending: Mutex<Pending>,
    keyframe: AtomicBool,
    dropped: AtomicU64,
}
impl Queue {
    pub fn push(&self, frame: Frame) {
        let mut q = self.pending.lock();
        let predictive = frame.status.video.codec == VideoCodec::H264 && !frame.bytes.is_empty();
        if q.generation != frame.status.generation || frame.bytes.is_empty() {
            q.frames.clear();
            q.status = None;
            q.bytes = 0;
            q.generation = frame.status.generation;
            q.wait_keyframe = predictive;
            q.requested = false;
        }
        q.cursor = frame.status.cursor;
        if let Some(status) = q.status.as_mut() {
            status.cursor = frame.status.cursor;
        }
        // About 67 ms at 120 fps absorbs normal relay delivery bursts without
        // accumulating an entire GOP. The byte bound also applies to IDRs.
        if q.frames.len() >= MAX_QUEUED_FRAMES || q.bytes + frame.bytes.len() > MAX_QUEUED_BYTES {
            self.dropped
                .fetch_add(q.frames.len() as u64, Ordering::Relaxed);
            q.frames.clear();
            q.bytes = 0;
            q.wait_keyframe = predictive;
            q.requested = false;
        }
        if predictive && q.wait_keyframe && !frame.status.video.keyframe {
            if !q.requested {
                self.keyframe.store(true, Ordering::Release);
                q.requested = true;
            }
            self.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        }
        if !predictive || frame.status.video.keyframe {
            q.wait_keyframe = false;
            q.requested = false;
        }
        q.bytes += frame.bytes.len();
        q.frames.push_back(frame);
    }
    pub fn pop(&self) -> Option<Frame> {
        let mut q = self.pending.lock();
        if let Some(status) = q.status.take() {
            return Some(Frame {
                status,
                bytes: std::sync::Arc::from([]),
            });
        }
        let mut frame = q.frames.pop_front()?;
        q.bytes -= frame.bytes.len();
        frame.status.cursor = q.cursor;
        Some(frame)
    }
    /// Independent, coalesced observation; never clears video references or
    /// acquires presentation credit. A new desktop requires a real frame first.
    pub fn push_status(&self, status: SessionStatus) -> bool {
        let mut q = self.pending.lock();
        if status.state != "streaming" || status.generation != q.generation {
            return false;
        }
        q.cursor = status.cursor;
        q.status = Some(status);
        true
    }
    pub fn take_keyframe_request(&self) -> bool {
        self.keyframe.swap(false, Ordering::AcqRel)
    }
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
    pub fn resynchronize(&self) {
        let mut q = self.pending.lock();
        self.dropped
            .fetch_add(q.frames.len() as u64, Ordering::Relaxed);
        q.frames.clear();
        q.bytes = 0;
        q.wait_keyframe = true;
        q.requested = true;
        self.keyframe.store(true, Ordering::Release);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::{SessionStatus, VideoFormat};
    fn frame(id: u64, keyframe: bool) -> Frame {
        Frame {
            status: SessionStatus {
                generation: 1,
                frame_id: id,
                video: VideoFormat {
                    codec: VideoCodec::H264,
                    keyframe,
                    ..Default::default()
                },
                ..Default::default()
            },
            bytes: vec![1].into(),
        }
    }
    #[test]
    fn overflow_discards_dependent_frames_until_idr() {
        let q = Queue::default();
        q.push(frame(1, true));
        for id in 2..=12 {
            q.push(frame(id, false));
        }
        assert!(q.pop().is_none());
        assert!(q.take_keyframe_request());
        q.push(frame(13, true));
        q.push(frame(14, false));
        assert_eq!(q.pop().map(|f| f.status.frame_id), Some(13));
        assert_eq!(q.pop().map(|f| f.status.frame_id), Some(14));
        assert_eq!(q.dropped(), 12);
        let mut switched = frame(15, false);
        switched.status.generation = 2;
        q.push(switched);
        assert!(q.pop().is_none());
        assert!(q.take_keyframe_request());
    }
    #[test]
    fn status_is_coalesced_without_reordering_or_invalidating_video() {
        let q = Queue::default();
        q.push(frame(1, true));
        q.push(frame(2, false));
        let mut status = SessionStatus {
            state: "streaming".into(),
            generation: 1,
            frame_id: 2,
            cursor: Some(CursorPosition {
                x: 10,
                y: 20,
                visible: true,
            }),
            ..Default::default()
        };
        assert!(q.push_status(status.clone()));
        status.cursor.as_mut().unwrap().x = 30;
        assert!(q.push_status(status.clone()));
        let update = q.pop().unwrap();
        assert!(update.bytes.is_empty());
        assert_eq!(update.status.cursor, status.cursor);
        for id in [1, 2] {
            let video = q.pop().unwrap();
            assert_eq!(video.status.frame_id, id);
            assert!(!video.bytes.is_empty());
            assert_eq!(video.status.cursor, status.cursor);
        }
        assert!(q.pop().is_none());
        assert!(!q.take_keyframe_request());
        assert_eq!(q.dropped(), 0);

        status.generation = 2;
        assert!(!q.push_status(status.clone()));
        status.generation = 1;
        assert!(q.push_status(status));
        let mut switched = frame(3, true);
        switched.status.generation = 2;
        q.push(switched);
        let next = q.pop().unwrap();
        assert!(!next.bytes.is_empty());
        assert_eq!(next.status.generation, 2);
        assert_eq!(next.status.cursor, None);
        assert!(q.pop().is_none());
    }
}
