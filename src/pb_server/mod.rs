mod client;
mod error;
mod server;
mod status;

use std::net::SocketAddr;
use std::sync::Arc;

use error::Result;
use snafu::{OptionExt, ResultExt};
use tokio::net::{TcpListener, TcpStream, ToSocketAddrs};
use tokio_util::sync::CancellationToken;
use tracing::instrument;

use self::client::handle_client_conn;
use self::error::{
    TaskCenterDecodeInitRequestSnafu, TaskCenterInitRequestTimeoutSnafu,
    TaskCenterReadInitRequestSnafu, TaskCenterSendListenerSnafu, TaskCenterSendStatusRespSnafu,
    TaskCenterSendStreamRespToManagerSnafu, TaskCenterSetKeepAliveSnafu,
};
use self::server::handle_server_conn;
use self::status::handle_show_status;
use crate::common::config::{control_io_timeout, IS_KEEPALIVE};
use crate::common::conn_id::{ConnIdProvider, RemoteConnId};
use crate::common::manager::{ForwardMessage, SenderChan, TaskManager};
use crate::common::message::command::{
    MessageSerializer, PbConnRequest, PbConnResponse, PbConnStatusReq, PbConnStatusResp,
};
use crate::common::message::{get_header_msg_reader, MessageReader};
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
        conn_sender: ConnTaskSender,
    },
    Subcribe {
        key: ImutableKey,
        conn_id: RemoteConnId,
        conn_sender: ConnTaskSender,
        excluded_server_conn_ids: Vec<RemoteConnId>,
    },
    Stream {
        stream: TcpStream,
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
        conn_id: RemoteConnId,
    },
    StatusQuery {
        response_sender: tokio::sync::oneshot::Sender<ServerStatusInfo>,
    },
    DeRegisterServerConn {
        key: ImutableKey,
        conn_id: RemoteConnId,
    },
    MarkServerConnSuspect {
        key: ImutableKey,
        conn_id: RemoteConnId,
    },
    DeRegisterClientConn {
        server_id: Option<RemoteConnId>,
        client_id: RemoteConnId,
    },
    Shutdown,
}

/// TODO: Add a task that notifies the writer to release
#[derive(Debug)]
pub enum ConnTask {
    Forward(ForwardMessage),
    RegisterResp,
    SubcribeResp {
        server_conn_id: RemoteConnId,
        server_generation: u64,
        need_codec: bool,
        is_datagram: bool,
    },
    SubcribeFailed {
        reason: String,
    },
    StreamReq {
        client_id: RemoteConnId,
        server_generation: u64,
    },
    StreamAck {
        server_id: RemoteConnId,
        server_generation: u64,
    },
    StreamResp {
        server_id: RemoteConnId,
        server_generation: u64,
        stream: TcpStream,
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
}

pub type ServerConnMap = hashbrown::HashMap<ImutableKey, Vec<ServerConnInfo>>;

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

fn mark_server_conn_health(
    server_conn_map: &mut ServerConnMap,
    key: &ImutableKey,
    conn_id: RemoteConnId,
    health: ServerConnHealth,
) -> bool {
    let Some(conns) = server_conn_map.get_mut(key) else {
        return false;
    };
    let Some(info) = conns.iter_mut().find(|info| info.conn_id == conn_id) else {
        return false;
    };
    info.health = health;
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
            reason: reason.clone(),
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

pub async fn run_server<A: ToSocketAddrs>(addr: A) -> std::io::Result<()> {
    run_server_with_shutdown(addr, CancellationToken::new(), None).await
}

pub async fn run_server_with_shutdown<A: ToSocketAddrs>(
    addr: A,
    shutdown_token: CancellationToken,
    status_channel: Option<
        tokio::sync::mpsc::UnboundedReceiver<tokio::sync::oneshot::Sender<ServerStatusInfo>>,
    >,
) -> std::io::Result<()> {
    let mut manager = ServerMananger::new(RemoteIdProvider::new());
    // represent the mapping of the `key` to the id of the server-side conn
    let mut server_conn_map = ServerConnMap::new();
    let mut pending_streams = hashbrown::HashMap::<RemoteConnId, (RemoteConnId, u64)>::new();
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
            result = handle_listener(task_sender, listener) => {
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
                conn_id,
            } => {
                let resp = match status {
                    PbConnStatusReq::RemoteId => {
                        PbConnResponse::Status(PbConnStatusResp::RemoteId {
                            server_map: format!("{server_conn_map:?}"),
                            active: manager.active_conn_id_msg(),
                            idle: manager.idle_conn_id_msg(),
                        })
                    }
                    PbConnStatusReq::Keys => PbConnResponse::Status(PbConnStatusResp::Keys(
                        server_conn_map.keys().map(|k| k.to_string()).collect(),
                    )),
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
                tokio::spawn(async move {
                    snafu_error_handle!(
                        handle_conn(conn_id, peer_addr, manager_task_sender, stream).await
                    );
                });
            }
            ManagerTask::DeRegisterServerConn { key, conn_id } => {
                let removed_from_service_map =
                    remove_server_conn(&mut server_conn_map, &key, conn_id);
                let removed_from_active_map = manager.deregister_conn(conn_id);
                pending_streams.retain(|_, (server_id, _)| *server_id != conn_id);
                tracing::info!(
                    event = "server_conn_deregistered",
                    key = %key,
                    conn_id = %conn_id,
                    removed_from_service_map,
                    removed_from_active_map,
                    registered_services = server_conn_map.len(),
                    server_connections = registered_server_conn_count(&server_conn_map),
                    active_connections = manager.active_conn_count(),
                    idle_connections = manager.idle_conn_count(),
                    "server connection deregistered"
                );
            }
            ManagerTask::MarkServerConnSuspect { key, conn_id } => {
                let updated = mark_server_conn_health(
                    &mut server_conn_map,
                    &key,
                    conn_id,
                    ServerConnHealth::Suspect,
                );
                tracing::warn!(
                    event = "server_conn_marked_suspect",
                    key = %key,
                    conn_id = %conn_id,
                    updated,
                    registered_services = server_conn_map.len(),
                    server_connections = registered_server_conn_count(&server_conn_map),
                    "server connection marked suspect"
                );
            }
            ManagerTask::DeRegisterClientConn {
                server_id,
                client_id,
            } => {
                pending_streams.remove(&client_id);
                let removed_server_conn = if let Some(server_id) = server_id {
                    manager.deregister_conn(server_id)
                } else {
                    false
                };
                let removed_client_conn = manager.deregister_conn(client_id);
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
            } => {
                let generation = next_server_generation;
                next_server_generation = next_server_generation.saturating_add(1).max(1);
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
                        });
                    }
                    hashbrown::hash_map::Entry::Vacant(v) => {
                        v.insert(vec![ServerConnInfo {
                            conn_id,
                            generation,
                            health: ServerConnHealth::Healthy,
                            need_codec,
                            is_datagram,
                        }]);
                    }
                }

                // response registered ok
                tracing::info!(
                    event = "server_conn_registered",
                    key = %key,
                    conn_id = %conn_id,
                    generation,
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
                    .send(ConnTask::RegisterResp)
                    .await
                    .map_err(|_| kanal::SendError(()))
                    .context(TaskCenterSendRegisterRespSnafu { key, conn_id }));
            }
            ManagerTask::Stream {
                stream,
                server_id,
                client_id,
                server_generation,
            } => {
                let Some((expected_control_conn_id, expected_generation)) =
                    pending_streams.get(&client_id).copied()
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
                        stream
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
                let Some((expected_server_id, expected_generation)) =
                    pending_streams.get(&client_id).copied()
                else {
                    tracing::warn!(
                        event = "stale_stream_ack_without_pending_client",
                        server_conn_id = %server_id,
                        client_conn_id = %client_id,
                        server_generation,
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
                excluded_server_conn_ids,
            } => {
                let Some(server_conn_id_list) = server_conn_map.get(&key).cloned() else {
                    tracing::warn!(
                        event = "subscribe_key_missing",
                        key = %key,
                        client_conn_id = %conn_id,
                        registered_services = server_conn_map.len(),
                        server_connections = registered_server_conn_count(&server_conn_map),
                        "subscribe key is not registered"
                    );
                    send_subcribe_failed(
                        &conn_sender,
                        &key,
                        conn_id,
                        format!("server key `{key}` is not registered"),
                    )
                    .await;
                    continue;
                };
                let mut selected = false;
                let mut candidates = Vec::new();
                for preferred_health in [ServerConnHealth::Healthy, ServerConnHealth::Suspect] {
                    candidates.extend(server_conn_id_list.iter().rev().copied().filter(|info| {
                        info.health == preferred_health
                            && !excluded_server_conn_ids.contains(&info.conn_id)
                    }));
                }
                for server_info in candidates {
                    let ServerConnInfo {
                        conn_id: server_conn_id,
                        generation: server_generation,
                        health,
                        need_codec,
                        is_datagram,
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
                    pending_streams.insert(conn_id, (server_conn_id, server_generation));
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
                    tracing::warn!(
                        event = "subscribe_no_usable_server_conn",
                        key = %key,
                        client_conn_id = %conn_id,
                        registered_services = server_conn_map.len(),
                        server_connections = registered_server_conn_count(&server_conn_map),
                        "no usable server connection for subscribe"
                    );
                    send_subcribe_failed(
                        &conn_sender,
                        &key,
                        conn_id,
                        format!("no usable server connection for key `{key}`"),
                    )
                    .await;
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

async fn handle_listener(task_sender: ManagerTaskSender, listener: TcpListener) -> Result<()> {
    loop {
        let (stream, addr) = listener.accept().await.context(ServerListenSnafu)?;
        tracing::debug!(
            event = "tcp_conn_accepted",
            peer_addr = %addr,
            "accepted tcp connection"
        );
        // set keepalive (optional) and nodelay
        if *IS_KEEPALIVE {
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

#[instrument(skip(manager_task_sender, conn), fields(conn_id = %conn_id, peer_addr = %peer_addr))]
async fn handle_conn(
    conn_id: RemoteConnId,
    peer_addr: SocketAddr,
    manager_task_sender: ManagerTaskSender,
    mut conn: TcpStream,
) -> Result<()> {
    // handle by action
    let init_request = get_init_request(&mut conn, conn_id).await?;
    match init_request {
        PbConnRequest::Register {
            key,
            need_codec,
            is_datagram,
        } => {
            tracing::info!(
                event = "init_request",
                request = "register",
                conn_id = %conn_id,
                peer_addr = %peer_addr,
                key = %key,
                need_codec,
                is_datagram,
                "received pb init request"
            );
            handle_server_conn(
                key.into(),
                need_codec,
                is_datagram,
                conn_id,
                manager_task_sender,
                conn,
            )
            .await?;
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
            handle_client_conn(key.into(), conn_id, manager_task_sender, conn).await?;
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
            let key = ImutableKey::from(key);
            manager_task_sender
                .send(ManagerTask::Stream {
                    stream: conn,
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
            handle_show_status(status, manager_task_sender, conn_id, conn).await?;
        }
    }
    Ok(())
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
