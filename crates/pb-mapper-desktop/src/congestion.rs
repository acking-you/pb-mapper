//! Opt-in renderer feedback. Socket writes only move bytes into local buffers;
//! admission is bounded by frames the remote renderer has actually presented.
use crate::{Frame, VideoCodec, VideoPreferences};
use parking_lot::Mutex;
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};
use tokio::sync::Notify;

const INITIAL_WINDOW: usize = 64 * 1024;
const MIN_WINDOW: usize = 16 * 1024;
const MAX_WINDOW: usize = 512 * 1024;
// Every unpresented frame may arrive in one transport burst. Never grant more
// frames than the receiver can retain without discarding predictive references.
const MAX_FRAMES: usize = crate::media::MAX_QUEUED_FRAMES;
const MAX_INDEPENDENT_FRAME: usize = 256 * 1024;
const MIN_BITRATE: u32 = 256_000;
const MAX_READY_AGE: Duration = Duration::from_millis(250);
const CONGESTED: Duration = Duration::from_millis(250);
// A large startup IDR includes serialization and decoder startup in its ACK.
// It must not teach the controller that seconds of queued video are normal.
const INITIAL_BASELINE_LIMIT: Duration = Duration::from_millis(250);
const OBSERVATION_INTERVAL: Duration = Duration::from_secs(1);
const RECOVERY_HOLD: Duration = Duration::from_secs(4);
const RESOLUTION_HOLD: Duration = Duration::from_secs(5);

struct Ready {
    generation: u64,
    id: u64,
    at: Instant,
}
struct Sent {
    generation: u64,
    id: u64,
    at: Instant,
    bytes: usize,
    keyframe: bool,
    baseline_sample: bool,
}
struct State {
    ready: VecDeque<Ready>,
    sent: VecDeque<Sent>,
    bytes: usize,
    window: usize,
    bitrate: u32,
    ceiling: VideoPreferences,
    last_ack: Option<Instant>,
    last_ack_frame: Option<(u64, u64)>,
    unacknowledged_since: Option<Instant>,
    recovering: bool,
    baseline: Option<Duration>,
    baseline_calibrated: bool,
    probe_sent: Option<Instant>,
    observed_since: Option<Instant>,
    delay_total: Duration,
    delay_samples: u32,
    adjusted: Instant,
    congested_since: Option<Instant>,
    healthy_since: Option<Instant>,
    quality: u8,
    upscale_since: Option<Instant>,
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
                last_ack_frame: None,
                unacknowledged_since: None,
                recovering: false,
                baseline: None,
                baseline_calibrated: false,
                probe_sent: None,
                observed_since: None,
                delay_total: Duration::ZERO,
                delay_samples: 0,
                adjusted: Instant::now(),
                congested_since: None,
                healthy_since: None,
                quality: quality(ceiling.bitrate),
                upscale_since: None,
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
    /// One small startup flight, sent before video. A peer that does not echo
    /// it keeps the existing frame-based fallback; no extra timer is needed.
    pub fn start_probe(&self) -> u64 {
        self.state.lock().probe_sent = Some(Instant::now());
        1
    }
    pub fn probe_received(&self, nonce: u64) {
        self.probe_received_at(nonce, Instant::now());
    }
    fn probe_received_at(&self, nonce: u64, now: Instant) {
        if nonce != 1 {
            return;
        }
        let mut state = self.state.lock();
        let Some(sent) = state.probe_sent.take() else {
            return;
        };
        let roundtrip = now.saturating_duration_since(sent);
        if roundtrip > crate::wire::IO_TIMEOUT {
            return;
        }
        // This receipt bypasses decoder and display work. Never retire sent
        // frames, renew the stall deadline or grant presentation/input credit.
        state.baseline = Some(roundtrip);
        state.baseline_calibrated = true;
    }
    /// Only the host's capture context may retire an unavailable desktop's
    /// presentation obligation. A renderer request cannot extend this deadline.
    pub fn context_changed(&self, unavailable: bool) {
        let mut state = self.state.lock();
        state.ready.clear();
        state.sent.clear();
        state.bytes = 0;
        state.waiting = false;
        state.recovering = false;
        if unavailable {
            state.unacknowledged_since = None;
        }
        drop(state);
        self.changed.notify_one();
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
        // An asynchronous encoder can still return an old predictive output
        // after an IDR request. It must not spend the recovery's first credit.
        if state.recovering
            && state.sent.is_empty()
            && frame.status.video.codec == VideoCodec::H264
            && !frame.status.video.keyframe
        {
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
            && (state.recovering
                || state.sent.len() >= MAX_FRAMES
                || state.bytes.saturating_add(bytes) > state.window)
        {
            state.waiting = true;
            return Admission::Wait;
        }
        state.unacknowledged_since.get_or_insert(now);
        // Calibrate from a small flight of predictive frames, never from a
        // startup/recovery IDR. Every eligible frame carries the observation:
        // a cumulative ACK may skip the first frame of the flight entirely.
        let keyframe = frame.status.video.keyframe;
        let baseline_sample = !keyframe
            && state.bytes.saturating_add(bytes) <= MIN_WINDOW
            && state.sent.iter().all(|sent| !sent.keyframe);
        state.bytes += bytes;
        state.sent.push_back(Sent {
            generation: frame.status.generation,
            id: frame.status.frame_id,
            at: now,
            bytes,
            keyframe,
            baseline_sample,
        });
        // A replacement must reach the renderer before its dependent frames.
        // Otherwise a retained, large window can overflow the viewer's queue
        // again, discard this IDR, and leave both ends waiting: the viewer for
        // another IDR and the host for the only recovery credit's ACK.
        state.waiting =
            state.recovering || state.bytes >= state.window || state.sent.len() >= MAX_FRAMES;
        Admission::Send
    }
    /// A decoder reset retires frames it can no longer acknowledge. Permit one
    /// replacement image until its real acknowledgment advances the live ledger;
    /// repeated requests neither create credit nor postpone the ACK deadline.
    pub fn request_keyframe(&self) -> bool {
        let mut state = self.state.lock();
        if state.recovering {
            return false;
        }
        state.recovering = true;
        state.ready.clear();
        state.sent.clear();
        state.bytes = 0;
        state.waiting = false;
        // Preserve unacknowledged_since even while the retired ledger is empty,
        // together with the earned window, bitrate and congestion history.
        drop(state);
        self.changed.notify_one();
        true
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
        // Compare the same send-to-presentation interval as the path baseline.
        // Time spent waiting for our own admission credit is bounded separately
        // by MAX_READY_AGE; counting it as network pressure creates a feedback
        // loop in which a smaller window continually cuts its own bitrate.
        let latency = now.saturating_duration_since(state.sent[index].at);
        let baseline_sample = state.sent[index]
            .baseline_sample
            .then(|| now.saturating_duration_since(state.sent[index].at));
        let mut acknowledged = 0usize;
        for _ in 0..=index {
            if let Some(sent) = state.sent.pop_front() {
                acknowledged += sent.bytes;
            }
        }
        state.bytes = state.bytes.saturating_sub(acknowledged);
        let first_ack = state.last_ack.is_none();
        state.last_ack = Some(now);
        state.last_ack_frame = Some((generation, id));
        state.unacknowledged_since = (!state.sent.is_empty()).then_some(now);
        state.recovering = false;
        if !state.baseline_calibrated
            && let Some(baseline) = baseline_sample
        {
            // An empty/small flight measures path and presentation delay without
            // a preceding large frame's serialization. Its sender wait is not
            // part of the baseline. A WAN can legitimately exceed 250 ms.
            state.baseline = Some(baseline);
            state.baseline_calibrated = true;
        } else {
            let baseline = baseline_sample.unwrap_or(latency);
            let baseline = if state.baseline_calibrated {
                baseline
            } else {
                baseline.min(INITIAL_BASELINE_LIMIT)
            };
            state.baseline = Some(state.baseline.map_or(baseline, |old| old.min(baseline)));
        }
        state.waiting = false;
        if first_ack || latency < state.congested_after() {
            let desired = state.desired_window();
            // Earned ACKs below the congestion threshold restore credit even
            // before bitrate recovery. Otherwise a reduced window can trap the
            // stream at low FPS with moderate latency forever, hiding whether
            // the actual link can sustain the requested codec rate.
            state.window = state.window.saturating_mul(2).min(desired);
        }
        if first_ack {
            state.observed_since = Some(now);
        } else {
            state.observe_delay(latency, now);
        }
        drop(state);
        self.changed.notify_one();
    }
    pub fn budget(&self) -> (VideoPreferences, u32, bool) {
        self.budget_at(Instant::now())
    }
    fn budget_at(&self, now: Instant) -> (VideoPreferences, u32, bool) {
        let mut state = self.state.lock();
        let stall =
            Duration::from_secs(1).max(state.congested_after() + Duration::from_millis(250));
        if state
            .unacknowledged_since
            .is_some_and(|since| now.saturating_duration_since(since) >= stall)
            && now.saturating_duration_since(state.adjusted) >= Duration::from_secs(1)
        {
            let reduced = state.bitrate.saturating_mul(2) / 3;
            state.reduce(now, reduced);
            state.trace_feedback(now, None, 0, "ack_stall");
        }
        let desired = quality(state.bitrate);
        // Downshift promptly when the current size cannot be afforded. Going
        // back up needs 25% headroom and a stable interval, so a rate hovering
        // around 1/2/4 Mbps cannot repeatedly destroy the decoder's references.
        if desired < state.quality
            && state.bitrate < minimum_bitrate(state.quality).saturating_mul(4) / 5
        {
            state.quality = desired;
            state.upscale_since = None;
        } else if desired > state.quality
            && state.bitrate >= minimum_bitrate(state.quality + 1).saturating_mul(5) / 4
            && state.healthy_since.is_some()
        {
            let since = *state.upscale_since.get_or_insert(now);
            if now.saturating_duration_since(since) >= RESOLUTION_HOLD
                && state.congested_since.is_none()
            {
                state.quality += 1;
                state.upscale_since = None;
            }
        } else {
            state.upscale_since = None;
        }
        let mut prefs = state.ceiling;
        let (width, height, fps) = match state.quality {
            0 => (640, 360, 30),
            1 => (640, 360, 60),
            2 => (960, 540, 60),
            3 => (1280, 720, 60),
            _ => (prefs.max_width, prefs.max_height, prefs.max_fps),
        };
        prefs.max_width = prefs.max_width.min(width);
        prefs.max_height = prefs.max_height.min(height);
        prefs.max_fps = prefs.max_fps.min(fps);
        (prefs, state.bitrate, state.waiting)
    }
    pub fn stalled(&self) -> bool {
        self.stalled_at(Instant::now())
    }
    fn stalled_at(&self, now: Instant) -> bool {
        let state = self.state.lock();
        state
            .unacknowledged_since
            .is_some_and(|since| now.saturating_duration_since(since) >= Duration::from_secs(4))
    }
}
impl State {
    fn observe_delay(&mut self, latency: Duration, now: Instant) {
        self.delay_total += latency;
        self.delay_samples += 1;
        let since = *self.observed_since.get_or_insert(now);
        if now.saturating_duration_since(since) < OBSERVATION_INTERVAL {
            return;
        }
        let delay = self.delay_total / self.delay_samples;
        let samples = self.delay_samples;
        self.delay_total = Duration::ZERO;
        self.delay_samples = 0;
        self.observed_since = Some(now);
        // Presentation ACKs include decoder/surface scheduling and up to one
        // feedback interval. Average an interval instead of reacting to each
        // scheduling peak. Two consecutive bad intervals detect persistent
        // pressure; using their minimum would hide the majority of queued work.
        let mut action = "hold";
        if delay >= self.congested_after() {
            action = "congestion_pending";
            let since = *self.congested_since.get_or_insert(now);
            if now.saturating_duration_since(since) >= OBSERVATION_INTERVAL {
                self.reduce(now, self.bitrate / 2);
                action = "decrease";
            }
        } else {
            self.congested_since = None;
            let since = *self.healthy_since.get_or_insert(now);
            // Only actual sustained congestion resets recovery. An isolated
            // scheduling spike must not repeatedly erase four seconds of ACK
            // progress and trap a healthy stream at its minimum codec rate.
            if now.saturating_duration_since(since) >= RECOVERY_HOLD
                && now.saturating_duration_since(self.adjusted) >= OBSERVATION_INTERVAL
            {
                self.bitrate = (self.bitrate + self.bitrate / 4 + 64_000).min(self.ceiling.bitrate);
                self.adjusted = now;
                action = "probe";
            }
        }
        self.trace_feedback(now, Some(delay), samples, action);
    }
    fn trace_feedback(
        &self,
        now: Instant,
        delay: Option<Duration>,
        samples: u32,
        action: &'static str,
    ) {
        if !tracing::enabled!(target: "pb_mapper_desktop::feedback", tracing::Level::DEBUG) {
            return;
        }
        tracing::debug!(
            target: "pb_mapper_desktop::feedback",
            event = "desktop_feedback_interval",
            action,
            samples,
            bitrate = self.bitrate,
            quality = self.quality,
            baseline_ms = self.baseline.map(|value| value.as_secs_f64() * 1000.0),
            baseline_calibrated = self.baseline_calibrated,
            delay_ms = delay.map(|value| value.as_secs_f64() * 1000.0),
            congested_after_ms = self.congested_after().as_secs_f64() * 1000.0,
            window_bytes = self.window,
            desired_window_bytes = self.desired_window(),
            outstanding_frames = self.sent.len(),
            outstanding_bytes = self.bytes,
            oldest_frame_id = self.sent.front().map(|frame| frame.id),
            newest_frame_id = self.sent.back().map(|frame| frame.id),
            recovering = self.recovering,
            waiting = self.waiting,
            last_ack_generation = self.last_ack_frame.map(|(generation, _)| generation),
            last_ack_frame_id = self.last_ack_frame.map(|(_, id)| id),
            last_ack_age_ms = self.last_ack.map(|at| now.saturating_duration_since(at).as_secs_f64() * 1000.0),
            unacknowledged_age_ms = self.unacknowledged_since.map(|at| now.saturating_duration_since(at).as_secs_f64() * 1000.0),
            healthy_ms = self.healthy_since.map(|at| now.saturating_duration_since(at).as_secs_f64() * 1000.0),
        );
    }
    fn desired_window(&self) -> usize {
        let interval = (self.baseline.unwrap_or_default() + Duration::from_millis(50))
            .max(Duration::from_millis(125));
        (u128::from(self.bitrate) * interval.as_nanos() / 8_000_000_000)
            .clamp(MIN_WINDOW as u128, MAX_WINDOW as u128) as usize
    }
    fn congested_after(&self) -> Duration {
        CONGESTED.max(self.baseline.unwrap_or_default() + Duration::from_millis(100))
    }
    fn reduce(&mut self, now: Instant, bitrate: u32) {
        self.bitrate = bitrate.clamp(MIN_BITRATE, self.ceiling.bitrate);
        self.window = (self.window / 2).min(self.desired_window()).max(MIN_WINDOW);
        self.adjusted = now;
        self.healthy_since = None;
        self.upscale_since = None;
    }
}

fn quality(bitrate: u32) -> u8 {
    match bitrate {
        0..500_000 => 0,
        500_000..1_000_000 => 1,
        1_000_000..2_000_000 => 2,
        2_000_000..4_000_000 => 3,
        _ => 4,
    }
}
fn minimum_bitrate(quality: u8) -> u32 {
    match quality {
        0 => MIN_BITRATE,
        1 => 500_000,
        2 => 1_000_000,
        3 => 2_000_000,
        _ => 4_000_000,
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
    fn an_unpresented_flight_cannot_overflow_the_receiver_queue() {
        let feedback = Feedback::new(VideoPreferences::default());
        let receiver = crate::media::Queue::default();
        let at = Instant::now();
        for id in 1..=MAX_FRAMES as u64 {
            let image = frame(id, 1024);
            ready(&feedback, &image, at);
            assert_eq!(feedback.admit_at(&image, 1024, at), Admission::Send);
            receiver.push(image);
        }
        let next = frame(MAX_FRAMES as u64 + 1, 1024);
        ready(&feedback, &next, at);
        assert_eq!(feedback.admit_at(&next, 1024, at), Admission::Wait);
        assert!(feedback.budget_at(at).2);
        let mut newest = 0;
        while let Some(image) = receiver.pop() {
            newest = image.status.frame_id;
        }
        assert_eq!(newest, MAX_FRAMES as u64);
        assert_eq!(receiver.dropped(), 0);
        assert!(!receiver.take_keyframe_request());
        let presented_at = at + Duration::from_millis(100);
        feedback.presented_at(1, newest, presented_at);
        assert_eq!(
            feedback.admit_at(&next, 1024, presented_at),
            Admission::Send
        );
    }
    #[test]
    fn keyframe_recovery_retires_old_frames_without_resetting_unacknowledged_limit() {
        let feedback = Feedback::new(VideoPreferences::default());
        let at = Instant::now();
        let initial = frame(1, INITIAL_WINDOW + 1);
        ready(&feedback, &initial, at);
        assert_eq!(
            feedback.admit_at(&initial, initial.bytes.len(), at),
            Admission::Send
        );
        let pending = frame(2, 1024);
        ready(&feedback, &pending, at);
        assert_eq!(feedback.admit_at(&pending, 1024, at), Admission::Wait);

        assert!(feedback.request_keyframe());
        assert!(!feedback.request_keyframe());
        // This frame may already be outside the media queue in Host::send.
        assert_eq!(feedback.admit_at(&pending, 1024, at), Admission::Discard);
        assert!(feedback.stalled_at(at + Duration::from_secs(4)));
        let replacement_at = at + Duration::from_secs(1);
        let delayed_output = frame(3, 1024);
        ready(&feedback, &delayed_output, replacement_at);
        assert_eq!(
            feedback.admit_at(&delayed_output, 1024, replacement_at),
            Admission::Discard
        );
        let mut replacement = frame(4, INITIAL_WINDOW + 1);
        replacement.status.video.keyframe = true;
        ready(&feedback, &replacement, replacement_at);
        assert_eq!(
            feedback.admit_at(&replacement, replacement.bytes.len(), replacement_at),
            Admission::Send
        );
        assert!(!feedback.request_keyframe());
        feedback.presented_at(1, 1, at + Duration::from_millis(3500));
        {
            let state = feedback.state.lock();
            assert_eq!(state.bytes, replacement.bytes.len());
            assert!(state.waiting);
            assert_eq!(state.window, INITIAL_WINDOW);
            assert_eq!(state.bitrate, VideoPreferences::default().bitrate);
            assert_eq!(state.last_ack, None);
        }
        // Replacing debt and receiving a retired ACK cannot buy another four
        // seconds, nor can repeated requests retire the replacement again.
        assert!(!feedback.request_keyframe());
        assert!(!feedback.stalled_at(at + Duration::from_millis(3999)));
        assert!(feedback.stalled_at(at + Duration::from_secs(4)));
    }

    #[test]
    fn replacement_idr_exclusively_holds_credit_until_its_actual_ack() {
        let feedback = Feedback::new(VideoPreferences::default());
        let at = Instant::now();
        let retired = frame(1, 1024);
        ready(&feedback, &retired, at);
        assert_eq!(feedback.admit_at(&retired, 1024, at), Admission::Send);
        assert!(feedback.request_keyframe());

        let mut replacement = frame(2, 1024);
        replacement.status.video.keyframe = true;
        ready(&feedback, &replacement, at);
        assert_eq!(feedback.admit_at(&replacement, 1024, at), Admission::Send);
        assert!(feedback.budget_at(at).2);
        let dependent = frame(3, 1024);
        ready(&feedback, &dependent, at);
        assert_eq!(feedback.admit_at(&dependent, 1024, at), Admission::Wait);
        assert!(!feedback.request_keyframe());
        feedback.presented_at(1, 1, at + Duration::from_millis(50));
        assert!(feedback.budget_at(at + Duration::from_millis(50)).2);

        let presented_at = at + Duration::from_millis(100);
        feedback.presented_at(1, 2, presented_at);
        assert!(!feedback.budget_at(presented_at).2);
        assert_eq!(
            feedback.admit_at(&dependent, 1024, presented_at),
            Admission::Send
        );
    }
    #[test]
    fn unavailable_capture_retires_debt_but_new_video_still_requires_acks() {
        let feedback = Feedback::new(VideoPreferences::default());
        let at = Instant::now();
        let initial = frame(1, INITIAL_WINDOW + 1);
        ready(&feedback, &initial, at);
        assert_eq!(
            feedback.admit_at(&initial, initial.bytes.len(), at),
            Admission::Send
        );
        feedback.context_changed(false);
        assert!(feedback.stalled_at(at + Duration::from_secs(4)));
        feedback.context_changed(true);
        assert!(!feedback.stalled_at(at + Duration::from_secs(60)));
        assert!(!feedback.budget().2);
        feedback.context_changed(false);
        let recovered_at = at + Duration::from_secs(60);
        let mut recovered = frame(2, 1024);
        recovered.status.generation = 2;
        recovered.status.video.keyframe = true;
        ready(&feedback, &recovered, recovered_at);
        assert_eq!(
            feedback.admit_at(&recovered, 1024, recovered_at),
            Admission::Send
        );
        assert!(feedback.request_keyframe());
        assert!(feedback.stalled_at(recovered_at + Duration::from_secs(4)));
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

    #[test]
    fn startup_probe_measures_path_without_retiring_presentation_debt() {
        let feedback = Feedback::new(VideoPreferences::default());
        let nonce = feedback.start_probe();
        let sent = feedback.state.lock().probe_sent.unwrap();
        let initial = frame(1, 128 * 1024);
        ready(&feedback, &initial, sent);
        assert_eq!(
            feedback.admit_at(&initial, initial.bytes.len(), sent),
            Admission::Send
        );
        feedback.probe_received_at(nonce + 1, sent + Duration::from_millis(350));
        assert!(!feedback.state.lock().baseline_calibrated);
        feedback.probe_received_at(nonce, sent + Duration::from_millis(350));
        feedback.probe_received_at(nonce, sent + Duration::from_secs(2));
        let state = feedback.state.lock();
        assert_eq!(state.baseline, Some(Duration::from_millis(350)));
        assert!(state.baseline_calibrated);
        assert_eq!(state.window, INITIAL_WINDOW);
        assert_eq!(state.sent.len(), 1);
        assert_eq!(state.bytes, initial.bytes.len());
        assert_eq!(state.last_ack, None);
        assert_eq!(state.unacknowledged_since, Some(sent));
        drop(state);
        assert!(feedback.stalled_at(sent + Duration::from_secs(4)));
    }

    #[test]
    fn slow_first_idr_does_not_normalize_persistent_video_delay() {
        let feedback = Feedback::new(VideoPreferences::default());
        let start = Instant::now();
        for id in 1..=4 {
            let at = start + Duration::from_secs(id * 2);
            let image = frame(id, 150_000);
            ready(&feedback, &image, at);
            assert_eq!(
                feedback.admit_at(&image, image.bytes.len(), at),
                Admission::Send
            );
            feedback.presented_at(1, id, at + Duration::from_millis(900));
        }
        let state = feedback.state.lock();
        assert!(state.bitrate < 4_000_000);
        assert!(state.congested_after() <= Duration::from_millis(350));
        assert!(state.window < INITIAL_WINDOW);
    }

    #[test]
    fn cumulative_small_frame_acks_calibrate_wan_delay_and_recover_through_jitter() {
        let feedback = Feedback::new(VideoPreferences::default());
        let start = Instant::now();
        feedback.state.lock().reduce(start, MIN_BITRATE);
        let idr = frame(1, 150_000);
        ready(&feedback, &idr, start);
        assert_eq!(
            feedback.admit_at(&idr, idr.bytes.len(), start),
            Admission::Send
        );
        feedback.presented_at(1, 1, start + Duration::from_millis(900));
        assert!(!feedback.state.lock().baseline_calibrated);

        for batch in 1..=40 {
            let at = start + Duration::from_secs(batch + 1);
            // The renderer drops the first two pictures and acknowledges only
            // the newest frame. A calibration tied to the first sent ID would
            // never learn this path's baseline.
            for offset in 0..3 {
                let image = frame(batch * 3 + offset, 1024);
                let ready_at = at + Duration::from_millis(offset * 5);
                // Admission may wait for the previous flight's ACK. Its local
                // queue age must not be added to a send-to-ACK path baseline.
                ready(&feedback, &image, ready_at - Duration::from_millis(200));
                assert_eq!(feedback.admit_at(&image, 1024, ready_at), Admission::Send);
            }
            let latency = if batch % 5 == 0 { 900 } else { 420 };
            feedback.presented_at(1, batch * 3 + 2, at + Duration::from_millis(latency));
            feedback.budget_at(at + Duration::from_millis(latency));
        }
        let state = feedback.state.lock();
        assert!(state.baseline_calibrated);
        assert_eq!(state.baseline, Some(Duration::from_millis(410)));
        assert!(state.bitrate > 1_000_000);
        assert!(state.quality > 0);
        assert!(state.window <= MAX_WINDOW);
    }

    #[test]
    fn resolution_recovers_with_headroom_without_chattering_at_rate_boundaries() {
        let feedback = Feedback::new(VideoPreferences::default());
        let at = Instant::now();
        feedback.state.lock().reduce(at, 1_500_000);
        assert_eq!(feedback.budget_at(at).0.max_width, 960);
        // A bitrate crossing is not itself permission to reset the codec.
        // Keep the current format while a borderline link repeatedly crosses
        // the 2 Mbps threshold in either direction.
        for second in 0..20 {
            let mut state = feedback.state.lock();
            state.bitrate = if second % 2 == 0 {
                2_100_000
            } else {
                1_900_000
            };
            state.healthy_since = Some(at);
            drop(state);
            assert_eq!(
                feedback
                    .budget_at(at + Duration::from_secs(second))
                    .0
                    .max_width,
                960
            );
        }
        feedback.state.lock().bitrate = 2_600_000;
        let recovered = at + Duration::from_secs(20);
        assert_eq!(feedback.budget_at(recovered).0.max_width, 960);
        assert_eq!(
            feedback.budget_at(recovered + RESOLUTION_HOLD).0.max_width,
            1280
        );
        feedback
            .state
            .lock()
            .reduce(recovered + RESOLUTION_HOLD, 600_000);
        assert_eq!(
            feedback.budget_at(recovered + RESOLUTION_HOLD).0.max_width,
            640
        );
    }
}
