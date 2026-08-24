//! Relay server domain model and module wiring.
//!
//! ```text
//! TCP listener -> connection authentication/dispatch -> ManagerTask queue
//!                                                    -> routing runtime
//! registered control connection <------ ConnTask ----+------> subscriber
//! ```
//!
//! `connection` owns per-socket protocol/authentication concerns, while `runtime`
//! serializes global routing maps and quotas. Service-side and client-side tunnel loops
//! remain isolated in `server` and `client`.

mod admin;
mod client;
mod error;
// Moved here from `common`: the routing runtime is its only caller.
pub mod manager;
mod server;
mod status;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use error::Result;
use snafu::{OptionExt, ResultExt};
use tokio::net::{TcpListener, TcpStream, ToSocketAddrs};
use tokio_util::sync::CancellationToken;
use tracing::instrument;

use self::admin::handle_admin_request;
use self::client::handle_client_conn;
use self::error::{
    TaskCenterInitRequestTimeoutSnafu, TaskCenterSendListenerSnafu, TaskCenterSendStatusRespSnafu,
    TaskCenterSendStreamRespToManagerSnafu, TaskCenterSetKeepAliveSnafu,
};
use self::server::{LEGACY_SERVER_IDLE_TIMEOUT, ServerRegistration, handle_server_conn};
use self::status::handle_show_status;
use crate::error::{
    ServerListenSnafu, TaskCenterClientSendStreamSnafu, TaskCenterSendRegisterRespSnafu,
    TaskCenterSendStreamRespToClientSnafu, TaskCenterSendSubcribeRespSnafu,
    TaskCenterStreamConnIdNotExistSnafu,
};
use crate::manager::{ForwardMessage, SenderChan, TaskManager};
use pb_mapper_auth::{ADMIN_KEY_ID, AuthConfig, AuthContext, AuthRuntime};
use pb_mapper_core::config::{
    control_io_timeout, keep_alive_from_env, server_lease_sweep_interval, server_lease_timeout,
};
use pb_mapper_core::conn_id::{ConnIdProvider, RemoteConnId};
use pb_mapper_core::{snafu_error_get_or_continue, snafu_error_handle};
use pb_mapper_protocol::MessageWriter;
use pb_mapper_protocol::command::{
    AdminConnectionInfo, AdminConnectionPage, AdminServiceInfo, AdminServicePage,
    CONTROL_PROTOCOL_V2, MessageSerializer, PbConnRequest, PbConnResponse, PbConnStatusReq,
    PbConnStatusResp, PbServiceConnStatus,
};
use pb_mapper_protocol::secure::{HeaderProtocol, ServerHeaderSession, ServerSecurity};
use uni_stream::stream::{set_tcp_keep_alive, set_tcp_nodelay};

pub enum ManagerTask {
    Accept {
        stream: TcpStream,
        peer_addr: SocketAddr,
    },
    Register {
        key: ImutableKey,
        conn_id: RemoteConnId,
        need_codec: bool,
        is_datagram: bool,
        protocol_version: u16,
        conn_sender: ConnTaskSender,
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
    },
    Stream {
        key: ImutableKey,
        stream: TcpStream,
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
        conn_sender: ConnTaskSender,
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
    Forward(ForwardMessage),
    RegisterResp {
        generation: u64,
        protocol_version: u16,
        lease_ttl_ms: u64,
    },
    RegisterFailed {
        code: String,
        reason: String,
        retryable: bool,
    },
    SubcribeResp {
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
        server_id: RemoteConnId,
        server_generation: u64,
        stream: TcpStream,
        session: ServerHeaderSession,
    },
    StatusResp(PbConnResponse),
}

pub(crate) type ManagerTaskSender = SenderChan<ManagerTask>;
pub(crate) type ConnTaskSender = SenderChan<ConnTask>;

pub type ImutableKey = Arc<str>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerConnHealth {
    Healthy,
    Suspect,
}

#[derive(Debug, Clone, Copy)]
pub struct ServerConnInfo {
    pub conn_id: RemoteConnId,
    pub generation: u64,
    pub health: ServerConnHealth,
    pub need_codec: bool,
    pub is_datagram: bool,
    pub protocol_version: u16,
    pub last_rx_at: Instant,
}

pub type ServerConnMap = hashbrown::HashMap<ImutableKey, Vec<ServerConnInfo>>;

struct NamespaceRateLimit {
    tokens: f64,
    last_refill: Instant,
    rate_per_second: f64,
    burst: f64,
}

impl NamespaceRateLimit {
    fn new(rate_per_second: usize, burst: usize) -> Self {
        Self {
            tokens: burst as f64,
            last_refill: Instant::now(),
            rate_per_second: rate_per_second as f64,
            burst: burst as f64,
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

fn env_limit(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
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
    if protocol_version >= CONTROL_PROTOCOL_V2 {
        server_lease_timeout()
    } else {
        LEGACY_SERVER_IDLE_TIMEOUT
    }
}

fn remove_server_conn(
    server_conn_map: &mut ServerConnMap,
    key: &ImutableKey,
    conn_id: RemoteConnId,
) -> bool {
    if let Some(ids) = server_conn_map.get_mut(key)
        && let Some(idx) = ids.iter().position(|info| info.conn_id == conn_id)
    {
        ids.remove(idx);
        if ids.is_empty() {
            server_conn_map.remove(key);
        }
        return true;
    }
    false
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
fn sweep_server_conn_leases(server_conn_map: &mut ServerConnMap) -> Vec<StaleServerConn> {
    let now = Instant::now();
    let mut stale = Vec::new();
    for (key, infos) in server_conn_map.iter_mut() {
        for info in infos.iter_mut() {
            let idle_for = now.duration_since(info.last_rx_at);
            if idle_for <= server_conn_idle_timeout(info.protocol_version) {
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

async fn send_subcribe_failed(
    conn_sender: &ConnTaskSender,
    key: &ImutableKey,
    conn_id: RemoteConnId,
    reason: impl Into<String>,
) {
    let reason = reason.into();
    if conn_sender
        .send(ConnTask::SubcribeFailed {
            code: "service_not_available".to_string(),
            reason: reason.clone(),
            retryable: true,
        })
        .await
        .is_err()
    {
        tracing::debug!(
            event = "subscribe_failure_receiver_dropped",
            key = %key,
            client_conn_id = %conn_id,
            reason = %reason,
            "subscribe failure receiver dropped"
        );
    }
}

async fn send_subcribe_retry(
    conn_sender: &ConnTaskSender,
    key: &ImutableKey,
    conn_id: RemoteConnId,
    reason: impl Into<String>,
) {
    let reason = reason.into();
    if conn_sender
        .send(ConnTask::SubcribeRetry {
            reason: reason.clone(),
        })
        .await
        .is_err()
    {
        tracing::debug!(
            event = "subscribe_retry_receiver_dropped",
            key = %key,
            client_conn_id = %conn_id,
            reason = %reason,
            "subscribe retry receiver dropped"
        );
    }
}

mod runtime;
pub use runtime::{
    run_server, run_server_on_listener, run_server_with_auth_config, run_server_with_shutdown,
};
mod connection;
use connection::{
    compose_service_key, decrement_namespace_stream_count, handle_conn, handle_listener,
    release_namespace_rate_limit_if_idle, split_scoped_service_key, validate_service_name,
};

#[cfg(test)]
mod lease_tests {
    use super::*;

    fn conn(conn_id: u32, protocol_version: u16, idle_for: Duration) -> ServerConnInfo {
        ServerConnInfo {
            conn_id: RemoteConnId::from(conn_id),
            generation: 1,
            health: ServerConnHealth::Healthy,
            need_codec: false,
            is_datagram: false,
            protocol_version,
            last_rx_at: Instant::now() - idle_for,
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
    fn sweep_leaves_a_registration_that_is_renewing_its_lease() {
        let mut map = map(vec![conn(1, CONTROL_PROTOCOL_V2, Duration::from_secs(2))]);

        assert!(sweep_server_conn_leases(&mut map).is_empty());
        assert_eq!(health(&map, 1), ServerConnHealth::Healthy);
    }

    #[test]
    fn sweep_marks_before_it_retires() {
        let mut map = map(vec![conn(1, CONTROL_PROTOCOL_V2, Duration::from_secs(60))]);

        // First sweep only withdraws it from the subscribe candidates.
        assert!(sweep_server_conn_leases(&mut map).is_empty());
        assert_eq!(health(&map, 1), ServerConnHealth::Suspect);

        let stale = sweep_server_conn_leases(&mut map);
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].conn_id, RemoteConnId::from(1));
        assert_eq!(stale[0].protocol_version, CONTROL_PROTOCOL_V2);
    }

    #[test]
    fn activity_between_sweeps_clears_suspicion() {
        let key = ImutableKey::from("sf-backend");
        let mut map = map(vec![conn(1, CONTROL_PROTOCOL_V2, Duration::from_secs(60))]);

        assert!(sweep_server_conn_leases(&mut map).is_empty());
        assert!(record_server_conn_activity(
            &mut map,
            &key,
            RemoteConnId::from(1)
        ));

        assert!(sweep_server_conn_leases(&mut map).is_empty());
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

        assert!(sweep_server_conn_leases(&mut map).is_empty());
        assert_eq!(health(&map, 1), ServerConnHealth::Healthy);
        assert_eq!(health(&map, 2), ServerConnHealth::Suspect);
    }

    #[test]
    fn sweep_reports_every_stale_connection_of_a_service() {
        let idle_for = Duration::from_secs(60);
        let mut map = map(vec![
            conn(1, CONTROL_PROTOCOL_V2, idle_for),
            conn(2, CONTROL_PROTOCOL_V2, idle_for),
            conn(3, CONTROL_PROTOCOL_V2, Duration::from_secs(1)),
        ]);

        assert!(sweep_server_conn_leases(&mut map).is_empty());
        let mut stale: Vec<u32> = sweep_server_conn_leases(&mut map)
            .into_iter()
            .map(|stale| stale.conn_id.into())
            .collect();
        stale.sort_unstable();
        assert_eq!(stale, vec![1, 2]);
    }
}
