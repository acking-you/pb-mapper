//! The viewer's network session: `handshake` authenticates, then `run_leg`
//! races the reader against the writer on one connection until one of them
//! ends. The session runs on one connection at a time, its live leg; a
//! standby's leg runs beside it, reading and writing only what keeps it
//! alive, until it takes the live leg's place (`standby`).
use super::{
    ConnectError, CursorSlot, Notice, VideoSink, VideoUnit, ViewerConfig, ViewerLink, ViewerReason,
    ViewerShared,
    outbound::{Next, Outbound},
    stats::Traffic,
};
use crate::{
    Input, QualityPreset, VideoCodec, VideoLimits,
    modes::MAX_SIDE,
    protocol::{HostMessage, RejectReason, Route, SessionState, ViewerMessage, valid_capabilities},
    wire::{self, WireError},
};
use parking_lot::Mutex;
use pb_mapper_client::sdk::{Connection, RelayTransport, TunnelMetrics};
use std::{
    io,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicU8, AtomicU64, Ordering},
    },
    task::{Context, Poll},
    time::{Duration, Instant},
};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio_util::sync::CancellationToken;

/// The host answers `Authenticate` within this.
const ACCEPT_TIMEOUT: Duration = Duration::from_secs(5);
/// The host writes `Stats` every second, so silence this long is a dead link.
pub(super) const VIEWER_HEADER_TIMEOUT: Duration = Duration::from_secs(4);
/// The viewer tick: fps cap, path and stats.
pub(super) const VIEWER_TICK: Duration = Duration::from_millis(250);
/// Ticks per path re-sample and stats roll-up.
const TICKS_PER_SECOND: u64 = 4;
/// Why a session whose decoder failed for good ended.
const DECODER_FAILED: &str = "the remote picture could not be decoded";
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
#[derive(Clone)]
pub(super) struct Accepted {
    pub(super) codec: VideoCodec,
    pub(super) quality: QualityPreset,
    pub(super) capabilities: Vec<String>,
    /// What a standby of this session presents, from a host that gave one.
    pub(super) ticket: Option<String>,
}
impl Accepted {
    pub(super) fn listed(&self, wanted: &str) -> bool {
        self.capabilities
            .iter()
            .any(|capability| capability == wanted)
    }
}

/// `Authenticate`, then the host's answer: for a standby of the session
/// whose ticket is `standby_for`, when there is one.
pub(super) async fn handshake<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
    token: String,
    config: &ViewerConfig,
    standby_for: Option<String>,
) -> std::result::Result<Accepted, ConnectError> {
    let authenticate = ViewerMessage::Authenticate {
        token,
        codecs: config.wire_codecs(),
        limits: config.limits,
        quality: config.quality,
        capabilities: config.capabilities(),
        standby_for,
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
            ticket,
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
                ticket,
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
            | HostMessage::CursorShape(_)
            | HostMessage::PeerCursor { .. }
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

/// How a leg ended.
pub(super) enum Ended {
    /// This end closed it: `Viewer::stop`, or with a reason, a decoder that
    /// cannot recover.
    Local(Option<(ViewerReason, String)>),
    /// The host said why, in `Stopped`.
    Host(ViewerReason, String),
    /// The connection failed, closed or went silent.
    Transport(ViewerReason, String),
    /// The host broke the protocol.
    Protocol(String),
}
impl Ended {
    fn failed(failure: WireError) -> Self {
        let reason = match &failure {
            // A timeout first, the transport's own included.
            WireError::TimedOut => ViewerReason::Timeout,
            WireError::Io(failure) if failure.kind() == io::ErrorKind::TimedOut => {
                ViewerReason::Timeout
            }
            WireError::Framing(framing) => return Self::Protocol((*framing).into()),
            WireError::Io(_) => ViewerReason::Transport,
        };
        Self::Transport(reason, failure.to_string())
    }
    fn protocol(message: &str) -> Self {
        Self::Protocol(message.into())
    }
    /// Why the session stops with this end; no reason: stopped here.
    pub(super) fn into_stop(self) -> (Option<ViewerReason>, String) {
        match self {
            Self::Local(failure) => failure.map_or((None, String::new()), |(reason, message)| {
                (Some(reason), message)
            }),
            Self::Host(reason, message) | Self::Transport(reason, message) => {
                (Some(reason), message)
            }
            Self::Protocol(message) => (
                Some(ViewerReason::Protocol),
                format!("desktop protocol error: {message}"),
            ),
        }
    }
}

/// What a leg does with what it reads and what it writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
enum Role {
    /// It keeps a standby's session alive: probes, its path and pings.
    Standby,
    /// It carries the session the app sees.
    Live,
    /// A standby took its place: it reads and drops what still comes until
    /// its host session ends, which the promotion does, and writes only
    /// pings.
    Retired,
}

/// One connection to the host: the live session's, or a standby's, which
/// stands by until it takes the live one's place.
pub(super) struct Leg {
    /// A `Role`: a standby's turns live once, and a live one retired once.
    role: AtomicU8,
    /// When its session began: what its arrival times and read times count
    /// from.
    pub(super) epoch: Instant,
    /// When it last read a byte, and the end of a whole message: in
    /// microseconds since `epoch`.
    read_at: AtomicU64,
    message_at: AtomicU64,
    /// What a standby writes while it stands by: its probe echoes, its path
    /// and pings.
    outbound: Outbound,
    /// The path it reported last.
    route: Mutex<Option<Route>>,
    /// The tunnel it rides on; `None` only over a test stream.
    pub(super) connection: Option<Arc<Connection>>,
}
impl Leg {
    /// The session's first leg, live from the start.
    pub(super) fn live(connection: Option<Arc<Connection>>) -> Arc<Self> {
        Self::new(Role::Live, Instant::now(), None, false, connection)
    }
    /// A standby's leg, whose session began at `epoch`, stamping arrival
    /// times from then for a host that, by `arrival_time`, reads them.
    pub(super) fn standby(
        epoch: Instant,
        arrival_time: bool,
        keyframe_seq: bool,
        connection: Option<Arc<Connection>>,
    ) -> Arc<Self> {
        Self::new(
            Role::Standby,
            epoch,
            arrival_time.then_some(epoch),
            keyframe_seq,
            connection,
        )
    }
    fn new(
        role: Role,
        epoch: Instant,
        arrival_epoch: Option<Instant>,
        keyframe_seq: bool,
        connection: Option<Arc<Connection>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            role: AtomicU8::new(role as u8),
            epoch,
            read_at: AtomicU64::new(0),
            message_at: AtomicU64::new(0),
            outbound: Outbound::new(arrival_epoch, keyframe_seq),
            route: Mutex::new(None),
            connection,
        })
    }
    fn role(&self) -> Role {
        match self.role.load(Ordering::Acquire) {
            0 => Role::Standby,
            1 => Role::Live,
            _ => Role::Retired,
        }
    }
    pub(super) fn is_live(&self) -> bool {
        self.role() == Role::Live
    }
    /// The standby takes the session: from its next message on, its reader
    /// and writer serve the app's, and a probe it took as a standby is
    /// answered on the session's outbound.
    pub(super) fn go_live(&self, session: &Outbound) {
        self.role.store(Role::Live as u8, Ordering::Release);
        session.adopt_probe(&self.outbound);
        self.outbound.wake();
    }
    /// A standby took this leg's place: from its next message on it touches
    /// nothing of the session's, until its host session ends.
    pub(super) fn retire(&self) {
        self.role.store(Role::Retired as u8, Ordering::Release);
        self.outbound.wake();
    }
    /// The path this leg reported last, if any.
    pub(super) fn route(&self) -> Option<Route> {
        *self.route.lock()
    }
    fn stamp(&self, at: &AtomicU64) {
        let elapsed = pb_mapper_core::time::saturating_micros(self.epoch.elapsed());
        at.store(elapsed, Ordering::Release);
    }
    fn since(&self, at: &AtomicU64, now: Instant) -> Duration {
        let then = self.epoch + Duration::from_micros(at.load(Ordering::Acquire));
        now.saturating_duration_since(then)
    }
    /// How long since it last read a byte, or since its session began.
    pub(super) fn silent_for(&self, now: Instant) -> Duration {
        self.since(&self.read_at, now)
    }
    /// How long since it last read a whole message, or since its session
    /// began.
    pub(super) fn quiet_for(&self, now: Instant) -> Duration {
        self.since(&self.message_at, now)
    }
}

/// A leg's read half, noting when it read a byte.
struct Stamped<'a, R> {
    inner: R,
    leg: &'a Leg,
}
impl<R: AsyncRead + Unpin> AsyncRead for Stamped<'_, R> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let before = buf.filled().len();
        let polled = Pin::new(&mut self.inner).poll_read(cx, buf);
        if matches!(polled, Poll::Ready(Ok(()))) && buf.filled().len() > before {
            self.leg.stamp(&self.leg.read_at);
        }
        polled
    }
}

/// What every leg of a session reads and writes into.
pub(super) struct Shared {
    pub(super) shared: Arc<ViewerShared>,
    pub(super) sink: Arc<VideoSink>,
    pub(super) cancel: CancellationToken,
    pub(super) codec: VideoCodec,
    pub(super) limits: VideoLimits,
    /// The host listed `cursor_shape`, as this viewer always does.
    pub(super) cursor_shapes: bool,
    /// Both ends listed `peer_cursors`.
    pub(super) peer_cursors: bool,
}

/// Race one leg's reader against its writer; the first to end ends the leg.
/// A leg this end closed keeps its read side until the host consumed the
/// close and closed its own, or the grace expires.
pub(super) async fn run_leg<S: AsyncRead + AsyncWrite + Send + 'static>(
    stream: S,
    leg: Arc<Leg>,
    session: Arc<Shared>,
) -> Ended {
    let Shared {
        shared,
        sink,
        cancel,
        codec,
        limits,
        cursor_shapes,
        peer_cursors,
    } = &*session;
    let (read, mut write) = tokio::io::split(stream);
    let mut read = Stamped {
        inner: read,
        leg: &leg,
    };
    let mut reader = Reader {
        shared,
        sink,
        leg: &leg,
        codec: *codec,
        limits: *limits,
        cursor_shapes: *cursor_shapes,
        peer_cursors: *peer_cursors,
        last_seq: 0,
        generation: 0,
    };
    let mut writer = Writer {
        shared,
        sink,
        leg: &leg,
        ticks: 0,
    };
    let ended = tokio::select! {
        biased;
        ended = writer.run(&mut write, cancel) => ended,
        ended = reader.run(&mut read) => ended,
    };
    if let Ended::Local(failure) = &ended {
        // A failure shows at once; the drain below may take a while.
        if let Some((reason, message)) = failure.clone()
            && leg.is_live()
        {
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
    }
    ended
}

struct Reader<'a> {
    shared: &'a ViewerShared,
    sink: &'a VideoSink,
    leg: &'a Leg,
    codec: VideoCodec,
    limits: VideoLimits,
    /// Both ends listed `cursor_shape`: shapes may come, and no other host
    /// may send one.
    cursor_shapes: bool,
    /// Both ends listed `peer_cursors`: peer cursors may come, and no other
    /// host may send one.
    peer_cursors: bool,
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
                        return Ended::Transport(
                            ViewerReason::Transport,
                            "the remote computer closed the connection".into(),
                        );
                    }
                    Err(failure) => return Ended::failed(failure),
                };
            // The header is read, or, when the reader came late to it, the
            // previous message is done: a probe's arrival time.
            let at = Instant::now();
            let handled = match self.leg.role() {
                Role::Live => self.handle(message, read, at).await,
                Role::Standby => self.stand_by(message, at),
                Role::Retired => Self::retired(message, read).await,
            };
            if let Err(ended) = handled {
                return ended;
            }
            self.leg.stamp(&self.leg.message_at);
        }
    }

    /// A standby's message, read while it stands by: its host session is
    /// paused and holds no slot, so it sends no picture and no pointer.
    /// Probes are answered on the leg's own outbound; the rest only shows
    /// the connection lives.
    fn stand_by(&mut self, message: HostMessage, at: Instant) -> std::result::Result<(), Ended> {
        match message {
            HostMessage::Probe { nonce } => {
                self.leg.outbound.probe(nonce, at);
                // The leg went live meanwhile: the session's outbound answers.
                if self.leg.is_live() {
                    self.shared.outbound.adopt_probe(&self.leg.outbound);
                }
            }
            HostMessage::State(host) => self.generation = host.generation,
            HostMessage::Stats(_) | HostMessage::Notice { .. } => {}
            HostMessage::Stopped { reason, message } => {
                return Err(Ended::Host(reason.into(), message));
            }
            HostMessage::Video { .. }
            | HostMessage::Cursor { .. }
            | HostMessage::CursorShape(_)
            | HostMessage::PeerCursor { .. } => {
                return Err(Ended::protocol("a picture or pointer on a standby"));
            }
            HostMessage::Accepted { .. } | HostMessage::Rejected { .. } => {
                return Err(Ended::protocol("a handshake answer after the handshake"));
            }
        }
        Ok(())
    }

    /// A retired leg's message: its payload read and dropped. Its host
    /// session ends it, with or without a word.
    async fn retired<R: AsyncRead + Unpin>(
        message: HostMessage,
        read: &mut R,
    ) -> std::result::Result<(), Ended> {
        match message {
            HostMessage::Video { .. } => {
                wire::read_payload(read).await.map_err(Ended::failed)?;
            }
            HostMessage::CursorShape(record) => {
                wire::read_payload_of(read, record.payload_len())
                    .await
                    .map_err(Ended::failed)?;
            }
            HostMessage::Stopped { reason, message } => {
                return Err(Ended::Host(reason.into(), message));
            }
            HostMessage::State(_)
            | HostMessage::Cursor { .. }
            | HostMessage::PeerCursor { .. }
            | HostMessage::Probe { .. }
            | HostMessage::Stats(_)
            | HostMessage::Notice { .. }
            | HostMessage::Accepted { .. }
            | HostMessage::Rejected { .. } => {}
        }
        Ok(())
    }

    /// `at`: when `message`'s header was read. A unit's arrival time is
    /// when its payload was, and only a probe's is `at`.
    async fn handle<R: AsyncRead + Unpin>(
        &mut self,
        message: HostMessage,
        read: &mut R,
        at: Instant,
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
                // the ones the sink drops. Its arrival time is now, the unit
                // read whole: the way toward this viewer holds the unit's own
                // transfer, as the host's round trips always did, and the way
                // back holds nothing but the way back.
                self.shared.outbound.receipt(seq, now);
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
            HostMessage::Probe { nonce } => self.shared.outbound.probe(nonce, at),
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
                shape,
            } => {
                // The shape of every report, whatever its position: a move
                // of this viewer's never overtook a shape.
                let named = match shape {
                    Some(_) if !self.cursor_shapes => {
                        return Err(Ended::protocol(
                            "a cursor shape this viewer did not ask for",
                        ));
                    }
                    Some(slot) => match self.shared.shapes.lock().number(slot) {
                        Some(number) => Some((slot, number)),
                        None => return Err(Ended::protocol("a cursor shape never defined")),
                    },
                    None => None,
                };
                let controlling = self.shared.state.lock().controlling();
                let mut cursor = self.shared.cursor.lock();
                if let Some((slot, number)) = named {
                    cursor.slot = Some(slot);
                    cursor.shape = number;
                }
                if generation == self.generation
                    && !cursor.superseded(generation, input_sequence, controlling)
                {
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
            HostMessage::CursorShape(record) => {
                if !self.cursor_shapes {
                    return Err(Ended::protocol(
                        "a cursor shape this viewer did not ask for",
                    ));
                }
                let rgba = wire::read_payload_of(read, record.payload_len())
                    .await
                    .map_err(Ended::failed)?;
                let shape = record
                    .shape(&rgba)
                    .map_err(|_| Ended::protocol("cursor shape out of bounds"))?;
                let number = self.shared.shapes.lock().define(record.id, shape);
                let mut cursor = self.shared.cursor.lock();
                if cursor.slot == Some(record.id) {
                    cursor.shape = number;
                }
            }
            HostMessage::PeerCursor {
                generation,
                peer,
                color,
                x,
                y,
                visible,
            } => {
                if !self.peer_cursors {
                    return Err(Ended::protocol("a peer cursor this viewer did not ask for"));
                }
                if generation == self.generation {
                    let mut peers = self.shared.peer_cursors.lock();
                    peers.peers[peer.index()] = super::PeerCursorEntry {
                        generation,
                        color: color.as_u8(),
                        x,
                        y,
                        visible,
                    };
                    peers.seq += 1;
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
                return Err(Ended::Host(reason.into(), message));
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
    leg: &'a Leg,
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
            // A standby writes its own outbound until it goes live.
            let live = self.leg.is_live();
            let outbound = if live {
                &self.shared.outbound
            } else {
                &self.leg.outbound
            };
            if now >= next_tick && !live {
                if self.leg.role() == Role::Standby {
                    self.stand_by();
                }
                next_tick = now + VIEWER_TICK;
            }
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
            match outbound.next(now, last_write) {
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
                        // Going live wakes a standby's writer here.
                        () = outbound.notified() => {}
                        () = tokio::time::sleep_until(wake) => {}
                    }
                }
            }
        }
    }

    /// A standby's tick: once a second, its own path, reported when it
    /// changes, as the live leg's tick does.
    fn stand_by(&mut self) {
        let second = self.ticks.is_multiple_of(TICKS_PER_SECOND);
        self.ticks += 1;
        let Some(metrics) = self
            .leg
            .connection
            .as_deref()
            .filter(|_| second)
            .map(Connection::metrics)
        else {
            return;
        };
        let (route, _) = route(&metrics);
        if self.leg.route.lock().replace(route) != Some(route) {
            self.leg.outbound.route(route);
        }
    }

    /// Every `VIEWER_TICK`: the fps cap, keyframe retries and the stall
    /// watchdog; once a second also the path, the tunnel's traffic and the
    /// stats roll-up. Why the decoder failed, once it failed for good.
    fn tick(&mut self, now: Instant) -> Option<String> {
        let second = self.ticks.is_multiple_of(TICKS_PER_SECOND);
        self.ticks += 1;
        let tunnel = self
            .leg
            .connection
            .as_deref()
            .filter(|_| second)
            .map(Connection::metrics);
        if let Some(metrics) = &tunnel {
            let (route, link) = route(metrics);
            *self.leg.route.lock() = Some(route);
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
pub(super) fn route(metrics: &TunnelMetrics) -> (Route, Option<ViewerLink>) {
    if metrics.sessions.active_direct > 0 {
        (Route::Direct, metrics.link.as_ref().map(ViewerLink::from))
    } else if metrics.relay.transport == RelayTransport::Quic {
        (Route::RelayQuic, None)
    } else {
        (Route::RelayTcp, None)
    }
}
