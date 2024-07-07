use snafu::ResultExt;
use tokio::net::TcpStream;
use tokio::time::Instant;
use tracing::instrument;

use super::error::{
    ClientConnEncodeSubcribeRespSnafu, ClientConnRecvStreamSnafu, ClientConnRecvSubcribeRespSnafu,
    ClientConnSendDeregisterClientSnafu, ClientConnSendSubcribeSnafu,
    ClientConnStreamRespNotMatchSnafu, ClientConnSubcribeRespNotMatchSnafu,
    ClientConnWriteSubcribeRespSnafu,
};
use super::{ConnTask, ImutableKey, ManagerTask, ManagerTaskSender, Result};
use crate::common::conn_id::RemoteConnId;
use crate::common::forward::{start_forward, NormalForwardReader, NormalForwardWriter};
use crate::common::message::command::{
    MessageSerializer,  PbConnResponse,
};
use crate::common::message::{
    MessageWriter, NormalMessageWriter,
};
use crate::pb_server::error::{ClientConnEncodeStreamRespSnafu, ClientConnWriteStreamRespSnafu};
use crate::snafu_error_handle;

/// Ensure that client-side connections are properly deregistered before a normal connection is
/// disconnected or an exception occurs
struct ClientConnGuard<'a> {
    client_id: RemoteConnId,
    server_id: Option<RemoteConnId>,
    sender: &'a ManagerTaskSender,
    key: &'a ImutableKey,
}

impl<'a> Drop for ClientConnGuard<'a> {
    fn drop(&mut self) {
        snafu_error_handle!(self
            .sender
            .send(ManagerTask::DeRegisterClientConn {
                server_id: self.server_id,
                client_id: self.client_id
            })
            .context(ClientConnSendDeregisterClientSnafu {
                key: self.key.clone(),
                server_id: self.server_id,
                client_id: self.client_id,
            }));
    }
}

const DEFAULT_CLIENT_CHAN_CAP: usize = 32;

/// 1. Request server stream
/// 2. Forward the traffic between client stream and server stream
#[instrument(skip(task_sender, conn))]
pub async fn handle_client_conn(
    key: ImutableKey,
    conn_id: RemoteConnId,
    task_sender: ManagerTaskSender,
    mut conn: TcpStream,
) -> Result<()> {
    let prev_time = Instant::now();
    let (mut server_stream, server_id) = {
        match get_server_stream(&mut conn, key.clone(), conn_id, task_sender.clone()).await {
            Ok(res) => res,
            Err(e) => {
                let _guard = ClientConnGuard {
                    client_id: conn_id,
                    server_id: None,
                    sender: &task_sender,
                    key: &key,
                };
                return Err(e);
            }
        }
    };

    let duration = Instant::now() - prev_time;

    tracing::info!(
        "[time cost:{duration:?}] get server stream ok! we will start forward traffic. \
         key:{key}   server:{server_id}<->client:{conn_id}"
    );

    let _guard = ClientConnGuard {
        client_id: conn_id,
        server_id: Some(server_id),
        sender: &task_sender,
        key: &key,
    };

    let (mut client_reader, mut client_writer) = conn.split();
    let (mut server_reader, mut server_writer) = server_stream.split();

    // response message to server to indicate that stream handling has finished
    {
        let mut msg_writer = NormalMessageWriter::new(&mut server_writer);
        let msg = PbConnResponse::Stream
            .encode()
            .context(ClientConnEncodeStreamRespSnafu {
                key: key.clone(),
                conn_id,
            })?;
        msg_writer
            .write_msg(&msg)
            .await
            .context(ClientConnWriteStreamRespSnafu {
                key: key.clone(),
                conn_id,
            })?;
    }

    start_forward(
        NormalForwardReader::new(&mut client_reader),
        NormalForwardWriter::new(&mut client_writer),
        NormalForwardReader::new(&mut server_reader),
        NormalForwardWriter::new(&mut server_writer),
    )
    .await;

    Ok(())
}

async fn get_server_stream(
    conn: &mut TcpStream,
    key: ImutableKey,
    conn_id: RemoteConnId,
    task_sender: ManagerTaskSender,
) -> Result<(TcpStream, RemoteConnId)> {
    let (tx, rx) = flume::bounded(DEFAULT_CLIENT_CHAN_CAP);
    task_sender
        .send_async(ManagerTask::Subcribe {
            key: key.clone(),
            conn_id,
            conn_sender: tx,
        })
        .await
        .context(ClientConnSendSubcribeSnafu {
            key: key.clone(),
            conn_id,
        })?;

    let resp = rx
        .recv_async()
        .await
        .context(ClientConnRecvSubcribeRespSnafu {
            key: key.clone(),
            conn_id,
        })?;

    if !matches!(resp, ConnTask::SubcribeResp) {
        ClientConnSubcribeRespNotMatchSnafu {
            key: key.clone(),
            conn_id,
        }
        .fail()?
    }

    let resp = rx.recv_async().await.context(ClientConnRecvStreamSnafu {
        key: key.clone(),
        conn_id,
    })?;

    if let ConnTask::StreamResp { server_id, stream } = resp {
        // response message to client to indicate that subcribe handling has finished
        let mut msg_writer = NormalMessageWriter::new(conn);
        let msg = PbConnResponse::Subcribe {
            client_id: conn_id.into(),
            server_id: server_id.into(),
        }
        .encode()
        .context(ClientConnEncodeSubcribeRespSnafu {
            key: key.clone(),
            conn_id,
        })?;
        msg_writer
            .write_msg(&msg)
            .await
            .context(ClientConnWriteSubcribeRespSnafu {
                key: key.clone(),
                conn_id,
            })?;
        Ok((stream, server_id))
    } else {
        ClientConnStreamRespNotMatchSnafu {
            key: key.clone(),
            conn_id,
        }
        .fail()
    }
}
