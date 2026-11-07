//! The desktop viewer (protocol v2). `Viewer::connect` returns once the host
//! accepted; two tasks then run the session under one supervisor:
//!
//! - The read task decodes host messages. For every video record it queues
//!   the transport receipt before anything else, so the host's send credit
//!   covers units the sink later drops; then it hands the unit to the
//!   `VideoSink` and updates the state, cursor and stats slots. No FFI, Dart
//!   or decoder work happens on it.
//! - The write task writes `Outbound`'s messages, one per pick in priority
//!   order, and runs the 250 ms viewer tick.
//!
//! Input is authorised only by a presentation the sink validated, stamped by
//! Rust and preceded on the wire by that presentation. Receipts, probes,
//! cursor and input traffic never grant or renew anything.
//!
//! The sink keeps only the newest decodable frames (`video_sink`), recovery
//! asks for IDRs on a limited backoff (`recovery`), and when the consumer
//! falls behind the host is capped to what it presents (`fps_cap`). A decoder
//! that cannot recover ends the session with `decoder_failed`.
pub(crate) mod fps_cap;
mod outbound;
pub(crate) mod recovery;
pub(crate) mod status;

pub use status::{
    DecoderInfo, DecoderState, DesktopView, IntervalStats, NoticeInfo, PresentedInfo, VideoInfo,
    ViewerReason, ViewerState, ViewerStats, ViewerStatus,
};

use crate::{
    Display, Input, Invitation, KeyframeReason, QualityPreset, Result, VideoCodec, VideoLimits,
    error,
    protocol::{
        DesktopKind, HostMessage, NoticeCode, RejectReason, Route, SessionState, StateReason,
        VideoSize, ViewerMessage, valid_capabilities,
    },
    security, valid_service,
    video_sink::{VideoSink, VideoUnit},
    wire,
};
use outbound::{Barrier, Lifecycle, Next, Outbound};
use parking_lot::Mutex;
use pb_mapper_client::sdk::{
    Client, ConnectOptions, Connection, RelayTransport, TunnelDiagnostics,
};
use serde::Serialize;
use status::{CursorSlot, HostStats, ViewerStatsAcc, millis};
use std::{
    io,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

/// The TLS handshake, pinning included, must finish within this.
const TLS_TIMEOUT: Duration = Duration::from_secs(15);
/// The host answers `Authenticate` within this.
const ACCEPT_TIMEOUT: Duration = Duration::from_secs(5);
/// The host writes `Stats` every second, so silence this long is a dead link.
const VIEWER_HEADER_TIMEOUT: Duration = Duration::from_secs(4);
/// The viewer tick: fps cap, path and stats.
const VIEWER_TICK: Duration = Duration::from_millis(250);
/// Ticks per path re-sample and stats roll-up.
const TICKS_PER_SECOND: u64 = 4;
/// Display and JPEG geometry stays within this many pixels.
const MAX_DISPLAY: u32 = 32768;
/// Why a session whose decoder failed for good ended.
const DECODER_FAILED: &str = "the remote picture could not be decoded";

/// How a viewer connects.
pub struct ViewerConfig {
    /// One or two codecs the renderer decodes, in preference order.
    pub codecs: Vec<VideoCodec>,
    /// Fixed for the session; see `VideoLimits::for_viewer`.
    pub limits: VideoLimits,
    /// The initial preset; `Viewer::set_quality` changes it live.
    pub quality: QualityPreset,
    /// Called as the connection passes each stage.
    pub progress: Option<Arc<dyn Fn(ConnectStage) + Send + Sync>>,
}
impl ViewerConfig {
    fn validate(&self) -> std::result::Result<(), ConnectError> {
        if self.codecs.is_empty() || self.codecs.len() > 2 || self.limits.validate().is_err() {
            return Err(ConnectError::InvalidRequest);
        }
        Ok(())
    }
    fn progress(&self, stage: ConnectStage) {
        if let Some(progress) = &self.progress {
            progress(stage);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectStage {
    /// Opening the tunnel to the host, direct or through the relay.
    Path,
    /// TLS, pinned to the invitation's certificate fingerprint.
    Identity,
    /// The invitation's token, answered by `Accepted` or `Rejected`.
    Authorizing,
}

#[derive(Debug)]
pub enum ConnectError {
    /// The host's certificate is not the invitation's: trust was revoked, or
    /// this is not that computer.
    IdentityChanged,
    /// The host speaks another desktop protocol version.
    VersionMismatch,
    /// Another viewer is connected.
    Busy,
    /// The invitation's token was refused.
    Denied,
    /// The codecs, limits or invitation are invalid, here or at the host.
    InvalidRequest,
    /// The embedding app abandoned the attempt.
    Cancelled,
    Transport(io::Error),
    Protocol(String),
}
impl std::fmt::Display for ConnectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::IdentityChanged => f.write_str(
                "the remote computer's identity changed; ask its owner for a new invitation",
            ),
            Self::VersionMismatch => f.write_str(
                "The other computer runs a different pb-mapper version. Update pb-mapper on both computers.",
            ),
            Self::Busy => f.write_str("the remote computer is already being viewed"),
            Self::Denied => f.write_str("the remote computer refused this invitation"),
            Self::InvalidRequest => f.write_str("invalid desktop invitation or video settings"),
            Self::Cancelled => f.write_str("desktop connection cancelled"),
            Self::Transport(error) => write!(f, "{error}"),
            Self::Protocol(message) => write!(f, "desktop protocol error: {message}"),
        }
    }
}
impl std::error::Error for ConnectError {}

#[derive(Debug)]
pub enum InputError {
    /// No current frame was presented recently, the viewer is paused, or the
    /// generation is not the current one.
    NotFresh,
    NotControlling,
    /// The input queue overflowed; control was revoked.
    Congestion,
    Closed,
    Invalid(String),
}
impl std::fmt::Display for InputError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFresh => f.write_str("input needs a current, recently presented picture"),
            Self::NotControlling => f.write_str("control of the remote computer is not granted"),
            Self::Congestion => {
                f.write_str("input congestion; control released while video recovers")
            }
            Self::Closed => f.write_str("the desktop session is closed"),
            Self::Invalid(message) => f.write_str(message),
        }
    }
}
impl std::error::Error for InputError {}

/// The last host `State`.
struct LastState {
    state: SessionState,
    reason: Option<StateReason>,
    message: String,
    control_allowed: bool,
    controlling: bool,
    generation: u64,
    display: Option<Display>,
    desktop: DesktopKind,
    video: Option<VideoSize>,
}

/// The session as both tasks and the API see it.
pub(crate) struct ViewerStateSlot {
    codec: VideoCodec,
    capabilities: Vec<String>,
    quality: QualityPreset,
    host: Option<LastState>,
    route: Option<Route>,
    paused: bool,
    /// The app asked for control and has not revoked it since. The host's
    /// `controlling` counts only while this holds.
    control_requested: bool,
    /// A reason the viewer itself caused, until control is requested again.
    local_reason: Option<ViewerReason>,
    notice: Option<(NoticeCode, String, Instant)>,
    stopped: Option<(Option<ViewerReason>, String)>,
}
impl ViewerStateSlot {
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
    pub(crate) fn streaming(&self) -> bool {
        self.viewer_state() == ViewerState::Streaming
    }
    pub(crate) fn generation(&self) -> u64 {
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
            Some((reason, _)) => *reason,
            None => self
                .local_reason
                .or_else(|| self.host.as_ref()?.reason.map(ViewerReason::from)),
        }
    }
    fn video_size(&self) -> Option<VideoSize> {
        self.host.as_ref()?.video
    }
}

/// State shared by the API, both session tasks and the sink.
///
/// Lock discipline: `Outbound`'s input queue may be held while taking the
/// state slot, the sink, the control barriers or the outbound slots, and the
/// sink while taking the outbound slots; no other lock is held while taking
/// another, and none across an `.await`.
pub(crate) struct ViewerShared {
    pub state: Mutex<ViewerStateSlot>,
    pub stats: Mutex<ViewerStatsAcc>,
    pub cursor: Mutex<CursorSlot>,
    pub outbound: Outbound,
}
impl ViewerShared {
    /// The first end wins: a later transport error never replaces the host's
    /// own reason.
    fn mark_stopped(&self, reason: Option<ViewerReason>, message: String) {
        let mut state = self.state.lock();
        if state.stopped.is_none() {
            state.stopped = Some((reason, message));
        }
    }
}

/// What `Accepted` settled for the session.
struct Accepted {
    codec: VideoCodec,
    quality: QualityPreset,
    capabilities: Vec<String>,
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

    /// Authenticate over an established stream, then run the session on it.
    pub(crate) async fn start<S>(
        mut stream: S,
        token: String,
        config: ViewerConfig,
        connection: Option<Arc<Connection>>,
    ) -> std::result::Result<Self, ConnectError>
    where
        S: AsyncRead + AsyncWrite + Send + Unpin + 'static,
    {
        let accepted = handshake(&mut stream, token, &config).await?;
        let now = Instant::now();
        let shared = Arc::new(ViewerShared {
            state: Mutex::new(ViewerStateSlot {
                codec: accepted.codec,
                capabilities: accepted.capabilities,
                quality: accepted.quality,
                host: None,
                route: None,
                paused: false,
                control_requested: false,
                local_reason: None,
                notice: None,
                stopped: None,
            }),
            stats: Mutex::new(ViewerStatsAcc::new(now)),
            cursor: Mutex::new(CursorSlot::default()),
            outbound: Outbound::new(),
        });
        let sink = VideoSink::for_session(Arc::downgrade(&shared), config.limits.max_fps);
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

    /// The session for polls: never waits on the network.
    pub fn status(&self) -> ViewerStatus {
        let now = Instant::now();
        let sink = self.sink.snapshot(now);
        let mut stats = ViewerStats::default();
        let last_video = {
            let mut acc = self.shared.stats.lock();
            acc.fill(now, sink.viewer_ms, &mut stats);
            (acc.last_video_at, acc.last_video_size)
        };
        let state = self.shared.state.lock();
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
                Some((_, message)) => message.clone(),
                None => host.map(|host| host.message.clone()).unwrap_or_default(),
            },
            control_allowed: state.control_allowed(),
            controlling: state.controlling(),
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
                .map(|at| millis(now.saturating_duration_since(at))),
            quality: state.quality,
            capabilities: state.capabilities.clone(),
            notice: state.notice.as_ref().map(|(code, message, at)| NoticeInfo {
                code: *code,
                message: message.clone(),
                age_ms: millis(now.saturating_duration_since(*at)),
            }),
            decoder: sink.decoder,
            stats,
        }
    }

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
            state.paused = false;
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
            Some(connection) => connection.stop().await.map_err(error),
            None => Ok(()),
        }
    }
}
impl Drop for Viewer {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

/// `Authenticate`, then the host's answer.
async fn handshake<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
    token: String,
    config: &ViewerConfig,
) -> std::result::Result<Accepted, ConnectError> {
    let authenticate = ViewerMessage::Authenticate {
        token,
        codecs: config.codecs.clone(),
        limits: config.limits,
        quality: config.quality,
        capabilities: Vec::new(),
    };
    wire::write_message(stream, &authenticate, None)
        .await
        .map_err(ConnectError::Transport)?;
    drop(authenticate);
    match wire::read_header::<_, HostMessage>(stream, ACCEPT_TIMEOUT).await {
        Ok(Some(HostMessage::Accepted {
            codec,
            quality,
            capabilities,
            ..
        })) => {
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
        Ok(Some(_)) => Err(ConnectError::Protocol(
            "the host did not answer the authentication".into(),
        )),
        Ok(None) => Err(ConnectError::Transport(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "the remote computer closed the connection during authentication",
        ))),
        Err(failure) if failure.kind() == io::ErrorKind::Other => {
            Err(ConnectError::Protocol(failure.to_string()))
        }
        Err(failure) => Err(ConnectError::Transport(failure)),
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
    fn failed(failure: io::Error) -> Self {
        // `wire` reports malformed and oversized records as `Other`; the
        // transport reports its own kinds.
        let reason = match failure.kind() {
            io::ErrorKind::TimedOut => ViewerReason::Timeout,
            io::ErrorKind::Other => ViewerReason::Protocol,
            _ => ViewerReason::Transport,
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

struct Session {
    shared: Arc<ViewerShared>,
    sink: Arc<VideoSink>,
    connection: Option<Arc<Connection>>,
    cancel: CancellationToken,
    codec: VideoCodec,
    limits: VideoLimits,
}

/// The supervisor: the first task to end ends the session.
async fn run<S: AsyncRead + AsyncWrite + Send + 'static>(stream: S, session: Session) {
    let Session {
        shared,
        sink,
        connection,
        cancel,
        codec,
        limits,
    } = session;
    let (mut read, mut write) = tokio::io::split(stream);
    let mut reader = Reader {
        shared: &shared,
        sink: &sink,
        codec,
        limits,
        last_seq: 0,
        generation: 0,
    };
    let mut writer = Writer {
        shared: &shared,
        sink: &sink,
        connection: connection.as_deref(),
        ticks: 0,
    };
    let ended = tokio::select! {
        biased;
        ended = writer.run(&mut write, &cancel) => ended,
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
        loop {
            let message = match wire::read_header(read, VIEWER_HEADER_TIMEOUT).await {
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
                // JPEG reports the display geometry; H.264 its encoded size.
                let bounded = self.codec == VideoCodec::Jpeg
                    || (width <= self.limits.max_width && height <= self.limits.max_height);
                if !bounded
                    || !(1..=MAX_DISPLAY).contains(&width)
                    || !(1..=MAX_DISPLAY).contains(&height)
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
            HostMessage::State {
                state,
                reason,
                message,
                control_allowed,
                controlling,
                generation,
                display,
                desktop,
                video,
            } => {
                if display.as_ref().is_some_and(|display| {
                    !(1..=MAX_DISPLAY).contains(&display.width)
                        || !(1..=MAX_DISPLAY).contains(&display.height)
                }) {
                    return Err(Ended::protocol("invalid display geometry"));
                }
                self.generation = generation;
                // The sink first, so nothing of an old generation is accepted
                // once the state names the new one. The host pauses only for
                // this viewer, whose own pause covers that.
                self.sink.on_state(
                    generation,
                    matches!(state, SessionState::Streaming | SessionState::Paused),
                );
                self.shared.state.lock().host = Some(LastState {
                    state,
                    reason,
                    message,
                    control_allowed,
                    controlling,
                    generation,
                    display,
                    desktop,
                    video,
                });
            }
            HostMessage::Cursor {
                generation,
                x,
                y,
                visible,
            } => {
                if generation == self.generation {
                    let mut cursor = self.shared.cursor.lock();
                    *cursor = CursorSlot {
                        generation,
                        x,
                        y,
                        visible,
                        seq: cursor.seq + 1,
                    };
                }
            }
            HostMessage::Stats {
                capture_fps,
                pace_fps,
                target_fps,
                fps_cap,
                bitrate,
                encoder_bitrate,
                rtt_ms,
                base_rtt_ms,
                queue_delay_ms,
                dropped_frames,
                keyframes,
                adaptation,
                rate_model,
                ..
            } => {
                self.shared.stats.lock().host = Some(HostStats {
                    capture_fps,
                    pace_fps,
                    target_fps,
                    fps_cap,
                    bitrate,
                    encoder_bitrate,
                    rtt_ms,
                    base_rtt_ms,
                    queue_delay_ms,
                    dropped_frames,
                    keyframes,
                    adaptation,
                    rate_model,
                });
            }
            HostMessage::Notice { code, message } => {
                self.shared.state.lock().notice = Some((code, message, Instant::now()));
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
        loop {
            let now = Instant::now();
            let mut failure = None;
            if now >= next_tick {
                if self.tick(now) {
                    failure = Some((ViewerReason::DecoderFailed, DECODER_FAILED.into()));
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
                    if let Err(failure) = wire::write_message(write, &message, None).await {
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
    /// watchdog; once a second also the path and the stats roll-up. True when
    /// the decoder failed for good.
    fn tick(&mut self, now: Instant) -> bool {
        let second = self.ticks.is_multiple_of(TICKS_PER_SECOND);
        self.ticks += 1;
        if second && let Some(connection) = self.connection {
            let route = route(&connection.diagnostics());
            let changed = self.shared.state.lock().route.replace(route) != Some(route);
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
            self.shared
                .stats
                .lock()
                .roll(now, streaming, sink.presented_fps, sink.viewer_ms);
        }
        self.sink.decoder_failed()
    }
}

/// The path this viewer's tunnel carries the session on.
fn route(diagnostics: &TunnelDiagnostics) -> Route {
    if diagnostics
        .direct
        .as_ref()
        .is_some_and(|direct| direct.active_direct > 0)
    {
        Route::Direct
    } else if diagnostics.relay_transport == RelayTransport::Quic {
        Route::RelayQuic
    } else {
        Route::RelayTcp
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::{Key, protocol::StopReason, video_sink::Pull};
    use tokio::io::DuplexStream;

    const TOKEN: &str = "0123456789012345678901234567890123456789012";

    fn config() -> ViewerConfig {
        ViewerConfig {
            codecs: vec![VideoCodec::H264],
            limits: VideoLimits::for_viewer(60.0),
            quality: QualityPreset::Balanced,
            progress: None,
        }
    }

    async fn send(host: &mut DuplexStream, message: HostMessage, payload: Option<&[u8]>) {
        wire::write_message(host, &message, payload).await.unwrap();
    }

    /// The viewer's next message other than a ping.
    async fn receive(host: &mut DuplexStream) -> ViewerMessage {
        loop {
            match wire::read_header(host, Duration::from_secs(5))
                .await
                .unwrap()
            {
                Some(ViewerMessage::Ping {}) => {}
                Some(message) => return message,
                None => panic!("the viewer closed the stream"),
            }
        }
    }

    /// Read until a message matches, returning it.
    async fn until(
        host: &mut DuplexStream,
        mut matches: impl FnMut(&ViewerMessage) -> bool,
    ) -> ViewerMessage {
        loop {
            let message = receive(host).await;
            if matches(&message) {
                return message;
            }
        }
    }

    fn state(generation: u64, controlling: bool) -> HostMessage {
        HostMessage::State {
            state: SessionState::Streaming,
            reason: None,
            message: String::new(),
            control_allowed: true,
            controlling,
            generation,
            display: Some(Display {
                id: 1,
                name: "scripted".into(),
                x: 0,
                y: 0,
                width: 1920,
                height: 1080,
            }),
            desktop: DesktopKind::Ordinary,
            video: Some(VideoSize {
                width: 1920,
                height: 1080,
            }),
        }
    }

    fn video(seq: u64, generation: u64, frame_id: u64, keyframe: bool) -> HostMessage {
        HostMessage::Video {
            seq,
            generation,
            frame_id,
            keyframe,
            duplicate: false,
            width: 1920,
            height: 1080,
            pts_us: 0,
            host_us: 2_000,
        }
    }

    async fn pull(consumer: &crate::VideoConsumer) -> VideoUnit {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match consumer.next(Duration::ZERO, 1 << 20) {
                    Pull::Unit(unit) => return unit,
                    Pull::Timeout => tokio::time::sleep(Duration::from_millis(5)).await,
                    other => panic!("expected a unit, got {other:?}"),
                }
            }
        })
        .await
        .unwrap()
    }

    async fn eventually(mut condition: impl FnMut() -> bool) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while !condition() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
    }

    fn accepted() -> HostMessage {
        HostMessage::Accepted {
            codec: VideoCodec::H264,
            limits: VideoLimits::for_viewer(60.0),
            nominal_fps: 60,
            quality: QualityPreset::Balanced,
            capabilities: vec!["lock".into()],
        }
    }

    /// Connect over a duplex stream; the host side answers `answer`.
    async fn connect(
        answer: HostMessage,
    ) -> (
        std::result::Result<Viewer, ConnectError>,
        DuplexStream,
        ViewerMessage,
    ) {
        let (viewer, mut host) = tokio::io::duplex(1 << 20);
        let connecting = tokio::spawn(Viewer::start(viewer, TOKEN.into(), config(), None));
        let authenticate = receive(&mut host).await;
        send(&mut host, answer, None).await;
        (connecting.await.unwrap(), host, authenticate)
    }

    #[tokio::test]
    async fn scripted_host_session_acknowledges_first_and_authorises_input_by_presentation() {
        for (reason, rejected) in [
            (RejectReason::Busy, "Busy"),
            (RejectReason::Denied, "Denied"),
            (RejectReason::InvalidRequest, "InvalidRequest"),
        ] {
            let (result, _, _) = connect(HostMessage::Rejected { reason }).await;
            assert_eq!(format!("{:?}", result.err().unwrap()), rejected);
        }

        let (viewer, mut host, authenticate) = connect(accepted()).await;
        let viewer = viewer.unwrap();
        let ViewerMessage::Authenticate {
            token,
            codecs,
            capabilities,
            ..
        } = authenticate
        else {
            panic!("expected Authenticate first");
        };
        assert_eq!(token, TOKEN);
        assert_eq!(codecs, [VideoCodec::H264]);
        assert!(capabilities.is_empty());

        // Receipts and probe echoes need no consumer at all.
        send(&mut host, state(1, false), None).await;
        send(&mut host, HostMessage::Probe { nonce: 7 }, None).await;
        send(&mut host, video(1, 1, 1, true), Some(&[1; 64])).await;
        let (mut echoed, mut received) = (false, false);
        while !(echoed && received) {
            match receive(&mut host).await {
                ViewerMessage::ProbeEcho { nonce: 7 } => echoed = true,
                ViewerMessage::Received { seq: 1 } => received = true,
                other => panic!("unexpected {other:?}"),
            }
        }

        // Input waits for a presented frame and for control.
        let shift = [Input::Key {
            key: Key::Shift,
            down: true,
        }];
        assert!(matches!(viewer.input(1, &shift), Err(InputError::NotFresh)));
        let consumer = viewer.video_sink().attach().unwrap();
        let unit = pull(&consumer).await;
        assert!(unit.keyframe && unit.discontinuity);
        assert!(consumer.presented(1, 1));
        until(&mut host, |m| {
            matches!(
                m,
                ViewerMessage::Presented {
                    generation: 1,
                    frame_id: 1
                }
            )
        })
        .await;
        assert!(viewer.status().fresh);
        assert!(matches!(
            viewer.input(1, &shift),
            Err(InputError::NotControlling)
        ));
        viewer.request_control(true).unwrap();
        until(&mut host, |m| {
            matches!(m, ViewerMessage::Control { enabled: true })
        })
        .await;
        send(&mut host, state(1, true), None).await;
        eventually(|| viewer.status().controlling).await;
        assert!(matches!(viewer.input(2, &shift), Err(InputError::NotFresh)));
        viewer.input(1, &shift).unwrap();
        let ViewerMessage::Input { event } = receive(&mut host).await else {
            panic!("expected the input");
        };
        assert_eq!(
            (event.generation, event.frame_id, event.sequence),
            (1, 1, 1)
        );

        // A unit of an older generation is acknowledged and never shown.
        send(&mut host, state(2, false), None).await;
        send(&mut host, video(2, 1, 2, false), Some(&[2; 64])).await;
        until(&mut host, |m| {
            matches!(m, ViewerMessage::Received { seq: 2 })
        })
        .await;
        assert!(matches!(
            consumer.next(Duration::from_millis(20), 1 << 20),
            Pull::Timeout
        ));
        let status = viewer.status();
        assert!(!status.fresh && !status.controlling && status.generation == 2);

        // The host's own reason ends the session, and the consumer exits.
        send(
            &mut host,
            HostMessage::Stopped {
                reason: StopReason::HostEnded,
                message: "ended by the host".into(),
            },
            None,
        )
        .await;
        eventually(|| viewer.status().state == ViewerState::Stopped).await;
        let status = viewer.status();
        assert_eq!(status.reason, Some(ViewerReason::HostEnded));
        assert_eq!(status.message, "ended by the host");
        assert!(matches!(
            consumer.next(Duration::ZERO, 1 << 20),
            Pull::Closed
        ));
        assert!(matches!(viewer.input(2, &shift), Err(InputError::Closed)));
        viewer.stop().await.unwrap();
    }

    #[tokio::test]
    async fn a_decoder_that_cannot_recover_ends_the_session() {
        let (viewer, mut host, _) = connect(accepted()).await;
        let viewer = viewer.unwrap();
        send(&mut host, state(1, false), None).await;
        send(&mut host, video(1, 1, 1, true), Some(&[1; 64])).await;
        let consumer = viewer.video_sink().attach().unwrap();
        pull(&consumer).await;
        assert!(consumer.presented(1, 1));

        // The first error asks for an IDR; the fifth with no picture since
        // gives up, and the app is told why.
        assert!(consumer.keyframe(KeyframeReason::DecoderError));
        for _ in 0..4 {
            assert!(!consumer.keyframe(KeyframeReason::DecoderError));
        }
        eventually(|| viewer.status().state == ViewerState::Stopped).await;
        let status = viewer.status();
        assert_eq!(status.reason, Some(ViewerReason::DecoderFailed));
        assert_eq!(status.decoder.state, DecoderState::Failed);
        assert!(matches!(
            consumer.next(Duration::ZERO, 1 << 20),
            Pull::Closed
        ));
        drop(host);
        viewer.stop().await.unwrap();
    }
}
