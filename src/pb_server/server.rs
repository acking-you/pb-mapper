use snafu::ResultExt;
use tokio::net::TcpStream;
use tracing::instrument;

use super::error::{
    ServerConnEncodeRegisterRespSnafu, ServerConnRecvConnTaskSnafu,
    ServerConnRecvServerRegisteredRespSnafu, ServerConnRegisteredRespNotMatchSnafu,
    ServerConnSendRegisterSnafu, ServerConnWriteRegisteredOkSnafu,
    ServerConnWriteStreamRequestSnafu,
};
use super::{ConnTask, ImutableKey, ManagerTask, ManagerTaskSender, Result};
use crate::common::conn_id::RemoteConnId;
use crate::common::message::{
    LocalServerRequest, MessageSerializer, MessageWriter, NormalMessageWriter, PbConnResponse,
};
use crate::pb_server::error::ServerConnSendDeregisterServerSnafu;
use crate::{snafu_error_get_or_continue, snafu_error_handle};

/// Ensure that server-side connections are properly deregistered before a normal connection is
/// disconnected or an exception occurs
struct ServerConnGuard<'a> {
    key: &'a ImutableKey,
    conn_id: RemoteConnId,
    sender: &'a ManagerTaskSender,
}

impl<'a> Drop for ServerConnGuard<'a> {
    fn drop(&mut self) {
        snafu_error_handle!(self
            .sender
            .send(ManagerTask::DeRegisterServerConn {
                key: self.key.clone(),
                conn_id: self.conn_id,
            })
            .context(ServerConnSendDeregisterServerSnafu {
                key: self.key.clone(),
                conn_id: self.conn_id
            }))
    }
}

const DEFAULT_SERVER_CHAN_CAP: usize = 32 * 4;

/// Maintaining a connection to the server.
/// This connection is used to send channel request
#[instrument(skip(task_sender))]
pub async fn handle_server_conn(
    key: ImutableKey,
    conn_id: RemoteConnId,
    task_sender: ManagerTaskSender,
    mut conn: TcpStream,
) -> Result<()> {
    let (tx, rx) = flume::bounded(DEFAULT_SERVER_CHAN_CAP);

    // register metadate
    task_sender
        .send_async(ManagerTask::Register {
            key: key.clone(),
            conn_id,
            conn_sender: tx,
        })
        .await
        .context(ServerConnSendRegisterSnafu {
            key: key.clone(),
            conn_id,
        })?;

    let response = rx
        .recv_async()
        .await
        .context(ServerConnRecvServerRegisteredRespSnafu {
            key: key.clone(),
            conn_id,
        })?;

    if !matches!(response, ConnTask::RegisterResp) {
        ServerConnRegisteredRespNotMatchSnafu {
            key: key.clone(),
            conn_id,
        }
        .fail()?
    }

    // metadata register has finished,next we will handle `ConnTask`
    let _guard = ServerConnGuard {
        key: &key,
        conn_id,
        sender: &task_sender,
    };

    let mut msg_writer = NormalMessageWriter::new(&mut conn);
    // response msg to local server to indicate that register handling has finished
    {
        let msg = PbConnResponse::Register
            .encode()
            .context(ServerConnEncodeRegisterRespSnafu {
                key: key.clone(),
                conn_id,
            })?;
        msg_writer
            .write_msg(&msg)
            .await
            .context(ServerConnWriteRegisteredOkSnafu {
                key: key.clone(),
                conn_id,
            })?;
    }
    loop {
        let request = snafu_error_get_or_continue!(rx.recv_async().await.context(
            ServerConnRecvConnTaskSnafu {
                key: key.clone(),
                conn_id
            }
        ));
        // TODO: handle stop task
        // FIXME: Maybe it can be parallelized here?
        if let ConnTask::StreamReq(conn_id) = request {
            let msg = snafu_error_get_or_continue!(LocalServerRequest::Stream {
                client_id: conn_id.into(),
            }
            .encode());
            msg_writer
                .write_msg(&msg)
                .await
                .context(ServerConnWriteStreamRequestSnafu {
                    key: key.clone(),
                    conn_id,
                })?
        }
    }
}
