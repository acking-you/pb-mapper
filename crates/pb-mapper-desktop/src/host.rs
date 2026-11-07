use crate::{
    DesktopKind, HostPath, HostState, HostStatus, HostViewer, Invitation, QualityPreset, Result,
    VideoCodec, VideoLimits, error, platform,
    protocol::{
        CAPABILITY_LOCK, HostMessage, RejectReason, SessionState, StopReason, ViewerMessage,
        valid_capabilities,
    },
    session::{self, Session, stop_message},
    shared::Policy,
    wire,
    worker::{self, Factory, WorkerConfig},
};
use parking_lot::Mutex;
use pb_mapper_client::sdk::{Client, RegisterOptions, RegisterRequest, Registration, Transport};
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
    sync::{Mutex as AsyncMutex, OwnedSemaphorePermit, Semaphore},
    task::JoinSet,
};
use tokio_rustls::{TlsAcceptor, server::TlsStream};
use tokio_util::sync::CancellationToken;

/// The TLS handshake and the viewer's `Authenticate` must arrive within this.
pub(crate) const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(3);
/// A `Rejected` and the close after it.
pub(crate) const REJECT_WRITE_TIMEOUT: Duration = Duration::from_secs(1);
/// Connections in their handshake or session at once; one holds the viewer slot.
const MAX_CONNECTIONS: usize = 8;

pub struct HostOptions {
    pub service: String,
    pub display_id: u32,
    pub allow_control: bool,
    pub allow_direct: bool,
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
        let id = options.display_id;
        Self::start_with_backend(
            client,
            options,
            Arc::new(move || platform::native_backend(id)),
        )
        .await
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
        let id = options.display_id;
        Self::start_with_backend_and_identity(
            client,
            options,
            Arc::new(move || platform::native_backend(id)),
            None,
            identity,
        )
        .await
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
    /// The one viewer's path, sampled from the registration's sessions.
    fn path(&self) -> HostPath {
        match self.registration.diagnostics().direct {
            None => HostPath::Relay,
            Some(paths) if paths.active_direct > 0 => HostPath::Direct,
            Some(paths) if paths.active_relay > 0 => HostPath::Relay,
            Some(_) => HostPath::Unknown,
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

/// What every Windows backend can do: the native desktop and the helper both
/// lock the workstation.
fn capabilities() -> Vec<String> {
    if cfg!(windows) {
        vec![CAPABILITY_LOCK.into()]
    } else {
        Vec::new()
    }
}

/// What the viewer asked for in an accepted `Authenticate`.
struct Request {
    codec: VideoCodec,
    limits: VideoLimits,
    quality: QualityPreset,
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
            accepted = self.handshake(socket) => accepted,
        };
        let Some((stream, permit, request)) = accepted else {
            return;
        };
        let session = Session::new(
            self.policy.clone(),
            self.cancel.child_token(),
            request.codec,
            request.limits,
            request.quality,
        );
        *self.session.lock() = Some(session.clone());
        let mut worker = worker::spawn_session(
            session.shared.clone(),
            self.factory.clone(),
            WorkerConfig {
                codec: request.codec,
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

    /// TLS, `Authenticate`, then `Accepted` or `Rejected`. The token is
    /// checked first, so `busy` never tells a stranger that someone is
    /// connected.
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
            || limits.validate().is_err()
            || !valid_capabilities(&capabilities)
        {
            RejectReason::InvalidRequest
        } else if let Ok(permit) = self.slot.clone().try_acquire_owned() {
            let codec = if codecs.contains(&VideoCodec::H264) {
                VideoCodec::H264
            } else {
                VideoCodec::Jpeg
            };
            let accepted = HostMessage::Accepted {
                codec,
                limits,
                nominal_fps: limits.max_fps,
                quality,
                capabilities: self.capabilities.clone(),
            };
            wire::write_message(&mut stream, &accepted, None)
                .await
                .ok()?;
            return Some((
                stream,
                permit,
                Request {
                    codec,
                    limits,
                    quality,
                },
            ));
        } else {
            RejectReason::Busy
        };
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
        let mut viewer = tls(client, invitation).await.unwrap();
        let authenticate = ViewerMessage::Authenticate {
            token,
            codecs,
            limits: VideoLimits::for_viewer(60.0),
            quality: QualityPreset::Balanced,
            capabilities: vec![],
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
