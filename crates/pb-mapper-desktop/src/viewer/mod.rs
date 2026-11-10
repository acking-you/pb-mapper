//! The desktop viewer (protocol v2). `Viewer::connect` returns once the host
//! accepted; one spawned task then runs the session, racing its two halves
//! (`run`):
//!
//! - The reader decodes host messages. For every video record it queues
//!   the transport receipt before anything else, so the host's send credit
//!   covers units the sink later drops; then it hands the unit to the
//!   `VideoSink` and updates the state, cursor and stats slots. No FFI, Dart
//!   or decoder work happens on it.
//! - The writer writes `Outbound`'s messages, one per pick in priority
//!   order, and runs the 250 ms viewer tick.
//!
//! Input is authorised only by a presentation the sink validated, stamped by
//! Rust and preceded on the wire by that presentation. Receipts, probes,
//! cursor and input traffic never grant or renew anything.
//!
//! The sink keeps only the newest decodable frames (`sink`), recovery
//! asks for IDRs on a limited backoff (`recovery`), and when the consumer
//! falls behind the host is capped to what it presents (`fps_cap`). A decoder
//! that cannot recover ends the session with `decoder_failed`.
mod config;
mod error;
mod fps_cap;
mod outbound;
mod recovery;
mod session;
mod sink;
mod stats;
mod status;

pub use config::{ConnectStage, Decoders, ViewerConfig};
pub use error::{ConnectError, InputError};
pub use sink::{AttachError, Pull, VideoConsumer, VideoSink, VideoUnit};
pub use status::{
    DecoderInfo, DecoderState, DesktopView, IntervalStats, NoticeInfo, PresentedInfo, VideoInfo,
    ViewerLink, ViewerReason, ViewerState, ViewerStats, ViewerStatus,
};

use crate::{
    HostOs, Input, Invitation, KeyframeReason, QualityPreset, Result, VideoCodec, error,
    modes::{DisplayModeError, Mode, ModeList, ModeTarget},
    protocol::{
        CAPABILITY_DISPLAY_MODE, DesktopKind, NoticeCode, Route, SessionState, StateRecord,
        VideoSize,
    },
    security, valid_service,
};
use outbound::{Barrier, Lifecycle, ModeRequest, Outbound};
use parking_lot::Mutex;
use pb_mapper_client::sdk::{
    Client, ConnectOptions, Connection, TunnelMetrics, TunnelMetricsStream,
};
use session::{Session, handshake, run};
use stats::ViewerStatsAcc;
use status::millis;
use std::{
    io,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_util::sync::CancellationToken;

/// The TLS handshake, pinning included, must finish within this.
const TLS_TIMEOUT: Duration = Duration::from_secs(15);

/// The session as the reader, the writer and the API see it.
struct ViewerStateSlot {
    codec: VideoCodec,
    capabilities: Vec<String>,
    quality: QualityPreset,
    /// The last host `State`.
    host: Option<StateRecord>,
    route: Option<Route>,
    /// The direct link under `route`, as of the last path sample.
    link: Option<ViewerLink>,
    paused: bool,
    /// The app asked for control and has not revoked it since. The host's
    /// `controlling` counts only while this holds.
    control_requested: bool,
    /// What the app last asked `request_control` for on this viewer; `None`
    /// before it asked. A pause or congestion ends control, clearing
    /// `control_requested`, but not this.
    control_wanted: Option<bool>,
    /// A reason the viewer itself caused, until control is requested again.
    local_reason: Option<ViewerReason>,
    notice: Option<Notice>,
    stopped: Option<Stopped>,
    /// When the session last became streaming: video is due only from then,
    /// so a pause or unavailable capture never counts as time without it.
    entered_streaming: Instant,
}
/// The host's last notice.
struct Notice {
    code: NoticeCode,
    message: String,
    /// Its number in this session.
    seq: u64,
    /// When it came.
    at: Instant,
}
/// How the session ended; no reason: stopped here.
#[derive(Clone, Default)]
struct Stopped {
    reason: Option<ViewerReason>,
    message: String,
}
impl ViewerStateSlot {
    /// Make a change, noting when it starts the session streaming.
    fn update(&mut self, now: Instant, change: impl FnOnce(&mut Self)) {
        let streaming = self.streaming();
        change(self);
        if !streaming && self.streaming() {
            self.entered_streaming = now;
        }
    }
    fn viewer_state(&self) -> ViewerState {
        if self.stopped.is_some() {
            return ViewerState::Stopped;
        }
        if self.paused {
            return ViewerState::Paused;
        }
        match self.host.as_ref().map(|host| host.state) {
            None | Some(SessionState::Connecting) => ViewerState::Connecting,
            Some(SessionState::Streaming) => ViewerState::Streaming,
            Some(SessionState::Paused) => ViewerState::Paused,
            Some(SessionState::CaptureUnavailable) => ViewerState::CaptureUnavailable,
        }
    }
    /// Presentations of the current generation can authorise input.
    fn streaming(&self) -> bool {
        self.viewer_state() == ViewerState::Streaming
    }
    fn generation(&self) -> u64 {
        self.host.as_ref().map_or(0, |host| host.generation)
    }
    fn controlling(&self) -> bool {
        self.stopped.is_none()
            && self.control_requested
            && self.host.as_ref().is_some_and(|host| host.controlling)
    }
    fn control_allowed(&self) -> bool {
        self.stopped.is_none() && self.host.as_ref().is_some_and(|host| host.control_allowed)
    }
    fn reason(&self) -> Option<ViewerReason> {
        match &self.stopped {
            Some(stopped) => stopped.reason,
            None => self
                .local_reason
                .or_else(|| self.host.as_ref()?.reason.map(ViewerReason::from)),
        }
    }
    fn video_size(&self) -> Option<VideoSize> {
        self.host.as_ref()?.video
    }
    /// The host display's modes, from a host that offers them.
    fn modes(&self) -> Option<&ModeList> {
        self.capabilities
            .iter()
            .any(|capability| capability == CAPABILITY_DISPLAY_MODE)
            .then(|| self.host.as_ref()?.modes.as_ref())
            .flatten()
    }
}

/// State shared by the API, the reader, the writer and the sink.
///
/// Lock discipline: `Outbound`'s input queue may be held while taking the
/// state slot, the sink, the control barriers or the outbound slots, and the
/// sink while taking the outbound slots; no other lock is held while taking
/// another, and none across an `.await`.
struct ViewerShared {
    state: Mutex<ViewerStateSlot>,
    stats: Mutex<ViewerStatsAcc>,
    cursor: Mutex<CursorSlot>,
    outbound: Outbound,
}
impl ViewerShared {
    /// The first end wins: a later transport error never replaces the host's
    /// own reason.
    fn mark_stopped(&self, reason: Option<ViewerReason>, message: String) {
        let mut state = self.state.lock();
        if state.stopped.is_none() {
            state.stopped = Some(Stopped { reason, message });
        }
    }
}

/// The newest cursor of the current generation.
#[derive(Clone, Copy, Default)]
struct CursorSlot {
    generation: u64,
    x: i32,
    y: i32,
    visible: bool,
    /// Advances with every update, so a reader can tell a new position.
    seq: u64,
    /// The generation and sequence of the last move written to the host.
    moved: Option<(u64, u64)>,
}
impl CursorSlot {
    /// A report of `generation` the host sampled before it took the last move
    /// this viewer wrote in that generation (`input_sequence` below the
    /// move's) is superseded while this viewer controls: the host's pointer has
    /// gone where this viewer put it since, and the host does not report that
    /// echo, so taking the report would leave the cursor where the host's is
    /// not. The first report of a generation is never superseded, nor is one
    /// without a watermark (an older host).
    fn superseded(&self, generation: u64, input_sequence: Option<u64>, controlling: bool) -> bool {
        controlling
            && self.generation == generation
            && input_sequence
                .zip(self.moved)
                .is_some_and(|(taken, moved)| moved.0 == generation && taken < moved.1)
    }
}

// Embedders share a viewer across threads, and native threads own consumers.
const _: () = {
    const fn shared<T: Send + Sync>() {}
    shared::<Viewer>();
    shared::<crate::VideoConsumer>();
};

pub struct Viewer {
    shared: Arc<ViewerShared>,
    sink: Arc<VideoSink>,
    /// `None` only for sessions over a test stream.
    connection: Option<Arc<Connection>>,
    cancel: CancellationToken,
    join: tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl Viewer {
    /// Returns only after the host's `Accepted`, or a typed rejection, pin or
    /// version failure.
    pub async fn connect(
        client: &Client,
        invitation: &Invitation,
        options: ConnectOptions,
        config: ViewerConfig,
    ) -> std::result::Result<Self, ConnectError> {
        config.validate()?;
        // The service is public, so check what an SDK caller may have changed.
        if !valid_service(&invitation.service) {
            return Err(ConnectError::InvalidRequest);
        }
        config.progress(ConnectStage::Path);
        let (stream, connection) = client
            .connect_tcp(invitation.service.clone(), options)
            .await
            .map_err(|failure| ConnectError::Transport(io::Error::other(failure)))?;
        config.progress(ConnectStage::Identity);
        let name = quinn::rustls::pki_types::ServerName::try_from(security::NAME)
            .map_err(|failure| ConnectError::Transport(io::Error::other(failure)))?;
        let tls = security::client(invitation.fingerprint).map_err(ConnectError::Transport)?;
        let stream = match tokio::time::timeout(TLS_TIMEOUT, tls.connect(name, stream)).await {
            Err(_) => {
                return Err(ConnectError::Transport(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "desktop TLS handshake timed out",
                )));
            }
            Ok(Err(failure)) => {
                return Err(match security::handshake_failure(&failure) {
                    Some(security::HandshakeFailure::IdentityChanged) => {
                        ConnectError::IdentityChanged
                    }
                    Some(security::HandshakeFailure::VersionMismatch) => {
                        ConnectError::VersionMismatch
                    }
                    None => ConnectError::Transport(failure),
                });
            }
            Ok(Ok(stream)) => stream,
        };
        config.progress(ConnectStage::Authorizing);
        Self::start(
            stream,
            invitation.token_text(),
            config,
            Some(Arc::new(connection)),
        )
        .await
    }

    /// A session over a stream whose other end plays the host from
    /// `Authenticate` on: for embedders' tests, which have no host to reach.
    #[doc(hidden)]
    pub async fn over_stream_for_test<S>(
        stream: S,
        token: String,
        config: ViewerConfig,
    ) -> std::result::Result<Self, ConnectError>
    where
        S: AsyncRead + AsyncWrite + Send + Unpin + 'static,
    {
        config.validate()?;
        Self::start(stream, token, config, None).await
    }

    /// Authenticate over an established stream, then run the session on it.
    async fn start<S>(
        mut stream: S,
        token: String,
        config: ViewerConfig,
        connection: Option<Arc<Connection>>,
    ) -> std::result::Result<Self, ConnectError>
    where
        S: AsyncRead + AsyncWrite + Send + Unpin + 'static,
    {
        let accepted = handshake(&mut stream, token, &config).await?;
        tracing::info!(
            target: "pb_mapper_desktop::session",
            event = "desktop_viewer_connected",
            codec = ?accepted.codec,
            quality = ?accepted.quality,
        );
        let now = Instant::now();
        let shared = Arc::new(ViewerShared {
            state: Mutex::new(ViewerStateSlot {
                codec: accepted.codec,
                capabilities: accepted.capabilities,
                quality: accepted.quality,
                host: None,
                route: None,
                link: None,
                paused: false,
                control_requested: false,
                control_wanted: None,
                local_reason: None,
                notice: None,
                stopped: None,
                entered_streaming: now,
            }),
            stats: Mutex::new(ViewerStatsAcc::new(now, config.limits.max_fps)),
            cursor: Mutex::new(CursorSlot::default()),
            outbound: Outbound::new(),
        });
        let sink = VideoSink::for_session(
            Arc::downgrade(&shared),
            config.limits.max_fps,
            config.fps_override,
        );
        let cancel = CancellationToken::new();
        let join = tokio::spawn(run(
            stream,
            Session {
                shared: shared.clone(),
                sink: sink.clone(),
                connection: connection.clone(),
                cancel: cancel.clone(),
                codec: accepted.codec,
                limits: config.limits,
            },
        ));
        Ok(Self {
            shared,
            sink,
            connection,
            cancel,
            join: tokio::sync::Mutex::new(Some(join)),
        })
    }

    /// The figures of the tunnel the session runs over, sampled now: its
    /// path, the bytes it carried, the relay's health and the direct link.
    /// `None` only for a session over a test stream.
    pub fn metrics(&self) -> Option<TunnelMetrics> {
        self.connection
            .as_ref()
            .map(|connection| connection.metrics())
    }

    /// [`Self::metrics`] at once, then every `every` and as the tunnel
    /// changes, until the session's tunnel stops: see
    /// [`TunnelMetricsStream`].
    pub fn subscribe_metrics(&self, every: std::time::Duration) -> Option<TunnelMetricsStream> {
        self.connection
            .as_ref()
            .map(|connection| connection.subscribe_metrics(every))
    }

    /// The session for polls: never waits on the network.
    pub fn status(&self) -> ViewerStatus {
        current_status(&self.shared, &self.sink)
    }

    /// What reads this viewer's status, apart from the viewer.
    pub(crate) fn status_handle(&self) -> ViewerStatusHandle {
        ViewerStatusHandle {
            shared: self.shared.clone(),
            sink: self.sink.clone(),
        }
    }
}

/// A viewer's status, readable from any task until the viewer is gone.
#[derive(Clone)]
pub(crate) struct ViewerStatusHandle {
    shared: Arc<ViewerShared>,
    sink: Arc<VideoSink>,
}
impl ViewerStatusHandle {
    pub(crate) fn status(&self) -> ViewerStatus {
        current_status(&self.shared, &self.sink)
    }
}

/// The session's status now, as `Viewer::status` reports it.
fn current_status(shared: &ViewerShared, sink: &VideoSink) -> ViewerStatus {
    let now = Instant::now();
    let sink = sink.snapshot(now);
    let mut stats = ViewerStats::default();
    let last_video = {
        let mut acc = shared.stats.lock();
        acc.fill(now, sink.viewer_ms, &mut stats);
        (acc.last_video_at, acc.last_video_size)
    };
    let state = shared.state.lock();
    let generation = state.generation();
    let fresh = sink
        .presented
        .is_some_and(|presented| presented.fresh(state.streaming(), generation, now));
    let video = state
        .video_size()
        .map(|size| (size.width, size.height))
        .or(last_video.1)
        .map(|(width, height)| VideoInfo {
            codec: state.codec,
            width,
            height,
        });
    let (path, relay_transport) = match state.route {
        Some(Route::Direct) => ("direct", None),
        Some(Route::RelayTcp) => ("relay", Some("tcp")),
        Some(Route::RelayQuic) => ("relay", Some("quic")),
        None => ("", None),
    };
    stats.path = path.into();
    stats.relay_transport = relay_transport.map(Into::into);
    stats.preset = state.quality;
    stats.link = state.link.clone();
    stats.presented_fps = sink.presented_fps;
    stats.width = video.map_or(0, |video| video.width);
    stats.height = video.map_or(0, |video| video.height);
    stats.codec = state.codec;
    stats.keyframes_requested = sink.keyframes_requested;
    stats.backlog_drops = sink.backlog_drops;
    stats.dropped_frames += sink.discarded;
    stats.present_interval = sink.present_interval;
    let host = state.host.as_ref();
    ViewerStatus {
        state: state.viewer_state(),
        stage: None,
        reason: state.reason(),
        message: match &state.stopped {
            Some(stopped) => stopped.message.clone(),
            None => host.map(|host| host.message.clone()).unwrap_or_default(),
        },
        control_allowed: state.control_allowed(),
        controlling: state.controlling(),
        control_wanted: state.control_wanted,
        fresh,
        paused: state.paused,
        generation,
        display: host.and_then(|host| host.display.clone()),
        desktop: host.map_or(DesktopKind::Ordinary, |host| host.desktop),
        video,
        presented: sink.presented.map(|presented| PresentedInfo {
            generation: presented.generation,
            frame_id: presented.frame_id,
            age_ms: millis(now.saturating_duration_since(presented.presented_at)),
        }),
        last_video_age_ms: last_video
            .0
            .map(|at| millis(now.saturating_duration_since(at.max(state.entered_streaming)))),
        quality: state.quality,
        modes: state.modes().cloned(),
        host_os: state
            .capabilities
            .iter()
            .find_map(|capability| HostOs::from_capability(capability)),
        capabilities: state.capabilities.clone(),
        notice: state.notice.as_ref().map(|notice| NoticeInfo {
            code: notice.code,
            message: notice.message.clone(),
            seq: notice.seq,
            age_ms: millis(now.saturating_duration_since(notice.at)),
        }),
        decoder: sink.decoder,
        stats,
    }
}

impl Viewer {
    /// The per-vsync view: no allocation, no waiting.
    pub fn view(&self) -> DesktopView {
        let now = Instant::now();
        let presented = self.sink.presented();
        let cursor = *self.shared.cursor.lock();
        let state = self.shared.state.lock();
        let generation = state.generation();
        let host = state.host.as_ref();
        let display = host.and_then(|host| host.display.as_ref());
        let video = state.video_size();
        let fresh =
            presented.is_some_and(|presented| presented.fresh(state.streaming(), generation, now));
        let cursor_visible = cursor.visible && cursor.generation == generation;
        let flag = |set: bool, bit: u8| if set { bit } else { 0 };
        let current = presented.filter(|presented| presented.generation == generation);
        DesktopView {
            generation,
            presented_frame_id: current.map_or(0, |presented| presented.frame_id),
            cursor_seq: cursor.seq,
            presented_age_ms: current.map_or(u32::MAX, |presented| {
                u32::try_from(millis(
                    now.saturating_duration_since(presented.presented_at),
                ))
                .unwrap_or(u32::MAX - 1)
            }),
            display_width: display.map_or(0, |display| display.width),
            display_height: display.map_or(0, |display| display.height),
            video_width: video.map_or(0, |video| video.width),
            video_height: video.map_or(0, |video| video.height),
            cursor_x: if cursor_visible { cursor.x } else { 0 },
            cursor_y: if cursor_visible { cursor.y } else { 0 },
            state: DesktopView::state_code(state.viewer_state()),
            flags: flag(cursor_visible, DesktopView::FLAG_CURSOR_VISIBLE)
                | flag(state.controlling(), DesktopView::FLAG_CONTROLLING)
                | flag(state.control_allowed(), DesktopView::FLAG_CONTROL_ALLOWED)
                | flag(
                    host.is_some_and(|host| host.desktop == DesktopKind::Secure),
                    DesktopView::FLAG_SECURE_DESKTOP,
                )
                | flag(fresh, DesktopView::FLAG_FRESH)
                | flag(state.paused, DesktopView::FLAG_PAUSED),
        }
    }

    /// The session's single video sink, for its one native consumer.
    pub fn video_sink(&self) -> Arc<VideoSink> {
        self.sink.clone()
    }

    /// Ask for control, or give it up. A revoke always reaches the host
    /// before a later grant, and either discards queued input.
    pub fn request_control(&self, enabled: bool) -> Result<()> {
        {
            let mut state = self.shared.state.lock();
            if self.cancel.is_cancelled() || state.stopped.is_some() {
                // Teardown already ends control; an idempotent revoke must not
                // replace the original failure.
                return if enabled {
                    Err(error("desktop session is disconnected"))
                } else {
                    Ok(())
                };
            }
            state.control_requested = enabled;
            state.control_wanted = Some(enabled);
            if enabled {
                state.local_reason = None;
            }
        }
        self.shared.outbound.barrier(if enabled {
            Barrier::Grant
        } else {
            Barrier::Revoke
        });
        Ok(())
    }

    /// Release every held key and button on the host. Repeated releases
    /// coalesce; queued input is discarded.
    pub fn release_input(&self) -> Result<()> {
        if !self.cancel.is_cancelled() {
            self.shared.outbound.barrier(Barrier::Release);
        }
        Ok(())
    }

    /// Queue one atomic batch of 1..=16 inputs. It needs a fresh presented
    /// frame of `generation` and control; Rust stamps the presented frame and
    /// consecutive sequences. A single move replaces a queued move; a full
    /// queue revokes control and fails with `Congestion`.
    pub fn input(&self, generation: u64, inputs: &[Input]) -> std::result::Result<(), InputError> {
        {
            let state = self.shared.state.lock();
            if self.cancel.is_cancelled() || state.stopped.is_some() {
                return Err(InputError::Closed);
            }
            if let Some(display) = state.host.as_ref().and_then(|host| host.display.as_ref()) {
                for input in inputs {
                    input
                        .validate(display)
                        .map_err(|failure| InputError::Invalid(failure.to_string()))?;
                }
            }
        }
        self.shared.outbound.submit(
            inputs,
            || {
                let presented = self.sink.presented_for_input()?;
                if !self.shared.state.lock().controlling() {
                    return Err(InputError::NotControlling);
                }
                if presented.0 != generation {
                    return Err(InputError::NotFresh);
                }
                Ok(presented)
            },
            || {
                let mut state = self.shared.state.lock();
                state.control_requested = false;
                state.local_reason = Some(ViewerReason::InputCongestion);
            },
        )
    }

    /// Change the preset live: never a reconnect, never an encoder rebuild.
    pub fn set_quality(&self, preset: QualityPreset) {
        self.shared.state.lock().quality = preset;
        self.shared.outbound.quality(preset);
    }

    /// Change the host display's real mode, live: never a reconnect. The
    /// picture comes back at the new size in a new generation, from an IDR.
    /// `FitLocal` takes the mode that best fits this viewer's display
    /// (`ModeList::fit_local`); `Original` puts back the mode the host found.
    /// Needs a host that offers modes and control of it. The request is
    /// queued like `set_quality`, the newest replacing one not yet sent, and
    /// never awaits: the host answers with the new `State`, or a
    /// `DisplayModeFailed` notice when it refuses. Returns the mode asked
    /// for; nothing is sent when the display already shows it.
    pub fn set_display_mode(
        &self,
        target: ModeTarget,
    ) -> std::result::Result<Mode, DisplayModeError> {
        let (mode, request) = {
            let state = self.shared.state.lock();
            if self.cancel.is_cancelled() || state.stopped.is_some() {
                return Err(DisplayModeError::Closed);
            }
            let modes = state.modes().ok_or(DisplayModeError::NotOffered)?;
            if !state.controlling() {
                return Err(DisplayModeError::NotControlling);
            }
            let mode = match target {
                ModeTarget::Original => modes.original,
                ModeTarget::FitLocal(local) => {
                    modes.fit_local(local).ok_or(DisplayModeError::NoFit)?
                }
                ModeTarget::Size(mode) if modes.supported.contains(&mode) => mode,
                ModeTarget::Size(_) => return Err(DisplayModeError::NotListed),
            };
            let request = (mode != modes.current).then_some(if target == ModeTarget::Original {
                ModeRequest::Restore
            } else {
                ModeRequest::Set(mode)
            });
            (mode, request)
        };
        if let Some(request) = request {
            self.shared.outbound.display_mode(request);
        }
        Ok(mode)
    }

    /// The viewer is hidden: control ends, the host stops capturing, and the
    /// sink drops what it holds. Probes and pings continue.
    pub fn pause(&self) {
        {
            let mut state = self.shared.state.lock();
            if state.paused || state.stopped.is_some() {
                return;
            }
            state.paused = true;
            state.control_requested = false;
        }
        self.shared.outbound.barrier(Barrier::Revoke);
        self.shared.outbound.lifecycle(Lifecycle::Pause);
        self.sink.pause();
    }

    /// The host resumes with an IDR.
    pub fn resume(&self) {
        {
            let mut state = self.shared.state.lock();
            if !state.paused || state.stopped.is_some() {
                return;
            }
            state.update(Instant::now(), |slot| slot.paused = false);
        }
        self.shared.outbound.lifecycle(Lifecycle::Resume);
        self.sink.resume();
    }

    /// Ask for an IDR through the sink's limiter; false means it was held back.
    pub fn request_keyframe(&self, reason: KeyframeReason) -> bool {
        self.sink.request_keyframe(reason)
    }

    /// End the session and its tunnel. Safe to call more than once.
    pub async fn stop(&self) -> Result<()> {
        self.cancel.cancel();
        let mut join = self.join.lock().await;
        if let Some(task) = join.as_mut() {
            let _ = task.await;
        }
        join.take();
        match &self.connection {
            Some(connection) => connection.stop().await.map_err(io::Error::other),
            None => Ok(()),
        }
    }
}
impl Drop for Viewer {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

#[cfg(test)]
mod tests;
