//! Setup coordination is serialized by the existing routing manager. Established
//! direct sessions are observations only and never acquire an authorization lease.
use super::{ConnTask, ConnTaskSender, ImutableKey, ServerConnHealth, ServerConnMap};
use pb_mapper_auth::AuthContext;
use pb_mapper_core::conn_id::RemoteConnId;
use pb_mapper_protocol::direct::*;
use std::{
    collections::HashMap,
    net::SocketAddr,
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, oneshot};

#[cfg(feature = "p2p")]
mod stun;

#[derive(Clone, Debug)]
pub struct DirectServerConfig {
    /// UDP address the STUN service binds.
    pub listen: SocketAddr,
    /// Public address announced to clients. Port 0 takes the bound port. An
    /// unspecified IP announces only the port: each client then uses the relay
    /// IP it dialed, which is right whenever the UDP port is forwarded like the
    /// TCP one, including behind cloud 1:1 NAT where the host never sees its
    /// public IP.
    pub advertise: SocketAddr,
}

pub(crate) async fn bind(
    config: Option<DirectServerConfig>,
) -> std::io::Result<(
    Option<DirectCapability>,
    Option<tokio_util::task::AbortOnDropHandle<()>>,
)> {
    let Some(config) = config else {
        return Ok((None, None));
    };
    #[cfg(not(feature = "p2p"))]
    {
        let _ = config;
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "binary built without p2p",
        ))
    }
    #[cfg(feature = "p2p")]
    {
        let socket = tokio::net::UdpSocket::bind(config.listen).await?;
        let mut advertise = config.advertise;
        if advertise.port() == 0 {
            advertise.set_port(socket.local_addr()?.port());
        }
        if !advertise.ip().is_unspecified() && !valid_candidate(&advertise) {
            return Err(std::io::Error::other(
                "direct advertise address must be reachable unicast IP:port",
            ));
        }
        let capability = DirectCapability {
            version: DIRECT_VERSION,
            stun_addr: advertise,
        };
        if advertise.ip().is_unspecified() {
            tracing::info!(
                event = "direct_service_listening",
                listen = %socket.local_addr()?,
                advertise_port = advertise.port(),
                "direct coordination announces its port; clients pair it with the relay address they dialed"
            );
        } else {
            tracing::info!(event = "direct_service_listening", listen = %socket.local_addr()?, %advertise);
        }
        let task = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
            if let Err(error) = stun::serve(socket).await {
                tracing::error!(%error, "STUN service stopped");
            }
        }));
        Ok((Some(capability), Some(task)))
    }
}

pub type RelaySelection = (ImutableKey, RemoteConnId, u64);

fn conn_task(event: DirectEvent) -> ConnTask {
    ConnTask::Direct(Box::new(event))
}

pub enum DirectTask {
    Connect {
        connection: RemoteConnId,
        namespace: u64,
        auth: AuthContext,
        sender: mpsc::Sender<DirectEvent>,
    },
    Client {
        connection: RemoteConnId,
        message: DirectClientMessage,
    },
    Provider {
        connection: RemoteConnId,
        key: ImutableKey,
        message: DirectProviderMessage,
    },
    Disconnect {
        connection: RemoteConnId,
    },
    Relay {
        auth: AuthContext,
        namespace: u64,
        id: SessionId,
        secret: SessionSecret,
        response: oneshot::Sender<Option<RelaySelection>>,
    },
}

struct Consumer {
    namespace: u64,
    auth: AuthContext,
    sender: mpsc::Sender<DirectEvent>,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Offered,
    Granted,
    SelectingRelay,
    RelaySelected,
    Attached,
    Closed,
}
struct Entry {
    request: u64,
    consumer: RemoteConnId,
    provider: RemoteConnId,
    provider_auth: AuthContext,
    key: ImutableKey,
    grant: SessionGrant,
    phase: Phase,
    deadline: Instant,
}
impl Entry {
    fn pending(&self) -> bool {
        matches!(
            self.phase,
            Phase::Offered | Phase::Granted | Phase::SelectingRelay | Phase::RelaySelected
        )
    }
}

pub(crate) struct DirectState {
    pub capability: Option<DirectCapability>,
    instance: SessionId,
    consumers: HashMap<RemoteConnId, Consumer>,
    entries: HashMap<SessionId, Entry>,
    fallback_credits: FallbackCredits,
    observations: HashMap<(SessionId, SessionId), Observation>,
}

/// Rate tokens charged for direct setups that ended before a grant. The
/// consumer then falls back to an ordinary subscription of the same service,
/// which spends one of these instead of a second token; capacity is still
/// checked. An unused credit expires with the setup deadline it came from.
#[derive(Default)]
struct FallbackCredits {
    by_key: HashMap<ImutableKey, Vec<Instant>>,
    total: usize,
}
impl FallbackCredits {
    const CAPACITY: usize = 4096;
    fn add(&mut self, key: ImutableKey, until: Instant) {
        if self.total < Self::CAPACITY {
            self.by_key.entry(key).or_default().push(until);
            self.total += 1;
        }
    }
    fn take(&mut self, key: &ImutableKey) -> bool {
        let Some(credits) = self.by_key.get_mut(key) else {
            return false;
        };
        let held = credits.len();
        let now = Instant::now();
        credits.retain(|until| *until > now);
        let taken = credits.pop().is_some();
        self.total -= held - credits.len();
        if credits.is_empty() {
            self.by_key.remove(key);
        }
        taken
    }
    fn sweep(&mut self) {
        let now = Instant::now();
        self.by_key.retain(|_, credits| {
            credits.retain(|until| *until > now);
            !credits.is_empty()
        });
        self.total = self.by_key.values().map(Vec::len).sum();
    }
}

impl DirectState {
    pub fn new(capability: Option<DirectCapability>) -> Self {
        let mut instance = [0; 16];
        if capability.is_some() {
            instance = random_session_id();
        }
        Self {
            capability,
            instance,
            consumers: HashMap::new(),
            entries: HashMap::new(),
            fallback_credits: FallbackCredits::default(),
            observations: HashMap::new(),
        }
    }
    /// Spend a credit left by a direct setup that ended before its grant.
    pub fn take_fallback_credit(&mut self, key: &ImutableKey) -> bool {
        self.fallback_credits.take(key)
    }
    pub fn pending_in(&self, namespace: u64) -> usize {
        self.entries
            .values()
            .filter(|e| e.grant.namespace == namespace && e.pending())
            .count()
    }
    fn send(&self, connection: RemoteConnId, event: DirectEvent) -> bool {
        self.consumers
            .get(&connection)
            .is_some_and(|c| c.sender.try_send(event).is_ok())
    }
    pub fn connect(
        &mut self,
        connection: RemoteConnId,
        namespace: u64,
        auth: AuthContext,
        sender: mpsc::Sender<DirectEvent>,
    ) {
        if let Some(capability) = &self.capability
            && self.consumers.len() < 1024
            && auth.ensure_active().is_ok()
            && sender
                .try_send(DirectEvent::Ready(capability.clone()))
                .is_ok()
        {
            self.consumers.insert(
                connection,
                Consumer {
                    namespace,
                    auth,
                    sender,
                },
            );
        }
    }
    #[allow(clippy::too_many_arguments)]
    pub fn open(
        &mut self,
        connection: RemoteConnId,
        request: u64,
        key: String,
        is_datagram: bool,
        peer: PeerInfo,
        registrations: &ServerConnMap,
        route: impl Fn(RemoteConnId) -> Option<ConnTaskSender>,
        mut admit: impl FnMut(u64, usize) -> bool,
    ) {
        let Some(consumer) = self.consumers.get(&connection) else {
            return;
        };
        if consumer.auth.ensure_active().is_err()
            || !peer.valid()
            || key.is_empty()
            || key.len() > 1024
            || self.entries.len() >= 4096
        {
            self.send(
                connection,
                DirectEvent::Rejected {
                    request,
                    reason: "direct setup is invalid or capacity exhausted".into(),
                },
            );
            return;
        }
        let Ok(key) =
            super::connection::scoped_service_key(&consumer.auth, consumer.namespace, &key)
        else {
            self.send(
                connection,
                DirectEvent::Rejected {
                    request,
                    reason: "invalid service name".into(),
                },
            );
            return;
        };
        // Newest first, like the relay. A relay-only or revoked registration
        // must not hide an older direct-capable one of the same service.
        let selected = self.capability.as_ref().and_then(|capability| {
            registrations.get(&key)?.iter().rev().find_map(|provider| {
                let auth = provider.direct.as_ref()?;
                if provider.health != ServerConnHealth::Healthy
                    || provider.is_datagram != is_datagram
                    || auth.ensure_active().is_err()
                {
                    return None;
                }
                Some((capability, provider, auth, route(provider.conn_id)?))
            })
        });
        let Some((capability, provider, provider_auth, sender)) = selected else {
            self.send(connection, DirectEvent::LegacyRelay { request });
            return;
        };
        // Legacy fallback is charged by the ordinary subscription path. Only
        // consume a setup token once a direct-capable provider has been found.
        if !admit(consumer.namespace, self.pending_in(consumer.namespace)) {
            self.send(
                connection,
                DirectEvent::Rejected {
                    request,
                    reason: "direct setup capacity exhausted".into(),
                },
            );
            return;
        }
        let id = random_session_id();
        let grant = SessionGrant {
            id,
            server_instance: self.instance,
            generation: provider.generation,
            namespace: consumer.namespace,
            service: super::split_scoped_service_key(&key).1.into(),
            is_datagram,
            consumer_key: consumer.auth.key_id.as_u64(),
            provider_key: provider_auth.key_id.as_u64(),
            consumer: peer,
            provider: PeerInfo {
                certificate: vec![],
                candidates: vec![],
            },
            secret: SessionSecret::random(),
            ttl_ms: SETUP_TTL_MS,
        };
        if sender
            .try_send(conn_task(DirectEvent::Offer {
                grant: grant.clone(),
                stun_addr: capability.stun_addr,
            }))
            .is_err()
        {
            let until = Instant::now() + Duration::from_millis(SETUP_TTL_MS);
            self.fallback_credits.add(key, until);
            self.send(
                connection,
                DirectEvent::Rejected {
                    request,
                    reason: "provider coordination queue full".into(),
                },
            );
            return;
        }
        self.entries.insert(
            id,
            Entry {
                request,
                consumer: connection,
                provider: provider.conn_id,
                provider_auth: provider_auth.clone(),
                key,
                grant,
                phase: Phase::Offered,
                deadline: Instant::now() + Duration::from_millis(SETUP_TTL_MS),
            },
        );
    }
    pub fn client(
        &mut self,
        connection: RemoteConnId,
        message: DirectClientMessage,
        route: impl Fn(RemoteConnId) -> Option<ConnTaskSender>,
    ) {
        let Some(consumer) = self.consumers.get(&connection) else {
            return;
        };
        if consumer.auth.ensure_active().is_err() {
            return;
        }
        if matches!(message, DirectClientMessage::Ping) {
            self.send(connection, DirectEvent::Pong);
            return;
        }
        if let DirectClientMessage::CancelOpen { request } = message {
            for entry in self
                .entries
                .values_mut()
                .filter(|e| e.consumer == connection && e.request == request && e.pending())
            {
                if entry.phase == Phase::Offered {
                    self.fallback_credits.add(entry.key.clone(), entry.deadline);
                }
                entry.phase = Phase::Closed;
                if let Some(sender) = route(entry.provider) {
                    let _ = sender.try_send(conn_task(DirectEvent::Cancel { id: entry.grant.id }));
                }
            }
            return;
        }
        if let DirectClientMessage::Report(update) = message {
            let auth = consumer.auth.clone();
            let namespace = consumer.namespace;
            self.observe(&auth, namespace, update, true);
            return;
        }
        let id = match &message {
            DirectClientMessage::RelaySelect { id } | DirectClientMessage::Cancel { id } => *id,
            _ => return,
        };
        let Some(entry) = self
            .entries
            .get_mut(&id)
            .filter(|e| e.consumer == connection)
        else {
            return;
        };
        match message {
            DirectClientMessage::RelaySelect { .. }
                if entry.phase == Phase::Granted && Instant::now() < entry.deadline =>
            {
                entry.phase = Phase::SelectingRelay;
                if let Some(sender) = route(entry.provider) {
                    let _ = sender.try_send(conn_task(DirectEvent::RelaySelect { id }));
                }
            }
            DirectClientMessage::RelaySelect { .. } if entry.phase == Phase::RelaySelected => {
                self.send(connection, DirectEvent::RelaySelected { id });
            }
            DirectClientMessage::Cancel { .. } if entry.pending() => {
                entry.phase = Phase::Closed;
                if let Some(sender) = route(entry.provider) {
                    let _ = sender.try_send(conn_task(DirectEvent::Cancel { id }));
                }
            }
            _ => {}
        }
    }
    pub fn provider(
        &mut self,
        connection: RemoteConnId,
        key: &str,
        message: DirectProviderMessage,
        auth: Option<AuthContext>,
        route: impl Fn(RemoteConnId) -> Option<ConnTaskSender>,
    ) {
        if let DirectProviderMessage::Report(update) = message {
            if let Some(auth) = auth {
                let (namespace, service) = super::split_scoped_service_key(key);
                if service == update.session.service {
                    self.observe(&auth, namespace, update, false);
                }
            }
            return;
        }
        let id = match &message {
            DirectProviderMessage::Accept { id, .. }
            | DirectProviderMessage::Reject { id, .. }
            | DirectProviderMessage::RelaySelected { id } => *id,
            DirectProviderMessage::Report(_) => return,
        };
        let Some(entry) = self.entries.get_mut(&id).filter(|e| {
            e.provider == connection
                && e.key.as_ref() == key
                && e.provider_auth.ensure_active().is_ok()
        }) else {
            return;
        };
        let consumer = entry.consumer;
        match message {
            DirectProviderMessage::Accept { peer, .. }
                if entry.phase == Phase::Offered
                    && Instant::now() < entry.deadline
                    && peer.valid() =>
            {
                entry.grant.provider = peer;
                entry.grant.ttl_ms = entry
                    .deadline
                    .saturating_duration_since(Instant::now())
                    .as_millis() as u64;
                entry.phase = Phase::Granted;
                let event = DirectEvent::Granted {
                    request: entry.request,
                    grant: entry.grant.clone(),
                };
                if let Some(sender) = route(connection) {
                    let _ = sender.try_send(conn_task(event.clone()));
                }
                self.send(consumer, event);
            }
            DirectProviderMessage::Reject { reason, .. } if entry.pending() => {
                if entry.phase == Phase::Offered {
                    self.fallback_credits.add(entry.key.clone(), entry.deadline);
                }
                entry.phase = Phase::Closed;
                let request = entry.request;
                self.send(
                    consumer,
                    DirectEvent::Rejected {
                        request,
                        reason: reason.chars().take(256).collect(),
                    },
                );
            }
            DirectProviderMessage::RelaySelected { .. } if entry.phase == Phase::SelectingRelay => {
                entry.phase = Phase::RelaySelected;
                self.send(consumer, DirectEvent::RelaySelected { id });
            }
            _ => {}
        }
    }
    pub fn relay(
        &mut self,
        auth: &AuthContext,
        namespace: u64,
        id: SessionId,
        secret: SessionSecret,
    ) -> Option<RelaySelection> {
        let entry = self.entries.get_mut(&id)?;
        if entry.phase != Phase::RelaySelected
            || Instant::now() >= entry.deadline
            || auth.ensure_active().is_err()
            || entry.provider_auth.ensure_active().is_err()
            || auth.key_id.as_u64() != entry.grant.consumer_key
            || namespace != entry.grant.namespace
            || secret != entry.grant.secret
        {
            return None;
        }
        entry.phase = Phase::Attached;
        Some((entry.key.clone(), entry.provider, entry.grant.generation))
    }
    pub fn disconnect(
        &mut self,
        connection: RemoteConnId,
        route: impl Fn(RemoteConnId) -> Option<ConnTaskSender>,
    ) {
        self.consumers.remove(&connection);
        for entry in self
            .entries
            .values_mut()
            .filter(|e| e.consumer == connection && e.pending())
        {
            if entry.phase == Phase::Offered {
                self.fallback_credits.add(entry.key.clone(), entry.deadline);
            }
            entry.phase = Phase::Closed;
            if let Some(sender) = route(entry.provider) {
                let _ = sender.try_send(conn_task(DirectEvent::Cancel { id: entry.grant.id }));
            }
        }
    }
    pub fn sweep(&mut self, route: impl Fn(RemoteConnId) -> Option<ConnTaskSender>) {
        for entry in self.entries.values_mut() {
            if entry.pending() && Instant::now() >= entry.deadline {
                entry.phase = Phase::Closed;
                if let Some(sender) = route(entry.provider) {
                    let _ = sender.try_send(conn_task(DirectEvent::Cancel { id: entry.grant.id }));
                }
            }
        }
        self.entries
            .retain(|_, entry| entry.deadline > Instant::now());
        self.fallback_credits.sweep();
        self.observations
            .retain(|_, observation| observation.touched.elapsed() < Duration::from_secs(120));
    }
    fn observe(
        &mut self,
        auth: &AuthContext,
        namespace: u64,
        update: SessionUpdate,
        consumer: bool,
    ) {
        let SessionUpdate {
            session,
            secret,
            mut report,
        } = update;
        if auth.ensure_active().is_err()
            || session.namespace != namespace
            || session.id != report.id
            || auth.key_id.as_u64()
                != if consumer {
                    session.consumer_key
                } else {
                    session.provider_key
                }
            || session.service.len() > 1024
            || super::connection::scoped_service_key(auth, namespace, &session.service).is_err()
        {
            return;
        }
        let identity = (session.server_instance, session.id);
        if !self.observations.contains_key(&identity) {
            let known = self
                .entries
                .get(&session.id)
                .filter(|_| session.server_instance == self.instance);
            if let Some(entry) = known
                && (secret != entry.grant.secret || session != SessionSummary::from(&entry.grant))
            {
                return;
            }
            // A control outage can outlive both the setup and observation TTL
            // without interrupting direct data. Restore an authenticated claim
            // even in this incarnation, but never recreate setup authority.
            let restored = known.is_none();
            if !self.make_observation_room(namespace) {
                return;
            }
            self.observations.insert(
                identity,
                Observation {
                    restored,
                    session: session.clone(),
                    secret,
                    consumer: None,
                    provider: None,
                    touched: Instant::now(),
                },
            );
        }
        let Some(observation) = self.observations.get_mut(&identity) else {
            return;
        };
        if observation.secret != secret || observation.session != session {
            return;
        }
        let slot = if consumer {
            &mut observation.consumer
        } else {
            &mut observation.provider
        };
        if slot.as_ref().is_some_and(|(old, _)| old.seq >= report.seq) {
            return;
        }
        report.reason = report.reason.map(|r| r.chars().take(256).collect());
        observation.touched = Instant::now();
        *slot = Some((report, observation.touched));
    }
    fn make_observation_room(&mut self, namespace: u64) -> bool {
        let namespace_full = self
            .observations
            .values()
            .filter(|o| o.session.namespace == namespace)
            .count()
            >= 1024;
        if !namespace_full && self.observations.len() < 4096 {
            return true;
        }
        // Retention is a history budget, not a reason to hide new active
        // sessions. Reclaim the oldest closed observation in the constrained
        // scope, preserving active/conflicting reports and namespace isolation.
        let oldest = self
            .observations
            .iter()
            .filter(|(_, o)| !namespace_full || o.session.namespace == namespace)
            .filter(|(_, o)| o.closed())
            .min_by_key(|(_, o)| o.touched)
            .map(|(id, _)| *id);
        if let Some(id) = oldest {
            self.observations.remove(&id);
            true
        } else {
            false
        }
    }
    pub fn observations(&self, namespace: u64) -> Vec<SessionObservation> {
        self.observations
            .values()
            .filter(|o| o.session.namespace == namespace)
            .map(|o| {
                let observed = |r: &Option<(SessionReport, Instant)>| {
                    r.as_ref().map(|(report, at)| ObservedReport {
                        report: report.clone(),
                        age_ms: at.elapsed().as_millis() as u64,
                    })
                };
                let consumer = observed(&o.consumer);
                let provider = observed(&o.provider);
                let stale = [&consumer, &provider]
                    .into_iter()
                    .any(|r| r.as_ref().is_none_or(|r| r.age_ms > 45_000));
                let conflicting = consumer
                    .as_ref()
                    .zip(provider.as_ref())
                    .is_some_and(|(c, p)| {
                        c.report.path != p.report.path || c.report.state != p.report.state
                    });
                SessionObservation {
                    server_instance: o.session.server_instance,
                    restored: o.restored,
                    id: o.session.id,
                    service: o.session.service.clone(),
                    is_datagram: o.session.is_datagram,
                    consumer,
                    provider,
                    stale,
                    conflicting,
                }
            })
            .collect()
    }
}
struct Observation {
    restored: bool,
    session: SessionSummary,
    secret: SessionSecret,
    consumer: Option<(SessionReport, Instant)>,
    provider: Option<(SessionReport, Instant)>,
    touched: Instant,
}
impl Observation {
    fn closed(&self) -> bool {
        (self.consumer.is_some() || self.provider.is_some())
            && [&self.consumer, &self.provider]
                .into_iter()
                .flatten()
                .all(|(report, _)| report.state == SessionState::Closed)
    }
}

struct ControlGuard {
    sender: super::ManagerTaskSender,
    connection: RemoteConnId,
}
impl Drop for ControlGuard {
    fn drop(&mut self) {
        let sender = self.sender.clone();
        let connection = self.connection;
        // Cleanup owns no sockets and is fenced by the existing connection incarnation.
        tokio::spawn(async move {
            let _ = sender
                .send(super::ManagerTask::Direct(DirectTask::Disconnect {
                    connection,
                }))
                .await;
            let _ = sender
                .send(super::ManagerTask::DeRegisterClientConn {
                    server_id: None,
                    client_id: connection,
                })
                .await;
        });
    }
}

pub(crate) async fn handle_control(
    connection: RemoteConnId,
    namespace: u64,
    socket: tokio::net::TcpStream,
    session: pb_mapper_protocol::secure::ServerHeaderSession,
    sender: super::ManagerTaskSender,
) -> std::io::Result<()> {
    use pb_mapper_protocol::{
        MessageReader, MessageWriter,
        command::{MessageSerializer, PbConnResponse},
    };
    let io = |error: pb_mapper_core::error::Error| std::io::Error::other(error.to_string());
    let auth = session.context().map_err(io)?.clone();
    let cancellation = auth
        .cancellation_token()
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    let (events, mut rx) = mpsc::channel(64);
    let _guard = ControlGuard {
        sender: sender.clone(),
        connection,
    };
    sender
        .send(super::ManagerTask::Direct(DirectTask::Connect {
            connection,
            namespace,
            auth,
            sender: events,
        }))
        .await
        .map_err(|_| std::io::Error::other("router closed"))?;
    let (mut reader, mut writer) = socket.into_split();
    let mut reader = session.continuation_reader(&mut reader).map_err(io)?;
    let mut writer = session.response_writer(&mut writer).map_err(io)?;
    let read = async {
        loop {
            let msg = tokio::time::timeout(Duration::from_secs(45), reader.read_msg())
                .await
                .map_err(|_| std::io::Error::other("direct control idle"))?
                .map_err(io)?;
            let message = DirectClientMessage::decode(msg).map_err(io)?;
            sender
                .send(super::ManagerTask::Direct(DirectTask::Client {
                    connection,
                    message,
                }))
                .await
                .map_err(|_| std::io::Error::other("router closed"))?;
        }
        #[allow(unreachable_code)]
        Ok::<(), std::io::Error>(())
    };
    let write = async {
        while let Some(event) = rx.recv().await {
            let bytes = PbConnResponse::Direct(event).encode().map_err(io)?;
            tokio::time::timeout(Duration::from_secs(5), writer.write_msg(&bytes))
                .await
                .map_err(|_| std::io::Error::other("direct control write timed out"))?
                .map_err(io)?;
        }
        Ok::<(), std::io::Error>(())
    };
    tokio::select! { result = read => result, result = write => result, () = cancellation.cancelled() => Ok(()) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pb_mapper_auth::{ADMIN_KEY_ID, AuthConfig, AuthRuntime};

    #[tokio::test]
    async fn relay_grants_are_scoped_single_use_and_observations_cannot_authorize_them() {
        let key = [17; 32];
        let dir = std::env::temp_dir().join(format!(
            "pb-direct-state-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let auth = AuthRuntime::start(
            key,
            AuthConfig {
                state_dir: dir.clone(),
                ..AuthConfig::default()
            },
        )
        .await
        .unwrap();
        let admin = auth.authenticate_presented(ADMIN_KEY_ID, &key).unwrap();
        let mut state = DirectState::new(Some(DirectCapability {
            version: DIRECT_VERSION,
            stun_addr: "127.0.0.1:10811".parse().unwrap(),
        }));
        let grant = SessionGrant {
            id: random_session_id(),
            server_instance: state.instance,
            generation: 7,
            namespace: 0,
            service: "echo".into(),
            is_datagram: false,
            consumer_key: 0,
            provider_key: 0,
            consumer: PeerInfo {
                certificate: vec![1],
                candidates: vec![],
            },
            provider: PeerInfo {
                certificate: vec![2],
                candidates: vec![],
            },
            secret: SessionSecret::random(),
            ttl_ms: SETUP_TTL_MS,
        };
        let (sender, mut events) = mpsc::channel(8);
        state.connect(1.into(), 0, admin.clone(), sender);
        assert!(matches!(events.recv().await, Some(DirectEvent::Ready(_))));
        state.open(
            1.into(),
            1,
            "echo".into(),
            false,
            grant.consumer.clone(),
            &ServerConnMap::default(),
            |_| None,
            |_, _| panic!("legacy fallback must leave admission to the ordinary subscription"),
        );
        assert!(matches!(
            events.recv().await,
            Some(DirectEvent::LegacyRelay { request: 1 })
        ));
        // A newer relay-only registration must not hide an older direct one.
        let registration =
            |conn_id: u32, direct: Option<AuthContext>| super::super::ServerConnInfo {
                direct,
                conn_id: conn_id.into(),
                generation: u64::from(conn_id),
                health: ServerConnHealth::Healthy,
                need_codec: false,
                is_datagram: false,
                protocol_version: 2,
                last_rx_at: Instant::now(),
                stream_ack_budget: Duration::from_secs(1),
                suspect_check: None,
                retire_token: tokio_util::sync::CancellationToken::new(),
            };
        let mut registrations = ServerConnMap::default();
        registrations.insert(
            "echo".into(),
            vec![registration(3, Some(admin.clone())), registration(4, None)],
        );
        let (provider, offers) = kanal::bounded_async(1);
        state.open(
            1.into(),
            2,
            "echo".into(),
            false,
            grant.consumer.clone(),
            &registrations,
            |id| (id == 3.into()).then(|| provider.clone()),
            |_, _| true,
        );
        let Ok(Some(ConnTask::Direct(offer))) = offers.try_recv() else {
            panic!("the direct-capable registration must receive the offer");
        };
        let DirectEvent::Offer { grant: offered, .. } = *offer else {
            panic!("expected an offer");
        };
        assert_eq!(offered.generation, 3);
        // Abandoned before its grant, the setup's token pays for the ordinary
        // subscription the consumer falls back to, exactly once.
        state.client(
            1.into(),
            DirectClientMessage::CancelOpen { request: 2 },
            |_| None,
        );
        let service = ImutableKey::from("echo");
        assert!(state.take_fallback_credit(&service));
        assert!(!state.take_fallback_credit(&service));
        state.entries.clear();
        state.entries.insert(
            grant.id,
            Entry {
                request: 1,
                consumer: 1.into(),
                provider: 2.into(),
                provider_auth: admin.clone(),
                key: "echo".into(),
                grant: grant.clone(),
                phase: Phase::Granted,
                deadline: Instant::now() + Duration::from_secs(8),
            },
        );
        let update = SessionUpdate {
            session: (&grant).into(),
            secret: grant.secret,
            report: SessionReport {
                id: grant.id,
                seq: 1,
                path: SessionPath::Direct,
                state: SessionState::Active,
                reason: None,
            },
        };
        state.observe(&admin, 1, update.clone(), true);
        assert!(
            state.observations(0).is_empty(),
            "cross-namespace reports must be rejected"
        );
        state.observe(&admin, 0, update.clone(), true);
        assert!(
            state.relay(&admin, 0, grant.id, grant.secret).is_none(),
            "a report never selects relay"
        );
        state.entries.get_mut(&grant.id).unwrap().phase = Phase::RelaySelected;
        assert!(state.relay(&admin, 1, grant.id, grant.secret).is_none());
        assert!(
            state
                .relay(&admin, 0, grant.id, SessionSecret([0; 32]))
                .is_none()
        );
        assert_eq!(state.relay(&admin, 0, grant.id, grant.secret).unwrap().2, 7);
        assert!(
            state.relay(&admin, 0, grant.id, grant.secret).is_none(),
            "relay attach cannot replay"
        );
        let mut old = update.clone();
        old.report.seq = 0;
        old.report.state = SessionState::Closed;
        state.observe(&admin, 0, old, true);
        assert_eq!(
            state.observations(0)[0]
                .consumer
                .as_ref()
                .unwrap()
                .report
                .state,
            SessionState::Active
        );
        state.entries.get_mut(&grant.id).unwrap().deadline = Instant::now();
        state.sweep(|_| None);
        assert!(
            state.entries.is_empty(),
            "expired grants must release setup capacity"
        );
        assert_eq!(
            state.observations(0).len(),
            1,
            "observations outlive setup grants"
        );
        for _ in 1..1024 {
            let mut closed = update.clone();
            closed.session.id = random_session_id();
            closed.report.id = closed.session.id;
            closed.report.state = SessionState::Closed;
            state.observe(&admin, 0, closed, true);
        }
        let mut fresh = update.clone();
        fresh.session.id = random_session_id();
        fresh.report.id = fresh.session.id;
        state.observe(&admin, 0, fresh.clone(), true);
        let observed = state.observations(0);
        assert_eq!(observed.len(), 1024);
        assert!(
            observed.iter().any(|o| o.id == fresh.session.id),
            "closed history must not hide new active reports"
        );
        assert!(
            observed.iter().any(|o| o.id == grant.id),
            "active reports must survive history eviction"
        );
        // A long control outage on the same server must recover observations
        // after retention expires, without recovering the expired attach token.
        state.entries.clear();
        state.observations.clear();
        state.observe(&admin, 0, update.clone(), true);
        assert!(state.observations(0)[0].restored);
        assert!(state.relay(&admin, 0, grant.id, grant.secret).is_none());
        let mut restarted = DirectState::new(state.capability.clone());
        restarted.observe(&admin, 0, update, true);
        let restored = restarted.observations(0);
        assert!(restored[0].restored && restored[0].stale);
        assert!(
            restarted.relay(&admin, 0, grant.id, grant.secret).is_none(),
            "restored observations never recreate setup grants"
        );
        drop(state);
        drop(restarted);
        auth.shutdown_actor().await;
        let _ = std::fs::remove_dir_all(dir);
    }
}
