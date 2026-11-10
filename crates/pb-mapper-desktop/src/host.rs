//! A desktop host: its lifecycle, status and the display-mode restore retry.

mod acceptor;
mod builder;
#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests;

use self::acceptor::Acceptor;
use self::builder::Launched;
pub use self::builder::{Access, Custom, DisplaySelection, HostBuilder, HostOptions, Native};
use crate::{
    DesktopKind, HostPath, HostState, HostStatus, HostViewer, Invitation, ModeList, Result, error,
    lane::LaneCommand,
    launch::{Launch, Recovery},
    modes::ModeCommand,
    platform,
    protocol::{SessionState, StopReason},
    security::HostIdentity,
    session::{Session, stop_message},
    shared::Policy,
};
use parking_lot::Mutex;
use pb_mapper_client::sdk::{
    Client, RegisterOptions, RegisterRequest, Registration, Transport, TunnelMetrics,
    TunnelMetricsStream,
};
use std::{
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    net::TcpListener,
    sync::{Mutex as AsyncMutex, Semaphore},
};
use tokio_util::sync::CancellationToken;

/// How long `Accepted` waits for a new backend to describe itself: which
/// display it captures, what it can do and, for a viewer that decodes H.265,
/// whether it encodes H.265; the viewer waits 5 s for `Accepted`. A slower
/// answer leaves that session on H.264, its first IDR budgeted for the
/// viewer's bounds, with the capabilities its launcher falls back to.
const BACKEND_ANSWER_WAIT: Duration = Duration::from_secs(3);
/// How soon a display mode a session could not put back is tried again
/// while no session is live; each try that leaves it doubles the wait, up to
/// `RESTORE_RETRY_MAX`.
const RESTORE_RETRY: Duration = Duration::from_secs(2);
const RESTORE_RETRY_MAX: Duration = Duration::from_secs(60);

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
    /// The status a finished stop left, for watches that outlive the host;
    /// dropped with the host, which ends them too.
    last: tokio::sync::watch::Sender<Option<HostStatus>>,
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
            last: tokio::sync::watch::Sender::new(None),
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
        self.shared.status()
    }
    /// What reads this host's status apart from the host, without keeping
    /// it, or its registration, alive.
    pub(crate) fn status_handle(&self) -> HostStatusHandle {
        HostStatusHandle {
            shared: Arc::downgrade(&self.shared),
            last: self.last.subscribe(),
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
        self.last.send_replace(Some(self.shared.status()));
        Ok(())
    }
}
impl Drop for Host {
    fn drop(&mut self) {
        self.shared.policy.set(false);
        self.cancel.cancel();
    }
}

/// A host's status, readable from any task while the host lives, and the
/// status its stop left after.
#[derive(Clone)]
pub(crate) struct HostStatusHandle {
    shared: Weak<Shared>,
    last: tokio::sync::watch::Receiver<Option<HostStatus>>,
}
impl HostStatusHandle {
    /// `None` once the host and what it ran are gone.
    pub(crate) fn status(&self) -> Option<HostStatus> {
        Some(self.shared.upgrade()?.status())
    }
    /// The `Stopped` status, once a stop has finished; `None` once the host
    /// was dropped without one.
    pub(crate) async fn stopped(mut self) -> Option<HostStatus> {
        let _ = self.last.wait_for(Option::is_some).await;
        self.last.borrow().clone()
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
