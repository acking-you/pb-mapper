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
    HostOs, Input, Invitation, KeyframeReason, QualityPreset, Result, VideoCodec, VideoLimits,
    error,
    modes::{DisplayModeError, Mode, ModeList, ModeTarget},
    protocol::{
        CAPABILITY_CURSOR_SEQUENCE, CAPABILITY_DISPLAY_MODE, CAPABILITY_ENCODER_DWELL,
        CAPABILITY_FPS_OVERRIDE, CAPABILITY_H265, DesktopKind, HostMessage, NoticeCode,
        RejectReason, Route, SessionState, StateRecord, VideoSize, ViewerMessage,
        valid_capabilities,
    },
    security, valid_service,
    video_sink::{VideoSink, VideoUnit},
    wire,
};
use outbound::{Barrier, Lifecycle, ModeRequest, Next, Outbound};
use parking_lot::Mutex;
use pb_mapper_client::sdk::{
    AddressFamily, Client, ConnectOptions, Connection, LanEvidence, LinkKind, LinkSample, LinkVia,
    RelayTransport, TunnelMetrics, TunnelMetricsStream,
};
use serde::Serialize;
use status::{CursorSlot, HostStats, Traffic, ViewerStatsAcc, millis};
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
#[derive(Clone)]
pub struct ViewerConfig {
    /// The codecs the renderer decodes, in preference order: one or two of
    /// H.264 and JPEG, and `H265` beside them when it decodes that too. H.265
    /// never goes in `Authenticate.codecs`, which an older host rejects: it is
    /// offered as the `h265` capability, and only a host that can encode it
    /// answers with it. Whatever the host picks, it is one of these.
    pub codecs: Vec<VideoCodec>,
    /// Fixed for the session; see `VideoLimits::choose`.
    pub limits: VideoLimits,
    /// `limits.max_fps` is a frame rate the user chose, sent as the
    /// `fps_override` capability: the host's ceiling for every preset, and
    /// the fps cap's, in place of each preset's own.
    pub fps_override: bool,
    /// The initial preset; `Viewer::set_quality` changes it live.
    pub quality: QualityPreset,
    /// Called as the connection passes each stage.
    pub progress: Option<Arc<dyn Fn(ConnectStage) + Send + Sync>>,
}
/// What a renderer decodes in hardware: the largest picture each decoder
/// takes at 60 fps, in either orientation; `None` where it has none.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Decoders {
    pub h264: Option<(u32, u32)>,
    pub h265: Option<(u32, u32)>,
}

impl ViewerConfig {
    /// A viewer of `codecs`, at the default resolution and the display's 60
    /// fps, with the balanced preset.
    pub fn new(codecs: Vec<VideoCodec>) -> Self {
        Self {
            codecs,
            limits: VideoLimits::choose(60.0, crate::Resolution::default(), None),
            fps_override: false,
            quality: QualityPreset::Balanced,
            progress: None,
        }
    }
    /// The offer for what the renderer decodes: H.265 where it decodes it,
    /// then H.264 where it decodes that, else JPEG, which needs no decoder.
    /// The limits are `resolution`'s at `max_fps`, or at the display's
    /// `refresh_hz` (see [`VideoLimits::choose`]), fitted inside the largest
    /// picture of the decoder the host picks first: so a phone keeps H.265
    /// at the default size. A host that declines H.265 sends H.264 within
    /// those limits, which the H.264 decoder may not take; connect again
    /// without H.265 then.
    pub fn for_decoders(
        decoders: Decoders,
        resolution: crate::Resolution,
        refresh_hz: f64,
        max_fps: Option<u32>,
    ) -> Self {
        let fallback = if decoders.h264.is_some() {
            VideoCodec::H264
        } else {
            VideoCodec::Jpeg
        };
        let codecs = decoders
            .h265
            .map(|_| VideoCodec::H265)
            .into_iter()
            .chain([fallback])
            .collect();
        let limits = VideoLimits::choose(refresh_hz, resolution, max_fps);
        Self {
            codecs,
            limits: match decoders.h265.or(decoders.h264) {
                Some(largest) => limits.clamped(largest),
                None => limits,
            },
            fps_override: max_fps.is_some(),
            quality: QualityPreset::Balanced,
            progress: None,
        }
    }
    /// H.265 at most once, and beside one or two codecs every host knows.
    fn validate(&self) -> std::result::Result<(), ConnectError> {
        let offered = self.wire_codecs().len();
        let h265 = self
            .codecs
            .iter()
            .filter(|codec| **codec == VideoCodec::H265)
            .count();
        if !(1..=2).contains(&offered) || h265 > 1 || self.limits.validate().is_err() {
            return Err(ConnectError::InvalidRequest);
        }
        Ok(())
    }
    /// What `Authenticate.codecs` lists: every codec but H.265.
    fn wire_codecs(&self) -> Vec<VideoCodec> {
        self.codecs
            .iter()
            .copied()
            .filter(|codec| *codec != VideoCodec::H265)
            .collect()
    }
    /// What this viewer reads and asks for, as `Authenticate.capabilities`.
    fn capabilities(&self) -> Vec<String> {
        let mut capabilities = vec![
            CAPABILITY_ENCODER_DWELL.to_owned(),
            CAPABILITY_CURSOR_SEQUENCE.to_owned(),
            CAPABILITY_DISPLAY_MODE.to_owned(),
        ];
        if self.codecs.contains(&VideoCodec::H265) {
            capabilities.push(CAPABILITY_H265.to_owned());
        }
        if self.fps_override {
            capabilities.push(CAPABILITY_FPS_OVERRIDE.to_owned());
        }
        capabilities
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

/// The session as both tasks and the API see it.
pub(crate) struct ViewerStateSlot {
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
    /// The last notice, its number in this session, and when it came.
    notice: Option<(NoticeCode, String, u64, Instant)>,
    stopped: Option<(Option<ViewerReason>, String)>,
    /// When the session last became streaming: video is due only from then,
    /// so a pause or unavailable capture never counts as time without it.
    entered_streaming: Instant,
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
    /// The host display's modes, from a host that offers them.
    fn modes(&self) -> Option<&ModeList> {
        self.capabilities
            .iter()
            .any(|capability| capability == CAPABILITY_DISPLAY_MODE)
            .then(|| self.host.as_ref()?.modes.as_ref())
            .flatten()
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
        self.status_handle().status()
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
                Some((_, message)) => message.clone(),
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
            notice: state
                .notice
                .as_ref()
                .map(|(code, message, seq, at)| NoticeInfo {
                    code: *code,
                    message: message.clone(),
                    seq: *seq,
                    age_ms: millis(now.saturating_duration_since(*at)),
                }),
            decoder: sink.decoder,
            stats,
        }
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
        codecs: config.wire_codecs(),
        limits: config.limits,
        quality: config.quality,
        capabilities: config.capabilities(),
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
    let started = Instant::now();
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
    // The first stop recorded, not this one: a local stop after a failure
    // keeps the failure. No reason: stopped here.
    let (reason, detail) = shared.state.lock().stopped.clone().unwrap_or_default();
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
            HostMessage::State(host) => {
                if host.display.as_ref().is_some_and(|display| {
                    !(1..=MAX_DISPLAY).contains(&display.width)
                        || !(1..=MAX_DISPLAY).contains(&display.height)
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
            HostMessage::Stats {
                capture_fps,
                pace_fps,
                target_fps,
                fps_cap,
                bitrate,
                encoder_bitrate,
                sent_bitrate,
                rtt_ms,
                base_rtt_ms,
                queue_delay_ms,
                ack_queue_ms,
                encoder_dwell_ms,
                dropped_frames,
                skipped_ticks,
                keyframes,
                adaptation,
                rate_model,
                ..
            } => {
                let host = HostStats {
                    capture_fps,
                    pace_fps,
                    target_fps,
                    fps_cap,
                    bitrate,
                    encoder_bitrate,
                    sent_bitrate,
                    rtt_ms,
                    base_rtt_ms,
                    queue_delay_ms,
                    ack_queue_ms,
                    encoder_dwell_ms,
                    dropped_frames,
                    skipped_ticks,
                    keyframes,
                    adaptation,
                    rate_model,
                };
                self.shared.stats.lock().on_host(Instant::now(), host);
            }
            HostMessage::Notice { code, message } => {
                let mut state = self.shared.state.lock();
                let seq = state.notice.as_ref().map_or(1, |(_, _, seq, _)| seq + 1);
                state.notice = Some((code, message, seq, Instant::now()));
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

/// The direct link a session runs on, as `ViewerStats::link` reports it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ViewerLink {
    /// `lan`, `internet` or `private`.
    pub kind: LinkKind,
    pub family: AddressFamily,
    /// How the peer address was learned.
    pub via: LinkVia,
    pub peer: String,
    pub local_ip: Option<String>,
    /// QUIC's round trip and its lowest, rounded up to whole milliseconds:
    /// the network alone, unlike `ViewerStats::rtt_ms`, which the host's
    /// probe measures behind the video.
    pub rtt_ms: u32,
    pub min_rtt_ms: u32,
    pub setup_ms: u32,
    /// What suggested a shared network before the setup.
    pub same_lan: Option<LanEvidence>,
}

impl From<&LinkSample> for ViewerLink {
    fn from(link: &LinkSample) -> Self {
        let ms = |us: u64| u32::try_from(us.div_ceil(1000)).unwrap_or(u32::MAX);
        Self {
            kind: link.kind,
            family: link.family,
            via: link.via,
            peer: link.peer.to_string(),
            local_ip: link.local_ip.map(|ip| ip.to_string()),
            rtt_ms: ms(link.stats.rtt_us),
            min_rtt_ms: ms(link.stats.min_rtt_us),
            setup_ms: u32::try_from(link.setup_ms).unwrap_or(u32::MAX),
            same_lan: link.same_lan,
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::{Display, Key, protocol::StopReason, video_sink::Pull};
    use tokio::io::DuplexStream;

    const TOKEN: &str = "0123456789012345678901234567890123456789012";

    fn config() -> ViewerConfig {
        ViewerConfig {
            codecs: vec![VideoCodec::H264],
            limits: VideoLimits::for_viewer(60.0),
            fps_override: false,
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
        HostMessage::State(StateRecord {
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
            modes: None,
        })
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
        accepted_with(VideoCodec::H264)
    }

    fn accepted_with(codec: VideoCodec) -> HostMessage {
        HostMessage::Accepted {
            codec,
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
        connect_with(config(), answer).await
    }

    async fn connect_with(
        config: ViewerConfig,
        answer: HostMessage,
    ) -> (
        std::result::Result<Viewer, ConnectError>,
        DuplexStream,
        ViewerMessage,
    ) {
        let (viewer, mut host) = tokio::io::duplex(1 << 20);
        let connecting = tokio::spawn(Viewer::start(viewer, TOKEN.into(), config, None));
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
        assert_eq!(
            capabilities,
            [
                CAPABILITY_ENCODER_DWELL,
                CAPABILITY_CURSOR_SEQUENCE,
                CAPABILITY_DISPLAY_MODE
            ]
        );

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

    /// H.265 and a chosen frame rate are capabilities, so an older host sees
    /// only H.264 and values it ignores; H.265 is taken only when offered.
    #[tokio::test]
    async fn h265_is_offered_as_a_capability_and_accepted_only_when_offered() {
        let h265 = || ViewerConfig {
            codecs: vec![VideoCodec::H265, VideoCodec::H264],
            fps_override: true,
            ..config()
        };
        let (viewer, mut host, authenticate) =
            connect_with(h265(), accepted_with(VideoCodec::H265)).await;
        let viewer = viewer.unwrap();
        let ViewerMessage::Authenticate {
            codecs,
            capabilities,
            ..
        } = authenticate
        else {
            panic!("expected Authenticate first");
        };
        assert_eq!(codecs, [VideoCodec::H264]);
        assert_eq!(
            capabilities,
            [
                CAPABILITY_ENCODER_DWELL,
                CAPABILITY_CURSOR_SEQUENCE,
                CAPABILITY_DISPLAY_MODE,
                CAPABILITY_H265,
                CAPABILITY_FPS_OVERRIDE
            ]
        );
        assert_eq!(viewer.status().stats.codec, VideoCodec::H265);
        send(&mut host, state(1, false), None).await;
        send(&mut host, video(1, 1, 1, true), Some(&[1; 64])).await;
        let consumer = viewer.video_sink().attach().unwrap();
        assert_eq!(pull(&consumer).await.codec, VideoCodec::H265);
        drop(host);
        viewer.stop().await.unwrap();

        let (unasked, _, _) = connect(accepted_with(VideoCodec::H265)).await;
        assert!(matches!(unasked, Err(ConnectError::Protocol(_))));
        // H.265 alone would leave an older host no codec it knows.
        let alone = ViewerConfig {
            codecs: vec![VideoCodec::H265],
            ..config()
        };
        let (stream, _host) = tokio::io::duplex(64);
        assert!(matches!(
            Viewer::over_stream_for_test(stream, TOKEN.into(), alone).await,
            Err(ConnectError::InvalidRequest)
        ));
    }

    /// The offer follows the decoders, and the limits the one the host picks
    /// first: a phone whose H.265 decoder stops at 1080p, probed upright,
    /// keeps H.265 at the default size. Without a decoder, JPEG.
    #[test]
    fn the_offer_and_limits_follow_the_decoders() {
        let native = |decoders| {
            ViewerConfig::for_decoders(decoders, crate::Resolution::default(), 60.0, None)
        };
        let phone = native(Decoders {
            h264: Some((3840, 2160)),
            h265: Some((1080, 1920)),
        });
        assert_eq!(phone.codecs, [VideoCodec::H265, VideoCodec::H264]);
        let bounds = |config: &ViewerConfig| (config.limits.max_width, config.limits.max_height);
        assert_eq!(bounds(&phone), (1920, 1080));
        let none = native(Decoders::default());
        assert!(
            none.capabilities()
                .contains(&CAPABILITY_DISPLAY_MODE.to_owned())
        );
        assert_eq!(
            (none.codecs.as_slice(), bounds(&none)),
            (&[VideoCodec::Jpeg][..], (3840, 2160))
        );
    }

    /// A host that did not list `display_mode` is never asked for a mode,
    /// even one whose state carries modes; one that did is asked, live, and
    /// only while this viewer controls it.
    #[tokio::test]
    async fn display_modes_are_asked_only_of_a_host_that_offers_them() {
        let mode = |width, height| Mode { width, height };
        for offered in [false, true] {
            let mut answer = accepted();
            if let HostMessage::Accepted { capabilities, .. } = &mut answer
                && offered
            {
                capabilities.push(CAPABILITY_DISPLAY_MODE.into());
            }
            let (viewer, mut host, _) = connect(answer).await;
            let viewer = viewer.unwrap();
            let with_modes = |controlling| {
                let HostMessage::State(mut record) = state(1, controlling) else {
                    unreachable!()
                };
                record.modes = Some(ModeList {
                    current: mode(1920, 1080),
                    original: mode(1920, 1080),
                    supported: vec![mode(1920, 1080), mode(1280, 720)],
                });
                HostMessage::State(record)
            };
            send(&mut host, with_modes(false), None).await;
            eventually(|| viewer.status().generation == 1).await;
            assert_eq!(viewer.status().modes.is_some(), offered);
            let smaller = ModeTarget::Size(mode(1280, 720));
            if !offered {
                assert_eq!(
                    viewer.set_display_mode(smaller),
                    Err(DisplayModeError::NotOffered)
                );
                continue;
            }
            assert_eq!(
                viewer.set_display_mode(smaller),
                Err(DisplayModeError::NotControlling)
            );
            viewer.request_control(true).unwrap();
            send(&mut host, with_modes(true), None).await;
            eventually(|| viewer.status().controlling).await;
            assert_eq!(
                viewer.set_display_mode(ModeTarget::Size(mode(800, 600))),
                Err(DisplayModeError::NotListed)
            );
            assert_eq!(viewer.set_display_mode(smaller), Ok(mode(1280, 720)));
            until(&mut host, |message| {
                matches!(
                    message,
                    ViewerMessage::DisplayMode {
                        width: 1280,
                        height: 720
                    }
                )
            })
            .await;
            viewer.stop().await.unwrap();
        }
    }

    /// A report the host sampled before it took this viewer's last move is set
    /// aside while this viewer controls: the host never reports that move's
    /// echo, so the cursor would stay where the host's pointer is not. One at
    /// or after the move is taken, and so is any once control is given up.
    #[tokio::test]
    async fn a_cursor_report_older_than_the_last_move_is_set_aside_while_controlling() {
        let (viewer, mut host, _) = connect(accepted()).await;
        let viewer = viewer.unwrap();
        send(&mut host, state(1, false), None).await;
        send(&mut host, video(1, 1, 1, true), Some(&[1; 64])).await;
        let consumer = viewer.video_sink().attach().unwrap();
        pull(&consumer).await;
        assert!(consumer.presented(1, 1));
        viewer.request_control(true).unwrap();
        until(&mut host, |m| {
            matches!(m, ViewerMessage::Control { enabled: true })
        })
        .await;
        send(&mut host, state(1, true), None).await;
        eventually(|| viewer.status().controlling).await;

        let cursor = |x, input_sequence| HostMessage::Cursor {
            generation: 1,
            x,
            y: 0,
            visible: true,
            input_sequence: Some(input_sequence),
        };
        let seen = || {
            let view = viewer.view();
            (view.cursor_seq, view.cursor_x)
        };
        // The first report of a generation is taken, however old.
        send(&mut host, cursor(10, 0), None).await;
        eventually(|| seen().1 == 10).await;
        let first = seen().0;
        viewer.input(1, &[Input::Move { x: 20, y: 0 }]).unwrap();
        let ViewerMessage::Input { event } =
            until(&mut host, |m| matches!(m, ViewerMessage::Input { .. })).await
        else {
            unreachable!()
        };
        send(&mut host, cursor(30, event.sequence - 1), None).await;
        send(&mut host, cursor(40, event.sequence), None).await;
        eventually(|| seen().1 == 40).await;
        assert_eq!(seen().0, first + 1, "the older report was taken");

        viewer.request_control(false).unwrap();
        send(&mut host, cursor(50, 0), None).await;
        eventually(|| seen().1 == 50).await;
        drop(host);
        viewer.stop().await.unwrap();
    }

    /// Dart reconnects after 5 s without video while streaming: neither
    /// unavailable capture nor a pause may count, however long, before the
    /// host's IDR arrives.
    #[tokio::test]
    async fn time_without_video_counts_only_while_streaming() {
        let (viewer, mut host, _) = connect(accepted()).await;
        let viewer = viewer.unwrap();
        send(&mut host, state(1, false), None).await;
        send(&mut host, video(1, 1, 1, true), Some(&[1; 64])).await;
        until(&mut host, |m| {
            matches!(m, ViewerMessage::Received { seq: 1 })
        })
        .await;
        let age_within = |due: Instant| {
            let status = viewer.status();
            assert_eq!(status.state, ViewerState::Streaming);
            assert!(status.last_video_age_ms.unwrap() <= millis(due.elapsed()));
        };

        let HostMessage::State(streaming) = state(1, false) else {
            unreachable!()
        };
        let unavailable = StateRecord {
            state: SessionState::CaptureUnavailable,
            ..streaming
        };
        send(&mut host, HostMessage::State(unavailable), None).await;
        eventually(|| viewer.status().state == ViewerState::CaptureUnavailable).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        let due = Instant::now();
        send(&mut host, state(2, false), None).await;
        eventually(|| viewer.status().state == ViewerState::Streaming).await;
        age_within(due);

        viewer.pause();
        tokio::time::sleep(Duration::from_millis(200)).await;
        let due = Instant::now();
        viewer.resume();
        age_within(due);
        drop(host);
        viewer.stop().await.unwrap();
    }

    /// The host's figures reach the status the quality sheet reads: its
    /// skipped ticks as a rate, what it sent, its encoder dwell and receipt
    /// queue, beside the preset and ceiling the session asked for.
    #[tokio::test]
    async fn host_stats_reach_the_viewer_status() {
        let (viewer, mut host, _) = connect(accepted()).await;
        let viewer = viewer.unwrap();
        let stats = |skipped_ticks, keyframes| HostMessage::Stats {
            capture_fps: 59.5,
            pace_fps: 60,
            target_fps: 60,
            fps_cap: 0,
            bitrate: 4_000_000,
            encoder_bitrate: 4_000_000,
            sent_bitrate: 3_900_000,
            rtt_ms: Some(40),
            base_rtt_ms: Some(38),
            queue_delay_ms: Some(2),
            ack_queue_ms: 12,
            encoder_dwell_ms: Some(3),
            dropped_frames: 0,
            skipped_ticks,
            keyframes,
            frames: 60,
            bytes: 500_000,
            adaptation: crate::Adaptation::Steady,
            rate_model: crate::RateModel::Nominal,
            gop: u32::MAX,
        };
        let keyframes = || viewer.status().stats.keyframes;
        send(&mut host, stats(10, 1), None).await;
        eventually(|| keyframes() == 1).await;
        // 30 more right behind it, as a link that held the first delivers
        // them: the host sent the two at least `STATS_INTERVAL` apart.
        send(&mut host, stats(40, 2), None).await;
        eventually(|| keyframes() == 2).await;
        let json = serde_json::to_value(viewer.status().stats).unwrap();
        let skipped = json["skipped_per_s"].as_f64().unwrap();
        assert!((15.0..=30.0).contains(&skipped), "{skipped}");
        assert_eq!(json["preset"], "balanced");
        assert_eq!(json["max_fps"], 60);
        assert_eq!(json["sent_bitrate"], 3_900_000);
        assert_eq!(json["encoder_dwell_ms"], 3);
        assert_eq!(json["ack_queue_ms"], 12);
        viewer.set_quality(QualityPreset::Smooth);
        assert_eq!(viewer.status().stats.preset, QualityPreset::Smooth);
        drop(host);
        viewer.stop().await.unwrap();
    }

    /// An IDR slower to cross than the hold after a new generation: its
    /// header answers the hold, so nothing asks for another while its payload
    /// is still in transit.
    #[tokio::test]
    async fn an_idr_slower_than_the_hold_is_not_asked_for_again() {
        let (viewer, mut host, _) = connect(accepted()).await;
        let viewer = viewer.unwrap();
        send(&mut host, state(1, false), None).await;
        // The header now, its payload record only once the hold has expired.
        send(&mut host, video(1, 1, 1, true), None).await;
        tokio::time::sleep(recovery::HOLD_AFTER_DISCONTINUITY + 3 * VIEWER_TICK).await;
        host.write_u32(64).await.unwrap();
        host.write_all(&[1; 64]).await.unwrap();
        until(&mut host, |m| {
            matches!(m, ViewerMessage::Received { seq: 1 })
        })
        .await;
        assert_eq!(viewer.status().stats.keyframes_requested, 0);
        drop(host);
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
        // gives up, and the app is told why, in the decoder's words.
        let error = "Create H.264 decoder (0x80040154)";
        assert!(consumer.failed(error));
        for _ in 0..4 {
            assert!(!consumer.failed(error));
        }
        eventually(|| viewer.status().state == ViewerState::Stopped).await;
        let status = viewer.status();
        assert_eq!(status.reason, Some(ViewerReason::DecoderFailed));
        assert!(status.message.contains("0x80040154"), "{}", status.message);
        assert_eq!(status.decoder.state, DecoderState::Failed);
        assert!(matches!(
            consumer.next(Duration::ZERO, 1 << 20),
            Pull::Closed
        ));
        drop(host);
        viewer.stop().await.unwrap();
    }
}
