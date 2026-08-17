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
    TaskCenterDecodeInitRequestSnafu, TaskCenterInitRequestTimeoutSnafu,
    TaskCenterReadInitRequestSnafu, TaskCenterSendListenerSnafu, TaskCenterSendStatusRespSnafu,
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
use crate::common::message::secure::{ServerHeaderSession, ServerSecurity};
use crate::common::message::{get_header_msg_reader, MessageReader, MessageWriter};
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

struct RemoteIdProvider {
    next_id: RemoteConnId,
}

impl RemoteIdProvider {
    fn new() -> Self {
        Self {
            next_id: RemoteConnId::default(),
        }
    }
}

impl ConnIdProvider<RemoteConnId> for RemoteIdProvider {
    fn get_next_id(&mut self) -> RemoteConnId {
        let ret = self.next_id;
        self.next_id += 1;
        ret
    }

    fn is_valid_id(&self, id: &RemoteConnId) -> bool {
        id < &self.next_id
    }
}
type ServerMananger = TaskManager<ManagerTask, ConnTask, RemoteConnId, RemoteIdProvider>;

/// Run a server that takes its keep-alive setting from the environment.
///
/// Callers that own the setting — the binary, and the UI, which has a toggle for
/// it — should use [`run_server_with_shutdown`] and pass it explicitly.
pub async fn run_server<A: ToSocketAddrs>(addr: A) -> std::io::Result<()> {
    run_server_with_shutdown(addr, CancellationToken::new(), None, keep_alive_from_env()).await
}

pub async fn run_server_with_shutdown<A: ToSocketAddrs>(
    addr: A,
    shutdown_token: CancellationToken,
    status_channel: Option<
        tokio::sync::mpsc::UnboundedReceiver<tokio::sync::oneshot::Sender<ServerStatusInfo>>,
    >,
    keep_alive: bool,
) -> std::io::Result<()> {
    run_server_with_auth_config(
        addr,
        shutdown_token,
        status_channel,
        keep_alive,
        AuthConfig::default(),
    )
    .await
}

pub async fn run_server_with_auth_config<A: ToSocketAddrs>(
    addr: A,
    shutdown_token: CancellationToken,
    status_channel: Option<
        tokio::sync::mpsc::UnboundedReceiver<tokio::sync::oneshot::Sender<ServerStatusInfo>>,
    >,
    keep_alive: bool,
    auth_config: AuthConfig,
) -> std::io::Result<()> {
    let auth = AuthRuntime::from_process(auth_config)
        .await
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    let security = ServerSecurity::new(auth);
    let mut manager = ServerMananger::new(RemoteIdProvider::new());
    // represent the mapping of the `key` to the id of the server-side conn
    let mut server_conn_map = ServerConnMap::new();
    let mut pending_streams =
        hashbrown::HashMap::<RemoteConnId, (RemoteConnId, u64, ImutableKey)>::new();
    let mut namespace_stream_counts = hashbrown::HashMap::<u64, usize>::new();
    let mut namespace_rate_limits = hashbrown::HashMap::<u64, NamespaceRateLimit>::new();
    let max_services_per_namespace = env_limit("PB_MAPPER_MAX_SERVICES_PER_NAMESPACE", 256);
    let max_register_connections_per_service =
        env_limit("PB_MAPPER_MAX_REGISTER_CONNECTIONS_PER_SERVICE", 16);
    let max_streams_per_namespace = env_limit("PB_MAPPER_MAX_STREAMS_PER_NAMESPACE", 1024);
    let new_streams_per_second = env_limit("PB_MAPPER_NEW_STREAMS_PER_SECOND", 100);
    let new_streams_burst = env_limit("PB_MAPPER_NEW_STREAMS_BURST", 200);
    let mut next_server_generation = 1_u64;

    let listener = TcpListener::bind(addr).await?;
    let listen_addr = listener.local_addr()?;
    tracing::info!(
        event = "pb_server_listening",
        listen_addr = %listen_addr,
        control_timeout = ?control_io_timeout(),
        "pb-mapper server is listening"
    );

    let task_sender = manager.get_task_sender();
    let shutdown_token_clone = shutdown_token.clone();

    let listener_handle = tokio::spawn(async move {
        tokio::select! {
            result = handle_listener(task_sender, listener, keep_alive) => {
                if let Err(e) = result {
                    tracing::error!("Listener error: {}", e);
                }
            }
            _ = shutdown_token_clone.cancelled() => {
                tracing::info!("Listener shutdown requested");
            }
        }
    });

    let start_time = std::time::Instant::now();

    let status_forward_handle = status_channel.map(|mut receiver| {
        let status_sender = manager.get_task_sender();
        tokio::spawn(async move {
            while let Some(response_sender) = receiver.recv().await {
                if status_sender
                    .send(ManagerTask::StatusQuery { response_sender })
                    .await
                    .is_err()
                {
                    break;
                }
            }
        })
    });

    let shutdown_handle = {
        let shutdown_sender = manager.get_task_sender();
        tokio::spawn(async move {
            shutdown_token.cancelled().await;
            let _ = shutdown_sender.send(ManagerTask::Shutdown).await;
        })
    };

    loop {
        let task = match manager.wait_for_task().await {
            Ok(task) => task,
            Err(e) => {
                tracing::error!("Manager task error: {}", e);
                break;
            }
        };

        match task {
            ManagerTask::AdminServiceList {
                key_id,
                page,
                page_size,
                response_sender,
            } => {
                let page_size = page_size.clamp(1, 1000) as usize;
                let start = (page as usize).saturating_mul(page_size);
                let mut all = server_conn_map
                    .iter()
                    .filter_map(|(key, connections)| {
                        let (namespace, service_name) = split_scoped_service_key(key);
                        if key_id.is_some_and(|key_id| key_id != namespace) {
                            return None;
                        }
                        let first = connections.first()?;
                        Some(AdminServiceInfo {
                            key_id: namespace,
                            namespace,
                            service_name: service_name.to_string(),
                            transport: if first.is_datagram { "udp" } else { "tcp" }.to_string(),
                            codec_enabled: first.need_codec,
                            connection_count: connections.len() as u32,
                        })
                    })
                    .collect::<Vec<_>>();
                all.sort_by(|left, right| {
                    left.namespace
                        .cmp(&right.namespace)
                        .then_with(|| left.service_name.cmp(&right.service_name))
                });
                let items = all.iter().skip(start).take(page_size).cloned().collect();
                let next_page =
                    (start.saturating_add(page_size) < all.len()).then_some(page.saturating_add(1));
                let _ = response_sender.send(AdminServicePage {
                    schema_version: 1,
                    items,
                    next_page,
                });
            }
            ManagerTask::AdminConnectionList {
                key_id,
                page,
                page_size,
                response_sender,
            } => {
                let now = Instant::now();
                let page_size = page_size.clamp(1, 1000) as usize;
                let start = (page as usize).saturating_mul(page_size);
                let mut all = server_conn_map
                    .iter()
                    .flat_map(|(key, connections)| {
                        let (namespace, service_name) = split_scoped_service_key(key);
                        connections.iter().filter_map(move |connection| {
                            if key_id.is_some_and(|key_id| key_id != namespace) {
                                return None;
                            }
                            Some(AdminConnectionInfo {
                                key_id: namespace,
                                namespace,
                                service_name: service_name.to_string(),
                                conn_id: connection.conn_id.into(),
                                generation: connection.generation,
                                protocol_version: connection.protocol_version,
                                healthy: connection.health == ServerConnHealth::Healthy,
                                transport: if connection.is_datagram { "udp" } else { "tcp" }
                                    .to_string(),
                                codec_enabled: connection.need_codec,
                                last_rx_age_ms: now
                                    .duration_since(connection.last_rx_at)
                                    .as_millis()
                                    as u64,
                            })
                        })
                    })
                    .collect::<Vec<_>>();
                all.sort_by(|left, right| {
                    left.namespace
                        .cmp(&right.namespace)
                        .then_with(|| left.service_name.cmp(&right.service_name))
                        .then_with(|| left.conn_id.cmp(&right.conn_id))
                });
                let items = all.iter().skip(start).take(page_size).cloned().collect();
                let next_page =
                    (start.saturating_add(page_size) < all.len()).then_some(page.saturating_add(1));
                let _ = response_sender.send(AdminConnectionPage {
                    schema_version: 1,
                    items,
                    next_page,
                });
            }
            ManagerTask::StatusQuery { response_sender } => {
                let total_connections = server_conn_map
                    .values()
                    .map(|conns| conns.len() as u32)
                    .sum();

                let status_info = ServerStatusInfo {
                    active_connections: total_connections,
                    registered_services: server_conn_map.len() as u32,
                    uptime_seconds: start_time.elapsed().as_secs(),
                };

                // Send response back (ignore if receiver dropped)
                let _ = response_sender.send(status_info);
                tracing::debug!(
                    event = "status_query_served",
                    registered_services = server_conn_map.len(),
                    server_connections = total_connections,
                    active_connections = manager.active_conn_count(),
                    idle_connections = manager.idle_conn_count(),
                    "server status query served"
                );
            }
            ManagerTask::Status {
                conn_sender,
                status,
                namespace,
                conn_id,
            } => {
                let resp = match status {
                    PbConnStatusReq::RemoteId => {
                        let scoped = server_conn_map
                            .iter()
                            .filter(|(key, _)| split_scoped_service_key(key).0 == namespace)
                            .map(|(key, value)| (split_scoped_service_key(key).1, value))
                            .collect::<Vec<_>>();
                        let registered_ids = scoped
                            .iter()
                            .flat_map(|(_, connections)| {
                                connections.iter().map(|connection| connection.conn_id)
                            })
                            .collect::<Vec<_>>();
                        let client_ids = pending_streams
                            .iter()
                            .filter_map(|(client_id, (_, _, key))| {
                                (split_scoped_service_key(key).0 == namespace).then_some(*client_id)
                            })
                            .collect::<Vec<_>>();
                        PbConnResponse::Status(PbConnStatusResp::RemoteId {
                            server_map: format!("{scoped:?}"),
                            active: format!(
                                "registered={registered_ids:?}, clients={client_ids:?}"
                            ),
                            idle: "namespace scoped; use `pb-mapper admin connection list` for global inspection"
                                .to_string(),
                        })
                    }
                    PbConnStatusReq::Keys => PbConnResponse::Status(PbConnStatusResp::Keys(
                        server_conn_map
                            .keys()
                            .filter_map(|key| {
                                let (key_namespace, service_name) = split_scoped_service_key(key);
                                (key_namespace == namespace).then(|| service_name.to_string())
                            })
                            .collect(),
                    )),
                    PbConnStatusReq::Service { key } => {
                        let display_key = key.clone();
                        let key: ImutableKey = if namespace == 0 {
                            key.into()
                        } else {
                            Arc::from(format!("@{namespace:016x}\u{0}{key}"))
                        };
                        PbConnResponse::Status(PbConnStatusResp::Service {
                            key: display_key,
                            connections: service_status_connections(&server_conn_map, &key),
                        })
                    }
                };
                snafu_error_get_or_continue!(conn_sender
                    .send(ConnTask::StatusResp(resp))
                    .await
                    .map_err(|_| kanal::SendError(()))
                    .context(TaskCenterSendStatusRespSnafu { conn_id }));
            }
            ManagerTask::Accept { stream, peer_addr } => {
                let conn_id = manager.get_conn_id(
                    server_conn_map
                        .iter()
                        .flat_map(|(_, ids)| ids.iter().map(|v| v.conn_id)),
                );
                tracing::info!(
                    event = "conn_accepted",
                    conn_id = %conn_id,
                    peer_addr = %peer_addr,
                    registered_services = server_conn_map.len(),
                    server_connections = registered_server_conn_count(&server_conn_map),
                    active_connections = manager.active_conn_count(),
                    idle_connections = manager.idle_conn_count(),
                    "accepted pb connection"
                );
                let manager_task_sender = manager.get_task_sender();
                let security = security.clone();
                tokio::spawn(async move {
                    snafu_error_handle!(
                        handle_conn(conn_id, peer_addr, manager_task_sender, stream, security)
                            .await
                    );
                });
            }
            ManagerTask::DeRegisterServerConn { key, conn_id } => {
                let removed_from_service_map =
                    remove_server_conn(&mut server_conn_map, &key, conn_id);
                let removed_from_active_map = manager.deregister_conn(conn_id);
                let removed_pending_streams = remove_pending_streams_for_server(
                    &mut pending_streams,
                    &mut namespace_stream_counts,
                    conn_id,
                );
                release_namespace_rate_limit_if_idle(
                    split_scoped_service_key(&key).0,
                    &server_conn_map,
                    &pending_streams,
                    &mut namespace_rate_limits,
                );
                tracing::info!(
                    event = "server_conn_deregistered",
                    key = %key,
                    conn_id = %conn_id,
                    removed_from_service_map,
                    removed_from_active_map,
                    removed_pending_streams,
                    registered_services = server_conn_map.len(),
                    server_connections = registered_server_conn_count(&server_conn_map),
                    active_connections = manager.active_conn_count(),
                    idle_connections = manager.idle_conn_count(),
                    "server connection deregistered"
                );
            }
            ManagerTask::ServerConnActivity { key, conn_id } => {
                let recorded = record_server_conn_activity(&mut server_conn_map, &key, conn_id);
                tracing::debug!(
                    event = "server_conn_lease_renewed",
                    key = %key,
                    conn_id = %conn_id,
                    recorded,
                    "server control connection activity recorded"
                );
            }
            ManagerTask::RetireServerConn {
                key,
                conn_id,
                reason,
            } => {
                let conn_sender = manager.get_conn_sender_chan(&conn_id);
                let removed_from_service_map =
                    remove_server_conn(&mut server_conn_map, &key, conn_id);
                let removed_from_active_map = manager.deregister_conn(conn_id);
                let removed_pending_streams = remove_pending_streams_for_server(
                    &mut pending_streams,
                    &mut namespace_stream_counts,
                    conn_id,
                );
                release_namespace_rate_limit_if_idle(
                    split_scoped_service_key(&key).0,
                    &server_conn_map,
                    &pending_streams,
                    &mut namespace_rate_limits,
                );
                let retire_notified = conn_sender
                    .as_ref()
                    .and_then(|sender| {
                        sender
                            .try_send(ConnTask::Retire {
                                reason: reason.clone(),
                            })
                            .ok()
                    })
                    .is_some();
                tracing::warn!(
                    event = "server_conn_retired",
                    key = %key,
                    conn_id = %conn_id,
                    reason = %reason,
                    removed_from_service_map,
                    removed_from_active_map,
                    removed_pending_streams,
                    retire_notified,
                    registered_services = server_conn_map.len(),
                    server_connections = registered_server_conn_count(&server_conn_map),
                    active_connections = manager.active_conn_count(),
                    idle_connections = manager.idle_conn_count(),
                    "server connection retired"
                );
            }
            ManagerTask::DeRegisterClientConn {
                server_id,
                client_id,
            } => {
                let removed_namespace = pending_streams.remove(&client_id).map(|(_, _, key)| {
                    let namespace = split_scoped_service_key(&key).0;
                    decrement_namespace_stream_count(&mut namespace_stream_counts, namespace);
                    namespace
                });
                let removed_server_conn = if let Some(server_id) = server_id {
                    manager.deregister_conn(server_id)
                } else {
                    false
                };
                let removed_client_conn = manager.deregister_conn(client_id);
                if let Some(namespace) = removed_namespace {
                    release_namespace_rate_limit_if_idle(
                        namespace,
                        &server_conn_map,
                        &pending_streams,
                        &mut namespace_rate_limits,
                    );
                }
                if removed_server_conn || removed_client_conn {
                    tracing::info!(
                        event = "client_conn_deregistered",
                        server_conn_id = ?server_id,
                        client_conn_id = %client_id,
                        removed_server_conn,
                        removed_client_conn,
                        registered_services = server_conn_map.len(),
                        server_connections = registered_server_conn_count(&server_conn_map),
                        active_connections = manager.active_conn_count(),
                        idle_connections = manager.idle_conn_count(),
                        "client connection deregistered"
                    );
                } else {
                    tracing::debug!(
                        event = "client_conn_deregister_skipped",
                        server_conn_id = ?server_id,
                        client_conn_id = %client_id,
                        registered_services = server_conn_map.len(),
                        server_connections = registered_server_conn_count(&server_conn_map),
                        active_connections = manager.active_conn_count(),
                        idle_connections = manager.idle_conn_count(),
                        "client connection was already inactive"
                    );
                }
            }
            ManagerTask::Register {
                key,
                conn_id,
                conn_sender,
                need_codec,
                is_datagram,
                protocol_version,
            } => {
                let namespace = split_scoped_service_key(&key).0;
                let existing = server_conn_map.get(&key);
                let failure = if existing.is_some_and(|connections| {
                    connections
                        .first()
                        .is_some_and(|connection| connection.is_datagram != is_datagram)
                }) {
                    Some((
                        "service_transport_mismatch",
                        "the service name is already registered with a different transport",
                        false,
                    ))
                } else if existing.is_some_and(|connections| {
                    connections.len() >= max_register_connections_per_service
                }) {
                    Some((
                        "service_connection_limit_exceeded",
                        "the service has reached its register connection limit",
                        true,
                    ))
                } else if existing.is_none()
                    && server_conn_map
                        .keys()
                        .filter(|registered| split_scoped_service_key(registered).0 == namespace)
                        .count()
                        >= max_services_per_namespace
                {
                    Some((
                        "namespace_service_limit_exceeded",
                        "the namespace has reached its service name limit",
                        true,
                    ))
                } else {
                    None
                };
                if let Some((code, reason, retryable)) = failure {
                    let _ = conn_sender
                        .send(ConnTask::RegisterFailed {
                            code: code.to_string(),
                            reason: reason.to_string(),
                            retryable,
                        })
                        .await;
                    continue;
                }
                let generation = next_server_generation;
                next_server_generation = next_server_generation.saturating_add(1).max(1);
                let now = Instant::now();
                // sign up server connection
                manager.sign_up_conn_sender(conn_id, conn_sender.clone());
                match server_conn_map.entry(key.clone()) {
                    hashbrown::hash_map::Entry::Occupied(mut o) => {
                        o.get_mut().push(ServerConnInfo {
                            conn_id,
                            generation,
                            health: ServerConnHealth::Healthy,
                            need_codec,
                            is_datagram,
                            protocol_version,
                            last_rx_at: now,
                        });
                    }
                    hashbrown::hash_map::Entry::Vacant(v) => {
                        v.insert(vec![ServerConnInfo {
                            conn_id,
                            generation,
                            health: ServerConnHealth::Healthy,
                            need_codec,
                            is_datagram,
                            protocol_version,
                            last_rx_at: now,
                        }]);
                    }
                }

                // response registered ok
                tracing::info!(
                    event = "server_conn_registered",
                    key = %key,
                    conn_id = %conn_id,
                    generation,
                    protocol_version,
                    need_codec,
                    is_datagram,
                    service_connections = service_conn_count(&server_conn_map, &key),
                    registered_services = server_conn_map.len(),
                    server_connections = registered_server_conn_count(&server_conn_map),
                    active_connections = manager.active_conn_count(),
                    idle_connections = manager.idle_conn_count(),
                    "server connection registered"
                );
                snafu_error_get_or_continue!(conn_sender
                    .send(ConnTask::RegisterResp {
                        generation,
                        protocol_version,
                        lease_ttl_ms: server_lease_timeout().as_millis() as u64,
                    })
                    .await
                    .map_err(|_| kanal::SendError(()))
                    .context(TaskCenterSendRegisterRespSnafu { key, conn_id }));
            }
            ManagerTask::Stream {
                key,
                stream,
                session,
                server_id,
                client_id,
                server_generation,
            } => {
                let Some((expected_control_conn_id, expected_generation, expected_key)) =
                    pending_streams.get(&client_id).cloned()
                else {
                    tracing::warn!(
                        event = "stale_stream_without_pending_client",
                        server_conn_id = %server_id,
                        client_conn_id = %client_id,
                        server_generation,
                        "dropping stream for client without pending subscribe"
                    );
                    continue;
                };
                if key != expected_key {
                    tracing::warn!(
                        event = "stream_namespace_mismatch",
                        stream_conn_id = %server_id,
                        client_conn_id = %client_id,
                        expected_key = %expected_key,
                        actual_key = %key,
                        "dropping stream that does not belong to the pending namespace and service"
                    );
                    continue;
                }
                if server_generation != 0 && expected_generation != server_generation {
                    tracing::warn!(
                        event = "stale_stream_generation_mismatch",
                        stream_conn_id = %server_id,
                        client_conn_id = %client_id,
                        expected_control_conn_id = %expected_control_conn_id,
                        expected_generation,
                        server_generation,
                        "dropping stale stream for a previous subscribe attempt"
                    );
                    continue;
                }
                if let Some(info) = server_conn_map
                    .values_mut()
                    .flat_map(|infos| infos.iter_mut())
                    .find(|info| {
                        info.conn_id == expected_control_conn_id
                            && info.generation == expected_generation
                    })
                {
                    info.health = ServerConnHealth::Healthy;
                }
                tracing::debug!(
                    event = "stream_ready_for_client",
                    stream_conn_id = %server_id,
                    control_conn_id = %expected_control_conn_id,
                    client_conn_id = %client_id,
                    server_generation = expected_generation,
                    active_connections = manager.active_conn_count(),
                    "server stream ready for client"
                );
                let client_sender = snafu_error_get_or_continue!(manager
                    .get_conn_sender_chan(&client_id)
                    .context(TaskCenterStreamConnIdNotExistSnafu { conn_id: client_id }));
                snafu_error_handle!(client_sender
                    .send(ConnTask::StreamResp {
                        server_id,
                        server_generation: expected_generation,
                        stream,
                        session,
                    })
                    .await
                    .map_err(|_| kanal::SendError(()))
                    .context(TaskCenterSendStreamRespToClientSnafu { conn_id: client_id }));
            }
            ManagerTask::StreamAck {
                server_id,
                client_id,
                server_generation,
            } => {
                let recorded_activity =
                    record_server_conn_activity_by_conn_id(&mut server_conn_map, server_id);
                let Some((expected_server_id, expected_generation, _)) =
                    pending_streams.get(&client_id).cloned()
                else {
                    tracing::warn!(
                        event = "stale_stream_ack_without_pending_client",
                        server_conn_id = %server_id,
                        client_conn_id = %client_id,
                        server_generation,
                        recorded_activity,
                        "dropping stream ack for client without pending subscribe"
                    );
                    continue;
                };
                if expected_server_id != server_id || expected_generation != server_generation {
                    tracing::warn!(
                        event = "stale_stream_ack_generation_mismatch",
                        server_conn_id = %server_id,
                        client_conn_id = %client_id,
                        expected_server_conn_id = %expected_server_id,
                        expected_generation,
                        server_generation,
                        recorded_activity,
                        "dropping stale stream ack for a previous subscribe attempt"
                    );
                    continue;
                }
                if let Some(info) = server_conn_map
                    .values_mut()
                    .flat_map(|infos| infos.iter_mut())
                    .find(|info| info.conn_id == server_id && info.generation == server_generation)
                {
                    info.health = ServerConnHealth::Healthy;
                }
                let client_sender = snafu_error_get_or_continue!(manager
                    .get_conn_sender_chan(&client_id)
                    .context(TaskCenterStreamConnIdNotExistSnafu { conn_id: client_id }));
                snafu_error_handle!(client_sender
                    .send(ConnTask::StreamAck {
                        server_id,
                        server_generation,
                    })
                    .await
                    .map_err(|_| kanal::SendError(()))
                    .context(TaskCenterSendStreamRespToClientSnafu { conn_id: client_id }));
            }
            ManagerTask::Subcribe {
                key,
                conn_id,
                conn_sender,
                excluded_server_conns,
            } => {
                let namespace = split_scoped_service_key(&key).0;
                if namespace_stream_counts
                    .get(&namespace)
                    .copied()
                    .unwrap_or_default()
                    >= max_streams_per_namespace
                {
                    let _ = conn_sender
                        .send(ConnTask::SubcribeFailed {
                            code: "namespace_stream_limit_exceeded".to_string(),
                            reason: "the namespace has reached its active stream limit".to_string(),
                            retryable: true,
                        })
                        .await;
                    continue;
                }
                let Some(server_conn_id_list) = server_conn_map.get(&key).cloned() else {
                    let reason = format!("server key `{key}` is not registered");
                    tracing::warn!(
                        event = "subscribe_key_missing",
                        key = %key,
                        client_conn_id = %conn_id,
                        excluded_server_conns = ?excluded_server_conns,
                        registered_services = server_conn_map.len(),
                        server_connections = registered_server_conn_count(&server_conn_map),
                        "subscribe key is not registered"
                    );
                    if excluded_server_conns.is_empty() {
                        send_subcribe_failed(&conn_sender, &key, conn_id, reason).await;
                    } else {
                        send_subcribe_retry(&conn_sender, &key, conn_id, reason).await;
                    }
                    continue;
                };
                if !namespace_rate_limits
                    .entry(namespace)
                    .or_insert_with(|| {
                        NamespaceRateLimit::new(new_streams_per_second, new_streams_burst)
                    })
                    .allow()
                {
                    let _ = conn_sender
                        .send(ConnTask::SubcribeFailed {
                            code: "namespace_stream_rate_exceeded".to_string(),
                            reason: "the namespace new-stream rate limit was exceeded".to_string(),
                            retryable: true,
                        })
                        .await;
                    continue;
                }
                let mut selected = false;
                let mut candidates = Vec::new();
                candidates.extend(server_conn_id_list.iter().rev().copied().filter(|info| {
                    info.health == ServerConnHealth::Healthy
                        && !excluded_server_conns.contains(&(info.conn_id, info.generation))
                }));
                for server_info in candidates {
                    let ServerConnInfo {
                        conn_id: server_conn_id,
                        generation: server_generation,
                        health,
                        need_codec,
                        is_datagram,
                        protocol_version: _,
                        last_rx_at: _,
                    } = server_info;
                    let Some(server_conn_sender) = manager.get_conn_sender_chan(&server_conn_id)
                    else {
                        tracing::warn!(
                            event = "subscribe_stale_server_conn",
                            key = %key,
                            client_conn_id = %conn_id,
                            server_conn_id = %server_conn_id,
                            reason = "sender_not_found",
                            "subscribe skipped stale server connection"
                        );
                        remove_server_conn(&mut server_conn_map, &key, server_conn_id);
                        let _ = manager.deregister_conn(server_conn_id);
                        continue;
                    };
                    // 1. Send a request to get server stream
                    if let Err(e) = server_conn_sender
                        .send(ConnTask::StreamReq {
                            client_id: conn_id,
                            server_generation,
                        })
                        .await
                        .map_err(|_| kanal::SendError(()))
                        .context(TaskCenterClientSendStreamSnafu {
                            key: key.clone(),
                            conn_id,
                        })
                    {
                        let report = snafu::Report::from_error(e);
                        tracing::error!(
                            event = "subscribe_stream_request_failed",
                            key = %key,
                            client_conn_id = %conn_id,
                            server_conn_id = %server_conn_id,
                            error = %report,
                            "failed to send stream request to registered server"
                        );
                        remove_server_conn(&mut server_conn_map, &key, server_conn_id);
                        let _ = manager.deregister_conn(server_conn_id);
                        continue;
                    }
                    // sign up client connection after a server accepted the stream request
                    if manager.get_conn_sender_chan(&conn_id).is_none() {
                        manager.sign_up_conn_sender(conn_id, conn_sender.clone());
                    }
                    let is_new_stream = pending_streams
                        .insert(conn_id, (server_conn_id, server_generation, key.clone()))
                        .is_none();
                    if is_new_stream {
                        *namespace_stream_counts.entry(namespace).or_default() += 1;
                    }
                    // 2. Response subcribe ok
                    if let Err(e) = conn_sender
                        .send(ConnTask::SubcribeResp {
                            server_conn_id,
                            server_generation,
                            need_codec,
                            is_datagram,
                        })
                        .await
                        .map_err(|_| kanal::SendError(()))
                        .context(TaskCenterSendSubcribeRespSnafu {
                            key: key.clone(),
                            conn_id,
                        })
                    {
                        let report = snafu::Report::from_error(e);
                        tracing::error!(
                            event = "subscribe_response_failed",
                            key = %key,
                            client_conn_id = %conn_id,
                            server_conn_id = %server_conn_id,
                            error = %report,
                            "failed to send subscribe response to client"
                        );
                        manager.deregister_conn(conn_id);
                        selected = true;
                        break;
                    }
                    tracing::info!(
                        event = "subscribe_server_selected",
                        key = %key,
                        client_conn_id = %conn_id,
                        server_conn_id = %server_conn_id,
                        server_generation,
                        health = ?health,
                        need_codec,
                        is_datagram,
                        service_connections = service_conn_count(&server_conn_map, &key),
                        active_connections = manager.active_conn_count(),
                        "selected server connection for client subscribe"
                    );
                    selected = true;
                    break;
                }
                if !selected {
                    let reason = format!("no usable server connection for key `{key}`");
                    tracing::warn!(
                        event = "subscribe_no_usable_server_conn",
                        key = %key,
                        client_conn_id = %conn_id,
                        excluded_server_conns = ?excluded_server_conns,
                        registered_services = server_conn_map.len(),
                        server_connections = registered_server_conn_count(&server_conn_map),
                        "no usable server connection for subscribe"
                    );
                    if excluded_server_conns.is_empty() {
                        send_subcribe_failed(&conn_sender, &key, conn_id, reason).await;
                    } else {
                        send_subcribe_retry(&conn_sender, &key, conn_id, reason).await;
                    }
                }
            }
            ManagerTask::Shutdown => {
                tracing::info!("Server shutdown requested, stopping main loop");
                break;
            }
        }
    }

    // Gracefully shutdown the listener
    listener_handle.abort();
    shutdown_handle.abort();
    if let Some(handle) = status_forward_handle {
        handle.abort();
    }
    tracing::info!("Server shutdown completed");
    Ok(())
}

async fn handle_listener(
    task_sender: ManagerTaskSender,
    listener: TcpListener,
    keep_alive: bool,
) -> Result<()> {
    loop {
        let (stream, addr) = listener.accept().await.context(ServerListenSnafu)?;
        tracing::debug!(
            event = "tcp_conn_accepted",
            peer_addr = %addr,
            "accepted tcp connection"
        );
        // set keepalive (optional) and nodelay
        if keep_alive {
            snafu_error_handle!(set_tcp_keep_alive(&stream).context(TaskCenterSetKeepAliveSnafu));
        }
        snafu_error_handle!(set_tcp_nodelay(&stream), "remote stream set nodelay");
        task_sender
            .send(ManagerTask::Accept {
                stream,
                peer_addr: addr,
            })
            .await
            .map_err(|_| kanal::SendError(()))
            .context(TaskCenterSendListenerSnafu)?
    }
}

#[instrument(skip(manager_task_sender, conn, security), fields(conn_id = %conn_id, peer_addr = %peer_addr))]
async fn handle_conn(
    conn_id: RemoteConnId,
    peer_addr: SocketAddr,
    manager_task_sender: ManagerTaskSender,
    mut conn: TcpStream,
    security: ServerSecurity,
) -> Result<()> {
    let timeout = control_io_timeout();
    let initial = match tokio::time::timeout(timeout, security.read_initial(&mut conn)).await {
        Err(_) => TaskCenterInitRequestTimeoutSnafu { conn_id, timeout }.fail()?,
        Ok(Err(error)) => {
            let key_id = error
                .response_session
                .as_ref()
                .map(|session| session.key_id())
                .unwrap_or_default();
            let decision = security.record_failure_log(peer_addr.ip(), key_id, &error.failure.code);
            if decision.suppressed > 0 {
                tracing::warn!(
                    event = "auth_failures_suppressed",
                    peer_ip = %peer_addr.ip(),
                    key_id,
                    reason = %error.failure.code,
                    suppressed = decision.suppressed,
                    "suppressed repeated authentication failures in the previous window"
                );
            }
            if decision.emit {
                tracing::warn!(
                    event = "auth_failed",
                    auth_stage = "initial_frame",
                    conn_id = %conn_id,
                    peer_addr = %peer_addr,
                    key_id,
                    reason = %error.failure.code,
                    retryable = error.failure.retryable,
                    error = %error.failure.message,
                    "connection authentication failed"
                );
            }
            if let Some(session) = error.response_session {
                write_protocol_error(&mut conn, &session, &error.failure).await;
            }
            return Ok(());
        }
        Ok(Ok(initial)) => initial,
    };
    let init_request = match PbConnRequest::decode(&initial.payload) {
        Ok(request) => request,
        Err(error) => {
            tracing::warn!(
                event = "auth_failed",
                auth_stage = "request_decode",
                conn_id = %conn_id,
                peer_addr = %peer_addr,
                error = %error,
                "authenticated request could not be decoded"
            );
            write_protocol_error(
                &mut conn,
                &initial.session,
                &crate::common::auth::AuthFailure::new(
                    "request_decode_failed",
                    "authenticated request payload is malformed",
                    false,
                ),
            )
            .await;
            return Ok(());
        }
    };
    let mut requested_namespace = None;
    let mut force_register_namespace = false;
    let init_request = match init_request {
        PbConnRequest::RegisterScoped {
            need_codec,
            is_datagram,
            key,
            namespace,
            force_namespace,
            protocol_version,
            client_instance_id,
            heartbeat_interval_ms,
            heartbeat_tolerance_ms,
        } => {
            requested_namespace = Some(namespace);
            force_register_namespace = force_namespace;
            PbConnRequest::Register {
                need_codec,
                is_datagram,
                key,
                protocol_version,
                client_instance_id,
                heartbeat_interval_ms,
                heartbeat_tolerance_ms,
            }
        }
        PbConnRequest::SubcribeScoped { key, namespace } => {
            requested_namespace = Some(namespace);
            PbConnRequest::Subcribe { key }
        }
        PbConnRequest::StatusScoped { status, namespace } => {
            requested_namespace = Some(namespace);
            PbConnRequest::Status(status)
        }
        PbConnRequest::StreamScoped {
            key,
            namespace,
            dst_id,
            server_generation,
        } => {
            requested_namespace = Some(namespace);
            PbConnRequest::Stream {
                key,
                dst_id,
                server_generation,
            }
        }
        request => request,
    };
    let session = initial.session;
    let auth_context = match session.context() {
        Ok(context) => context.clone(),
        Err(error) => {
            tracing::warn!(conn_id = %conn_id, peer_addr = %peer_addr, %error, "missing auth context");
            return Ok(());
        }
    };
    tracing::info!(
        event = "auth_succeeded",
        auth_stage = "session",
        conn_id = %conn_id,
        peer_addr = %peer_addr,
        key_id = auth_context.key_id,
        namespace = auth_context.namespace,
        protocol = ?session.protocol(),
        is_admin = auth_context.is_admin,
        "connection authentication succeeded"
    );
    let effective_namespace = match resolve_namespace(
        &auth_context,
        requested_namespace,
        force_register_namespace,
        matches!(&init_request, PbConnRequest::Register { .. }),
    ) {
        Ok(namespace) => namespace,
        Err(failure) => {
            write_protocol_error(&mut conn, &session, &failure).await;
            return Ok(());
        }
    };
    match init_request {
        PbConnRequest::Register {
            key,
            need_codec,
            is_datagram,
            protocol_version,
            client_instance_id,
            heartbeat_interval_ms,
            heartbeat_tolerance_ms,
        } => {
            let protocol_version = protocol_version.unwrap_or(1);
            tracing::info!(
                event = "init_request",
                request = "register",
                conn_id = %conn_id,
                peer_addr = %peer_addr,
                key = %key,
                protocol_version,
                client_instance_id = ?client_instance_id,
                heartbeat_interval_ms = ?heartbeat_interval_ms,
                heartbeat_tolerance_ms = ?heartbeat_tolerance_ms,
                need_codec,
                is_datagram,
                "received pb init request"
            );
            let key = match scoped_service_key(&auth_context, effective_namespace, &key) {
                Ok(key) => key,
                Err(failure) => {
                    write_protocol_error(&mut conn, &session, &failure).await;
                    return Ok(());
                }
            };
            let cancellation = match auth_context.cancellation_token() {
                Ok(token) => token,
                Err(failure) => {
                    write_protocol_error(&mut conn, &session, &failure).await;
                    return Ok(());
                }
            };
            tokio::select! {
                result = handle_server_conn(
                ServerRegistration {
                    key,
                    need_codec,
                    is_datagram,
                    protocol_version,
                    conn_id,
                },
                manager_task_sender,
                conn,
                session,
                ) => result?,
                _ = cancellation.cancelled() => {
                    tracing::info!(event = "connection_auth_expired", key_id = auth_context.key_id, conn_id = %conn_id, "closing registered service connection");
                }
            }
        }
        PbConnRequest::Subcribe { key } => {
            tracing::info!(
                event = "init_request",
                request = "subscribe",
                conn_id = %conn_id,
                peer_addr = %peer_addr,
                key = %key,
                "received pb init request"
            );
            let key = match scoped_service_key(&auth_context, effective_namespace, &key) {
                Ok(key) => key,
                Err(failure) => {
                    write_protocol_error(&mut conn, &session, &failure).await;
                    return Ok(());
                }
            };
            let cancellation = match auth_context.cancellation_token() {
                Ok(token) => token,
                Err(failure) => {
                    write_protocol_error(&mut conn, &session, &failure).await;
                    return Ok(());
                }
            };
            tokio::select! {
                result = handle_client_conn(key, conn_id, manager_task_sender, conn, session) => result?,
                _ = cancellation.cancelled() => {
                    tracing::info!(event = "connection_auth_expired", key_id = auth_context.key_id, conn_id = %conn_id, "closing subscribed data connection");
                }
            }
        }
        PbConnRequest::Stream {
            key,
            dst_id,
            server_generation,
        } => {
            tracing::debug!(
                event = "init_request",
                request = "stream",
                conn_id = %conn_id,
                peer_addr = %peer_addr,
                key = %key,
                client_conn_id = dst_id,
                server_generation,
                "received pb init request"
            );
            let key = match scoped_service_key(&auth_context, effective_namespace, &key) {
                Ok(key) => key,
                Err(failure) => {
                    write_protocol_error(&mut conn, &session, &failure).await;
                    return Ok(());
                }
            };
            manager_task_sender
                .send(ManagerTask::Stream {
                    key: key.clone(),
                    stream: conn,
                    session,
                    server_id: conn_id,
                    client_id: dst_id.into(),
                    server_generation,
                })
                .await
                .map_err(|_| kanal::SendError(()))
                .context(TaskCenterSendStreamRespToManagerSnafu { key, conn_id })?;
        }
        PbConnRequest::Status(status) => {
            tracing::debug!(
                event = "init_request",
                request = "status",
                conn_id = %conn_id,
                peer_addr = %peer_addr,
                status = ?status,
                "received pb init request"
            );
            handle_show_status(
                status,
                effective_namespace,
                manager_task_sender,
                conn_id,
                conn,
                session,
            )
            .await?;
        }
        PbConnRequest::Admin(request) => {
            if !auth_context.is_admin {
                write_protocol_error(
                    &mut conn,
                    &session,
                    &crate::common::auth::AuthFailure::new(
                        "admin_permission_required",
                        "administrator credential is required for this operation",
                        false,
                    ),
                )
                .await;
                return Ok(());
            }
            handle_admin_request(
                request,
                security.auth().clone(),
                manager_task_sender,
                conn_id,
                conn,
                session,
            )
            .await?;
        }
        PbConnRequest::RegisterScoped { .. }
        | PbConnRequest::SubcribeScoped { .. }
        | PbConnRequest::StatusScoped { .. }
        | PbConnRequest::StreamScoped { .. } => unreachable!("scoped request was normalized"),
    }
    Ok(())
}

async fn write_protocol_error(
    conn: &mut TcpStream,
    session: &ServerHeaderSession,
    failure: &crate::common::auth::AuthFailure,
) {
    let response = PbConnResponse::error(
        failure.code.clone(),
        failure.message.clone(),
        failure.retryable,
    );
    let Ok(message) = response.encode() else {
        return;
    };
    let Ok(mut writer) = session.response_writer(conn) else {
        return;
    };
    if let Err(error) = writer.write_msg(&message).await {
        tracing::debug!(%error, reason = %failure.code, "failed to write structured protocol error");
    }
}

fn resolve_namespace(
    context: &AuthContext,
    requested: Option<u64>,
    force_register_namespace: bool,
    is_register: bool,
) -> std::result::Result<u64, crate::common::auth::AuthFailure> {
    let namespace = requested.unwrap_or(context.namespace);
    if !context.is_admin && namespace != context.namespace {
        return Err(crate::common::auth::AuthFailure::new(
            "namespace_access_denied",
            "temporary credentials can only access their own namespace",
            false,
        ));
    }
    if context.is_admin && is_register && namespace != 0 && !force_register_namespace {
        return Err(crate::common::auth::AuthFailure::new(
            "namespace_force_required",
            "administrator registration in a temporary namespace requires --force",
            false,
        ));
    }
    Ok(namespace)
}

fn scoped_service_key(
    context: &AuthContext,
    namespace: u64,
    service_name: &str,
) -> std::result::Result<ImutableKey, crate::common::auth::AuthFailure> {
    if service_name.is_empty() || service_name.len() > 1024 || service_name.contains('\0') {
        return Err(crate::common::auth::AuthFailure::new(
            "service_name_invalid",
            "service names must be 1-1024 bytes and must not contain NUL",
            false,
        ));
    }
    if !context.is_admin
        && (service_name.len() > 128
            || !service_name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._:-".contains(&byte)))
    {
        return Err(crate::common::auth::AuthFailure::new(
            "service_name_invalid",
            "temporary-key service names must be 1-128 ASCII bytes from [A-Za-z0-9._:-]",
            false,
        ));
    }
    if namespace == 0 {
        Ok(Arc::from(service_name))
    } else {
        Ok(Arc::from(format!("@{namespace:016x}\u{0}{service_name}")))
    }
}

fn split_scoped_service_key(key: &str) -> (u64, &str) {
    let Some((prefix, name)) = key.split_once('\0') else {
        return (0, key);
    };
    let Some(hex) = prefix.strip_prefix('@') else {
        return (0, key);
    };
    match u64::from_str_radix(hex, 16) {
        Ok(namespace) => (namespace, name),
        Err(_) => (0, key),
    }
}

fn decrement_namespace_stream_count(
    namespace_stream_counts: &mut hashbrown::HashMap<u64, usize>,
    namespace: u64,
) {
    let Some(count) = namespace_stream_counts.get_mut(&namespace) else {
        return;
    };
    *count = count.saturating_sub(1);
    if *count == 0 {
        namespace_stream_counts.remove(&namespace);
    }
}

fn release_namespace_rate_limit_if_idle(
    namespace: u64,
    server_conn_map: &ServerConnMap,
    pending_streams: &hashbrown::HashMap<RemoteConnId, (RemoteConnId, u64, ImutableKey)>,
    namespace_rate_limits: &mut hashbrown::HashMap<u64, NamespaceRateLimit>,
) {
    let has_registered_service = server_conn_map
        .keys()
        .any(|key| split_scoped_service_key(key).0 == namespace);
    let has_pending_stream = pending_streams
        .values()
        .any(|(_, _, key)| split_scoped_service_key(key).0 == namespace);
    if !has_registered_service && !has_pending_stream {
        namespace_rate_limits.remove(&namespace);
    }
}

fn remove_pending_streams_for_server(
    pending_streams: &mut hashbrown::HashMap<RemoteConnId, (RemoteConnId, u64, ImutableKey)>,
    namespace_stream_counts: &mut hashbrown::HashMap<u64, usize>,
    server_id_to_remove: RemoteConnId,
) -> usize {
    let mut removed = 0;
    pending_streams.retain(|_, (server_id, _, key)| {
        if *server_id != server_id_to_remove {
            return true;
        }
        decrement_namespace_stream_count(namespace_stream_counts, split_scoped_service_key(key).0);
        removed += 1;
        false
    });
    removed
}

pub async fn get_init_request(
    conn: &mut TcpStream,
    conn_id: RemoteConnId,
) -> Result<PbConnRequest> {
    let mut reader =
        get_header_msg_reader(conn).context(TaskCenterReadInitRequestSnafu { conn_id })?;
    let timeout = control_io_timeout();
    let msg = match tokio::time::timeout(timeout, reader.read_msg()).await {
        Ok(result) => result.context(TaskCenterReadInitRequestSnafu { conn_id })?,
        Err(_) => TaskCenterInitRequestTimeoutSnafu { conn_id, timeout }.fail()?,
    };
    PbConnRequest::decode(msg).context(TaskCenterDecodeInitRequestSnafu { conn_id })
}
