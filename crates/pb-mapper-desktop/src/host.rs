use crate::{
    Backend, BackendCaps, DesktopKind, Display, HostPath, HostState, HostStatus, HostViewer,
    Invitation, ModeList, QualityPreset, Result, VideoCodec, VideoLimits, VideoSize, error,
    lane::LaneCommand,
    launch::{Launch, Launcher, Recovery},
    modes::ModeCommand,
    platform,
    protocol::{
        CAPABILITY_DISPLAY_MODE, CAPABILITY_H265, HostMessage, RejectReason, SessionState,
        StopReason, ViewerMessage, valid_capabilities,
    },
    qos::scaled_size,
    security::HostIdentity,
    session::{self, Session, stop_message},
    shared::Policy,
    wire,
    worker::{self, WorkerConfig},
};
use parking_lot::Mutex;
use pb_mapper_client::sdk::{
    Client, RegisterOptions, RegisterRequest, Registration, Transport, TunnelMetrics,
    TunnelMetricsStream,
};
use std::{
    marker::PhantomData,
    path::PathBuf,
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
/// How long `Accepted` waits for a new backend to describe itself: which
/// display it captures, what it can do and, for a viewer that decodes H.265,
/// whether it encodes H.265; the viewer waits 5 s for `Accepted`. A slower
/// answer leaves that session on H.264, its first IDR budgeted for the
/// viewer's bounds, with the capabilities its launcher falls back to.
const BACKEND_ANSWER_WAIT: Duration = Duration::from_secs(3);
/// Connections in their handshake or session at once; one holds the viewer slot.
const MAX_CONNECTIONS: usize = 8;
/// How soon a display mode a session could not put back is tried again
/// while no session is live; each try that leaves it doubles the wait, up to
/// `RESTORE_RETRY_MAX`.
const RESTORE_RETRY: Duration = Duration::from_secs(2);
const RESTORE_RETRY_MAX: Duration = Duration::from_secs(60);

/// Which display a host shares.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum DisplaySelection {
    /// The primary display.
    #[default]
    Primary,
    /// The display of this [`Display::id`]. The id belongs to the running
    /// system: a reboot or a mode change may give the display another one.
    Id(u32),
    /// The display of this [`Display::name`] (`\\.\DISPLAY1` on Windows),
    /// which stays the same: the key to keep when a choice is saved.
    Device(String),
}

/// How a host reaches the desktop it shares.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum Access {
    /// This process's own desktop, as its user.
    #[default]
    Ordinary,
    /// The installed, authorized privileged helper, which also reaches the
    /// secure desktop (UAC and the lock screen). Windows only.
    Privileged,
}

/// What a host shares and how. Build it with [`HostOptions::new`], which
/// takes every path default from [`RegisterOptions::default`]: a viewer on the
/// same local network first, then a direct path, then the relay.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct HostOptions {
    /// The registered service: `desktop:` and up to 128 ASCII letters,
    /// digits, `.`, `_`, `:` or `-`.
    pub service: String,
    pub display: DisplaySelection,
    pub access: Access,
    /// Whether a viewer may take control. Off by default: viewing only.
    pub allow_control: bool,
    /// The registration's [`RegisterOptions::allow_direct`].
    pub allow_direct: bool,
    /// The registration's [`RegisterOptions::lan_first`]: probe a viewer's
    /// addresses on a shared local network first. Which path a session
    /// takes is the viewer's choice, under its own `ConnectOptions`.
    pub lan_first: bool,
    /// The registration's [`RegisterOptions::lan_tcp`]: serve a viewer on the
    /// same local network over TCP when it asks.
    pub lan_tcp: bool,
    /// Where an in-process host keeps what it must restore after a crash;
    /// `None` is the platform's own directory. A privileged host never uses
    /// it: the helper keeps its own, protected.
    pub state_dir: Option<PathBuf>,
}
impl HostOptions {
    pub fn new(service: impl Into<String>) -> Self {
        let register = RegisterOptions::default();
        Self {
            service: service.into(),
            display: DisplaySelection::default(),
            access: Access::default(),
            allow_control: false,
            allow_direct: register.allow_direct,
            lan_first: register.lan_first,
            lan_tcp: register.lan_tcp,
            state_dir: None,
        }
    }
}

/// The platform's own capture and input, through [`HostOptions::access`].
pub struct Native;
/// An embedder's backends, each built by the factory on its session's own
/// thread.
pub struct Custom<B, F> {
    factory: F,
    _backend: PhantomData<fn() -> B>,
}
/// What a host starts from once its source is known: the platform's own
/// (`native`, which a restart builds again for its new options), an
/// embedder's, or one the crate built itself (the background host's helper
/// client).
pub(crate) struct Launched {
    launch: Arc<dyn Launch>,
    native: bool,
}

/// How a [`Host`] is started: [`Host::builder`], then any of `identity`,
/// `admission` and `backend`, then `start`.
pub struct HostBuilder<S = Native> {
    options: HostOptions,
    identity: Option<HostIdentity>,
    admission: Option<Arc<dyn crate::ControlAdmission>>,
    source: S,
}
impl<S> HostBuilder<S> {
    /// Resume an explicitly authorized grant: the host keeps this identity,
    /// and so the invitation a viewer saved. Without one it generates a new
    /// identity. Revoke durable access by discarding the saved identity.
    pub fn identity(mut self, identity: HostIdentity) -> Self {
        self.identity = Some(identity);
        self
    }
    /// Coordinate control with the embedding application's other input
    /// writers.
    pub fn admission(mut self, admission: Arc<dyn crate::ControlAdmission>) -> Self {
        self.admission = Some(admission);
        self
    }
    /// Capture and inject with the embedder's backends instead of the
    /// platform's.
    pub fn backend<B, F>(self, factory: F) -> HostBuilder<Custom<B, F>>
    where
        B: Backend,
        F: Fn() -> Result<B> + Send + Sync + 'static,
    {
        self.source(Custom {
            factory,
            _backend: PhantomData,
        })
    }
    pub(crate) fn launch(self, launch: Arc<dyn Launch>) -> HostBuilder<Launched> {
        self.source(Launched {
            launch,
            native: false,
        })
    }
    fn source<T>(self, source: T) -> HostBuilder<T> {
        HostBuilder {
            options: self.options,
            identity: self.identity,
            admission: self.admission,
            source,
        }
    }
}
impl HostBuilder<Native> {
    /// Refused before registering where this platform cannot host, or the
    /// privileged helper is not ready.
    pub async fn start(self, client: &Client) -> Result<Host> {
        let launch = platform::native_launch(&self.options).await?;
        let mut native = self.launch(launch);
        native.source.native = true;
        native.start(client).await
    }
}
impl<B, F> HostBuilder<Custom<B, F>>
where
    B: Backend,
    F: Fn() -> Result<B> + Send + Sync + 'static,
{
    pub async fn start(self, client: &Client) -> Result<Host> {
        let HostBuilder {
            options,
            identity,
            admission,
            source,
        } = self;
        HostBuilder {
            options,
            identity,
            admission,
            source: Launched {
                launch: Arc::new(Launcher::new(source.factory, BackendCaps::default())),
                native: false,
            },
        }
        .start(client)
        .await
    }
}
impl HostBuilder<Launched> {
    pub(crate) async fn start(self, client: &Client) -> Result<Host> {
        Host::launch(client, self).await
    }
}

/// What a host's status reads, shared with its acceptor.
struct Shared {
    policy: Arc<Policy>,
    /// What sessions list when their backend did not describe itself.
    fallback: Vec<String>,
    /// The session holding the single viewer slot, if any.
    session: Mutex<Option<Live>>,
    stopped: AtomicBool,
    registration: Registration,
    /// The display-mode journal may name a display to put back, which the
    /// host-lived retry replays while no session is live.
    restore_pending: AtomicBool,
    /// Wakes that retry at once.
    restore: tokio::sync::Notify,
}
/// The session in the viewer slot, with what its backend can do.
struct Live {
    session: Arc<Session>,
    capabilities: Vec<String>,
}

pub struct Host {
    invitation: Invitation,
    identity: HostIdentity,
    options: HostOptions,
    client: Client,
    launch: Arc<dyn Launch>,
    /// The platform's own source, which a restart builds again for the
    /// options it restarts with.
    native: bool,
    admission: Option<Arc<dyn crate::ControlAdmission>>,
    shared: Arc<Shared>,
    cancel: CancellationToken,
    join: AsyncMutex<Option<tokio::task::JoinHandle<()>>>,
}
impl Host {
    pub fn builder(options: HostOptions) -> HostBuilder {
        HostBuilder {
            options,
            identity: None,
            admission: None,
            source: Native,
        }
    }
    /// `Host::builder(options).start(client)`: the platform's own capture,
    /// under a new identity.
    pub async fn start(client: &Client, options: HostOptions) -> Result<Self> {
        Self::builder(options).start(client).await
    }
    async fn launch(client: &Client, builder: HostBuilder<Launched>) -> Result<Self> {
        let HostBuilder {
            options,
            identity,
            admission,
            source: Launched { launch, native },
        } = builder;
        if !crate::valid_service(&options.service) {
            return Err(error(
                "desktop service must start with desktop: and contain at most 128 ASCII letters, numbers, periods, hyphens, underscores or colons",
            ));
        }
        let identity = match identity {
            Some(identity) => identity,
            None => HostIdentity::generate()?,
        };
        let tls = identity.acceptor()?;
        let invitation = identity.invitation(options.service.clone())?;
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let registration = client
            .register_with_options(
                RegisterRequest {
                    key: options.service.clone(),
                    local_addr: listener.local_addr()?.to_string(),
                    transport: Transport::Tcp,
                    codec: true,
                    force_namespace: false,
                },
                RegisterOptions {
                    allow_direct: options.allow_direct,
                    lan_first: options.lan_first,
                    lan_tcp: options.lan_tcp,
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
        let shared = Arc::new(Shared {
            policy: Arc::new(Policy::new(options.allow_control)),
            fallback: launch.fallback_caps().values(),
            session: Mutex::new(None),
            stopped: AtomicBool::new(false),
            registration,
            // What a host of this process left changed, replayed at once:
            // a restart too starts here.
            restore_pending: AtomicBool::new(true),
            restore: tokio::sync::Notify::new(),
        });
        let cancel = CancellationToken::new();
        if let Some(recovery) = launch.recovery() {
            tokio::spawn(restore_retry(shared.clone(), recovery, cancel.clone()));
        }
        let acceptor = Arc::new(Acceptor {
            tls,
            token: invitation.token_text(),
            launch: launch.clone(),
            admission: admission.clone(),
            shared: shared.clone(),
            slot: Arc::new(Semaphore::new(1)),
            cancel: cancel.clone(),
        });
        let join = tokio::spawn(acceptor.accept(listener));
        Ok(Self {
            invitation,
            identity,
            options,
            client: client.clone(),
            launch,
            native,
            admission,
            shared,
            cancel,
            join: AsyncMutex::new(Some(join)),
        })
    }
    /// The `pbd1` invitation to this host.
    pub fn invitation(&self) -> &Invitation {
        &self.invitation
    }
    /// The identity behind the invitation, which a restart keeps. Saving it
    /// remembers the grant; it is a secret.
    pub fn identity(&self) -> &HostIdentity {
        &self.identity
    }
    /// The options this host started with. Its live control policy is in
    /// [`Self::status`].
    pub fn options(&self) -> &HostOptions {
        &self.options
    }
    /// Host again with changed options, under the same identity, so a saved
    /// invitation stays valid, with the same backend source, admission and
    /// live control policy. The changed options are checked before this host
    /// stops, telling its viewer `reason`, and the new one starts; a refused
    /// change (`ErrorKind::InvalidInput`) ends this host too, since it was
    /// handed over: check a new service with [`HostIdentity::invitation`]
    /// first to keep it. It registers through the client this host started
    /// with; [`Self::restart_on`] takes the one the relay settings name now.
    pub async fn restart(
        self,
        reason: StopReason,
        change: impl FnOnce(&mut HostOptions),
    ) -> Result<Host> {
        let client = self.client.clone();
        self.restart_on(&client, reason, change).await
    }
    /// [`Self::restart`] through `client`: the relay credential and carrier
    /// as they are now, after a key rotation or a carrier change.
    pub async fn restart_on(
        self,
        client: &Client,
        reason: StopReason,
        change: impl FnOnce(&mut HostOptions),
    ) -> Result<Host> {
        let mut options = self.options.clone();
        options.allow_control = self.shared.policy.allowed();
        change(&mut options);
        let invalid = |failure: std::io::Error| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, failure.to_string())
        };
        if !crate::valid_service(&options.service) {
            return Err(invalid(error("invalid desktop service")));
        }
        self.identity
            .invitation(options.service.clone())
            .map_err(invalid)?;
        let launch = if self.native {
            platform::native_launch(&options).await?
        } else {
            self.launch.clone()
        };
        // One owner at a time: this registration goes before the new one.
        self.stop_with(reason).await?;
        let builder = HostBuilder {
            options,
            identity: Some(self.identity.clone()),
            admission: self.admission.clone(),
            source: Launched {
                launch,
                native: self.native,
            },
        };
        builder.start(client).await
    }
    /// Share another display; the viewer hears `Restarting` and reconnects.
    pub async fn switch_display(self, display: DisplaySelection) -> Result<Host> {
        self.restart(StopReason::Restarting, |options| options.display = display)
            .await
    }
    /// Publish under another service; the viewer, whose invitation names the
    /// old one, hears `HostStopped`.
    pub async fn rename(self, service: impl Into<String>) -> Result<Host> {
        let service = service.into();
        self.restart(StopReason::HostStopped, |options| options.service = service)
            .await
    }
    /// The host's state and its viewer, if one is connected: when its session
    /// started, whether it runs direct or through the relay, and whether it
    /// controls, has paused, and which quality it chose.
    pub fn status(&self) -> HostStatus {
        self.status_handle().status()
    }
    /// What reads this host's status, apart from the host.
    pub(crate) fn status_handle(&self) -> HostStatusHandle {
        HostStatusHandle {
            shared: self.shared.clone(),
            cancel: self.cancel.clone(),
        }
    }
    /// The host's tunnel figures, sampled now: its viewers' sessions on each
    /// path, the bytes they carried, the relay's health and the direct link.
    pub fn metrics(&self) -> TunnelMetrics {
        self.shared.registration.metrics()
    }
    /// [`Self::metrics`] at once, then every `every` and as the tunnel
    /// changes, until hosting stops: see [`TunnelMetricsStream`].
    pub fn subscribe_metrics(&self, every: Duration) -> TunnelMetricsStream {
        self.shared.registration.subscribe_metrics(every)
    }
    /// "Disconnect": end the connected viewer's session; it hears
    /// `Stopped{host_ended}`. Hosting continues, and the viewer may connect
    /// again with the same invitation.
    pub fn end_session(&self) {
        if let Some(live) = self.shared.session.lock().as_ref() {
            live.session
                .end(StopReason::HostEnded, stop_message(StopReason::HostEnded));
        }
    }
    /// Put this computer's display back in its own mode, which a viewer
    /// changed: at once in the live session, whether or not it controls
    /// (refused only on the secure desktop or while a change settles, which
    /// the viewer hears), else by replaying the journal. False when there is
    /// nothing to ask: no session, and no journal this host replays.
    pub fn restore_display_mode(&self) -> bool {
        if let Some(live) = self.shared.session.lock().as_ref() {
            return live
                .session
                .shared
                .lane
                .push(LaneCommand::DisplayMode(ModeCommand::HostRestore));
        }
        if self.launch.recovery().is_none() {
            return false;
        }
        self.shared.restore_pending.store(true, Ordering::Release);
        self.shared.restore.notify_one();
        true
    }
    /// "Make view-only" and back. Revocation gates every later input at the
    /// edge, and the worker releases held input within one backend step. It
    /// also puts the display's own mode back, which a grant does not.
    pub fn set_control_allowed(&self, allowed: bool) {
        self.shared.policy.set(allowed);
        if let Some(live) = self.shared.session.lock().as_ref() {
            live.session.shared.wake.notify();
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
        self.shared.policy.set(false);
        if let Some(live) = self.shared.session.lock().as_ref() {
            live.session.end(reason, stop_message(reason));
        }
        self.cancel.cancel();
        let mut join = self.join.lock().await;
        if let Some(task) = join.as_mut() {
            let _ = task.await;
        }
        join.take();
        self.shared.registration.stop().await.map_err(error)?;
        self.shared.stopped.store(true, Ordering::Release);
        Ok(())
    }
}
impl Drop for Host {
    fn drop(&mut self) {
        self.shared.policy.set(false);
        self.cancel.cancel();
    }
}

/// A host's status, readable from any task until the host is gone.
#[derive(Clone)]
pub(crate) struct HostStatusHandle {
    shared: Arc<Shared>,
    cancel: CancellationToken,
}
impl HostStatusHandle {
    pub(crate) fn status(&self) -> HostStatus {
        self.shared.status()
    }
    /// Whether the host stopped, or was dropped.
    #[expect(dead_code, reason = "Host::watch (pf-sdk) ends with the host")]
    pub(crate) fn stopped(&self) -> bool {
        self.shared.stopped.load(Ordering::Acquire) || self.cancel.is_cancelled()
    }
}

impl Shared {
    fn status(&self) -> HostStatus {
        let control_allowed = self.policy.allowed();
        let live = self
            .session
            .lock()
            .as_ref()
            .map(|live| (live.session.clone(), live.capabilities.clone()));
        let Some((session, capabilities)) = live else {
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
                capabilities: self.fallback.clone(),
                display_mode: None,
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
            display_mode: record.modes.filter(ModeList::changed),
        }
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
}

/// The host-lived retry of a display mode a session could not put back (the
/// ordinary desktop was away, say): while the journal may still name one
/// and no session is live, replay it, off the runtime, backing off from
/// `RESTORE_RETRY` to `RESTORE_RETRY_MAX`, and at once when asked. A
/// failure is warned of when it changes, never at every try. A live
/// session's backend replays it itself. Ends with the host.
async fn restore_retry(shared: Arc<Shared>, recovery: Recovery, cancel: CancellationToken) {
    let mut wait = RESTORE_RETRY;
    let mut warned = None;
    loop {
        if shared.restore_pending.load(Ordering::Acquire) && shared.session.lock().is_none() {
            let recovery = recovery.clone();
            match tokio::task::spawn_blocking(move || recovery()).await {
                Ok(Ok(pending)) => {
                    shared.restore_pending.store(pending, Ordering::Release);
                    warned = None;
                }
                Ok(Err(failure)) => {
                    let detail = failure.to_string();
                    if warned.as_ref() != Some(&detail) {
                        tracing::warn!(
                            target: "pb_mapper_desktop::session",
                            event = "display_mode_not_restored",
                            detail = %detail,
                        );
                        warned = Some(detail);
                    }
                }
                Err(_) => {}
            }
        }
        // Idle until asked once nothing is left.
        let retry = async {
            if shared.restore_pending.load(Ordering::Acquire) {
                tokio::time::sleep(wait).await;
            } else {
                std::future::pending::<()>().await;
            }
        };
        tokio::select! {
            () = cancel.cancelled() => return,
            () = shared.restore.notified() => wait = RESTORE_RETRY,
            () = retry => wait = (wait * 2).min(RESTORE_RETRY_MAX),
        }
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
    /// What `Accepted` listed.
    capabilities: Vec<String>,
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
    launch: Arc<dyn Launch>,
    admission: Option<Arc<dyn crate::ControlAdmission>>,
    shared: Arc<Shared>,
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
            capabilities,
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
            self.shared.policy.clone(),
            self.cancel.child_token(),
            codec,
            request.limits,
            request.quality,
            &request.capabilities,
            capabilities
                .iter()
                .any(|capability| capability == CAPABILITY_DISPLAY_MODE),
        );
        session.expect_video_size(video);
        *self.shared.session.lock() = Some(Live {
            session: session.clone(),
            capabilities,
        });
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
        let mut slot = self.shared.session.lock();
        if slot
            .as_ref()
            .is_some_and(|held| Arc::ptr_eq(&held.session, &session))
        {
            slot.take();
        }
        drop(slot);
        // Thread C put the display back as the session ended, unless it
        // could not: the retry looks.
        self.shared.restore_pending.store(true, Ordering::Release);
        self.shared.restore.notify_one();
        drop(permit);
    }

    /// The handshake, then `Accepted` with the session's codec and
    /// capabilities. Thread C starts building the backend as soon as the
    /// viewer holds the slot, and `Accepted` waits until the backend described
    /// itself: which display it captures, which sizes the first IDR's budget;
    /// what it can do; and, for a viewer that decodes H.265, whether it
    /// encodes H.265, unless an H.265 encoder already failed in this process.
    async fn accept_session(&self, socket: TcpStream) -> Option<Admitted> {
        let (mut stream, permit, request) = self.handshake(socket).await?;
        let offered = request
            .capabilities
            .iter()
            .any(|capability| capability == CAPABILITY_H265);
        // Never H.265 again in this process once one of its encoders failed,
        // whatever a backend says.
        let probe = offered && !worker::h265_failed();
        let mut prepared = self.launch.prepare(probe);
        let deadline = tokio::time::Instant::now() + BACKEND_ANSWER_WAIT;
        let description = tokio::time::timeout_at(deadline, &mut prepared.description)
            .await
            .ok()
            .and_then(|answer| answer.ok())
            .and_then(Result::ok);
        let video = expected_video(
            description.as_ref().map(|described| &described.display),
            request.limits,
        );
        let caps =
            description.map_or_else(|| self.launch.fallback_caps(), |described| described.caps);
        let h265 = match prepared.h265.as_mut() {
            Some(answer) => matches!(
                tokio::time::timeout_at(deadline, answer).await,
                Ok(Ok(true))
            ),
            None => false,
        };
        let codec = choose_codec(&request.codecs, probe && h265);
        let capabilities = caps.values();
        let accepted = HostMessage::Accepted {
            codec,
            limits: request.limits,
            nominal_fps: request.limits.max_fps,
            quality: request.quality,
            capabilities: capabilities.clone(),
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
            capabilities,
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
    use crate::security;
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
        let mut options = HostOptions::new("desktop:remembered");
        options.allow_direct = false;
        options
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
        let host = Host::builder(options())
            .backend(move || {
                created.fetch_add(1, Ordering::Relaxed);
                Ok(Screen)
            })
            .start(&client)
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
        let mut options = options();
        options.allow_control = true;
        let host = Host::builder(options)
            .backend(|| Ok(Screen))
            .start(&client)
            .await
            .unwrap();
        let status = host.status();
        assert_eq!(status.state, HostState::Waiting);
        assert!(status.viewer.is_none() && status.control_allowed);
        // An embedder's backend that describes nothing can do nothing more.
        assert!(status.capabilities.is_empty());

        let invitation = host.invitation().clone();
        let token = invitation.token_text();
        let (mut viewer, answer) =
            authenticate(&client, &invitation, token.clone(), vec![VideoCodec::Jpeg]).await;
        assert!(matches!(answer, HostMessage::Accepted { .. }), "{answer:?}");
        // The host publishes the session just after writing `Accepted`.
        until(|| host.status().viewer.is_some()).await;
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
                return Err(crate::BackendError::new(
                    crate::FailureKind::EncoderFailed { codec },
                    "the encoder stopped",
                )
                .into());
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
        /// It can lock, says nothing of its OS, and answers H.265 from a cache,
        /// whether asked or not: the host keeps H.265 off after a failure anyway.
        fn describe(&mut self, _: bool) -> Result<crate::Description> {
            Ok(crate::Description {
                display: self.display()?,
                caps: BackendCaps {
                    h265: true,
                    lock: true,
                    ..BackendCaps::default()
                },
            })
        }
    }

    /// A backend slower to describe itself than `Accepted` waits leaves its
    /// session with what its launcher falls back to.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_late_description_falls_back_to_the_launcher() {
        struct Late;
        impl crate::Backend for Late {
            fn display(&mut self) -> Result<crate::Display> {
                crate::Backend::display(&mut Screen)
            }
            fn input(&mut self, _: &crate::Input) -> Result<()> {
                Err(error("read-only test backend"))
            }
            fn release(&mut self) {}
            fn describe(&mut self, _: bool) -> Result<crate::Description> {
                std::thread::sleep(BACKEND_ANSWER_WAIT + Duration::from_millis(500));
                Ok(crate::Description {
                    display: self.display()?,
                    caps: BackendCaps::default(),
                })
            }
        }
        let relay = pb_mapper_testkit::Relay::start("desktop-late").await;
        let client = Client::from_credential(
            relay.addr().to_string(),
            pb_mapper_testkit::admin_credential(),
            true,
            None,
        );
        let fallback = BackendCaps {
            lock: true,
            os: Some(crate::HostOs::Windows),
            ..BackendCaps::default()
        };
        let host = Host::builder(options())
            .launch(Arc::new(Launcher::new(|| Ok(Late), fallback)))
            .start(&client)
            .await
            .unwrap();
        let invitation = host.invitation().clone();
        let (_viewer, answer) = authenticate(
            &client,
            &invitation,
            invitation.token_text(),
            vec![VideoCodec::Jpeg],
        )
        .await;
        let HostMessage::Accepted { capabilities, .. } = answer else {
            panic!("{answer:?}");
        };
        assert_eq!(capabilities, ["lock", "os_windows"]);
        host.stop().await.unwrap();
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
        let host = Host::builder(options())
            .backend(move || {
                Ok(Hevc {
                    broken: encoder.clone(),
                    bitrates: encoded.clone(),
                })
            })
            .start(&client)
            .await
            .unwrap();
        let invitation = host.invitation().clone();
        let token = invitation.token_text();
        let h265 = || vec![CAPABILITY_H265.to_string()];
        let codec = |answer: &HostMessage| match answer {
            HostMessage::Accepted {
                codec,
                capabilities,
                ..
            } => {
                // What this session's backend described, nothing more.
                assert_eq!(capabilities, &[crate::protocol::CAPABILITY_LOCK]);
                Some(*codec)
            }
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
        let start = |identity| {
            Host::builder(options())
                .identity(identity)
                .backend(|| Ok(Screen))
                .start(&client)
        };
        let host = start(identity).await.unwrap();
        // What the viewer keeps is the pbd1 text.
        let invitation = Invitation::decode(&host.invitation().encode()).unwrap();
        host.stop().await.unwrap();
        let resumed = start(crate::HostIdentity::decode_secret(&saved).unwrap())
            .await
            .unwrap();
        let viewer = async |control_allowed: bool| {
            let (mut viewer, answer) = authenticate(
                &client,
                &invitation,
                invitation.token_text(),
                vec![VideoCodec::Jpeg],
            )
            .await;
            assert!(matches!(answer, HostMessage::Accepted { .. }), "{answer:?}");
            let Some(HostMessage::State(state)) = next(&mut viewer).await else {
                panic!("no state");
            };
            assert_eq!(state.control_allowed, control_allowed);
            viewer
        };
        let mut watching = viewer(false).await;

        // Another display keeps the identity, the backend and the live
        // control policy; the viewer hears why it may reconnect.
        resumed.set_control_allowed(true);
        let switched = resumed
            .switch_display(crate::DisplaySelection::Id(2))
            .await
            .unwrap();
        loop {
            match next(&mut watching).await {
                Some(HostMessage::Stopped { reason, .. }) => {
                    assert_eq!(reason, StopReason::Restarting);
                    break;
                }
                Some(_) => {}
                None => panic!("closed without Stopped"),
            }
        }
        assert_eq!(switched.invitation().encode(), invitation.encode());
        assert_eq!(switched.options().display, crate::DisplaySelection::Id(2));
        drop(viewer(true).await);
        switched.stop().await.unwrap();

        // A new identity revokes the remembered invitation: the pin fails.
        let replacement = start(crate::HostIdentity::generate().unwrap())
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
