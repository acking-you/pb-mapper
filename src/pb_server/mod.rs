mod client;
mod error;
mod server;
mod status;

use std::sync::Arc;

use error::Result;
use snafu::{OptionExt, ResultExt};
use tokio::net::{TcpListener, TcpStream, ToSocketAddrs};
use tokio_util::sync::CancellationToken;
use tracing::instrument;

use self::client::handle_client_conn;
use self::error::{
    TaskCenterDecodeInitRequestSnafu, TaskCenterReadInitRequestSnafu, TaskCenterSendListenerSnafu,
    TaskCenterSendStatusRespSnafu, TaskCenterSendStreamRespToManagerSnafu,
    TaskCenterSetKeepAliveSnafu, TaskCenterSubcribeServerConnIdNotExistSnafu,
    TaskCenterSubcribeServerConnKeyNotExistSnafu,
};
use self::server::handle_server_conn;
use self::status::handle_show_status;
use crate::common::config::IS_KEEPALIVE;
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
    Accept(TcpStream),
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
    },
    Stream {
        stream: TcpStream,
        server_id: RemoteConnId,
        client_id: RemoteConnId,
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
        need_codec: bool,
        is_datagram: bool,
    },
    StreamReq(RemoteConnId),
    StreamResp {
        server_id: RemoteConnId,
        stream: TcpStream,
    },
    StatusResp(PbConnResponse),
}

pub(crate) type ManagerTaskSender = SenderChan<ManagerTask>;
pub(crate) type ConnTaskSender = SenderChan<ConnTask>;

pub type ImutableKey = Arc<str>;

#[derive(Debug, Clone, Copy)]
pub struct ServerConnInfo {
    pub conn_id: RemoteConnId,
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

pub async fn run_server<A: ToSocketAddrs>(addr: A) {
    run_server_with_shutdown(addr, CancellationToken::new(), None).await;
}

pub async fn run_server_with_shutdown<A: ToSocketAddrs>(
    addr: A,
    shutdown_token: CancellationToken,
    status_channel: Option<
        tokio::sync::mpsc::UnboundedReceiver<tokio::sync::oneshot::Sender<ServerStatusInfo>>,
    >,
) {
    let mut manager = ServerMananger::new(RemoteIdProvider::new());
    // represent the mapping of the `key` to the id of the server-side conn
    let mut server_conn_map = ServerConnMap::new();

    let listener = TcpListener::bind(addr)
        .await
        .expect("start listener never fails");

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
            ManagerTask::Accept(stream) => {
                let conn_id = manager.get_conn_id(
                    server_conn_map
                        .iter()
                        .flat_map(|(_, ids)| ids.iter().map(|v| v.conn_id)),
                );
                let manager_task_sender = manager.get_task_sender();
                tokio::spawn(async move {
                    snafu_error_handle!(handle_conn(conn_id, manager_task_sender, stream).await);
                });
            }
            ManagerTask::DeRegisterServerConn { key, conn_id } => {
                if let Some(ids) = server_conn_map.get_mut(&key) {
                    if let Some(idx) = ids.iter().position(|info| info.conn_id == conn_id) {
                        ids.remove(idx);
                    }
                    if ids.is_empty() {
                        server_conn_map.remove(&key);
                    }
                }
                manager.deregister_conn(conn_id);
                tracing::info!("DeRegister Server ok! `{key}:{conn_id}`");
            }
            ManagerTask::DeRegisterClientConn {
                server_id,
                client_id,
            } => {
                if let Some(server_id) = server_id {
                    manager.deregister_conn(server_id);
                }
                manager.deregister_conn(client_id);
                tracing::info!(
                    "DeRegister Client ok! `server:{server_id:?}` <-> `client:{client_id}`"
                );
            }
            ManagerTask::Register {
                key,
                conn_id,
                conn_sender,
                need_codec,
                is_datagram,
            } => {
                // sign up server connection
                manager.sign_up_conn_sender(conn_id, conn_sender.clone());
                match server_conn_map.entry(key.clone()) {
                    hashbrown::hash_map::Entry::Occupied(mut o) => {
                        o.get_mut().push(ServerConnInfo {
                            conn_id,
                            need_codec,
                            is_datagram,
                        });
                    }
                    hashbrown::hash_map::Entry::Vacant(v) => {
                        v.insert(vec![ServerConnInfo {
                            conn_id,
                            need_codec,
                            is_datagram,
                        }]);
                    }
                }

                // response registered ok
                tracing::info!("Register Server ok! `{key}:{conn_id}`");
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
            } => {
                let client_sender = snafu_error_get_or_continue!(manager
                    .get_conn_sender_chan(&client_id)
                    .context(TaskCenterStreamConnIdNotExistSnafu { conn_id: client_id }));
                snafu_error_handle!(client_sender
                    .send(ConnTask::StreamResp { server_id, stream })
                    .await
                    .map_err(|_| kanal::SendError(()))
                    .context(TaskCenterSendStreamRespToClientSnafu { conn_id: client_id }));
            }
            ManagerTask::Subcribe {
                key,
                conn_id,
                conn_sender,
            } => {
                // sign up client connection
                manager.sign_up_conn_sender(conn_id, conn_sender.clone());
                let server_conn_id_list = snafu_error_get_or_continue!(server_conn_map
                    .get(&key)
                    .context(TaskCenterSubcribeServerConnKeyNotExistSnafu {
                        key: key.clone(),
                        conn_id
                    }));
                // Get at least one available server_conn_id from the list
                for server_info in server_conn_id_list.iter().rev() {
                    let ServerConnInfo {
                        conn_id: server_conn_id,
                        need_codec,
                        is_datagram,
                    } = *server_info;
                    let server_conn_sender = snafu_error_get_or_continue!(manager
                        .get_conn_sender_chan(&server_conn_id)
                        .context(TaskCenterSubcribeServerConnIdNotExistSnafu {
                            conn_id: server_conn_id,
                            key: key.clone(),
                        }));
                    // 1. Send a request to get server stream
                    snafu_error_get_or_continue!(server_conn_sender
                        .send(ConnTask::StreamReq(conn_id))
                        .await
                        .map_err(|_| kanal::SendError(()))
                        .context(TaskCenterClientSendStreamSnafu {
                            key: key.clone(),
                            conn_id
                        }));
                    // 2. Response subcribe ok
                    snafu_error_get_or_continue!(conn_sender
                        .send(ConnTask::SubcribeResp {
                            need_codec,
                            is_datagram,
                        })
                        .await
                        .map_err(|_| kanal::SendError(()))
                        .context(TaskCenterSendSubcribeRespSnafu {
                            key: key.clone(),
                            conn_id
                        }));
                    tracing::info!(
                                "Subcribe Server ok! \
                                 key:{key},server_conn_id:{server_conn_id},client_conn_id:{conn_id}"
                            );
                    break;
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
}

async fn handle_listener(task_sender: ManagerTaskSender, listener: TcpListener) -> Result<()> {
    loop {
        let (stream, addr) = listener.accept().await.context(ServerListenSnafu)?;
        tracing::info!("Accept new conn: {addr}");
        // set keepalive (optional) and nodelay
        if *IS_KEEPALIVE {
            snafu_error_handle!(set_tcp_keep_alive(&stream).context(TaskCenterSetKeepAliveSnafu));
        }
        snafu_error_handle!(set_tcp_nodelay(&stream), "remote stream set nodelay");
        task_sender
            .send(ManagerTask::Accept(stream))
            .await
            .map_err(|_| kanal::SendError(()))
            .context(TaskCenterSendListenerSnafu)?
    }
}

#[instrument(skip(manager_task_sender, conn))]
async fn handle_conn(
    conn_id: RemoteConnId,
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
            handle_client_conn(key.into(), conn_id, manager_task_sender, conn).await?;
        }
        PbConnRequest::Stream { key, dst_id } => {
            let key = ImutableKey::from(key);
            manager_task_sender
                .send(ManagerTask::Stream {
                    stream: conn,
                    server_id: conn_id,
                    client_id: dst_id.into(),
                })
                .await
                .map_err(|_| kanal::SendError(()))
                .context(TaskCenterSendStreamRespToManagerSnafu { key, conn_id })?;
        }
        PbConnRequest::Status(status) => {
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
    let msg = reader
        .read_msg()
        .await
        .context(TaskCenterReadInitRequestSnafu { conn_id })?;
    PbConnRequest::decode(msg).context(TaskCenterDecodeInitRequestSnafu { conn_id })
}
