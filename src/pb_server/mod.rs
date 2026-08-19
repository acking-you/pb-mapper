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
mod server;
mod status;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

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
use self::server::{handle_server_conn, ServerRegistration};
use self::status::handle_show_status;
use crate::common::auth::{AuthConfig, AuthContext, AuthRuntime};
use crate::common::config::{control_io_timeout, keep_alive_from_env, server_lease_timeout};
use crate::common::conn_id::{ConnIdProvider, RemoteConnId};
use crate::common::manager::{ForwardMessage, SenderChan, TaskManager};
use crate::common::message::command::{
    AdminConnectionInfo, AdminConnectionPage, AdminServiceInfo, AdminServicePage,
    MessageSerializer, PbConnRequest, PbConnResponse, PbConnStatusReq, PbConnStatusResp,
    PbServiceConnStatus,
};
use crate::common::message::secure::{HeaderProtocol, ServerHeaderSession, ServerSecurity};
use crate::common::message::MessageWriter;
use crate::pb_server::error::{
    ServerListenSnafu, TaskCenterClientSendStreamSnafu, TaskCenterSendRegisterRespSnafu,
    TaskCenterSendStreamRespToClientSnafu, TaskCenterSendSubcribeRespSnafu,
    TaskCenterStreamConnIdNotExistSnafu,
};
use crate::{snafu_error_get_or_continue, snafu_error_handle};
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
    DeRegisterServerConn {
        key: ImutableKey,
        conn_id: RemoteConnId,
    },
    RetireServerConn {
        key: ImutableKey,
        conn_id: RemoteConnId,
        reason: String,
    },
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

fn remove_server_conn(
    server_conn_map: &mut ServerConnMap,
    key: &ImutableKey,
    conn_id: RemoteConnId,
) -> bool {
    if let Some(ids) = server_conn_map.get_mut(key) {
        if let Some(idx) = ids.iter().position(|info| info.conn_id == conn_id) {
            ids.remove(idx);
            if ids.is_empty() {
                server_conn_map.remove(key);
            }
            return true;
        }
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
    decrement_namespace_stream_count, handle_conn, handle_listener,
    release_namespace_rate_limit_if_idle, split_scoped_service_key,
};
