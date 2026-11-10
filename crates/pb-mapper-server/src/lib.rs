//! Relay server domain model and module wiring.
//!
//! ```text
//! TCP / QUIC listener -> connection authentication/dispatch -> ManagerTask queue
//!                                                         -> routing runtime
//! registered control connection <------ ConnTask ----+------> subscriber
//! ```
//!
//! `connection` owns per-socket protocol/authentication concerns, while `runtime`
//! serializes global routing maps and quotas. Service-side and client-side tunnel loops
//! remain isolated in `server` and `client`.

mod admin;
mod client;
pub mod direct;
pub use direct::DirectServerConfig;
mod error;
// Moved here from `common`: the routing runtime is its only caller.
pub mod manager;
mod server;
mod status;

use std::net::SocketAddr;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::{Duration, Instant};

use error::Result;
use pb_mapper_protocol::transport::RelayStream;
use snafu::ResultExt;
use tokio::net::{TcpListener, ToSocketAddrs};
use tokio_util::sync::CancellationToken;
use tracing::instrument;

use self::admin::handle_admin_request;
use self::client::handle_client_conn;
use self::error::{
    TaskCenterInitRequestTimeoutSnafu, TaskCenterSendListenerSnafu,
    TaskCenterSendStreamRespToManagerSnafu, TaskCenterSetKeepAliveSnafu,
};
use self::server::{LEGACY_SERVER_IDLE_TIMEOUT, ServerRegistration, handle_server_conn};
use self::status::handle_show_status;
use crate::error::ServerListenSnafu;
use crate::manager::{ReceiverChan, SenderChan, TaskManager};
use pb_mapper_auth::{ADMIN_KEY_ID, AuthConfig, AuthContext, AuthRuntime};
use pb_mapper_core::config::{
    control_io_timeout, keep_alive_from_env, server_lease_sweep_interval, server_lease_timeout,
};
use pb_mapper_core::conn_id::RemoteConnId;
use pb_mapper_core::snafu_error_handle;
use pb_mapper_protocol::MessageWriter;
use pb_mapper_protocol::command::{
    AdminConnectionPage, AdminServicePage, CONTROL_PROTOCOL_V2, MessageSerializer, PbConnRequest,
    PbConnResponse, PbConnStatusReq, PbServiceConnStatus,
};
use pb_mapper_protocol::secure::{HeaderProtocol, ServerHeaderSession, ServerSecurity};
use uni_stream::stream::{set_tcp_keep_alive, set_tcp_nodelay};

pub enum ManagerTask {
    Direct(direct::DirectTask),
    Accept {
        stream: RelayStream,
        peer_addr: SocketAddr,
    },
    Register {
        key: ImutableKey,
        conn_id: RemoteConnId,
        need_codec: bool,
        is_datagram: bool,
        protocol_version: u16,
        conn_sender: ConnTaskSender,
        /// Cancelled to tear the control connection's socket down.
        ///
        /// Separate from `conn_sender` because the two fail independently: a
        /// registration that has stopped reading leaves the writer blocked inside
        /// a single `write_msg`, and a `ConnTask` queued behind it is not observed
        /// until that write returns — which, for a half-open socket, may be never.
        /// Retirement has to reach the socket without going through the work queue.
        retire_token: CancellationToken,
        direct: Option<AuthContext>,
    },
    ServerConnActivity {
        key: ImutableKey,
        conn_id: RemoteConnId,
    },
    Subcribe {
        key: ImutableKey,
        conn_id: RemoteConnId,
        conn_sender: ConnTaskSender,
        excluded_server_conns: Vec<(RemoteConnId, u64)>,
        selected: Option<(RemoteConnId, u64)>,
    },
    Stream {
        data_protocol: Option<u16>,
        key: ImutableKey,
        stream: RelayStream,
        session: ServerHeaderSession,
        server_id: RemoteConnId,
        client_id: RemoteConnId,
        server_generation: u64,
    },
    StreamAck {
        server_id: RemoteConnId,
        client_id: RemoteConnId,
        server_generation: u64,
    },
    Status {
        response_sender: tokio::sync::oneshot::Sender<PbConnResponse>,
        status: PbConnStatusReq,
        namespace: u64,
        conn_id: RemoteConnId,
    },
    StatusQuery {
        response_sender: tokio::sync::oneshot::Sender<ServerStatusInfo>,
    },
    AdminServiceList {
        key_id: Option<u64>,
        page: u32,
        page_size: u16,
        response_sender: tokio::sync::oneshot::Sender<AdminServicePage>,
    },
    AdminConnectionList {
        key_id: Option<u64>,
        page: u32,
        page_size: u16,
        response_sender: tokio::sync::oneshot::Sender<AdminConnectionPage>,
    },
    AdminConnectionRetire {
        key: ImutableKey,
        /// Absent retires every connection registered under `key`.
        conn_id: Option<RemoteConnId>,
        response_sender: tokio::sync::oneshot::Sender<u32>,
    },
    DeRegisterServerConn {
        key: ImutableKey,
        conn_id: RemoteConnId,
    },
    RetireServerConn {
        key: ImutableKey,
        conn_id: RemoteConnId,
        reason: String,
    },
    /// A single slow stream requests confirmation, not immediate deregistration.
    SuspectServerConn {
        key: ImutableKey,
        conn_id: RemoteConnId,
        generation: u64,
    },
    /// Retire only if no authenticated activity followed the suspicion.
    ConfirmServerConn {
        key: ImutableKey,
        conn_id: RemoteConnId,
        generation: u64,
        observed_at: Instant,
    },
    /// Look for registrations that stopped renewing their lease, and retire them.
    ///
    /// Sent by a ticker task rather than driven by a timer inside the manager loop:
    /// that loop's single await is a channel receive, and racing it against a tick
    /// in `tokio::select!` would drop a task the receive had already taken.
    SweepServerLeases,
    DeRegisterClientConn {
        server_id: Option<RemoteConnId>,
        client_id: RemoteConnId,
    },
    Shutdown,
}

#[derive(Debug)]
pub enum ConnTask {
    /// Boxed: rare and large, it must not widen every relay mailbox slot.
    Direct(Box<pb_mapper_protocol::direct::DirectEvent>),
    RegisterResp {
        generation: u64,
        protocol_version: u16,
        lease_ttl_ms: u64,
        direct: Option<pb_mapper_protocol::direct::DirectCapability>,
    },
    RegisterFailed {
        code: String,
        reason: String,
        retryable: bool,
    },
    SubcribeResp {
        ack_timeout: Duration,
        server_conn_id: RemoteConnId,
        server_generation: u64,
        need_codec: bool,
        is_datagram: bool,
    },
    SubcribeFailed {
        code: String,
        reason: String,
        retryable: bool,
    },
    SubcribeRetry {
        reason: String,
    },
    StreamReq {
        client_id: RemoteConnId,
        server_generation: u64,
    },
    Retire {
        reason: String,
    },
    StreamAck {
        server_id: RemoteConnId,
        server_generation: u64,
    },
    StreamResp {
        data_protocol: Option<u16>,
        server_id: RemoteConnId,
        server_generation: u64,
        stream: RelayStream,
        session: ServerHeaderSession,
    },
}

impl ManagerTask {
    /// Whether this task ends a connection, releasing its reusable routing ID.
    pub(crate) fn ends_connection(&self) -> bool {
        matches!(
            self,
            Self::DeRegisterServerConn { .. } | Self::DeRegisterClientConn { .. }
        )
    }
}

pub(crate) type ManagerTaskSender = manager::TaskSender;
pub(crate) type ConnTaskSender = SenderChan<ConnTask>;
pub(crate) type ConnTaskReceiver = ReceiverChan<ConnTask>;

pub type ImutableKey = Arc<str>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerConnHealth {
    Healthy,
    Suspect,
}

#[derive(Debug, Clone)]
pub struct ServerConnInfo {
    pub conn_id: RemoteConnId,
    pub generation: u64,
    pub health: ServerConnHealth,
    pub need_codec: bool,
    pub is_datagram: bool,
    pub protocol_version: u16,
    pub last_rx_at: Instant,
    /// Stream acknowledgement budget learned from completed requests and timeouts.
    pub stream_ack_budget: Duration,
    /// At most one outstanding confirmation timer per control connection.
    pub suspect_check: Option<Instant>,
    /// Cancelled to tear this connection's socket down; see
    /// [`ManagerTask::Register::retire_token`].
    ///
    /// Held here rather than in the task manager's sender map so that retiring a
    /// connection reaches the socket even when its work queue is wedged. This is
    /// also why the struct is `Clone` and no longer `Copy`.
    pub retire_token: CancellationToken,
    pub direct: Option<AuthContext>,
}

pub type ServerConnMap = hashbrown::HashMap<ImutableKey, Vec<ServerConnInfo>>;

/// A subscriber's stream request: the server connection it was routed to, that
/// connection's generation, the service key it asked for, and when.
#[derive(Clone)]
pub(crate) struct PendingStream {
    pub(crate) server_conn_id: RemoteConnId,
    pub(crate) server_generation: u64,
    pub(crate) key: ImutableKey,
    pub(crate) requested_at: Instant,
}

/// Subscribers' stream requests by their own connection ID, counted per
/// namespace as they are added and removed.
#[derive(Default)]
pub(crate) struct PendingStreams {
    by_client: hashbrown::HashMap<RemoteConnId, PendingStream>,
    per_namespace: hashbrown::HashMap<u64, usize>,
}

impl PendingStreams {
    pub(crate) fn get(&self, client_id: &RemoteConnId) -> Option<&PendingStream> {
        self.by_client.get(client_id)
    }

    pub(crate) fn contains(&self, client_id: &RemoteConnId) -> bool {
        self.by_client.contains_key(client_id)
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (&RemoteConnId, &PendingStream)> {
        self.by_client.iter()
    }

    /// Record a client's stream request, replacing the one it made before.
    pub(crate) fn insert(&mut self, client_id: RemoteConnId, stream: PendingStream) {
        let namespace = split_scoped_service_key(&stream.key).0;
        match self.by_client.insert(client_id, stream) {
            Some(replaced) if split_scoped_service_key(&replaced.key).0 == namespace => return,
            Some(replaced) => self.uncount(split_scoped_service_key(&replaced.key).0),
            None => {}
        }
        *self.per_namespace.entry(namespace).or_default() += 1;
    }

    pub(crate) fn remove(&mut self, client_id: &RemoteConnId) -> Option<PendingStream> {
        let removed = self.by_client.remove(client_id)?;
        self.uncount(split_scoped_service_key(&removed.key).0);
        Some(removed)
    }

    /// How many requests in `namespace` are pending.
    pub(crate) fn count_in(&self, namespace: u64) -> usize {
        self.per_namespace
            .get(&namespace)
            .copied()
            .unwrap_or_default()
    }

    /// Whether `namespace` can take `extra` more streams than it has pending.
    pub(crate) fn has_room(&self, namespace: u64, extra: usize, limit: NonZeroUsize) -> bool {
        extra + self.count_in(namespace) < limit.get()
    }

    fn uncount(&mut self, namespace: u64) {
        let Some(count) = self.per_namespace.get_mut(&namespace) else {
            return;
        };
        *count = count.saturating_sub(1);
        if *count == 0 {
            self.per_namespace.remove(&namespace);
        }
    }
}

/// Per-namespace new-stream rate limiters, keyed by the namespace's key ID.
pub(crate) struct NamespaceRateLimits {
    buckets: hashbrown::HashMap<u64, NamespaceRateLimit>,
    rate_per_second: NonZeroUsize,
    burst: NonZeroUsize,
}

impl NamespaceRateLimits {
    pub(crate) fn new(rate_per_second: NonZeroUsize, burst: NonZeroUsize) -> Self {
        Self {
            buckets: hashbrown::HashMap::new(),
            rate_per_second,
            burst,
        }
    }

    /// Take a token for one new stream in `namespace`.
    pub(crate) fn try_take(&mut self, namespace: u64) -> bool {
        self.buckets
            .entry(namespace)
            .or_insert_with(|| NamespaceRateLimit::new(self.rate_per_second, self.burst))
            .allow()
    }

    /// Forget `namespace`'s bucket, as nothing in it is registered or pending.
    fn release(&mut self, namespace: u64) {
        self.buckets.remove(&namespace);
    }
}

struct NamespaceRateLimit {
    tokens: f64,
    last_refill: Instant,
    rate_per_second: f64,
    burst: f64,
}

impl NamespaceRateLimit {
    fn new(rate_per_second: NonZeroUsize, burst: NonZeroUsize) -> Self {
        Self {
            tokens: burst.get() as f64,
            last_refill: Instant::now(),
            rate_per_second: rate_per_second.get() as f64,
            burst: burst.get() as f64,
        }
    }

    fn allow(&mut self) -> bool {
        let now = Instant::now();
        self.tokens = (self.tokens
            + now.duration_since(self.last_refill).as_secs_f64() * self.rate_per_second)
            .min(self.burst);
        self.last_refill = now;
        if self.tokens < 1.0 {
            false
        } else {
            self.tokens -= 1.0;
            true
        }
    }
}

/// Routing limits and timings, read from the environment once as the relay
/// starts.
pub(crate) struct RoutingConfig {
    pub(crate) max_services_per_namespace: NonZeroUsize,
    pub(crate) max_register_connections_per_service: NonZeroUsize,
    pub(crate) max_streams_per_namespace: NonZeroUsize,
    pub(crate) new_streams_per_second: NonZeroUsize,
    pub(crate) new_streams_burst: NonZeroUsize,
    /// The stream acknowledgement budget a registration starts with, and the
    /// floor its learned budget never drops below.
    pub(crate) stream_ack_timeout: Duration,
    /// The lease a protocol-v2 registration is granted.
    pub(crate) lease_timeout: Duration,
}

impl RoutingConfig {
    pub(crate) fn from_env() -> Self {
        Self {
            max_services_per_namespace: env_limit(
                "PB_MAPPER_MAX_SERVICES_PER_NAMESPACE",
                const { limit(256) },
            ),
            max_register_connections_per_service: env_limit(
                "PB_MAPPER_MAX_REGISTER_CONNECTIONS_PER_SERVICE",
                const { limit(16) },
            ),
            max_streams_per_namespace: env_limit(
                "PB_MAPPER_MAX_STREAMS_PER_NAMESPACE",
                const { limit(1024) },
            ),
            new_streams_per_second: env_limit(
                "PB_MAPPER_NEW_STREAMS_PER_SECOND",
                const { limit(100) },
            ),
            new_streams_burst: env_limit("PB_MAPPER_NEW_STREAMS_BURST", const { limit(200) }),
            stream_ack_timeout: pb_mapper_core::config::stream_ack_timeout(),
            lease_timeout: server_lease_timeout(),
        }
    }

    /// The ceiling a learned stream acknowledgement budget never exceeds.
    pub(crate) fn max_stream_ack_budget(&self) -> Duration {
        Duration::from_secs(5).max(self.stream_ack_timeout)
    }
}

/// `name`'s value when it is a positive integer, otherwise `default`.
fn env_limit(name: &str, default: NonZeroUsize) -> NonZeroUsize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

/// A default limit; evaluated in a `const` block, so zero fails the build.
const fn limit(value: usize) -> NonZeroUsize {
    match NonZeroUsize::new(value) {
        Some(limit) => limit,
        None => panic!("a routing limit is never zero"),
    }
}

#[derive(Debug, Clone)]
pub struct ServerStatusInfo {
    pub active_connections: u32,
    pub registered_services: u32,
    pub uptime_seconds: u64,
}

/// How long a registered control connection may go quiet before the relay treats
/// it as gone.
///
/// Two very different numbers, because the two protocol versions keep their
/// registrations alive in very different ways. A v2 client renews a lease it was
/// told the length of, every [`control_heartbeat_interval`] — 2s against a 15s
/// lease. A v1 client has no lease and pings on a schedule of its own, minutes
/// apart, so its threshold has to be generous enough that jitter never costs it a
/// healthy registration.
///
/// Both the per-connection reader timeout and the periodic sweep read the
/// threshold from here: they are two views of one rule, and a sweep stricter than
/// the reader would retire connections that are behaving exactly as designed.
///
/// [`control_heartbeat_interval`]: pb_mapper_core::config::control_heartbeat_interval
pub(crate) fn server_conn_idle_timeout(protocol_version: u16) -> Duration {
    idle_timeout(protocol_version, server_lease_timeout)
}

/// [`server_conn_idle_timeout`], taking the lease from `lease_timeout` and only
/// when the protocol has one.
fn idle_timeout(protocol_version: u16, lease_timeout: impl FnOnce() -> Duration) -> Duration {
    if protocol_version >= CONTROL_PROTOCOL_V2 {
        lease_timeout()
    } else {
        LEGACY_SERVER_IDLE_TIMEOUT
    }
}

/// Drop one connection from the service map, returning what was registered.
///
/// The removed [`ServerConnInfo`] is returned rather than a bare `bool` because it
/// carries the connection's [`ServerConnInfo::retire_token`], and the map is the
/// only place that holds it — a caller tearing the connection down needs it after
/// the entry is gone.
fn remove_server_conn(
    server_conn_map: &mut ServerConnMap,
    key: &ImutableKey,
    conn_id: RemoteConnId,
) -> Option<ServerConnInfo> {
    let ids = server_conn_map.get_mut(key)?;
    let idx = ids.iter().position(|info| info.conn_id == conn_id)?;
    let removed = ids.remove(idx);
    if ids.is_empty() {
        server_conn_map.remove(key);
    }
    Some(removed)
}

/// Drop a connection from the service map and tear its socket down.
///
/// For the callers that found a registration unusable and have nothing further to
/// say to it — a subscribe whose sender was gone, or whose stream request would not
/// send. Removing the entry alone would leave the socket task running until its own
/// reader timed out, holding its connection ID and its slot in the service's quota.
fn cancel_and_remove_server_conn(
    server_conn_map: &mut ServerConnMap,
    key: &ImutableKey,
    conn_id: RemoteConnId,
) -> bool {
    match remove_server_conn(server_conn_map, key, conn_id) {
        Some(info) => {
            info.retire_token.cancel();
            true
        }
        None => false,
    }
}

fn registered_server_conn_count(server_conn_map: &ServerConnMap) -> usize {
    server_conn_map.values().map(Vec::len).sum()
}

fn service_conn_count(server_conn_map: &ServerConnMap, key: &ImutableKey) -> usize {
    server_conn_map.get(key).map(Vec::len).unwrap_or_default()
}

fn service_status_connections(
    server_conn_map: &ServerConnMap,
    key: &ImutableKey,
) -> Vec<PbServiceConnStatus> {
    let now = Instant::now();
    server_conn_map
        .get(key)
        .map(|infos| {
            infos
                .iter()
                .map(|info| PbServiceConnStatus {
                    conn_id: info.conn_id.into(),
                    generation: info.generation,
                    protocol_version: info.protocol_version,
                    healthy: info.health == ServerConnHealth::Healthy,
                    last_rx_age_ms: now.duration_since(info.last_rx_at).as_millis() as u64,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// One registration the sweep decided to retire.
struct StaleServerConn {
    key: ImutableKey,
    conn_id: RemoteConnId,
    protocol_version: u16,
    idle_for: Duration,
}

/// Advance every registration's health by one sweep, and report the ones that ran
/// out of grace.
///
/// Two stages, because retiring on the first sweep would be both premature and
/// less useful:
///
/// 1. A [`ServerConnHealth::Healthy`] connection past its idle threshold becomes
///    [`ServerConnHealth::Suspect`]. It stays registered but drops out of the
///    subscribe candidate filter, so a subscriber stops being handed a
///    registration that has gone quiet — the fix that matters most, and the one
///    that costs nothing if the connection turns out to be alive.
/// 2. A connection still `Suspect` and still past its threshold on a later sweep
///    is returned for retirement.
///
/// Any activity resets health to `Healthy` (see [`record_server_conn_activity`]),
/// so a connection only reaches stage 2 by being quiet across the whole span. That
/// span also gives the connection's own reader timeout a chance to fire first,
/// which is the tidier teardown: the socket task unwinds and deregisters itself.
/// The sweep exists for the case where it does not — a task wedged on a socket
/// that will never return, which is how three services sat at a full 16/16
/// connection quota for seventeen hours.
fn sweep_server_conn_leases(
    server_conn_map: &mut ServerConnMap,
    lease_timeout: Duration,
) -> Vec<StaleServerConn> {
    let now = Instant::now();
    let mut stale = Vec::new();
    for (key, infos) in server_conn_map.iter_mut() {
        for info in infos.iter_mut() {
            let idle_for = now.duration_since(info.last_rx_at);
            if idle_for <= idle_timeout(info.protocol_version, || lease_timeout) {
                continue;
            }
            match info.health {
                ServerConnHealth::Healthy => info.health = ServerConnHealth::Suspect,
                ServerConnHealth::Suspect => stale.push(StaleServerConn {
                    key: key.clone(),
                    conn_id: info.conn_id,
                    protocol_version: info.protocol_version,
                    idle_for,
                }),
            }
        }
    }
    stale
}

fn record_server_conn_activity(
    server_conn_map: &mut ServerConnMap,
    key: &ImutableKey,
    conn_id: RemoteConnId,
) -> bool {
    let Some(infos) = server_conn_map.get_mut(key) else {
        return false;
    };
    let Some(info) = infos.iter_mut().find(|info| info.conn_id == conn_id) else {
        return false;
    };
    info.last_rx_at = Instant::now();
    info.health = ServerConnHealth::Healthy;
    true
}

fn record_server_conn_activity_by_conn_id(
    server_conn_map: &mut ServerConnMap,
    conn_id: RemoteConnId,
) -> bool {
    let Some(info) = server_conn_map
        .values_mut()
        .flat_map(|infos| infos.iter_mut())
        .find(|info| info.conn_id == conn_id)
    else {
        return false;
    };
    info.last_rx_at = Instant::now();
    info.health = ServerConnHealth::Healthy;
    true
}

#[cfg(feature = "relay-quic")]
mod quic;
mod runtime;

/// Optional QUIC listener, separate from the direct STUN UDP port.
#[derive(Clone, Debug)]
pub struct QuicServerConfig {
    pub listen: SocketAddr,
    /// Zero advertises the bound port; set for a NAT port mapping.
    pub advertise_port: u16,
}

#[derive(Clone, Debug, Default)]
pub struct RelayServerOptions {
    pub direct: Option<DirectServerConfig>,
    pub quic: Option<QuicServerConfig>,
}

pub use runtime::{
    run_server, run_server_on_listener, run_server_on_listener_with_direct,
    run_server_on_listener_with_options, run_server_with_auth_config, run_server_with_shutdown,
};
mod connection;
use connection::{
    compose_service_key, handle_conn, handle_listener, split_scoped_service_key,
    validate_service_name,
};

#[cfg(test)]
mod task_tests {
    use super::*;

    /// A mailbox slot is as wide as the widest task, and every registered
    /// connection preallocates its slots. Handing over a stream is the one large
    /// task routing needs; anything wider is boxed, as `ConnTask::Direct` is.
    #[test]
    fn no_conn_task_is_wider_than_a_stream_hand_off() {
        let stream_hand_off = size_of::<RelayStream>()
            + size_of::<ServerHeaderSession>()
            + size_of::<(Option<u16>, RemoteConnId, u64)>();
        assert!(
            size_of::<ConnTask>() <= stream_hand_off,
            "ConnTask is {} bytes, a stream hand-off {stream_hand_off}",
            size_of::<ConnTask>()
        );
    }
}

#[cfg(test)]
mod lease_tests {
    use super::*;

    const LEASE: Duration = Duration::from_secs(15);

    fn conn(conn_id: u32, protocol_version: u16, idle_for: Duration) -> ServerConnInfo {
        ServerConnInfo {
            direct: None,
            conn_id: RemoteConnId::from(conn_id),
            generation: 1,
            health: ServerConnHealth::Healthy,
            need_codec: false,
            is_datagram: false,
            protocol_version,
            last_rx_at: Instant::now() - idle_for,
            stream_ack_budget: pb_mapper_core::config::stream_ack_timeout(),
            suspect_check: None,
            retire_token: CancellationToken::new(),
        }
    }

    fn map(infos: Vec<ServerConnInfo>) -> ServerConnMap {
        let mut map = ServerConnMap::new();
        map.insert(ImutableKey::from("sf-backend"), infos);
        map
    }

    fn health(map: &ServerConnMap, conn_id: u32) -> ServerConnHealth {
        map.values()
            .flatten()
            .find(|info| info.conn_id == RemoteConnId::from(conn_id))
            .expect("connection is still registered")
            .health
    }

    #[test]
    fn sweep_marks_before_it_retires() {
        let mut map = map(vec![conn(1, CONTROL_PROTOCOL_V2, Duration::from_secs(60))]);

        // First sweep only withdraws it from the subscribe candidates.
        assert!(sweep_server_conn_leases(&mut map, LEASE).is_empty());
        assert_eq!(health(&map, 1), ServerConnHealth::Suspect);

        let stale = sweep_server_conn_leases(&mut map, LEASE);
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].conn_id, RemoteConnId::from(1));
        assert_eq!(stale[0].protocol_version, CONTROL_PROTOCOL_V2);
    }

    #[test]
    fn activity_between_sweeps_clears_suspicion() {
        let key = ImutableKey::from("sf-backend");
        let mut map = map(vec![conn(1, CONTROL_PROTOCOL_V2, Duration::from_secs(60))]);

        assert!(sweep_server_conn_leases(&mut map, LEASE).is_empty());
        assert!(record_server_conn_activity(
            &mut map,
            &key,
            RemoteConnId::from(1)
        ));

        assert!(sweep_server_conn_leases(&mut map, LEASE).is_empty());
        assert_eq!(health(&map, 1), ServerConnHealth::Healthy);
    }

    #[test]
    fn a_legacy_registration_keeps_its_far_longer_grace() {
        // Well past a v2 lease, nowhere near the v1 ping interval.
        let idle_for = Duration::from_secs(60);
        let mut map = map(vec![
            conn(1, 1, idle_for),
            conn(2, CONTROL_PROTOCOL_V2, idle_for),
        ]);

        assert!(sweep_server_conn_leases(&mut map, LEASE).is_empty());
        assert_eq!(health(&map, 1), ServerConnHealth::Healthy);
        assert_eq!(health(&map, 2), ServerConnHealth::Suspect);
    }

    #[test]
    fn sweep_reports_every_stale_connection_of_a_service() {
        let idle_for = Duration::from_secs(60);
        // The third is renewing its lease.
        let mut map = map(vec![
            conn(1, CONTROL_PROTOCOL_V2, idle_for),
            conn(2, CONTROL_PROTOCOL_V2, idle_for),
            conn(3, CONTROL_PROTOCOL_V2, Duration::from_secs(1)),
        ]);

        assert!(sweep_server_conn_leases(&mut map, LEASE).is_empty());
        let mut stale: Vec<u32> = sweep_server_conn_leases(&mut map, LEASE)
            .into_iter()
            .map(|stale| stale.conn_id.into())
            .collect();
        stale.sort_unstable();
        assert_eq!(stale, vec![1, 2]);
        assert_eq!(health(&map, 3), ServerConnHealth::Healthy);
    }
}

#[cfg(test)]
mod pending_stream_tests {
    use super::*;

    /// The per-namespace count is what admission and rate-limit release read in
    /// place of scanning every pending stream, so it must always equal the scan.
    #[test]
    fn namespace_counts_match_a_scan_of_the_pending_streams() {
        let keys = [
            ImutableKey::from("plain"),
            compose_service_key(1, "echo"),
            compose_service_key(1, "other"),
            compose_service_key(2, "echo"),
        ];
        let mut pending = PendingStreams::default();
        // A fixed pseudo-random walk over inserts, replacements and removals.
        let mut state = 0x2545_f491_u32;
        for _ in 0..2000 {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            let client_id = RemoteConnId::from(state % 16);
            if state & 0x100 == 0 {
                pending.remove(&client_id);
            } else {
                let key = &keys[(state >> 9) as usize % keys.len()];
                pending.insert(
                    client_id,
                    PendingStream {
                        server_conn_id: RemoteConnId::from(0),
                        server_generation: 1,
                        key: key.clone(),
                        requested_at: Instant::now(),
                    },
                );
            }
            for namespace in [0, 1, 2, 3] {
                let scanned = pending
                    .iter()
                    .filter(|(_, stream)| split_scoped_service_key(&stream.key).0 == namespace)
                    .count();
                assert_eq!(pending.count_in(namespace), scanned);
                assert_eq!(pending.per_namespace.contains_key(&namespace), scanned > 0);
            }
        }
    }
}
