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
use crate::common::checksum::{gen_random_key, AesKeyType};
use crate::common::conn_id::RemoteConnId;
use crate::common::message::command::{MessageSerializer, PbConnResponse};
use crate::common::message::forward::{
    start_datagram_forward, start_forward, CodecDatagramReader, CodecDatagramWriter,
    CodecForwardReader, CodecForwardWriter, NormalDatagramReader, NormalDatagramWriter,
    NormalForwardReader, NormalForwardWriter,
};
use crate::common::message::{get_decodec, get_encodec, get_header_msg_writer, MessageWriter};
use crate::pb_server::error::{
    ClientConnCreateHeaderToolSnafu, ClientConnEncodeStreamRespSnafu,
    ClientConnWriteStreamRespSnafu,
};
use crate::{
    create_component, snafu_error_get_or_return_ok, snafu_error_handle,
    start_forward_with_codec_key,
};

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
            .try_send(ManagerTask::DeRegisterClientConn {
                server_id: self.server_id,
                client_id: self.client_id
            })
            .map_err(|err| match err {
                kanal::SendTimeoutError::Closed(_) => kanal::SendTimeoutError::Closed(()),
                kanal::SendTimeoutError::Timeout(_) => kanal::SendTimeoutError::Timeout(()),
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
    let (mut server_stream, server_id, codec_key, is_datagram) = {
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
        let mut msg_writer = get_header_msg_writer(&mut server_writer)
            .context(ClientConnCreateHeaderToolSnafu { tool: "writer" })?;
        let msg = PbConnResponse::Stream { codec_key }.encode().context(
            ClientConnEncodeStreamRespSnafu {
                key: key.clone(),
                conn_id,
            },
        )?;
        msg_writer
            .write_msg(&msg)
            .await
            .context(ClientConnWriteStreamRespSnafu {
                key: key.clone(),
                conn_id,
            })?;
    }

    if is_datagram {
        match codec_key {
            Some(key) => {
                start_datagram_forward(
                    CodecDatagramReader::new(
                        &mut client_reader,
                        snafu_error_get_or_return_ok!(get_decodec(&key)),
                    ),
                    CodecDatagramWriter::new(
                        &mut client_writer,
                        snafu_error_get_or_return_ok!(get_encodec(&key)),
                    ),
                    CodecDatagramReader::new(
                        &mut server_reader,
                        snafu_error_get_or_return_ok!(get_decodec(&key)),
                    ),
                    CodecDatagramWriter::new(
                        &mut server_writer,
                        snafu_error_get_or_return_ok!(get_encodec(&key)),
                    ),
                )
                .await;
            }
            None => {
                start_datagram_forward(
                    NormalDatagramReader::new(&mut client_reader),
                    NormalDatagramWriter::new(&mut client_writer),
                    NormalDatagramReader::new(&mut server_reader),
                    NormalDatagramWriter::new(&mut server_writer),
                )
                .await;
            }
        }
    } else {
        start_forward_with_codec_key!(
            codec_key,
            &mut client_reader,
            &mut client_writer,
            &mut server_reader,
            &mut server_writer,
            true,
            true,
            true,
            true
        );
    }

    Ok(())
}

async fn get_server_stream(
    conn: &mut TcpStream,
    key: ImutableKey,
    conn_id: RemoteConnId,
    task_sender: ManagerTaskSender,
) -> Result<(TcpStream, RemoteConnId, Option<AesKeyType>, bool)> {
    let (tx, rx) = kanal::bounded_async(DEFAULT_CLIENT_CHAN_CAP);
    task_sender
        .send(ManagerTask::Subcribe {
            key: key.clone(),
            conn_id,
            conn_sender: tx,
        })
        .await
        .map_err(|_| kanal::SendError(()))
        .context(ClientConnSendSubcribeSnafu {
            key: key.clone(),
            conn_id,
        })?;

    let resp = rx.recv().await.context(ClientConnRecvSubcribeRespSnafu {
        key: key.clone(),
        conn_id,
    })?;

    // Note: A key will be generated for encrypting messages that are forwarded, and this key will
    // apply to all endpoints.
    let (codec_key, is_datagram) = match resp {
        ConnTask::SubcribeResp {
            need_codec,
            is_datagram,
        } => {
            let codec_key = if need_codec {
                Some(gen_random_key())
            } else {
                None
            };
            (codec_key, is_datagram)
        }
        _ => ClientConnSubcribeRespNotMatchSnafu {
            key: key.clone(),
            conn_id,
        }
        .fail()?,
    };

    let resp = rx.recv().await.context(ClientConnRecvStreamSnafu {
        key: key.clone(),
        conn_id,
    })?;

    if let ConnTask::StreamResp { server_id, stream } = resp {
        // response message to client to indicate that subcribe handling has finished
        let mut msg_writer = get_header_msg_writer(conn)
            .context(ClientConnCreateHeaderToolSnafu { tool: "writer" })?;
        let msg = PbConnResponse::Subcribe {
            codec_key,
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
        Ok((stream, server_id, codec_key, is_datagram))
    } else {
        ClientConnStreamRespNotMatchSnafu {
            key: key.clone(),
            conn_id,
        }
        .fail()
    }
}
