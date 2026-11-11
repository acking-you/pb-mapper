//! The viewer's network session: `handshake` authenticates, then `run`
//! races the reader against the writer until one of them ends.
use super::{
    ConnectError, CursorSlot, Notice, Stopped, VideoSink, VideoUnit, ViewerConfig, ViewerLink,
    ViewerReason, ViewerShared, outbound::Next, stats::Traffic,
};
use crate::{
    Input, QualityPreset, VideoCodec, VideoLimits,
    modes::MAX_SIDE,
    protocol::{HostMessage, RejectReason, Route, SessionState, ViewerMessage, valid_capabilities},
    wire::{self, WireError},
};
use pb_mapper_client::sdk::{Connection, RelayTransport, TunnelMetrics};
use std::{
    io,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

/// The host answers `Authenticate` within this.
const ACCEPT_TIMEOUT: Duration = Duration::from_secs(5);
/// The host writes `Stats` every second, so silence this long is a dead link.
const VIEWER_HEADER_TIMEOUT: Duration = Duration::from_secs(4);
/// The viewer tick: fps cap, path and stats.
pub(super) const VIEWER_TICK: Duration = Duration::from_millis(250);
/// Ticks per path re-sample and stats roll-up.
const TICKS_PER_SECOND: u64 = 4;
/// Why a session whose decoder failed for good ended.
const DECODER_FAILED: &str = "the remote picture could not be decoded";
/// Why a session whose task panicked ended.
const SESSION_PANICKED: &str = "the desktop session ended unexpectedly";
const _: () = assert!(
    VIEWER_TICK.as_millis() * TICKS_PER_SECOND as u128 == 1000,
    "the path sample and the stats roll-up run once a second"
);
const _: () = assert!(
    crate::session::STATS_INTERVAL.as_millis() < VIEWER_HEADER_TIMEOUT.as_millis(),
    "the host's Stats keep a live link inside the read timeout"
);
const _: () = assert!(
    crate::host::BACKEND_ANSWER_WAIT.as_nanos() < ACCEPT_TIMEOUT.as_nanos(),
    "the host answers Authenticate before the viewer gives up on Accepted"
);

/// What `Accepted` settled for the session.
pub(super) struct Accepted {
    pub(super) codec: VideoCodec,
    pub(super) quality: QualityPreset,
    pub(super) capabilities: Vec<String>,
}

/// `Authenticate`, then the host's answer.
pub(super) async fn handshake<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
    token: String,
    config: &ViewerConfig,
) -> std::result::Result<Accepted, ConnectError> {
    let authenticate = ViewerMessage::Authenticate {
        token,
        codecs: config.wire_codecs(),
        limits: config.limits,
        quality: config.quality,
        capabilities: config.capabilities(),
    };
    wire::write_message(stream, &authenticate, None)
        .await
        .map_err(|failure| ConnectError::Transport(failure.into()))?;
    drop(authenticate);
    match wire::read_header::<_, HostMessage>(stream, ACCEPT_TIMEOUT).await {
        Ok(Some(HostMessage::Accepted {
            codec,
            quality,
            capabilities,
            ..
        })) => {
            // H.265 too: only a viewer that offered it may receive it.
            if !config.codecs.contains(&codec) || !valid_capabilities(&capabilities) {
                return Err(ConnectError::Protocol(
                    "the host accepted with a codec or capabilities this viewer did not offer"
                        .into(),
                ));
            }
            Ok(Accepted {
                codec,
                quality,
                capabilities,
            })
        }
        Ok(Some(HostMessage::Rejected { reason })) => Err(match reason {
            RejectReason::Denied => ConnectError::Denied,
            RejectReason::Busy => ConnectError::Busy,
            RejectReason::InvalidRequest => ConnectError::InvalidRequest,
        }),
        Ok(Some(
            HostMessage::State { .. }
            | HostMessage::Video { .. }
            | HostMessage::Cursor { .. }
            | HostMessage::Probe { .. }
            | HostMessage::Stats { .. }
            | HostMessage::Notice { .. }
            | HostMessage::Stopped { .. },
        )) => Err(ConnectError::Protocol(
            "the host did not answer the authentication".into(),
        )),
        Ok(None) => Err(ConnectError::Transport(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "the remote computer closed the connection during authentication",
        ))),
        Err(WireError::Framing(framing)) => Err(ConnectError::Protocol(framing.into())),
        Err(failure @ (WireError::TimedOut | WireError::Io(_))) => {
            Err(ConnectError::Transport(failure.into()))
        }
    }
}

/// How a session ended.
enum Ended {
    /// This end closed it: `Viewer::stop`, or with a reason, a decoder that
    /// cannot recover.
    Local(Option<(ViewerReason, String)>),
    Stopped(ViewerReason, String),
}
impl Ended {
    fn failed(failure: WireError) -> Self {
        let reason = match &failure {
            // A timeout first, the transport's own included.
            WireError::TimedOut => ViewerReason::Timeout,
            WireError::Io(failure) if failure.kind() == io::ErrorKind::TimedOut => {
                ViewerReason::Timeout
            }
            WireError::Framing(_) => ViewerReason::Protocol,
            WireError::Io(_) => ViewerReason::Transport,
        };
        Self::Stopped(reason, failure.to_string())
    }
    fn protocol(message: &str) -> Self {
        Self::Stopped(
            ViewerReason::Protocol,
            format!("desktop protocol error: {message}"),
        )
    }
}

/// What `run` owns besides the stream. Dropping it ends the session however
/// `run` ends: returned, dropped with its runtime (polled or not) or
/// unwinding from a panic.
pub(super) struct Session {
    pub(super) shared: Arc<ViewerShared>,
    pub(super) sink: Arc<VideoSink>,
    pub(super) connection: Option<Arc<Connection>>,
    pub(super) cancel: CancellationToken,
    pub(super) codec: VideoCodec,
    pub(super) limits: VideoLimits,
}
impl Drop for Session {
    fn drop(&mut self) {
        // Each step is idempotent and the first stop wins, so after `run`'s
        // own end this changes nothing; otherwise the consumer exits and the
        // status says stopped. A drop with the runtime reads as a local stop,
        // with no reason; a panic as a transport failure, which a supervisor
        // retries as it did the stall the panic used to leave.
        self.cancel.cancel();
        self.sink.close();
        if std::thread::panicking() {
            self.shared
                .mark_stopped(Some(ViewerReason::Transport), SESSION_PANICKED.into());
        } else {
            self.shared.mark_stopped(None, String::new());
        }
    }
}

/// Races the reader against the writer; the first to end ends the session.
pub(super) async fn run<S: AsyncRead + AsyncWrite + Send + 'static>(stream: S, session: Session) {
    let started = Instant::now();
    let Session {
        shared,
        sink,
        connection,
        cancel,
        codec,
        limits,
    } = &session;
    let (mut read, mut write) = tokio::io::split(stream);
    let mut reader = Reader {
        shared,
        sink,
        codec: *codec,
        limits: *limits,
        last_seq: 0,
        generation: 0,
    };
    let mut writer = Writer {
        shared,
        sink,
        connection: connection.as_deref(),
        ticks: 0,
    };
    let ended = tokio::select! {
        biased;
        ended = writer.run(&mut write, cancel) => ended,
        ended = reader.run(&mut read) => ended,
    };
    let (reason, message) = match ended {
        Ended::Local(failure) => {
            // A failure shows at once; the drain below may take a while.
            if let Some((reason, message)) = failure {
                sink.close();
                shared.mark_stopped(Some(reason), message);
            }
            // Keep the tunnel until the host consumes our close_notify and
            // closes its side, or the grace expires; discard remaining video.
            let _ = tokio::time::timeout(
                wire::IO_TIMEOUT,
                tokio::io::copy(&mut read, &mut tokio::io::sink()),
            )
            .await;
            (None, String::new())
        }
        // The host's close after a local stop is not a failure.
        Ended::Stopped(..) if cancel.is_cancelled() => (None, String::new()),
        Ended::Stopped(reason, message) => (Some(reason), message),
    };
    cancel.cancel();
    sink.close();
    shared.mark_stopped(reason, message);
    // The first stop recorded, not this one: a local stop after a failure
    // keeps the failure. No reason: stopped here.
    let Stopped {
        reason,
        message: detail,
    } = shared.state.lock().stopped.clone().unwrap_or_default();
    let keyframes = shared.stats.lock().host.map_or(0, |host| host.keyframes);
    let local = sink.snapshot(Instant::now());
    tracing::info!(
        target: "pb_mapper_desktop::session",
        event = "desktop_viewer_ended",
        reason = ?reason,
        detail = ?detail,
        seconds = started.elapsed().as_secs(),
        keyframes,
        keyframes_requested = local.keyframes_requested,
        backlog_drops = local.backlog_drops,
    );
}

struct Reader<'a> {
    shared: &'a ViewerShared,
    sink: &'a VideoSink,
    codec: VideoCodec,
    limits: VideoLimits,
    last_seq: u64,
    /// The generation of the last `State`.
    generation: u64,
}
impl Reader<'_> {
    async fn run<R: AsyncRead + Unpin>(&mut self, read: &mut R) -> Ended {
        let mut record = Vec::new();
        loop {
            let message =
                match wire::read_header_with(read, &mut record, VIEWER_HEADER_TIMEOUT).await {
                    Ok(Some(message)) => message,
                    Ok(None) => {
                        return Ended::Stopped(
                            ViewerReason::Transport,
                            "the remote computer closed the connection".into(),
                        );
                    }
                    Err(failure) => return Ended::failed(failure),
                };
            if let Err(ended) = self.handle(message, read).await {
                return ended;
            }
        }
    }

    async fn handle<R: AsyncRead + Unpin>(
        &mut self,
        message: HostMessage,
        read: &mut R,
    ) -> std::result::Result<(), Ended> {
        match message {
            HostMessage::Video {
                seq,
                generation,
                frame_id,
                keyframe,
                duplicate,
                width,
                height,
                pts_us,
                host_us,
            } => {
                // Its header answers an awaited IDR; the payload may take
                // seconds on a slow link.
                if keyframe && generation == self.generation {
                    self.sink.idr_arriving();
                }
                let bytes = wire::read_payload(read).await.map_err(Ended::failed)?;
                let now = Instant::now();
                // Transport credit first: it covers every unit read, including
                // the ones the sink drops.
                self.shared.outbound.receipt(seq);
                if self.last_seq.checked_add(1) != Some(seq) {
                    return Err(Ended::protocol("video sequence gap"));
                }
                self.last_seq = seq;
                if generation > self.generation {
                    return Err(Ended::protocol("video before its generation's state"));
                }
                // JPEG reports the display geometry; H.264 and H.265 their
                // encoded size.
                let bounded = self.codec == VideoCodec::Jpeg
                    || (width <= self.limits.max_width && height <= self.limits.max_height);
                if !bounded || !(1..=MAX_SIDE).contains(&width) || !(1..=MAX_SIDE).contains(&height)
                {
                    return Err(Ended::protocol("video geometry outside the session limits"));
                }
                self.shared.stats.lock().on_video(
                    now,
                    bytes.len(),
                    duplicate,
                    (width, height),
                    host_us,
                );
                if generation < self.generation {
                    // Acknowledged above; never shown.
                    return Ok(());
                }
                self.sink.push(VideoUnit {
                    seq,
                    generation,
                    frame_id,
                    codec: self.codec,
                    width,
                    height,
                    keyframe,
                    duplicate,
                    discontinuity: false,
                    pts_us,
                    bytes,
                    received_at: now,
                });
            }
            HostMessage::Probe { nonce } => self.shared.outbound.probe(nonce),
            HostMessage::State(host) => {
                if host.display.as_ref().is_some_and(|display| {
                    !(1..=MAX_SIDE).contains(&display.width)
                        || !(1..=MAX_SIDE).contains(&display.height)
                }) {
                    return Err(Ended::protocol("invalid display geometry"));
                }
                if host.modes.as_ref().is_some_and(|modes| !modes.valid()) {
                    return Err(Ended::protocol("invalid display modes"));
                }
                self.generation = host.generation;
                // The sink first, so nothing of an old generation is accepted
                // once the state names the new one. The host pauses only for
                // this viewer, whose own pause covers that.
                self.sink.on_state(
                    host.generation,
                    matches!(host.state, SessionState::Streaming | SessionState::Paused),
                );
                self.shared.state.lock().update(Instant::now(), |slot| {
                    slot.host = Some(host);
                });
            }
            HostMessage::Cursor {
                generation,
                x,
                y,
                visible,
                input_sequence,
            } => {
                if generation == self.generation {
                    let controlling = self.shared.state.lock().controlling();
                    let mut cursor = self.shared.cursor.lock();
                    if !cursor.superseded(generation, input_sequence, controlling) {
                        *cursor = CursorSlot {
                            generation,
                            x,
                            y,
                            visible,
                            seq: cursor.seq + 1,
                            ..*cursor
                        };
                    }
                }
            }
            HostMessage::Stats(host) => self.shared.stats.lock().on_host(Instant::now(), host),
            HostMessage::Notice { code, message } => {
                let mut state = self.shared.state.lock();
                let seq = state.notice.as_ref().map_or(1, |notice| notice.seq + 1);
                state.notice = Some(Notice {
                    code,
                    message,
                    seq,
                    at: Instant::now(),
                });
            }
            HostMessage::Stopped { reason, message } => {
                return Err(Ended::Stopped(reason.into(), message));
            }
            HostMessage::Accepted { .. } | HostMessage::Rejected { .. } => {
                return Err(Ended::protocol("a handshake answer after the handshake"));
            }
        }
        Ok(())
    }
}

struct Writer<'a> {
    shared: &'a ViewerShared,
    sink: &'a VideoSink,
    connection: Option<&'a Connection>,
    ticks: u64,
}
impl Writer<'_> {
    async fn run<W: AsyncWrite + Unpin>(
        &mut self,
        write: &mut W,
        cancel: &CancellationToken,
    ) -> Ended {
        let mut last_write = Instant::now();
        let mut next_tick = last_write;
        let mut records = Vec::new();
        loop {
            let now = Instant::now();
            let mut failure = None;
            if now >= next_tick {
                if let Some(error) = self.tick(now) {
                    let message = if error.is_empty() {
                        DECODER_FAILED.into()
                    } else {
                        format!("{DECODER_FAILED}: {error}")
                    };
                    failure = Some((ViewerReason::DecoderFailed, message));
                }
                next_tick = now + VIEWER_TICK;
            }
            if cancel.is_cancelled() || failure.is_some() {
                // Never cut a message short: close TLS between messages so a
                // deliberate disconnect does not look truncated to the host.
                let _ = tokio::time::timeout(wire::IO_TIMEOUT, write.shutdown()).await;
                return Ended::Local(failure);
            }
            match self.shared.outbound.next(now, last_write) {
                Next::Send(message) => {
                    // Out of the queue, no barrier drops it: it reaches the
                    // host, or the session ends.
                    if let ViewerMessage::Input { event } = &message
                        && matches!(event.input, Input::Move { .. })
                    {
                        self.shared.cursor.lock().moved = Some((event.generation, event.sequence));
                    }
                    if let Err(failure) =
                        wire::write_message_with(write, &mut records, &message, None).await
                    {
                        return Ended::failed(failure);
                    }
                    last_write = Instant::now();
                }
                Next::Idle(due) => {
                    let wake = tokio::time::Instant::from_std(due.min(next_tick));
                    tokio::select! {
                        biased;
                        () = cancel.cancelled() => {}
                        () = self.shared.outbound.notified() => {}
                        () = tokio::time::sleep_until(wake) => {}
                    }
                }
            }
        }
    }

    /// Every `VIEWER_TICK`: the fps cap, keyframe retries and the stall
    /// watchdog; once a second also the path, the tunnel's traffic and the
    /// stats roll-up. Why the decoder failed, once it failed for good.
    fn tick(&mut self, now: Instant) -> Option<String> {
        let second = self.ticks.is_multiple_of(TICKS_PER_SECOND);
        self.ticks += 1;
        let tunnel = self
            .connection
            .filter(|_| second)
            .map(|connection| connection.metrics());
        if let Some(metrics) = &tunnel {
            let (route, link) = route(metrics);
            let changed = {
                let mut state = self.shared.state.lock();
                state.link = link;
                state.route.replace(route) != Some(route)
            };
            if changed {
                self.shared.outbound.route(route);
            }
        }
        let (direct, streaming, quality) = {
            let state = self.shared.state.lock();
            (
                state.route == Some(Route::Direct),
                state.streaming(),
                state.quality,
            )
        };
        if let Some(cap) = self.sink.tick(now, streaming, direct, quality) {
            self.shared.outbound.fps_cap(cap);
        }
        if second {
            let sink = self.sink.snapshot(now);
            self.shared.stats.lock().roll(
                now,
                streaming,
                sink.presented_fps,
                sink.viewer_ms,
                tunnel.as_ref().map(traffic),
            );
        }
        self.sink.decoder_failure()
    }
}

/// What the session's tunnel carried so far, on every path: TLS and every
/// message, each way. A viewer's tunnel carries this one session.
fn traffic(metrics: &TunnelMetrics) -> Traffic {
    let paths = metrics.traffic;
    // `direct` includes `lan`.
    Traffic {
        sent: paths.relay.sent.saturating_add(paths.direct.sent),
        received: paths.relay.received.saturating_add(paths.direct.received),
    }
}

/// The path this viewer's tunnel carries the session on, and on a direct
/// one, its link. A viewer's tunnel carries this one session.
fn route(metrics: &TunnelMetrics) -> (Route, Option<ViewerLink>) {
    if metrics.sessions.active_direct > 0 {
        (Route::Direct, metrics.link.as_ref().map(ViewerLink::from))
    } else if metrics.relay.transport == RelayTransport::Quic {
        (Route::RelayQuic, None)
    } else {
        (Route::RelayTcp, None)
    }
}
