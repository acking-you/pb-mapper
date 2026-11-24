//! A desktop host: its lifecycle, status and the display-mode restore retry.

mod acceptor;
mod builder;
#[cfg(test)]
mod tests;

use self::acceptor::Acceptor;
use self::builder::Launched;
pub use self::builder::{Access, Custom, DisplaySelection, HostBuilder, HostOptions, Native};
use crate::{
    DesktopKind, Display, HostPath, HostState, HostStatus, HostViewer, Invitation, ModeList,
    Result, VideoCodec, VideoLimits, error,
    launch::{Launch, Recovery},
    platform,
    protocol::{MAX_VIEWERS, PeerColor, PeerSlot, RateModel, SessionState, StopReason},
    qos::ClosingRate,
    security::HostIdentity,
    session::{Session, stop_message},
    shared::Policy,
    watch::{ChangeSignal, HostChanges},
    worker::{self, RoomShared, RoomSlotConfig, WorkerExit, WorkerHandle},
};
use parking_lot::Mutex;
use pb_mapper_client::sdk::{
    Client, RegisterOptions, RegisterRequest, Registration, Transport, TunnelMetrics,
    TunnelMetricsStream,
};
use pb_mapper_core::timeout::RetryBackoff;
use std::{
    io,
    net::{Ipv4Addr, SocketAddr},
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Mutex as AsyncMutex, Semaphore};
use tokio_util::sync::CancellationToken;

/// How long `Accepted` waits for a new backend to describe itself: which
/// display it captures, what it can do and, for a viewer that decodes H.265,
/// whether it encodes H.265; the viewer waits 5 s for `Accepted`. A slower
/// answer leaves that session on H.264, its first IDR budgeted for the
/// viewer's bounds, with the capabilities its launcher falls back to.
pub(crate) const BACKEND_ANSWER_WAIT: Duration = Duration::from_secs(3);
/// How soon a display mode a session could not put back is tried again
/// while no session is live; each try that leaves it doubles the wait, up to
/// `RESTORE_RETRY_MAX`.
const RESTORE_RETRY: Duration = Duration::from_secs(2);
const RESTORE_RETRY_MAX: Duration = Duration::from_secs(60);
const _: () = assert!(
    !RESTORE_RETRY.is_zero() && RESTORE_RETRY.as_nanos() <= RESTORE_RETRY_MAX.as_nanos(),
    "RetryBackoff::new's preconditions: the retry backs off from RESTORE_RETRY up to the cap"
);

/// What a host's status reads, shared with its acceptor.
struct Shared {
    policy: Arc<Policy>,
    /// What sessions list when their backend did not describe itself.
    fallback: Vec<String>,
    /// Active multi-viewer room, while at least one viewer is connected.
    room: Mutex<Option<Arc<Room>>>,
    stopped: AtomicBool,
    registration: Registration,
    /// The display-mode journal may name a display to put back, which the
    /// host-lived retry replays while no session is live.
    restore_pending: AtomicBool,
    /// Wakes that retry at once.
    restore: tokio::sync::Notify,
    /// Bumped wherever `status` may now read otherwise: see
    /// [`Host::changes`].
    changes: ChangeSignal,
    /// Where the last session its transport ended left its rate: the next
    /// session of its preset that opens a room, within `qos::START_MEMORY`,
    /// starts no higher. Kept here rather than in a room, which ends with
    /// its last viewer.
    closing_rate: Mutex<Option<ClosingRate>>,
}

pub(super) struct RoomSlot {
    pub conn_id: u64,
    pub session: Arc<Session>,
}

struct RoomInner {
    closed: bool,
    codec: VideoCodec,
    next_conn_id: u64,
    slots: [Option<RoomSlot>; MAX_VIEWERS],
}

/// A live desktop room sharing one thread C across up to `MAX_VIEWERS` viewers.
pub(super) struct Room {
    pub shared: Arc<RoomShared>,
    pub host_capabilities: Vec<String>,
    pub display: Option<Display>,
    pub h265: bool,
    /// The backend's rate model, which every viewer's QoS starts from.
    pub rate_model: RateModel,
    inner: Mutex<RoomInner>,
    exit_rx: tokio::sync::watch::Receiver<Option<WorkerExit>>,
    join: AsyncMutex<Option<tokio::task::JoinHandle<WorkerExit>>>,
}
impl Room {
    #[expect(
        clippy::too_many_arguments,
        reason = "each is a distinct fact of the backend or the first session"
    )]
    pub(super) fn new_with_first_slot(
        shared: Arc<RoomShared>,
        mut worker: WorkerHandle,
        host_capabilities: Vec<String>,
        display: Option<Display>,
        h265: bool,
        rate_model: RateModel,
        codec: VideoCodec,
        session: Arc<Session>,
    ) -> Arc<Self> {
        let (exit_tx, exit_rx) = tokio::sync::watch::channel(None);
        let join = tokio::spawn(async move {
            let exit = worker.exited().await;
            let _ = exit_tx.send(Some(exit.clone()));
            exit
        });
        let mut slots: [Option<RoomSlot>; MAX_VIEWERS] = std::array::from_fn(|_| None);
        slots[PeerSlot::FIRST.index()] = Some(RoomSlot {
            conn_id: 1,
            session,
        });
        Arc::new(Self {
            shared,
            host_capabilities,
            display,
            h265,
            rate_model,
            inner: Mutex::new(RoomInner {
                closed: false,
                codec,
                next_conn_id: 2,
                slots,
            }),
            exit_rx,
            join: AsyncMutex::new(Some(join)),
        })
    }

    pub(super) fn is_closed(&self) -> bool {
        self.inner.lock().closed || self.exit_rx.borrow().is_some()
    }

    pub(super) fn is_empty(&self) -> bool {
        self.inner.lock().slots.iter().all(Option::is_none)
    }

    pub(super) fn current_codec(&self) -> VideoCodec {
        self.inner.lock().codec
    }

    pub(super) fn live_sessions(&self) -> Vec<(u64, Arc<Session>)> {
        self.inner
            .lock()
            .slots
            .iter()
            .flatten()
            .map(|slot| (slot.conn_id, slot.session.clone()))
            .collect()
    }

    pub(super) fn end_all(&self, reason: StopReason) {
        let message = stop_message(reason);
        for (_, session) in self.live_sessions() {
            session.end(reason, message);
        }
    }

    /// Pick a codec every live viewer in the room and the newcomer can decode,
    /// returning any live sessions whose current codec differs.
    pub(super) fn negotiate_codec(
        &self,
        offered_codecs: &[VideoCodec],
        offered_h265: bool,
    ) -> Option<(VideoCodec, Vec<Arc<Session>>)> {
        let inner = self.inner.lock();
        if inner.closed || self.exit_rx.borrow().is_some() {
            return None;
        }
        // A compatible join should preserve every existing session and grant.
        let current_supported = match inner.codec {
            VideoCodec::H265 => offered_h265 && !worker::h265_failed(),
            codec => offered_codecs.contains(&codec),
        };
        if current_supported {
            return Some((inner.codec, Vec::new()));
        }
        let can_h265 = self.h265
            && !worker::h265_failed()
            && offered_h265
            && inner
                .slots
                .iter()
                .flatten()
                .all(|slot| slot.session.offered_h265);
        let can_h264 = offered_codecs.contains(&VideoCodec::H264)
            && inner
                .slots
                .iter()
                .flatten()
                .all(|slot| slot.session.offered_codecs.contains(&VideoCodec::H264));
        let can_jpeg = offered_codecs.contains(&VideoCodec::Jpeg)
            && inner
                .slots
                .iter()
                .flatten()
                .all(|slot| slot.session.offered_codecs.contains(&VideoCodec::Jpeg));
        let common = if can_h265 {
            VideoCodec::H265
        } else if can_h264 {
            VideoCodec::H264
        } else if can_jpeg {
            VideoCodec::Jpeg
        } else {
            return None;
        };
        let incompatible = inner
            .slots
            .iter()
            .flatten()
            .filter(|slot| slot.session.codec != common)
            .map(|slot| slot.session.clone())
            .collect();
        Some((common, incompatible))
    }

    pub(super) fn set_codec(&self, codec: VideoCodec) {
        self.inner.lock().codec = codec;
        self.shared.set_codec(codec);
        self.shared.wake.notify();
    }

    pub(super) fn free_slot(&self) -> Option<PeerSlot> {
        let inner = self.inner.lock();
        if inner.closed || self.exit_rx.borrow().is_some() {
            return None;
        }
        (0..MAX_VIEWERS)
            .find(|&idx| inner.slots[idx].is_none())
            .and_then(PeerSlot::new)
    }

    pub(super) fn attach(
        &self,
        slot: PeerSlot,
        session: Arc<Session>,
        limits: VideoLimits,
    ) -> Option<u64> {
        let conn_id = {
            let mut inner = self.inner.lock();
            if inner.closed || inner.slots[slot.index()].is_some() {
                return None;
            }
            let conn_id = inner.next_conn_id;
            inner.next_conn_id += 1;
            inner.slots[slot.index()] = Some(RoomSlot {
                conn_id,
                session: session.clone(),
            });
            conn_id
        };
        let color = PeerColor::new(slot.index()).unwrap_or_default();
        self.shared.set_slot(
            slot,
            conn_id,
            RoomSlotConfig {
                shared: session.shared.clone(),
                color,
                limits,
                cursor_shapes: session.cursor_shape,
                peer_cursors: session.peer_cursors,
            },
        );
        self.shared.wake.notify();
        Some(conn_id)
    }

    /// Detach `(slot, conn_id)`. Returns whether the room now holds zero slots.
    pub(super) fn detach(&self, slot: PeerSlot, conn_id: u64) -> bool {
        let empty = {
            let mut inner = self.inner.lock();
            if inner.slots[slot.index()]
                .as_ref()
                .is_some_and(|held| held.conn_id == conn_id)
            {
                inner.slots[slot.index()] = None;
            }
            let empty = inner.slots.iter().all(Option::is_none);
            if empty {
                inner.closed = true;
            }
            empty
        };
        self.shared.clear_slot(slot, conn_id);
        self.shared.wake.notify();
        empty
    }

    pub(super) async fn shutdown(&self) {
        self.inner.lock().closed = true;
        self.shared.cancel.cancel();
        self.shared.wake.notify();
        if let Some(join) = self.join.lock().await.take() {
            let _ = join.await;
        }
    }

    pub(super) async fn wait_for_session(&self, session_cancel: CancellationToken) -> WorkerExit {
        let mut exit_rx = self.exit_rx.clone();
        if let Some(exit) = exit_rx.borrow().clone() {
            return exit;
        }
        tokio::select! {
            biased;
            found = exit_rx.wait_for(Option::is_some) => {
                found
                    .ok()
                    .and_then(|guard| guard.clone())
                    .unwrap_or(WorkerExit::Cancelled)
            }
            () = session_cancel.cancelled() => WorkerExit::Cancelled,
        }
    }
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
    /// The display-mode restore retry, when the launcher journals; it ends
    /// on `cancel`, and dropping the host aborts it.
    restore: Option<tokio::task::JoinHandle<()>>,
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
        let listener = pb_mapper_core::listener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))?;
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
            .map_err(io::Error::other)?;
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
            room: Mutex::new(None),
            stopped: AtomicBool::new(false),
            registration,
            // What a host of this process left changed, replayed at once:
            // a restart too starts here.
            restore_pending: AtomicBool::new(true),
            restore: tokio::sync::Notify::new(),
            changes: ChangeSignal::new(),
            closing_rate: Mutex::new(None),
        });
        let cancel = CancellationToken::new();
        let restore = launch
            .recovery()
            .map(|recovery| tokio::spawn(restore_retry(shared.clone(), recovery, cancel.clone())));
        let acceptor = Arc::new(Acceptor {
            tls,
            token: invitation.token_text(),
            launch: launch.clone(),
            admission: admission.clone(),
            shared: shared.clone(),
            room: AsyncMutex::new(None),
            slot: Arc::new(Semaphore::new(MAX_VIEWERS)),
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
            restore,
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
        let invalid = |failure: io::Error| io::Error::new(io::ErrorKind::InvalidInput, failure);
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
    /// Wakes, carrying nothing, when [`Self::status`] may read otherwise: a
    /// viewer arrived or left; a session's state, control, display, pause,
    /// quality or reported path changed; control was allowed or withdrawn;
    /// or hosting stopped. Each wake comes after the change, so a status
    /// read then shows it, and a burst wakes it once. What moves on its
    /// own is not announced: a viewer's `since_ms`, and the path of one
    /// that never reported its own. A waiting host bumps nothing until one
    /// of these happens, and a bump that nothing waits for only counts. It
    /// ends, `changed` returning false, once the host and everything it ran
    /// are gone; it never keeps the host alive.
    pub fn changes(&self) -> HostChanges {
        self.shared.changes.subscribe()
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
        if let Some(room) = self.shared.room.lock().clone() {
            room.end_all(StopReason::HostEnded);
        }
    }
    /// Put this computer's display back in its own mode, which a viewer
    /// changed: at once in the live session, whether or not it controls
    /// (refused only on the secure desktop or while a change settles, which
    /// the viewer hears), else by replaying the journal. False when there is
    /// nothing to ask: no session, and no journal this host replays.
    pub fn restore_display_mode(&self) -> bool {
        if let Some(room) = self.shared.room.lock().clone()
            && !room.is_empty()
        {
            room.shared.request_restore();
            return true;
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
        if let Some(room) = self.shared.room.lock().clone() {
            room.shared.wake.notify();
        }
        self.shared.changes.bump();
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
        if let Some(room) = self.shared.room.lock().clone() {
            room.end_all(reason);
        }
        self.cancel.cancel();
        let mut join = self.join.lock().await;
        if let Some(task) = join.as_mut() {
            let _ = task.await;
        }
        join.take();
        let stopped = self.shared.registration.stop().await;
        if stopped.is_ok() {
            self.shared.stopped.store(true, Ordering::Release);
            self.last.send_replace(Some(self.shared.status()));
        }
        // Control is withdrawn even when the relay did not take the stop.
        self.shared.changes.bump();
        stopped.map_err(io::Error::other)
    }
}
impl Drop for Host {
    fn drop(&mut self) {
        self.shared.policy.set(false);
        self.cancel.cancel();
        // A replay already running finishes on its own blocking thread; the
        // task that waits for it lets go of the host now.
        if let Some(restore) = &self.restore {
            restore.abort();
        }
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
    pub(super) fn has_live_room(&self) -> bool {
        self.room
            .lock()
            .as_ref()
            .is_some_and(|room| !room.is_empty())
    }

    pub(super) fn clear_room(&self, target: &Arc<Room>) {
        let mut held = self.room.lock();
        if held.as_ref().is_some_and(|cur| Arc::ptr_eq(cur, target)) {
            held.take();
        }
    }

    fn status(&self) -> HostStatus {
        let control_allowed = self.policy.allowed();
        let room = self.room.lock().clone();
        let sessions = room
            .as_ref()
            .map_or_else(Vec::new, |room| room.live_sessions());
        let (Some(room), false) = (room, sessions.is_empty()) else {
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
                viewers: Vec::new(),
                display: None,
                desktop: DesktopKind::Ordinary,
                capabilities: self.fallback.clone(),
                display_mode: None,
            };
        };
        let fallback_path = self.path();
        let mut entries = Vec::with_capacity(sessions.len());
        for (conn_id, session) in sessions {
            let record = session.shared.state.borrow().record.clone();
            let paused = session.paused();
            let viewer = HostViewer {
                since_ms: pb_mapper_core::time::saturating_millis(session.shared.epoch.elapsed()),
                started_unix_ms: session.started_unix_ms,
                path: session.reported_path().unwrap_or(fallback_path),
                controlling: control_allowed && record.controlling,
                paused,
                quality: session.quality(),
            };
            entries.push((conn_id, record, viewer));
        }
        entries.sort_by_key(|(conn_id, _, viewer)| (!viewer.controlling, viewer.paused, *conn_id));
        let display = entries
            .iter()
            .find_map(|(_, record, _)| record.display.clone())
            .or_else(|| room.display.clone());
        let display_mode = entries
            .iter()
            .find_map(|(_, record, _)| record.modes.clone().filter(ModeList::changed));
        let Some((_, primary_record, primary_viewer)) = entries.first().cloned() else {
            return HostStatus {
                state: HostState::Waiting,
                reason: None,
                message: String::new(),
                control_allowed,
                viewer: None,
                viewers: Vec::new(),
                display: None,
                desktop: DesktopKind::Ordinary,
                capabilities: self.fallback.clone(),
                display_mode: None,
            };
        };
        let viewers: Vec<HostViewer> = entries.into_iter().map(|(_, _, viewer)| viewer).collect();
        HostStatus {
            state: match primary_record.state {
                _ if primary_viewer.paused => HostState::Paused,
                SessionState::Connecting => HostState::Connecting,
                SessionState::Streaming => HostState::Streaming,
                SessionState::Paused => HostState::Paused,
                SessionState::CaptureUnavailable => HostState::CaptureUnavailable,
            },
            reason: primary_record.reason,
            message: primary_record.message,
            control_allowed,
            viewer: Some(primary_viewer),
            viewers,
            display,
            desktop: primary_record.desktop,
            capabilities: room.host_capabilities.clone(),
            display_mode,
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
    let mut backoff = RetryBackoff::new(RESTORE_RETRY, RESTORE_RETRY_MAX);
    let mut wait = backoff.next_delay();
    let mut warned = None;
    loop {
        if shared.restore_pending.load(Ordering::Acquire) && !shared.has_live_room() {
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
                Err(failure) if failure.is_panic() => tracing::warn!(
                    target: "pb_mapper_desktop::session",
                    event = "display_mode_restore_panicked",
                    detail = %failure,
                ),
                // Cancelled: the runtime is shutting down.
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
            biased;
            () = cancel.cancelled() => return,
            () = shared.restore.notified() => {
                backoff.reset();
                wait = backoff.next_delay();
            }
            () = retry => wait = backoff.next_delay(),
        }
    }
}
