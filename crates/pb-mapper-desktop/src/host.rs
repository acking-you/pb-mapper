use crate::{
    DesktopKind, Display, HostPath, HostState, HostStatus, HostViewer, Invitation, QualityPreset,
    Result, VideoCodec, VideoLimits, VideoSize, error, platform,
    protocol::{
        CAPABILITY_H265, CAPABILITY_LOCK, HostMessage, RejectReason, SessionState, StopReason,
        ViewerMessage, valid_capabilities,
    },
    qos::scaled_size,
    session::{self, Session, stop_message},
    shared::Policy,
    wire,
    worker::{self, Factory, WorkerConfig},
};
use parking_lot::Mutex;
use pb_mapper_client::sdk::{
    Client, RegisterOptions, RegisterRequest, Registration, Transport, TunnelMetrics,
    TunnelMetricsStream,
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::AsyncWriteExt,
    net::{TcpListener, TcpStream},
    sync::{Mutex as AsyncMutex, OwnedSemaphorePermit, Semaphore, oneshot},
    task::JoinSet,
};
use tokio_rustls::{TlsAcceptor, server::TlsStream};
use tokio_util::sync::CancellationToken;

/// The TLS handshake and the viewer's `Authenticate` must arrive within this.
pub(crate) const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(3);
/// A `Rejected` and the close after it.
pub(crate) const REJECT_WRITE_TIMEOUT: Duration = Duration::from_secs(1);
/// How long `Accepted` waits for a new backend to say which display it
/// captures and, for a viewer that decodes H.265, whether it encodes H.265;
/// the viewer waits 5 s for `Accepted`. A slower answer leaves that session on
/// H.264, its first IDR budgeted for the viewer's bounds.
const BACKEND_ANSWER_WAIT: Duration = Duration::from_secs(3);
/// Connections in their handshake or session at once; one holds the viewer slot.
const MAX_CONNECTIONS: usize = 8;

pub struct HostOptions {
    pub service: String,
    pub display_id: u32,
    pub allow_control: bool,
    pub allow_direct: bool,
    /// The registration's [`RegisterOptions::lan_first`]: probe a viewer's
    /// addresses on a shared local network first. Which path a session
    /// takes is the viewer's choice, under its own `ConnectOptions`.
    pub lan_first: bool,
}

pub struct Host {
    invitation: Invitation,
    policy: Arc<Policy>,
    capabilities: Vec<String>,
    /// The session holding the single viewer slot, if any.
    session: Arc<Mutex<Option<Arc<Session>>>>,
    stopped: AtomicBool,
    cancel: CancellationToken,
    registration: Registration,
    join: AsyncMutex<Option<tokio::task::JoinHandle<()>>>,
}
impl Host {
    pub async fn start(client: &Client, options: HostOptions) -> Result<Self> {
        let factory = native_factory(options.display_id)?;
        Self::start_with_backend(client, options, factory).await
    }
    pub async fn start_with_backend(
        client: &Client,
        options: HostOptions,
        factory: Factory,
    ) -> Result<Self> {
        Self::start_with_admission(client, options, factory, None).await
    }
    pub async fn start_with_admission(
        client: &Client,
        options: HostOptions,
        factory: Factory,
        admission: Option<Arc<dyn crate::ControlAdmission>>,
    ) -> Result<Self> {
        Self::start_with_backend_and_identity(
            client,
            options,
            factory,
            admission,
            crate::HostIdentity::generate()?,
        )
        .await
    }
    /// Resume a previously and explicitly authorized grant. Dropping or stopping
    /// this host closes sessions; revoke durable access by discarding the saved
    /// identity and generating a new one before the next start.
    pub async fn start_with_identity(
        client: &Client,
        options: HostOptions,
        identity: crate::HostIdentity,
    ) -> Result<Self> {
        let factory = native_factory(options.display_id)?;
        Self::start_with_backend_and_identity(client, options, factory, None, identity).await
    }
    pub async fn start_with_backend_and_identity(
        client: &Client,
        options: HostOptions,
        factory: Factory,
        admission: Option<Arc<dyn crate::ControlAdmission>>,
        identity: crate::HostIdentity,
    ) -> Result<Self> {
        if !crate::valid_service(&options.service) {
            return Err(error(
                "desktop service must start with desktop: and contain at most 128 ASCII letters, numbers, periods, hyphens, underscores or colons",
            ));
        }
        let tls = identity.acceptor()?;
        let invitation = identity.invitation(options.service.clone())?;
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let registration = client
            .register_with_options(
                RegisterRequest {
                    key: options.service,
                    local_addr: listener.local_addr()?.to_string(),
                    transport: Transport::Tcp,
                    codec: true,
                    force_namespace: false,
                },
                RegisterOptions {
                    allow_direct: options.allow_direct,
                    lan_first: options.lan_first,
                    ..Default::default()
                },
            )
            .await
            .map_err(error)?;
        if let Err(failure) = registration
            .wait_ready_timeout(Duration::from_secs(15))
            .await
        {
            let diagnostic = registration.diagnostics();
            let relay = client.relay_diagnostics().await;
            return Err(error(format!(
                "Desktop registration failed: {failure}; carrier={:?}, phase={}, failure={}, attempts={}{}",
                diagnostic.relay_transport,
                diagnostic.last_attempt_phase.as_str(),
                diagnostic
                    .last_failure
                    .map_or("none", |reason| reason.as_str()),
                diagnostic.attempts,
                relay
                    .last_error
                    .map_or(String::new(), |detail| format!("; {detail}")),
            )));
        }
        let policy = Arc::new(Policy::new(options.allow_control));
        let session = Arc::new(Mutex::new(None));
        let cancel = CancellationToken::new();
        let acceptor = Arc::new(Acceptor {
            tls,
            token: invitation.token_text(),
            factory,
            admission,
            policy: policy.clone(),
            capabilities: capabilities(),
            session: session.clone(),
            slot: Arc::new(Semaphore::new(1)),
            cancel: cancel.clone(),
        });
        let join = tokio::spawn(acceptor.accept(listener));
        Ok(Self {
            invitation,
            policy,
            capabilities: capabilities(),
            session,
            stopped: AtomicBool::new(false),
            cancel,
            registration,
            join: AsyncMutex::new(Some(join)),
        })
    }
    /// The `pbd1` invitation to this host.
    pub fn invitation(&self) -> &Invitation {
        &self.invitation
    }
    /// The host's state and its viewer, if one is connected: when its session
    /// started, whether it runs direct or through the relay, and whether it
    /// controls, has paused, and which quality it chose.
    pub fn status(&self) -> HostStatus {
        let control_allowed = self.policy.allowed();
        let capabilities = self.capabilities.clone();
        let Some(session) = self.session.lock().clone() else {
            return HostStatus {
                state: if self.stopped.load(Ordering::Acquire) {
                    HostState::Stopped
                } else {
                    HostState::Waiting
                },
                reason: None,
                message: String::new(),
                control_allowed,
                viewer: None,
                display: None,
                desktop: DesktopKind::Ordinary,
                capabilities,
            };
        };
        let record = session.shared.state.borrow().record.clone();
        let paused = session.paused();
        HostStatus {
            state: match record.state {
                _ if paused => HostState::Paused,
                SessionState::Connecting => HostState::Connecting,
                SessionState::Streaming => HostState::Streaming,
                SessionState::Paused => HostState::Paused,
                SessionState::CaptureUnavailable => HostState::CaptureUnavailable,
            },
            reason: record.reason,
            message: record.message,
            control_allowed,
            viewer: Some(HostViewer {
                since_ms: u64::try_from(session.shared.epoch.elapsed().as_millis())
                    .unwrap_or(u64::MAX),
                started_unix_ms: session.started_unix_ms,
                path: self.path(),
                controlling: control_allowed && record.controlling,
                paused,
                quality: session.quality(),
            }),
            display: record.display,
            desktop: record.desktop,
            capabilities,
        }
    }
    /// The host's tunnel figures, sampled now: its viewers' sessions on each
    /// path, the bytes they carried, the relay's health and the direct link.
    pub fn metrics(&self) -> TunnelMetrics {
        self.registration.metrics()
    }
    /// [`Self::metrics`] at once, then every `every` and as the tunnel
    /// changes, until hosting stops: see [`TunnelMetricsStream`].
    pub fn subscribe_metrics(&self, every: Duration) -> TunnelMetricsStream {
        self.registration.subscribe_metrics(every)
    }
    /// The one viewer's path, sampled from the registration's sessions,
    /// which it counts on the relay as well as direct.
    fn path(&self) -> HostPath {
        let sessions = self.registration.metrics().sessions;
        if sessions.active_direct > 0 {
            HostPath::Direct
        } else if sessions.active_relay > 0 {
            HostPath::Relay
        } else {
            HostPath::Unknown
        }
    }
    /// "Disconnect": end the connected viewer's session; it hears
    /// `Stopped{host_ended}`. Hosting continues, and the viewer may connect
    /// again with the same invitation.
    pub fn end_session(&self) {
        if let Some(session) = self.session.lock().as_ref() {
            session.end(StopReason::HostEnded, stop_message(StopReason::HostEnded));
        }
    }
    /// "Make view-only" and back. Revocation gates every later input at the
    /// edge, and the worker releases held input within one backend step.
    pub fn set_control_allowed(&self, allowed: bool) {
        self.policy.set(allowed);
        if let Some(session) = self.session.lock().as_ref() {
            session.shared.wake.notify();
        }
    }
    /// Stop hosting; a connected viewer hears `Stopped{host_stopped}`.
    pub async fn stop(&self) -> Result<()> {
        self.stop_with(StopReason::HostStopped).await
    }
    /// Stop hosting, telling a connected viewer why: `Restarting` when sharing
    /// resumes at once with the same identity (a display switch), so it
    /// reconnects; `Revoked` when its access was withdrawn.
    pub async fn stop_with(&self, reason: StopReason) -> Result<()> {
        self.policy.set(false);
        if let Some(session) = self.session.lock().as_ref() {
            session.end(reason, stop_message(reason));
        }
        self.cancel.cancel();
        let mut join = self.join.lock().await;
        if let Some(task) = join.as_mut() {
            let _ = task.await;
        }
        join.take();
        self.registration.stop().await.map_err(error)?;
        self.stopped.store(true, Ordering::Release);
        Ok(())
    }
}
impl Drop for Host {
    fn drop(&mut self) {
        self.policy.set(false);
        self.cancel.cancel();
    }
}

/// The platform's own capture and encoder for `display_id`. Refused before
/// the host registers where there are none, so that no service is published
/// only to fail every viewer at its encoder.
fn native_factory(display_id: u32) -> Result<Factory> {
    if !platform::capabilities().host {
        return Err(error(if cfg!(windows) {
            "Desktop hosting requires Windows Media Foundation"
        } else {
            "Desktop hosting currently requires Windows"
        }));
    }
    Ok(Arc::new(move || platform::native_backend(display_id)))
}

/// What every Windows backend can do: the native desktop and the helper both
/// lock the workstation.
fn capabilities() -> Vec<String> {
    if cfg!(windows) {
        vec![CAPABILITY_LOCK.into()]
    } else {
        Vec::new()
    }
}

/// What the viewer asked for in a valid `Authenticate`.
struct Request {
    /// One or two, never `h265`.
    codecs: Vec<VideoCodec>,
    limits: VideoLimits,
    quality: QualityPreset,
    /// What the viewer listed; values this host does not know are ignored.
    capabilities: Vec<String>,
}

/// A viewer that holds the slot and heard `Accepted`, with its thread C
/// waiting for the session.
struct Admitted {
    stream: TlsStream<TcpStream>,
    permit: OwnedSemaphorePermit,
    request: Request,
    codec: VideoCodec,
    /// The size the encoder is expected to produce.
    video: VideoSize,
    prepared: worker::Prepared,
}

/// A session's codec: H.265 when the viewer decodes it and the backend
/// encodes it (`h265`), else as every v2 host chooses: H.264 when listed,
/// else JPEG.
fn choose_codec(codecs: &[VideoCodec], h265: bool) -> VideoCodec {
    if h265 {
        VideoCodec::H265
    } else if codecs.contains(&VideoCodec::H264) {
        VideoCodec::H264
    } else {
        VideoCodec::Jpeg
    }
}

/// `factory`, telling the receiver which display the backend it builds
/// captures, read on thread C as soon as it is built, before `prepare` asks it
/// about H.265. The receiver hears nothing when the backend or its display
/// fails.
fn reporting_display(factory: &Factory) -> (Factory, oneshot::Receiver<Display>) {
    let (report, display) = oneshot::channel();
    let report = Mutex::new(Some(report));
    let factory = factory.clone();
    let reporting: Factory = Arc::new(move || {
        let report = report.lock().take();
        let mut backend = factory()?;
        if let Some(report) = report
            && let Ok(captured) = backend.display()
        {
            let _ = report.send(captured);
        }
        Ok(backend)
    });
    (reporting, display)
}

/// The size the encoder makes of `display` for `limits`, or the bounds
/// themselves when the display is not known.
fn expected_video(display: Option<&Display>, limits: VideoLimits) -> VideoSize {
    let (width, height) = display.map_or((limits.max_width, limits.max_height), |display| {
        scaled_size(
            display.width,
            display.height,
            limits.max_width,
            limits.max_height,
        )
    });
    VideoSize { width, height }
}

/// The listener's side of the host: every connection's handshake and session.
struct Acceptor {
    tls: TlsAcceptor,
    token: String,
    factory: Factory,
    admission: Option<Arc<dyn crate::ControlAdmission>>,
    policy: Arc<Policy>,
    capabilities: Vec<String>,
    session: Arc<Mutex<Option<Arc<Session>>>>,
    /// The single viewer slot.
    slot: Arc<Semaphore>,
    cancel: CancellationToken,
}
impl Acceptor {
    async fn accept(self: Arc<Self>, listener: TcpListener) {
        let mut connections = JoinSet::new();
        loop {
            tokio::select! {
                () = self.cancel.cancelled() => break,
                Some(_) = connections.join_next(), if !connections.is_empty() => {}
                accepted = listener.accept(), if connections.len() < MAX_CONNECTIONS => {
                    let Ok((socket, _)) = accepted else { break };
                    connections.spawn(self.clone().serve(socket));
                }
            }
        }
        // Each session sees the cancel and ends with `Stopped`.
        while connections.join_next().await.is_some() {}
    }

    async fn serve(self: Arc<Self>, socket: TcpStream) {
        let accepted = tokio::select! {
            () = self.cancel.cancelled() => return,
            accepted = self.accept_session(socket) => accepted,
        };
        let Some(Admitted {
            stream,
            permit,
            request,
            codec,
            video,
            prepared,
        }) = accepted
        else {
            return;
        };
        tracing::info!(
            target: "pb_mapper_desktop::session",
            event = "desktop_host_accepted",
            codec = ?codec,
            max_width = request.limits.max_width,
            max_height = request.limits.max_height,
            max_fps = request.limits.max_fps,
            width = video.width,
            height = video.height,
            quality = ?request.quality,
        );
        let session = Session::new(
            self.policy.clone(),
            self.cancel.child_token(),
            codec,
            request.limits,
            video,
            request.quality,
            &request.capabilities,
        );
        *self.session.lock() = Some(session.clone());
        let mut worker = prepared.start(
            session.shared.clone(),
            WorkerConfig {
                codec,
                limits: request.limits,
                nominal_fps: request.limits.max_fps,
                admission: self.admission.clone(),
            },
        );
        session::run(
            session.clone(),
            stream,
            async move { worker.exited().await },
        )
        .await;
        let mut slot = self.session.lock();
        if slot
            .as_ref()
            .is_some_and(|held| Arc::ptr_eq(held, &session))
        {
            slot.take();
        }
        drop(slot);
        drop(permit);
    }

    /// The handshake, then `Accepted` with the session's codec. Thread C starts
    /// building the backend as soon as the viewer holds the slot, and
    /// `Accepted` waits until the backend said which display it captures,
    /// which sizes the first IDR's budget, and, for a viewer that decodes
    /// H.265, whether it encodes H.265, unless an H.265 encoder already
    /// failed in this process.
    async fn accept_session(&self, socket: TcpStream) -> Option<Admitted> {
        let (mut stream, permit, request) = self.handshake(socket).await?;
        let offered = request
            .capabilities
            .iter()
            .any(|capability| capability == CAPABILITY_H265);
        let (factory, display) = reporting_display(&self.factory);
        let mut prepared = worker::prepare(factory, offered && !worker::h265_failed());
        let deadline = tokio::time::Instant::now() + BACKEND_ANSWER_WAIT;
        let display = tokio::time::timeout_at(deadline, display)
            .await
            .ok()
            .and_then(|answer| answer.ok());
        let video = expected_video(display.as_ref(), request.limits);
        let h265 = prepared
            .h265(deadline.saturating_duration_since(tokio::time::Instant::now()))
            .await;
        let codec = choose_codec(&request.codecs, h265);
        let accepted = HostMessage::Accepted {
            codec,
            limits: request.limits,
            nominal_fps: request.limits.max_fps,
            quality: request.quality,
            capabilities: self.capabilities.clone(),
        };
        wire::write_message(&mut stream, &accepted, None)
            .await
            .ok()?;
        Some(Admitted {
            stream,
            permit,
            request,
            codec,
            video,
            prepared,
        })
    }

    /// TLS and `Authenticate`; `Rejected` when it fails. The token is checked
    /// first, so `busy` never tells a stranger that someone is connected.
    async fn handshake(
        &self,
        socket: TcpStream,
    ) -> Option<(TlsStream<TcpStream>, OwnedSemaphorePermit, Request)> {
        // State and small video units must not wait for Nagle/delayed-ACK
        // coalescing on the publisher's accepted loopback connection.
        socket.set_nodelay(true).ok()?;
        let deadline = tokio::time::Instant::now() + HANDSHAKE_TIMEOUT;
        let mut stream = tokio::time::timeout_at(deadline, self.tls.accept(socket))
            .await
            .ok()?
            .ok()?;
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let Ok(Some(ViewerMessage::Authenticate {
            token,
            codecs,
            limits,
            quality,
            capabilities,
        })) = wire::read_header(&mut stream, remaining).await
        else {
            return None;
        };
        let rejected = if !self.token_matches(&token) {
            RejectReason::Denied
        } else if !(1..=2).contains(&codecs.len())
            // H.265 is the `h265` capability, never a codec offered.
            || codecs.contains(&VideoCodec::H265)
            || limits.validate().is_err()
            || !valid_capabilities(&capabilities)
        {
            RejectReason::InvalidRequest
        } else if let Ok(permit) = self.slot.clone().try_acquire_owned() {
            return Some((
                stream,
                permit,
                Request {
                    codecs,
                    limits,
                    quality,
                    capabilities,
                },
            ));
        } else {
            RejectReason::Busy
        };
        tracing::info!(
            target: "pb_mapper_desktop::session",
            event = "desktop_host_rejected",
            reason = ?rejected,
        );
        let _ = tokio::time::timeout(REJECT_WRITE_TIMEOUT, async {
            let rejected = HostMessage::Rejected { reason: rejected };
            wire::write_message(&mut stream, &rejected, None).await?;
            stream.shutdown().await
        })
        .await;
        None
    }

    /// A constant-time comparison, over digests so the length leaks nothing.
    fn token_matches(&self, presented: &str) -> bool {
        use ring::digest::{SHA256, digest};
        use subtle::ConstantTimeEq;
        bool::from(
            digest(&SHA256, presented.as_bytes())
                .as_ref()
                .ct_eq(digest(&SHA256, self.token.as_bytes()).as_ref()),
        )
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod trust_tests {
    use super::*;
    use crate::{protocol::StateRecord, security};
    use pb_mapper_client::sdk::{ConnectOptions, Connection, PathPolicy};
    use std::sync::atomic::AtomicU32;
    use tokio_rustls::client::TlsStream as ClientStream;

    struct Screen;
    impl crate::Backend for Screen {
        fn display(&mut self) -> Result<crate::Display> {
            Ok(crate::Display {
                id: 1,
                name: "trusted".into(),
                x: 0,
                y: 0,
                width: 10,
                height: 10,
            })
        }
        fn capture(&mut self) -> Result<Vec<u8>> {
            Ok(vec![1])
        }
        fn input(&mut self, _: &crate::Input) -> Result<()> {
            Err(error("read-only test backend"))
        }
        fn release(&mut self) {}
    }
    fn options() -> HostOptions {
        HostOptions {
            service: "desktop:remembered".into(),
            display_id: 1,
            allow_control: false,
            allow_direct: false,
            lan_first: true,
        }
    }

    /// A scripted v2 viewer: its TLS stream, and the tunnel it rides on.
    struct Viewer {
        stream: ClientStream<TcpStream>,
        _connection: Connection,
    }

    /// A v2 viewer's TLS connection to `invitation`, pinned to its fingerprint.
    async fn tls(client: &Client, invitation: &Invitation) -> std::io::Result<Viewer> {
        let (socket, connection) = client
            .connect_tcp(
                invitation.service.clone(),
                ConnectOptions {
                    path: PathPolicy::RelayOnly,
                    ..Default::default()
                },
            )
            .await
            .map_err(error)?;
        let stream = security::client(invitation.fingerprint)?
            .connect(
                quinn::rustls::pki_types::ServerName::try_from(security::NAME).unwrap(),
                socket,
            )
            .await?;
        Ok(Viewer {
            stream,
            _connection: connection,
        })
    }

    /// Authenticate with `token`; returns the viewer and the host's answer.
    async fn authenticate(
        client: &Client,
        invitation: &Invitation,
        token: String,
        codecs: Vec<VideoCodec>,
    ) -> (Viewer, HostMessage) {
        authenticate_with(client, invitation, token, codecs, vec![]).await
    }

    /// `authenticate`, listing `capabilities`.
    async fn authenticate_with(
        client: &Client,
        invitation: &Invitation,
        token: String,
        codecs: Vec<VideoCodec>,
        capabilities: Vec<String>,
    ) -> (Viewer, HostMessage) {
        let mut viewer = tls(client, invitation).await.unwrap();
        let authenticate = ViewerMessage::Authenticate {
            token,
            codecs,
            limits: VideoLimits::for_viewer(60.0),
            quality: QualityPreset::Balanced,
            capabilities,
        };
        wire::write_message(&mut viewer.stream, &authenticate, None)
            .await
            .unwrap();
        let answer = wire::read_header(&mut viewer.stream, Duration::from_secs(5))
            .await
            .unwrap()
            .unwrap();
        (viewer, answer)
    }

    /// The next message of a kind other than probe or stats, past a video
    /// unit's payload record; `None` at a clean close.
    async fn next(viewer: &mut Viewer) -> Option<HostMessage> {
        loop {
            match wire::read_header(&mut viewer.stream, Duration::from_secs(5))
                .await
                .unwrap()?
            {
                HostMessage::Probe { .. } | HostMessage::Stats { .. } => {}
                message @ HostMessage::Video { .. } => {
                    wire::read_payload(&mut viewer.stream).await.unwrap();
                    return Some(message);
                }
                message => return Some(message),
            }
        }
    }

    fn rejected(answer: &HostMessage) -> Option<RejectReason> {
        match answer {
            HostMessage::Rejected { reason } => Some(*reason),
            _ => None,
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn handshake_reports_denied_and_busy_before_close() {
        let relay = pb_mapper_testkit::Relay::start("desktop-handshake").await;
        let client = Client::from_credential(
            relay.addr().to_string(),
            pb_mapper_testkit::admin_credential(),
            true,
            None,
        );
        let backends = Arc::new(AtomicU32::new(0));
        let created = backends.clone();
        let factory: Factory = Arc::new(move || {
            created.fetch_add(1, Ordering::Relaxed);
            Ok(Box::new(Screen))
        });
        let host = Host::start_with_backend(&client, options(), factory)
            .await
            .unwrap();
        let invitation = host.invitation().clone();
        let token = invitation.token_text();
        let wrong = "A".repeat(token.len());

        // A wrong token is denied, then closed, before any capture starts.
        let (mut denied, answer) =
            authenticate(&client, &invitation, wrong.clone(), vec![VideoCodec::Jpeg]).await;
        assert_eq!(rejected(&answer), Some(RejectReason::Denied));
        assert!(next(&mut denied).await.is_none());
        assert_eq!(backends.load(Ordering::Relaxed), 0);

        // The first viewer holds the slot ...
        let (mut first, answer) =
            authenticate(&client, &invitation, token.clone(), vec![VideoCodec::Jpeg]).await;
        assert!(
            matches!(
                answer,
                HostMessage::Accepted {
                    codec: VideoCodec::Jpeg,
                    nominal_fps: 60,
                    ..
                }
            ),
            "{answer:?}"
        );
        // ... so a second viewer with the right token is busy, a malformed
        // request is invalid, and a wrong token is still denied, never busy.
        let (_, answer) =
            authenticate(&client, &invitation, token.clone(), vec![VideoCodec::H264]).await;
        assert_eq!(rejected(&answer), Some(RejectReason::Busy));
        let (_, answer) = authenticate(&client, &invitation, token.clone(), vec![]).await;
        assert_eq!(rejected(&answer), Some(RejectReason::InvalidRequest));
        let (_, answer) = authenticate(&client, &invitation, wrong, vec![VideoCodec::Jpeg]).await;
        assert_eq!(rejected(&answer), Some(RejectReason::Denied));

        // The first viewer's session is unaffected.
        assert!(matches!(
            next(&mut first).await,
            Some(HostMessage::State(_))
        ));
        drop(first);
        host.stop().await.unwrap();
    }

    /// Poll `condition` for up to 5 s.
    async fn until(mut condition: impl FnMut() -> bool) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while !condition() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn status_reports_the_viewer_and_end_session_keeps_hosting() {
        let relay = pb_mapper_testkit::Relay::start("desktop-viewer").await;
        let client = Client::from_credential(
            relay.addr().to_string(),
            pb_mapper_testkit::admin_credential(),
            true,
            None,
        );
        let host = Host::start_with_backend(
            &client,
            HostOptions {
                allow_control: true,
                ..options()
            },
            Arc::new(|| Ok(Box::new(Screen))),
        )
        .await
        .unwrap();
        let status = host.status();
        assert_eq!(status.state, HostState::Waiting);
        assert!(status.viewer.is_none() && status.control_allowed);
        assert_eq!(status.capabilities, capabilities());

        let invitation = host.invitation().clone();
        let token = invitation.token_text();
        let (mut viewer, answer) =
            authenticate(&client, &invitation, token.clone(), vec![VideoCodec::Jpeg]).await;
        assert!(matches!(answer, HostMessage::Accepted { .. }), "{answer:?}");
        let watching = host.status().viewer.unwrap();
        assert_eq!(
            (
                watching.path,
                watching.controlling,
                watching.paused,
                watching.quality
            ),
            (HostPath::Relay, false, false, QualityPreset::Balanced)
        );
        assert!(watching.started_unix_ms > 0);

        // The viewer hides and picks another preset; the host shows both.
        for message in [
            ViewerMessage::Pause {},
            ViewerMessage::Quality {
                preset: QualityPreset::Smooth,
            },
        ] {
            wire::write_message(&mut viewer.stream, &message, None)
                .await
                .unwrap();
        }
        until(|| {
            host.status()
                .viewer
                .is_some_and(|viewer| viewer.paused && viewer.quality == QualityPreset::Smooth)
        })
        .await;
        assert_eq!(host.status().state, HostState::Paused);

        // Disconnect ends this session only: the viewer hears why ...
        host.end_session();
        loop {
            match next(&mut viewer).await {
                Some(HostMessage::Stopped { reason, .. }) => {
                    assert_eq!(reason, StopReason::HostEnded);
                    break;
                }
                Some(_) => {}
                None => panic!("closed without Stopped"),
            }
        }
        assert!(next(&mut viewer).await.is_none());
        until(|| host.status().viewer.is_none()).await;
        assert_eq!(host.status().state, HostState::Waiting);

        // ... and hosting continues with the same invitation.
        let (_again, answer) =
            authenticate(&client, &invitation, token, vec![VideoCodec::Jpeg]).await;
        assert!(matches!(answer, HostMessage::Accepted { .. }), "{answer:?}");
        host.stop().await.unwrap();
        assert_eq!(host.status().state, HostState::Stopped);
    }

    /// A screen whose backend encodes H.265 until `broken`, then fails as a
    /// GPU's H.265 encoder that stopped encoding does. It keeps the bitrate
    /// each H.265 picture was encoded at.
    struct Hevc {
        broken: Arc<AtomicBool>,
        bitrates: Arc<Mutex<Vec<u32>>>,
    }
    impl crate::Backend for Hevc {
        fn display(&mut self) -> Result<crate::Display> {
            crate::Backend::display(&mut Screen)
        }
        fn capture(&mut self) -> Result<Vec<u8>> {
            Ok(vec![1])
        }
        fn capture_video(
            &mut self,
            _: &crate::DesktopContext,
            codec: VideoCodec,
            keyframe: bool,
            rate: crate::VideoRate,
        ) -> Result<Option<crate::EncodedFrame>> {
            if codec == VideoCodec::H265 && self.broken.load(Ordering::SeqCst) {
                return Err(error(format!(
                    "{}: the encoder stopped",
                    worker::H265_ENCODER_FAILED
                )));
            }
            if codec == VideoCodec::H265 {
                self.bitrates.lock().push(rate.bitrate);
            }
            Ok(Some(crate::EncodedFrame {
                format: crate::VideoFormat {
                    codec,
                    width: 10,
                    height: 10,
                    keyframe,
                    ..crate::VideoFormat::default()
                },
                bytes: vec![1],
            }))
        }
        fn input(&mut self, _: &crate::Input) -> Result<()> {
            Err(error("read-only test backend"))
        }
        fn release(&mut self) {}
        fn encodes_h265(&mut self) -> bool {
            true
        }
    }

    /// The only test that breaks an H.265 encoder: that turns H.265 off for
    /// the whole test process.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn h265_needs_both_ends_and_ends_with_its_encoder() {
        let relay = pb_mapper_testkit::Relay::start("desktop-h265").await;
        let client = Client::from_credential(
            relay.addr().to_string(),
            pb_mapper_testkit::admin_credential(),
            true,
            None,
        );
        let broken = Arc::new(AtomicBool::new(false));
        let bitrates = Arc::new(Mutex::new(Vec::new()));
        let (encoder, encoded) = (broken.clone(), bitrates.clone());
        let host = Host::start_with_backend(
            &client,
            options(),
            Arc::new(move || {
                Ok(Box::new(Hevc {
                    broken: encoder.clone(),
                    bitrates: encoded.clone(),
                }))
            }),
        )
        .await
        .unwrap();
        let invitation = host.invitation().clone();
        let token = invitation.token_text();
        let h265 = || vec![CAPABILITY_H265.to_string()];
        let codec = |answer: &HostMessage| match answer {
            HostMessage::Accepted { codec, .. } => Some(*codec),
            _ => None,
        };

        // A viewer that does not decode H.265 gets what every v2 host gives.
        let (viewer, answer) =
            authenticate(&client, &invitation, token.clone(), vec![VideoCodec::H264]).await;
        assert_eq!(codec(&answer), Some(VideoCodec::H264));
        drop(viewer);
        until(|| host.status().viewer.is_none()).await;
        // H.265 is a capability, never a codec offered.
        let (_, answer) =
            authenticate(&client, &invitation, token.clone(), vec![VideoCodec::H265]).await;
        assert_eq!(rejected(&answer), Some(RejectReason::InvalidRequest));

        // Both ends can: H.265, and its video.
        let (mut viewer, answer) = authenticate_with(
            &client,
            &invitation,
            token.clone(),
            vec![VideoCodec::H264],
            h265(),
        )
        .await;
        assert_eq!(codec(&answer), Some(VideoCodec::H265));
        // The encoder fails once video flows: the session ends, and says so.
        let mut videos = 0;
        let reason = loop {
            match next(&mut viewer).await {
                Some(HostMessage::Video { seq, .. }) => {
                    videos += 1;
                    if videos == 3 {
                        broken.store(true, Ordering::SeqCst);
                    }
                    let receipt = ViewerMessage::Received { seq };
                    wire::write_message(&mut viewer.stream, &receipt, None)
                        .await
                        .unwrap();
                }
                Some(HostMessage::Stopped { reason, .. }) => break reason,
                Some(_) => {}
                None => panic!("closed without Stopped"),
            }
        };
        assert_eq!(reason, StopReason::CaptureFailed);
        // Its first IDR was budgeted for the 10×10 display the backend
        // reported, not for the viewer's 1080p bounds: the first unit's size
        // changed nothing.
        let encoded = bitrates.lock().clone();
        assert!(
            encoded.len() >= 3 && encoded.iter().all(|bitrate| *bitrate == encoded[0]),
            "{encoded:?}"
        );
        drop(viewer);
        until(|| host.status().viewer.is_none()).await;
        // The reconnect gets H.264, from this process on.
        let (_viewer, answer) =
            authenticate_with(&client, &invitation, token, vec![VideoCodec::H264], h265()).await;
        assert_eq!(codec(&answer), Some(VideoCodec::H264));
        host.stop().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn remembered_invitation_survives_restart_but_not_revocation() {
        let relay = pb_mapper_testkit::Relay::start("desktop-trust").await;
        let client = Client::from_credential(
            relay.addr().to_string(),
            pb_mapper_testkit::admin_credential(),
            true,
            None,
        );
        let identity = crate::HostIdentity::generate().unwrap();
        let saved = identity.encode_secret().unwrap();
        let factory: Factory = Arc::new(|| Ok(Box::new(Screen)));
        let host = Host::start_with_backend_and_identity(
            &client,
            options(),
            factory.clone(),
            None,
            identity,
        )
        .await
        .unwrap();
        // What the viewer keeps is the pbd1 text.
        let invitation = Invitation::decode(&host.invitation().encode()).unwrap();
        host.stop().await.unwrap();
        let resumed = Host::start_with_backend_and_identity(
            &client,
            options(),
            factory.clone(),
            None,
            crate::HostIdentity::decode_secret(&saved).unwrap(),
        )
        .await
        .unwrap();
        let (mut viewer, answer) = authenticate(
            &client,
            &invitation,
            invitation.token_text(),
            vec![VideoCodec::Jpeg],
        )
        .await;
        assert!(matches!(answer, HostMessage::Accepted { .. }), "{answer:?}");
        assert!(matches!(
            next(&mut viewer).await,
            Some(HostMessage::State(StateRecord {
                control_allowed: false,
                ..
            }))
        ));
        drop(viewer);
        resumed.stop().await.unwrap();

        // A new identity revokes the remembered invitation: the pin fails.
        let replacement = Host::start_with_backend_and_identity(
            &client,
            options(),
            factory,
            None,
            crate::HostIdentity::generate().unwrap(),
        )
        .await
        .unwrap();
        let failure = tls(&client, &invitation).await.err().unwrap();
        assert_eq!(
            security::handshake_failure(&failure),
            Some(security::HandshakeFailure::IdentityChanged)
        );
        replacement.stop().await.unwrap();
    }
}
