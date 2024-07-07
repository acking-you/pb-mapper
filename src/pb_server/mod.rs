mod client;
mod error;
mod server;
mod status;

use std::sync::Arc;
use std::time::Duration;

use error::Result;
use snafu::{OptionExt, ResultExt};
use tokio::net::{TcpListener, TcpStream, ToSocketAddrs};
use tokio::time::sleep;
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
use crate::common::conn_id::{ConnIdProvider, RemoteConnId};
use crate::common::manager::{ForwardMessage, SenderChan, TaskManager};
use crate::common::message::command::{
    MessageSerializer, PbConnRequest, PbConnResponse, PbConnStatusReq, PbConnStatusResp,
};
use crate::common::message::{MessageReader, NormalMessageReader};
use crate::common::stream::set_tcp_keep_alive;
use crate::pb_server::error::{
    TaskCenterClientSendStreamSnafu, TaskCenterSendRegisterRespSnafu,
    TaskCenterSendStreamRespToClientSnafu, TaskCenterSendSubcribeRespSnafu,
    TaskCenterStreamConnIdNotExistSnafu,
};
use crate::{snafu_error_get_or_continue, snafu_error_handle};

pub enum ManagerTask {
    Accept(TcpStream),
    Register {
        key: ImutableKey,
        conn_id: RemoteConnId,
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
    DeRegisterServerConn {
        key: ImutableKey,
        conn_id: RemoteConnId,
    },
    DeRegisterClientConn {
        server_id: Option<RemoteConnId>,
        client_id: RemoteConnId,
    },
}

/// TODO: Add a task that notifies the writer to release
#[derive(Debug)]
pub enum ConnTask {
    Forward(ForwardMessage),
    RegisterResp,
    SubcribeResp,
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

pub type ServerConnMap = hashbrown::HashMap<ImutableKey, Vec<RemoteConnId>>;

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
    let mut manager = ServerMananger::new(RemoteIdProvider::new());
    // represent the mapping of the `key` to the id of the server-side conn
    let mut server_conn_map = ServerConnMap::new();

    let listener = TcpListener::bind(addr)
        .await
        .expect("start listener never fails");

    let task_sender = manager.get_task_sender();

    tokio::spawn(async move {
        snafu_error_handle!(handle_listener(task_sender, listener).await);
    });

    loop {
        let task = snafu_error_get_or_continue!(manager.wait_for_task().await);

        match task {
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
                    .send_async(ConnTask::StatusResp(resp))
                    .await
                    .context(TaskCenterSendStatusRespSnafu { conn_id }));
            }
            ManagerTask::Accept(stream) => {
                let conn_id =
                    manager.get_conn_id(server_conn_map.iter().flat_map(|(_, ids)| ids).copied());
                let manager_task_sender = manager.get_task_sender();
                tokio::spawn(async move {
                    snafu_error_handle!(handle_conn(conn_id, manager_task_sender, stream).await);
                });
            }
            ManagerTask::DeRegisterServerConn { key, conn_id } => {
                if let Some(ids) = server_conn_map.get_mut(&key) {
                    if let Some(idx) = ids.iter().position(|&v| v == conn_id) {
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
            } => {
                // sign up server connection
                manager.sign_up_conn_sender(conn_id, conn_sender.clone());
                match server_conn_map.entry(key.clone()) {
                    hashbrown::hash_map::Entry::Occupied(mut o) => {
                        o.get_mut().push(conn_id);
                    }
                    hashbrown::hash_map::Entry::Vacant(v) => {
                        v.insert(vec![conn_id]);
                    }
                }

                // response registered ok
                tracing::info!("Register Server ok! `{key}:{conn_id}`");
                snafu_error_get_or_continue!(conn_sender
                    .send_async(ConnTask::RegisterResp)
                    .await
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
                    .send_async(ConnTask::StreamResp { server_id, stream })
                    .await
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
                for server_conn_id in server_conn_id_list.iter().rev() {
                    let server_conn_sender = snafu_error_get_or_continue!(manager
                        .get_conn_sender_chan(server_conn_id)
                        .context(TaskCenterSubcribeServerConnIdNotExistSnafu {
                            conn_id: *server_conn_id,
                            key: key.clone(),
                        }));
                    // 1. Send a request to get server stream
                    snafu_error_get_or_continue!(server_conn_sender
                        .send_async(ConnTask::StreamReq(conn_id))
                        .await
                        .context(TaskCenterClientSendStreamSnafu {
                            key: key.clone(),
                            conn_id
                        }));
                    // 2. Response subcribe ok
                    snafu_error_get_or_continue!(conn_sender
                        .send_async(ConnTask::SubcribeResp)
                        .await
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
        }
    }
}

async fn handle_listener(task_sender: ManagerTaskSender, listener: TcpListener) -> Result<()> {
    loop {
        let stream = match listener.accept().await {
            Ok((stream, _)) => stream,
            Err(e) => {
                tracing::error!("server accpet error,we will slepp 3s. detail:{}", e);
                sleep(Duration::from_secs(3)).await;
                continue;
            }
        };
        // set keepalive
        snafu_error_handle!(set_tcp_keep_alive(&stream).context(TaskCenterSetKeepAliveSnafu));
        task_sender
            .send_async(ManagerTask::Accept(stream))
            .await
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
        PbConnRequest::Register { key } => {
            handle_server_conn(key.into(), conn_id, manager_task_sender, conn).await?;
        }
        PbConnRequest::Subcribe { key } => {
            handle_client_conn(key.into(), conn_id, manager_task_sender, conn).await?;
        }
        PbConnRequest::Stream { key, dst_id } => {
            let key = ImutableKey::from(key);
            manager_task_sender
                .send_async(ManagerTask::Stream {
                    stream: conn,
                    server_id: conn_id,
                    client_id: dst_id.into(),
                })
                .await
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
    let mut reader = NormalMessageReader::new(conn);
    let msg = reader
        .read_msg()
        .await
        .context(TaskCenterReadInitRequestSnafu { conn_id })?;
    PbConnRequest::decode(msg).context(TaskCenterDecodeInitRequestSnafu { conn_id })
}
