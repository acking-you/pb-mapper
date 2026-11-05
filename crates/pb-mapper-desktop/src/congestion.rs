//! Opt-in renderer feedback. Socket writes only move bytes into local buffers;
//! admission is bounded by frames the remote renderer has actually presented.
use crate::{Frame, VideoPreferences};
use parking_lot::Mutex;
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};
use tokio::sync::Notify;

const INITIAL_WINDOW: usize = 64 * 1024;
const MIN_WINDOW: usize = 16 * 1024;
const MAX_WINDOW: usize = 512 * 1024;
const MAX_FRAMES: usize = 24;
const MAX_INDEPENDENT_FRAME: usize = 256 * 1024;
const MIN_BITRATE: u32 = 256_000;
const MAX_READY_AGE: Duration = Duration::from_millis(250);
const CONGESTED: Duration = Duration::from_millis(350);

struct Ready {
    generation: u64,
    id: u64,
    at: Instant,
}
struct Sent {
    generation: u64,
    id: u64,
    ready: Instant,
    sent: Instant,
    bytes: usize,
}
struct State {
    ready: VecDeque<Ready>,
    sent: VecDeque<Sent>,
    bytes: usize,
    window: usize,
    bitrate: u32,
    ceiling: VideoPreferences,
    last_ack: Option<Instant>,
    baseline: Option<Duration>,
    adjusted: Instant,
    congested_since: Option<Instant>,
    waiting: bool,
}
pub(crate) struct Feedback {
    state: Mutex<State>,
    pub changed: Notify,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Admission {
    Send,
    Wait,
    Discard,
}
impl Feedback {
    pub fn new(ceiling: VideoPreferences) -> Self {
        Self {
            state: Mutex::new(State {
                ready: VecDeque::with_capacity(256),
                sent: VecDeque::with_capacity(MAX_FRAMES),
                bytes: 0,
                window: INITIAL_WINDOW,
                bitrate: ceiling.bitrate,
                ceiling,
                last_ack: None,
                baseline: None,
                adjusted: Instant::now(),
                congested_since: None,
                waiting: false,
            }),
            changed: Notify::new(),
        }
    }
    pub fn captured(&self, frame: &Frame) {
        if frame.bytes.is_empty() {
            return;
        }
        let mut state = self.state.lock();
        state.ready.push_back(Ready {
            generation: frame.status.generation,
            id: frame.status.frame_id,
            at: Instant::now(),
        });
        while state.ready.len() > 256 {
            state.ready.pop_front();
        }
    }
    pub fn admit(&self, frame: &Frame, bytes: usize) -> Admission {
        self.admit_at(frame, bytes, Instant::now())
    }
    fn admit_at(&self, frame: &Frame, bytes: usize, now: Instant) -> Admission {
        // Status-only updates carry no decoder work or presentation obligation.
        if frame.bytes.is_empty() {
            return Admission::Send;
        }
        let mut state = self.state.lock();
        let Some(ready) = state
            .ready
            .iter()
            .find(|ready| {
                ready.generation == frame.status.generation && ready.id == frame.status.frame_id
            })
            .map(|ready| ready.at)
        else {
            return Admission::Discard;
        };
        if now.saturating_duration_since(ready) > MAX_READY_AGE {
            return Admission::Discard;
        }
        if bytes > MAX_INDEPENDENT_FRAME {
            let reduced = state.bitrate / 2;
            state.reduce(now, reduced);
            return Admission::Discard;
        }
        // A single bounded access unit may exceed the current byte window. In
        // particular the initial IDR must not wait for an impossible first ACK.
        if !state.sent.is_empty()
            && (state.sent.len() >= MAX_FRAMES || state.bytes.saturating_add(bytes) > state.window)
        {
            state.waiting = true;
            return Admission::Wait;
        }
        state.bytes += bytes;
        state.sent.push_back(Sent {
            generation: frame.status.generation,
            id: frame.status.frame_id,
            ready,
            sent: now,
            bytes,
        });
        state.waiting = state.bytes >= state.window || state.sent.len() >= MAX_FRAMES;
        Admission::Send
    }
    /// A transport acknowledgment is independently checked against the sent
    /// ledger. It never grants control or renews input. An old-generation/late
    /// acknowledgment may drain transport debt while Engine rejects it as input
    /// authority under the unchanged two-second freshness rule.
    pub fn presented(&self, generation: u64, id: u64) {
        self.presented_at(generation, id, Instant::now());
    }
    fn presented_at(&self, generation: u64, id: u64, now: Instant) {
        let mut state = self.state.lock();
        let Some(index) = state
            .sent
            .iter()
            .position(|sent| sent.generation == generation && sent.id == id)
        else {
            return;
        };
        let latency = now.saturating_duration_since(state.sent[index].ready);
        let mut acknowledged = 0usize;
        for _ in 0..=index {
            if let Some(sent) = state.sent.pop_front() {
                acknowledged += sent.bytes;
            }
        }
        state.bytes = state.bytes.saturating_sub(acknowledged);
        let first_ack = state.last_ack.is_none();
        state.last_ack = Some(now);
        state.baseline = Some(state.baseline.map_or(latency, |old| old.min(latency)));
        state.waiting = false;
        if !first_ack && latency >= state.congested_after() {
            let since = *state.congested_since.get_or_insert(now);
            if now.saturating_duration_since(since) >= Duration::from_millis(500)
                && now.saturating_duration_since(state.adjusted) >= Duration::from_millis(750)
            {
                // ACK throughput is application-limited whenever capture waits
                // for renderer credit. It is not a measurement of link capacity.
                // Require sustained queueing, then step down rather than driving
                // the codec down to its own already-throttled output rate.
                let reduced = state.bitrate / 2;
                state.reduce(now, reduced);
            }
        } else {
            state.congested_since = None;
            let desired = state.desired_window();
            // Earned ACKs below the congestion threshold restore credit even
            // before bitrate recovery. Otherwise a reduced window can trap the
            // stream at low FPS with moderate latency forever, hiding whether
            // the actual link can sustain the requested codec rate.
            state.window = state.window.saturating_mul(2).min(desired);
            if latency < state.baseline.unwrap_or_default() + Duration::from_millis(100)
                && now.saturating_duration_since(state.adjusted) >= Duration::from_secs(1)
            {
                state.bitrate =
                    (state.bitrate.saturating_mul(5) / 4 + 64_000).min(state.ceiling.bitrate);
                state.adjusted = now;
            }
        }
        drop(state);
        self.changed.notify_one();
    }
    pub fn budget(&self) -> (VideoPreferences, u32, bool) {
        let mut state = self.state.lock();
        let now = Instant::now();
        let stall =
            Duration::from_secs(1).max(state.congested_after() + Duration::from_millis(250));
        if state.sent.front().is_some_and(|first| {
            now.saturating_duration_since(state.last_ack.unwrap_or(first.sent)) >= stall
        }) && now.saturating_duration_since(state.adjusted) >= Duration::from_secs(1)
        {
            let reduced = state.bitrate.saturating_mul(2) / 3;
            state.reduce(now, reduced);
        }
        let mut prefs = state.ceiling;
        let (width, height, fps) = if state.bitrate < 500_000 {
            (640, 360, 30)
        } else if state.bitrate < 1_000_000 {
            (640, 360, 60)
        } else if state.bitrate < 2_000_000 {
            (960, 540, 60)
        } else if state.bitrate < 4_000_000 {
            (1280, 720, 60)
        } else {
            (prefs.max_width, prefs.max_height, prefs.max_fps)
        };
        prefs.max_width = prefs.max_width.min(width);
        prefs.max_height = prefs.max_height.min(height);
        prefs.max_fps = prefs.max_fps.min(fps);
        (prefs, state.bitrate, state.waiting)
    }
    pub fn stalled(&self) -> bool {
        let state = self.state.lock();
        state.sent.front().is_some_and(|first| {
            Instant::now().saturating_duration_since(state.last_ack.unwrap_or(first.sent))
                >= Duration::from_secs(4)
        })
    }
}
impl State {
    fn desired_window(&self) -> usize {
        let interval = (self.baseline.unwrap_or_default() + Duration::from_millis(100))
            .max(Duration::from_millis(150));
        (u128::from(self.bitrate) * interval.as_nanos() / 8_000_000_000)
            .clamp(MIN_WINDOW as u128, MAX_WINDOW as u128) as usize
    }
    fn congested_after(&self) -> Duration {
        CONGESTED.max(self.baseline.unwrap_or_default() + Duration::from_millis(200))
    }
    fn reduce(&mut self, now: Instant, bitrate: u32) {
        self.bitrate = bitrate.clamp(MIN_BITRATE, self.ceiling.bitrate);
        self.window = (self.window / 2).min(self.desired_window()).max(MIN_WINDOW);
        self.adjusted = now;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SessionStatus, VideoCodec, VideoFormat};
    use std::sync::Arc;
    fn frame(id: u64, bytes: usize) -> Frame {
        Frame {
            status: SessionStatus {
                generation: 1,
                frame_id: id,
                video: VideoFormat {
                    codec: VideoCodec::H264,
                    keyframe: id == 1,
                    ..Default::default()
                },
                ..Default::default()
            },
            bytes: Arc::from(vec![1; bytes]),
        }
    }
    fn ready(feedback: &Feedback, frame: &Frame, at: Instant) {
        feedback.state.lock().ready.push_back(Ready {
            generation: frame.status.generation,
            id: frame.status.frame_id,
            at,
        });
    }
    #[test]
    fn admission_bounds_startup_and_rejects_unearned_acknowledgments() {
        let feedback = Feedback::new(VideoPreferences::default());
        let at = Instant::now();
        let idr = frame(1, INITIAL_WINDOW + 1);
        ready(&feedback, &idr, at);
        assert_eq!(
            feedback.admit_at(&idr, idr.bytes.len(), at),
            Admission::Send
        );
        let next = frame(2, 10_000);
        ready(&feedback, &next, at);
        assert_eq!(feedback.admit_at(&next, 10_000, at), Admission::Wait);
        feedback.presented_at(2, 1, at + Duration::from_millis(50));
        feedback.presented_at(1, 999, at + Duration::from_millis(50));
        assert_eq!(feedback.state.lock().bytes, idr.bytes.len());
        feedback.presented_at(1, 1, at + Duration::from_millis(100));
        assert_eq!(feedback.state.lock().bytes, 0);
        assert_eq!(
            feedback.admit_at(&next, 10_000, at + Duration::from_millis(251)),
            Admission::Discard
        );
        assert_eq!(feedback.admit_at(&frame(3, 0), 1024, at), Admission::Send);
    }
    #[test]
    fn renderer_baseline_and_congestion_recovery_use_real_ack_progress() {
        let feedback = Feedback::new(VideoPreferences::default());
        let start = Instant::now();
        for id in 1..=8 {
            let at = start + Duration::from_millis(id * 300);
            let image = frame(id, 16_000);
            ready(&feedback, &image, at);
            assert_eq!(feedback.admit_at(&image, 16_000, at), Admission::Send);
            feedback.presented_at(1, id, at + Duration::from_millis(235));
        }
        assert_eq!(
            feedback.state.lock().bitrate,
            VideoPreferences::default().bitrate
        );
        let at = start + Duration::from_secs(3);
        let image = frame(9, 50_000);
        ready(&feedback, &image, at);
        assert_eq!(feedback.admit_at(&image, 50_000, at), Admission::Send);
        feedback.presented_at(1, 9, at + Duration::from_millis(800));
        // One WAN scheduling spike must not be mistaken for the bitrate of
        // an application-limited stream. Persistent queueing still adapts.
        assert_eq!(
            feedback.state.lock().bitrate,
            VideoPreferences::default().bitrate
        );
        for id in 10..=13 {
            let at = start + Duration::from_secs(id - 6);
            let image = frame(id, 50_000);
            ready(&feedback, &image, at);
            assert_eq!(feedback.admit_at(&image, 50_000, at), Admission::Send);
            feedback.presented_at(1, id, at + Duration::from_millis(800));
        }
        let reduced = feedback.state.lock().bitrate;
        assert!(reduced < 4_000_000);
        for id in 14..=30 {
            let at = start + Duration::from_secs(id - 5);
            let image = frame(id, 16_000);
            ready(&feedback, &image, at);
            assert_eq!(feedback.admit_at(&image, 16_000, at), Admission::Send);
            feedback.presented_at(1, id, at + Duration::from_millis(235));
        }
        assert!(feedback.state.lock().bitrate > 4_000_000);
        assert!(feedback.state.lock().window <= MAX_WINDOW);
    }
}
