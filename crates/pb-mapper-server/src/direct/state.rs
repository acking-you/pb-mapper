//! The direct setup state machine the routing manager drives: consumers, setup
//! entries, fallback credits and session observations.
use super::RelaySelection;
use crate::{ConnTask, ConnTaskSender, ImutableKey, ServerConnHealth, ServerConnMap};
use pb_mapper_auth::AuthContext;
use pb_mapper_core::conn_id::RemoteConnId;
use pb_mapper_protocol::direct::*;
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};
use tokio::sync::mpsc;

mod observe;

use observe::Observation;

fn conn_task(event: DirectEvent) -> ConnTask {
    ConnTask::Direct(Box::new(event))
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
        let Ok(key) = crate::scoped_service_key(&consumer.auth, consumer.namespace, &key) else {
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
            service: crate::split_scoped_service_key(&key).1.into(),
            is_datagram,
            consumer_key: consumer.auth.key_id.as_u64(),
            provider_key: provider_auth.key_id.as_u64(),
            consumer: peer,
            provider: PeerInfo {
                certificate: vec![],
                candidates: vec![],
                lan_tcp: None,
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
                let (namespace, service) = crate::split_scoped_service_key(key);
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use pb_mapper_auth::{ADMIN_KEY_ID, AuthConfig, AuthRuntime};

    /// Revocation has to stop one key's setups without touching its namespace's
    /// others, including a setup that was already in flight when the key died.
    #[tokio::test]
    async fn revoking_one_key_of_a_namespace_stops_only_its_direct_setups() {
        let key = [18; 32];
        let dir = std::env::temp_dir().join(format!(
            "pb-direct-namespace-{}-{}",
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
        let namespace = auth.allocate_namespace(&admin).await.unwrap();
        let ttl = Duration::from_secs(600);
        let issue = || async {
            let issued = auth
                .issue_in_namespace(&admin, namespace, ttl, None)
                .await
                .unwrap();
            let id = issued.metadata.key_id;
            auth.authenticate_presented(id, &auth.derive_key(id).unwrap())
                .unwrap()
        };
        let consumer = issue().await;
        let provider = issue().await;
        let peer = issue().await;
        assert_eq!(consumer.namespace, namespace);
        assert_ne!(consumer.key_id.as_u64(), namespace);

        let mut state = DirectState::new(Some(DirectCapability {
            version: DIRECT_VERSION,
            stun_addr: "127.0.0.1:10811".parse().unwrap(),
        }));
        let (consumer_sender, mut consumer_events) = mpsc::channel(8);
        state.connect(1.into(), namespace, consumer.clone(), consumer_sender);
        assert!(matches!(
            consumer_events.recv().await,
            Some(DirectEvent::Ready(_))
        ));
        let (peer_sender, mut peer_events) = mpsc::channel(8);
        state.connect(2.into(), namespace, peer.clone(), peer_sender);
        assert!(matches!(
            peer_events.recv().await,
            Some(DirectEvent::Ready(_))
        ));
        let mut registrations = ServerConnMap::default();
        let scoped = crate::compose_service_key(namespace, "echo");
        registrations.insert(
            scoped.clone(),
            vec![crate::ServerConnInfo {
                direct: Some(provider.clone()),
                conn_id: 3.into(),
                generation: 3,
                health: ServerConnHealth::Healthy,
                need_codec: false,
                is_datagram: false,
                protocol_version: 2,
                last_rx_at: Instant::now(),
                stream_ack_budget: Duration::from_secs(1),
                suspect_check: None,
                retire_token: tokio_util::sync::CancellationToken::new(),
            }],
        );
        let (provider_sender, offers) = kanal::bounded_async(8);
        let route = |id: RemoteConnId| (id == 3.into()).then(|| provider_sender.clone());
        let peer_info = PeerInfo {
            certificate: vec![1],
            candidates: vec![],
            lan_tcp: None,
        };

        // Both consumers reach the one provider; the grant names the namespace and
        // each key separately, since none of the three is the others any more.
        for (connection, request) in [(1, 1), (2, 2)] {
            state.open(
                connection.into(),
                request,
                "echo".into(),
                false,
                peer_info.clone(),
                &registrations,
                route,
                |_, _| true,
            );
        }
        let mut grant_for = HashMap::new();
        for _ in 0..2 {
            let Ok(Some(ConnTask::Direct(offer))) = offers.try_recv() else {
                panic!("the provider must receive an offer per consumer");
            };
            let DirectEvent::Offer { grant, .. } = *offer else {
                panic!("expected an offer");
            };
            assert_eq!(grant.namespace, namespace);
            assert_eq!(grant.provider_key, provider.key_id.as_u64());
            grant_for.insert(grant.consumer_key, grant);
        }
        let consumer_grant = grant_for.remove(&consumer.key_id.as_u64()).unwrap();
        let peer_grant = grant_for.remove(&peer.key_id.as_u64()).unwrap();
        for grant in [&consumer_grant, &peer_grant] {
            state.entries.get_mut(&grant.id).unwrap().phase = Phase::RelaySelected;
        }

        auth.revoke(&admin, consumer.key_id).await.unwrap();

        // The in-flight setup dies with its consumer's key, and a new one is refused.
        assert!(
            state
                .relay(
                    &consumer,
                    namespace,
                    consumer_grant.id,
                    consumer_grant.secret
                )
                .is_none(),
            "a revoked consumer must not attach to a relay selection"
        );
        state.open(
            1.into(),
            3,
            "echo".into(),
            false,
            peer_info.clone(),
            &registrations,
            route,
            |_, _| panic!("a revoked consumer must not reach admission"),
        );
        assert!(matches!(
            consumer_events.recv().await,
            Some(DirectEvent::Rejected { request: 3, .. })
        ));

        // A peer in the same namespace is untouched.
        assert!(
            state
                .relay(&peer, namespace, peer_grant.id, peer_grant.secret)
                .is_some(),
            "another key's setup must survive"
        );

        // Revoking the provider stops what is left: an in-flight setup, and offers.
        let (second, second_grant) = {
            state.open(
                2.into(),
                4,
                "echo".into(),
                false,
                peer_info.clone(),
                &registrations,
                route,
                |_, _| true,
            );
            let Ok(Some(ConnTask::Direct(offer))) = offers.try_recv() else {
                panic!("the live provider must still receive an offer");
            };
            let DirectEvent::Offer { grant, .. } = *offer else {
                panic!("expected an offer");
            };
            (grant.id, grant)
        };
        state.entries.get_mut(&second).unwrap().phase = Phase::RelaySelected;
        auth.revoke(&admin, provider.key_id).await.unwrap();
        assert!(
            state
                .relay(&peer, namespace, second, second_grant.secret)
                .is_none(),
            "a revoked provider must not serve a relay selection"
        );
        state.open(
            2.into(),
            5,
            "echo".into(),
            false,
            peer_info,
            &registrations,
            route,
            |_, _| panic!("a revoked provider must not be offered a setup"),
        );
        assert!(matches!(
            peer_events.recv().await,
            Some(DirectEvent::LegacyRelay { request: 5 })
        ));

        drop(state);
        auth.shutdown_actor().await;
        let _ = std::fs::remove_dir_all(dir);
    }

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
                lan_tcp: None,
            },
            provider: PeerInfo {
                certificate: vec![2],
                candidates: vec![],
                lan_tcp: None,
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
        let registration = |conn_id: u32, direct: Option<AuthContext>| crate::ServerConnInfo {
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
                link: None,
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
