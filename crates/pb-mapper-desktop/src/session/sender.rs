//! B-send: everything the host writes to its viewer.

use super::{STATS_INTERVAL, STOP_WRITE_TIMEOUT, Session};
use crate::{
    VideoCodec,
    idr::IdrReason,
    outgoing::OUTGOING_MAX_AGE,
    protocol::{HostMessage, NoticeCode, StateReason, StateRecord, StatsRecord, StopReason},
    qos::QOS_TICK,
    shared::{CursorReport, EngineState},
    wire::{self, MAX_PAYLOAD},
};
use pb_mapper_core::time::saturating_micros;
use std::{
    sync::atomic::Ordering,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncWrite, AsyncWriteExt},
    sync::watch,
};
use tokio_util::sync::CancellationToken;

/// How soon B-send looks again at a head that waits for credit.
const CREDIT_RECHECK: Duration = Duration::from_millis(20);
const CONTROL_DENIED: &str = "Control is not allowed on this computer";

/// A write could not finish: the transport failed, or the stop budget ran out.
struct Abandoned;

/// Write one message. Once the session is cancelled, the message in flight and
/// the final `Stopped` share `STOP_WRITE_TIMEOUT`, so a local stop never waits
/// for a slow viewer to drain video.
async fn write<W: AsyncWrite + Unpin>(
    writer: &mut W,
    cancel: &CancellationToken,
    stop_at: &mut Option<Instant>,
    header: &HostMessage,
    payload: Option<&[u8]>,
) -> Result<(), Abandoned> {
    let write = wire::write_message(writer, header, payload);
    tokio::pin!(write);
    loop {
        if let Some(deadline) = *stop_at {
            return match tokio::time::timeout_at(deadline.into(), &mut write).await {
                Ok(Ok(())) => Ok(()),
                _ => Err(Abandoned),
            };
        }
        tokio::select! {
            biased;
            written = &mut write => return written.map_err(|_| Abandoned),
            () = cancel.cancelled() => *stop_at = Some(Instant::now() + STOP_WRITE_TIMEOUT),
        }
    }
}

#[derive(Default)]
struct Counters {
    frames: u64,
    bytes: u64,
    /// Units B-send dropped: of an old generation, or aged in the hand-off.
    dropped: u64,
    /// Video payload bytes since the last `Stats`.
    interval_bytes: u64,
}

/// B-send. One pass writes, in order: state, notice, QoS tick, stats, probe,
/// cursor and admitted video; then it waits for the next reason to write.
pub(super) struct Sender<'a, W> {
    session: &'a Session,
    writer: W,
    state: watch::Receiver<EngineState>,
    cursor: watch::Receiver<Option<CursorReport>>,
    /// The engine state last written as `State`.
    written: Option<StateRecord>,
    notice_serial: u64,
    /// The last cursor report written, or dropped as stale.
    cursor_serial: u64,
    next_tick: Instant,
    next_stats: Instant,
    stats_since: Instant,
    counters: Counters,
    stop_at: Option<Instant>,
}
impl<'a, W: AsyncWrite + Unpin> Sender<'a, W> {
    pub(super) fn new(session: &'a Session, writer: W) -> Self {
        let now = Instant::now();
        Self {
            session,
            writer,
            state: session.shared.state.subscribe(),
            cursor: session.shared.cursor.subscribe(),
            written: None,
            notice_serial: 0,
            cursor_serial: 0,
            next_tick: now + QOS_TICK,
            next_stats: now + STATS_INTERVAL,
            stats_since: now,
            counters: Counters::default(),
            stop_at: None,
        }
    }

    /// Returns the stop budget's deadline when the final `Stopped` went out.
    pub(super) async fn run(mut self) -> Option<Instant> {
        // The first probe goes out before anything is captured, so the
        // baseline starts clean.
        if self.probe(Instant::now()).await.is_err() {
            return None;
        }
        while !self.session.shared.cancel.is_cancelled() {
            match self.pass().await {
                Ok(wake_at) => self.wait(wake_at).await,
                Err(Abandoned) => return None,
            }
        }
        self.stop().await
    }

    async fn send(
        &mut self,
        header: &HostMessage,
        payload: Option<&[u8]>,
    ) -> Result<(), Abandoned> {
        write(
            &mut self.writer,
            &self.session.shared.cancel,
            &mut self.stop_at,
            header,
            payload,
        )
        .await
    }

    /// The final `Stopped`, then a TLS close, within what is left of the stop
    /// budget. Returns that budget's deadline once both went out.
    async fn stop(mut self) -> Option<Instant> {
        let (reason, message) = self.session.outcome()?;
        let deadline = *self
            .stop_at
            .get_or_insert_with(|| Instant::now() + STOP_WRITE_TIMEOUT);
        let stopped = HostMessage::Stopped { reason, message };
        tokio::time::timeout_at(deadline.into(), async {
            wire::write_message(&mut self.writer, &stopped, None).await?;
            self.writer.shutdown().await
        })
        .await
        .ok()?
        .ok()?;
        Some(deadline)
    }

    /// One pass. Returns when to look again if nothing wakes B-send first.
    async fn pass(&mut self) -> Result<Instant, Abandoned> {
        let now = Instant::now();
        let engine = self.write_engine().await?;
        if now >= self.next_tick {
            self.next_tick = now + QOS_TICK;
            self.session.tick(now);
        }
        if now >= self.next_stats {
            self.next_stats = now + STATS_INTERVAL;
            self.stats(&engine, now).await?;
        }
        self.probe(now).await?;
        let probe_due = self.session.qos.lock().probe_due_at(now);
        let mut wake_at = self.next_tick.min(self.next_stats);
        if let Some(due) = probe_due {
            wake_at = wake_at.min(due);
        }
        // Paused: no cursor and no video, while probes, state and stats go on.
        if !self.session.shared.targets.paused.load(Ordering::Acquire) {
            self.cursor().await?;
            if self.video().await? {
                wake_at = wake_at.min(Instant::now() + CREDIT_RECHECK);
            }
        }
        Ok(wake_at)
    }

    async fn wait(&mut self, wake_at: Instant) {
        let session = self.session;
        tokio::select! {
            biased;
            () = session.shared.cancel.cancelled() => {}
            () = session.shared.outgoing.notified() => {}
            () = session.credit_changed.notified() => {}
            _ = self.state.changed() => {}
            _ = self.cursor.changed() => {}
            () = tokio::time::sleep_until(wake_at.into()) => {}
        }
    }

    /// What the engine published last, after writing its `State` and its
    /// notice, each only if new.
    async fn write_engine(&mut self) -> Result<EngineState, Abandoned> {
        let engine = self.state.borrow_and_update().clone();
        self.write_state(&engine).await?;
        if let Some((code, message, serial)) = &engine.notice
            && *serial > self.notice_serial
        {
            self.notice_serial = *serial;
            // A display mode notice only to a viewer that reads them.
            if *code != NoticeCode::DisplayModeFailed || self.session.display_mode {
                let notice = HostMessage::Notice {
                    code: *code,
                    message: message.clone(),
                };
                self.send(&notice, None).await?;
            }
        }
        Ok(engine)
    }

    /// Write `State` when the engine's record changed, or to report a refused
    /// control request: that shows as its reason until the engine state
    /// changes.
    async fn write_state(&mut self, engine: &EngineState) -> Result<(), Abandoned> {
        let record = &self.session.for_viewer(&engine.record);
        let denied = self.session.control_denied.swap(false, Ordering::AcqRel);
        if !denied && self.written.as_ref() == Some(record) {
            return Ok(());
        }
        if let Some(video) = record.video
            && self.written.as_ref().and_then(|written| written.video) != Some(video)
        {
            self.session.update_qos(Instant::now(), |qos| {
                qos.set_video_size(video.width, video.height);
            });
        }
        self.written = Some(record.clone());
        let mut sent = record.clone();
        if denied {
            sent.reason = Some(StateReason::ControlDenied);
            sent.message = CONTROL_DENIED.into();
        }
        self.send(&HostMessage::State(sent), None).await
    }

    async fn probe(&mut self, now: Instant) -> Result<(), Abandoned> {
        let Some(nonce) = self.session.qos.lock().probe_due(now) else {
            return Ok(());
        };
        // A probe behind an IDR in flight measures that IDR's transfer too.
        let tainted = self.session.credit.lock().idr_inflight().is_some();
        self.session.qos.lock().probe_sent(nonce, now, tainted);
        self.send(&HostMessage::Probe { nonce }, None).await
    }

    /// Write C's newest cursor report, once. It waits for its generation's
    /// `State`, and one of an older generation is stale. C never reports the
    /// controller's own move coming back, so nothing here holds one back. Its
    /// input watermark goes only to a viewer that reads it: an older one
    /// rejects a field it does not know.
    async fn cursor(&mut self) -> Result<(), Abandoned> {
        let Some(report) = *self.cursor.borrow_and_update() else {
            return Ok(());
        };
        let stated = self
            .written
            .as_ref()
            .map_or(0, |written| written.generation);
        let cursor = report.cursor;
        if report.serial <= self.cursor_serial || cursor.generation > stated {
            return Ok(());
        }
        self.cursor_serial = report.serial;
        if cursor.generation < stated {
            return Ok(());
        }
        let message = HostMessage::Cursor {
            generation: cursor.generation,
            x: cursor.x,
            y: cursor.y,
            visible: cursor.visible,
            input_sequence: self
                .session
                .cursor_sequence
                .then_some(report.input_sequence),
        };
        self.send(&message, None).await
    }

    /// Write every unit credit admits. Returns whether the head waits for
    /// credit. Units of a generation older than the last `State` are dropped
    /// without a seq, so the seq stays contiguous. A head that ages past
    /// `OUTGOING_MAX_AGE` is replaced by an IDR, but never while one is in
    /// flight: on a link where an IDR takes longer than that to cross, the
    /// P-frames behind it would age out into another IDR, and that one into
    /// the next.
    async fn video(&mut self) -> Result<bool, Abandoned> {
        let shared = self.session.shared.clone();
        loop {
            let Some(head) = shared.outgoing.peek_meta() else {
                return Ok(false);
            };
            // C publishes a unit's state before the unit can be taken, which
            // may be after this pass wrote its own. That state goes first: a
            // unit behind an older `State`, one that still says connecting,
            // is no IDR the viewer waits for.
            if self.state.has_changed().unwrap_or(false) {
                self.write_engine().await?;
                continue;
            }
            let now = Instant::now();
            let stated = self
                .written
                .as_ref()
                .map_or(0, |written| written.generation);
            if head.generation < stated {
                if shared.outgoing.pop_if(head.frame_id).is_some() {
                    self.counters.dropped += 1;
                }
                continue;
            }
            let aging = self.session.credit.lock().aging_since(head.ready_at);
            if aging.is_some_and(|since| now.saturating_duration_since(since) > OUTGOING_MAX_AGE) {
                self.counters.dropped += shared.outgoing.clear() as u64;
                shared.idr.lock().request(IdrReason::HostResync);
                shared.wake.notify();
                continue;
            }
            if head.generation > stated {
                // C has yet to publish this generation's state; that change
                // wakes B-send again.
                return Ok(false);
            }
            // Every JPEG picture is independent; only an H.264 or H.265 IDR
            // uses the IDR headroom and taints probes.
            let idr = head.keyframe && self.session.codec != VideoCodec::Jpeg;
            if !self.session.admissible(head.len, idr, now) {
                return Ok(true);
            }
            let Some(unit) = shared.outgoing.pop_if(head.frame_id) else {
                continue;
            };
            if unit.codec != self.session.codec || !(1..=MAX_PAYLOAD).contains(&unit.bytes.len()) {
                self.session.end(
                    StopReason::CaptureFailed,
                    format!(
                        "the capture backend produced an unusable {:?} picture of {} bytes",
                        unit.codec,
                        unit.bytes.len()
                    ),
                );
                return Ok(false);
            }
            let now = Instant::now();
            let seq = self.session.credit.lock().book(unit.bytes.len(), idr, now);
            let waited = now.saturating_duration_since(unit.ready_at).as_micros();
            let video = HostMessage::Video {
                seq,
                generation: unit.generation,
                frame_id: unit.frame_id,
                keyframe: unit.keyframe,
                duplicate: unit.duplicate,
                width: unit.width,
                height: unit.height,
                pts_us: unit.pts_us,
                host_us: unit
                    .encode_us
                    .saturating_add(u32::try_from(waited).unwrap_or(u32::MAX)),
            };
            self.send(&video, Some(&unit.bytes)).await?;
            let len = unit.bytes.len() as u64;
            self.counters.frames += 1;
            self.counters.bytes += len;
            self.counters.interval_bytes += len;
            let epoch = shared.epoch;
            tracing::trace!(
                target: "pb_mapper_desktop::frames",
                event = "desktop_frame",
                seq,
                frame_id = unit.frame_id,
                keyframe = unit.keyframe,
                bytes = len,
                ready_us = saturating_micros(unit.ready_at.saturating_duration_since(epoch)),
                written_us = saturating_micros(Instant::now().saturating_duration_since(epoch)),
            );
        }
    }

    /// `Stats`, which is also the host's liveness, and the 1 Hz trace.
    async fn stats(&mut self, engine: &EngineState, now: Instant) -> Result<(), Abandoned> {
        let session = self.session;
        let qos = session.qos.lock().view(now);
        let (credit, ack_queue, window) = {
            let credit = session.credit.lock();
            (
                credit.view(),
                qos.base_rtt
                    .map_or(Duration::ZERO, |base| credit.ack_queue(base, now)),
                credit.window(
                    session.codec,
                    qos.pace_fps,
                    qos.expected_bps,
                    qos.base_rtt,
                    now,
                ),
            )
        };
        let produced = session.shared.outgoing.produced();
        let interval = now
            .saturating_duration_since(self.stats_since)
            .as_secs_f64();
        let sent_bitrate = if interval > 0.0 {
            (self.counters.interval_bytes as f64 * 8.0 / interval) as u32
        } else {
            0
        };
        self.stats_since = now;
        self.counters.interval_bytes = 0;
        let ms = |duration: Duration| u32::try_from(duration.as_millis()).unwrap_or(u32::MAX);
        let stats = HostMessage::Stats(StatsRecord {
            capture_fps: engine.capture_fps,
            pace_fps: qos.pace_fps,
            target_fps: qos.fps,
            fps_cap: qos.viewer_cap.unwrap_or(0),
            bitrate: u32::try_from(qos.expected_bps).unwrap_or(u32::MAX),
            encoder_bitrate: qos.encoder_bps,
            sent_bitrate,
            rtt_ms: qos.rtt.map(ms),
            base_rtt_ms: qos.base_rtt.map(ms),
            queue_delay_ms: qos.delay_ms,
            ack_queue_ms: ms(ack_queue),
            encoder_dwell_ms: engine
                .encoder_dwell
                .filter(|_| session.encoder_dwell)
                .map(ms),
            dropped_frames: engine.dropped_frames + self.counters.dropped,
            skipped_ticks: engine.skipped_ticks,
            keyframes: produced.keyframes,
            frames: self.counters.frames,
            bytes: self.counters.bytes,
            adaptation: qos.adaptation,
            rate_model: qos.rate_model,
            gop: engine.gop,
        });
        tracing::debug!(
            target: "pb_mapper_desktop::feedback",
            event = "desktop_feedback_interval",
            action = ?qos.adaptation,
            fps = qos.fps,
            pace_fps = qos.pace_fps,
            viewer_cap = qos.viewer_cap,
            paused = qos.paused,
            unresponsive = qos.unresponsive,
            bpf = qos.bpf,
            bitrate = qos.expected_bps,
            encoder_bitrate = qos.encoder_bps,
            sent_bitrate,
            rate_model = ?qos.rate_model,
            rtt_ms = qos.rtt.map(ms),
            baseline_ms = qos.base_rtt.map(ms),
            delay_ms = qos.delay_ms,
            tainted = qos.tainted,
            ack_queue_ms = ms(ack_queue),
            window_frames = window.frames,
            window_bytes = window.bytes,
            window_clamped = window.clamped,
            outstanding_frames = credit.outstanding_frames,
            outstanding_bytes = credit.outstanding_bytes,
            idr_inflight = credit.idr_inflight,
            skipped_ticks = engine.skipped_ticks,
            encoder_dwell_ms = engine.encoder_dwell.map(ms),
            keyframes = produced.keyframes,
            gop = engine.gop,
            input_wait_p50_us = engine.input_wait.p50_us,
            input_wait_p99_us = engine.input_wait.p99_us,
            input_wait_max_us = engine.input_wait.max_us,
        );
        self.send(&stats, None).await
    }
}
