use std::time::Duration;

use snafu::ResultExt;
use tokio::net::TcpStream;
use tracing::instrument;

use super::error::{
    ServerConnCreateHeaderToolSnafu, ServerConnDecodeStreamRequestSnafu,
    ServerConnEncodeRegisterRespSnafu, ServerConnRecvConnTaskSnafu,
    ServerConnRecvServerRegisteredRespSnafu, ServerConnRegisteredRespNotMatchSnafu,
    ServerConnSendRegisterSnafu, ServerConnWritePongRespSnafu, ServerConnWriteRegisteredOkSnafu,
    ServerConnWriteStreamRequestSnafu,
};
use super::{ConnTask, ImutableKey, ManagerTask, ManagerTaskSender, Result};
use crate::common::conn_id::RemoteConnId;
use crate::common::message::command::{
    LocalServer, MessageSerializer, PbConnResponse, PbServerRequest,
};
use crate::common::message::{
    get_header_msg_reader, get_header_msg_writer, MessageReader, MessageWriter,
};
use crate::pb_server::error::ServerConnSendDeregisterServerSnafu;
use crate::{snafu_error_get_or_continue, snafu_error_get_or_return_ok, snafu_error_handle};

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
            }));
        tracing::error!(
            "Server conn drop! key:{} conn_id:{}",
            self.key,
            self.conn_id
        );
    }
}

const DEFAULT_SERVER_CHAN_CAP: usize = 32 * 4;
const SERVER_TIMEOUT: Duration = Duration::from_secs(60);

/// Maintaining a connection to the server.
/// This connection is used to send channel request
#[instrument(skip(task_sender))]
pub async fn handle_server_conn(
    key: ImutableKey,
    need_codec: bool,
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
            need_codec,
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
    let (mut reader, mut writer) = conn.split();
    let mut msg_writer = get_header_msg_writer(&mut writer)
        .context(ServerConnCreateHeaderToolSnafu { tool: "writer" })?;
    let mut msg_reader = get_header_msg_reader(&mut reader)
        .context(ServerConnCreateHeaderToolSnafu { tool: "reader" })?;
    // response msg to local server to indicate that register handling has finished
    {
        let msg = PbConnResponse::Register(conn_id.into()).encode().context(
            ServerConnEncodeRegisterRespSnafu {
                key: key.clone(),
                conn_id,
            },
        )?;
        msg_writer
            .write_msg(&msg)
            .await
            .context(ServerConnWriteRegisteredOkSnafu {
                key: key.clone(),
                conn_id,
            })?;
    }
    loop {
        tokio::select! {
            // handle stream request
            ret = rx.recv_async() =>{
                let req = snafu_error_get_or_continue!(ret.context(ServerConnRecvConnTaskSnafu));
                snafu_error_get_or_continue!(
                    handle_stream_req(
                        req,
                        &mut msg_writer,
                        key.clone(),
                        conn_id
                    ).await
                );
            }
            // handle ping pong check
            ret = msg_reader.read_msg() =>{
                snafu_error_get_or_return_ok!(
                    handle_ping_pong_check(
                        snafu_error_get_or_return_ok!(ret), &mut msg_writer, key.clone(), conn_id
                    ).await
                );
            }
            // handle timeout
            _ = tokio::time::sleep(SERVER_TIMEOUT) =>{
                tracing::error!("Timeout trigger:{SERVER_TIMEOUT:?}");
                return Ok(());
            }
        }
    }
}

#[instrument(skip(msg, writer))]
async fn handle_ping_pong_check<T: MessageWriter>(
    msg: &[u8],
    writer: &mut T,
    key: ImutableKey,
    conn_id: RemoteConnId,
) -> Result<()> {
    // receive ping req
    let req = match PbServerRequest::decode(msg) {
        Ok(v) => v,
        Err(e) => {
            tracing::error!("We decode ping request error! detail:{e}");
            return Ok(());
        }
    };

    if !matches!(req, PbServerRequest::Ping) {
        tracing::error!("We expected `Ping`,but got `{req:?}`");
        return Ok(());
    }

    // response pong
    let resp = match LocalServer::Pong.encode() {
        Ok(v) => v,
        Err(e) => {
            tracing::error!("We encode pong response error! detail:{e}");
            return Ok(());
        }
    };

    writer
        .write_msg(&resp)
        .await
        .context(ServerConnWritePongRespSnafu { key, conn_id })
}

#[instrument(skip(req, writer))]
async fn handle_stream_req<T: MessageWriter>(
    req: ConnTask,
    writer: &mut T,
    key: ImutableKey,
    conn_id: RemoteConnId,
) -> Result<()> {
    // TODO: handle stop task
    // FIXME: Maybe it can be parallelized here?
    if let ConnTask::StreamReq(conn_id) = req {
        let msg = LocalServer::Stream {
            client_id: conn_id.into(),
        }
        .encode()
        .context(ServerConnDecodeStreamRequestSnafu {
            key: key.clone(),
            conn_id,
        })?;
        writer
            .write_msg(&msg)
            .await
            .context(ServerConnWriteStreamRequestSnafu {
                key: key.clone(),
                conn_id,
            })?
    } else {
        tracing::error!("We expected ConnTask::StreamReq,but got `{req:?}`");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::time::Instant;

    #[tokio::test]
    async fn test_sleep() {
        let expired_time = Instant::now();
        tokio::time::sleep(Duration::from_secs(1)).await;
        let mut cnt = 0;
        loop {
            tokio::select! {
                _ = tokio::time::sleep_until(expired_time)=>{
                    println!("end");
                    return;
                }
                _ = tokio::time::sleep(Duration::from_secs(3)) =>{
                    cnt += 1;
                    if cnt > 3 {
                        break;
                    }
                    println!("never print this: {cnt}");
                }
            }
        }
    }
}
