use std::sync::Arc;

use snafu::ResultExt;
use tracing::instrument;

use super::error::{
    ConnectLocalStreamSnafu, ConnectRemoteStreamSnafu, ControlIoTimeoutSnafu,
    DecodePbConnStreamRespSnafu, EncodePbConnStreamReqSnafu, PbConnStreamRespNotMatchSnafu, Result,
    WritePbConnStreamReqSnafu,
};
use crate::endpoint::RelayEndpoint;
use crate::server::error::CreateHeaderToolSnafu;
use pb_mapper_core::checksum::Credential;
use pb_mapper_core::config::{ResolvedAddrs, control_io_timeout};
use pb_mapper_core::snafu_error_handle;
use pb_mapper_protocol::command::{MessageSerializer, PbConnRequest, PbConnResponse};
use pb_mapper_protocol::data::{DATA_PROTOCOL_V2, DataCodec};
use pb_mapper_protocol::forward::StreamForward;
use pb_mapper_protocol::secure::ClientHeaderSession;
use uni_stream::stream::{StreamProvider, StreamSplit};

/// Where one forwarded session dials, both ends resolved.
///
/// Each end is the full candidate list rather than one address, so a session
/// opened long after registration still has every record to try.
#[derive(Clone, Debug)]
pub struct StreamConnect {
    pub local_addr: ResolvedAddrs,
    pub remote_addr: ResolvedAddrs,
    pub keep_alive: bool,
    pub namespace: Option<u64>,
    pub credential: Credential,
    pub idle_timeout: Option<std::time::Duration>,
}

/// Forward a stream using the carrier and relay selected by its registration.
#[instrument(skip(connect, setup_permit, endpoint), fields(key = %key))]
pub(crate) async fn handle_stream_on_endpoint<LocalStream: StreamProvider>(
    key: Arc<str>,
    client_id: u32,
    server_generation: u64,
    connect: StreamConnect,
    setup_permit: tokio::sync::OwnedSemaphorePermit,
    endpoint: RelayEndpoint,
) -> Result<()>
where
    LocalStream::Item: StreamForward,
{
    let StreamConnect {
        local_addr,
        remote_addr,
        keep_alive,
        namespace,
        credential,
        idle_timeout,
    } = connect;

    let offered_v2 = <LocalStream::Item as StreamForward>::supports_data_v2();
    let request = match namespace {
        Some(namespace) => PbConnRequest::StreamScoped {
            data_protocol: offered_v2.then_some(DATA_PROTOCOL_V2),
            key: key.to_string(),
            namespace,
            dst_id: client_id,
            server_generation,
        },
        None => PbConnRequest::Stream {
            data_protocol: offered_v2.then_some(DATA_PROTOCOL_V2),
            key: key.to_string(),
            dst_id: client_id,
            server_generation,
        },
    };
    let msg = request.encode().context(EncodePbConnStreamReqSnafu)?;

    let timeout = control_io_timeout().min(std::time::Duration::from_secs(5));
    let deadline = tokio::time::Instant::now() + timeout;
    let mut remote_stream = match tokio::time::timeout_at(
        deadline,
        endpoint.connect_addresses(&remote_addr, credential),
    )
    .await
    {
        Ok(result) => result.context(ConnectRemoteStreamSnafu)?,
        Err(_) => ControlIoTimeoutSnafu {
            action: "connect remote stream",
            timeout,
        }
        .fail()?,
    };
    snafu_error_handle!(remote_stream.configure(keep_alive));

    // Use the credential captured at registration. A later UI/process key
    // change must not move these streams into another namespace.
    let codec_key = {
        let session = ClientHeaderSession::new_v2(&credential)
            .context(CreateHeaderToolSnafu { action: "session" })?;
        let response = session
            .exchange(
                &mut remote_stream,
                &msg,
                deadline.saturating_duration_since(tokio::time::Instant::now()),
            )
            .await
            .context(WritePbConnStreamReqSnafu)?;
        let resp = PbConnResponse::decode(&response).context(DecodePbConnStreamRespSnafu)?;
        match resp {
            PbConnResponse::Stream {
                codec_key,
                data_protocol,
            } => DataCodec::from_response(codec_key, data_protocol, offered_v2).context(
                CreateHeaderToolSnafu {
                    action: "data codec",
                },
            )?,
            PbConnResponse::Error(error) => PbConnStreamRespNotMatchSnafu {
                resp: format!("{}: {}", error.code, error.message),
            }
            .fail()?,
            _ => PbConnStreamRespNotMatchSnafu {
                resp: format!("{resp:?}"),
            }
            .fail()?,
        }
    };

    // start forward network traffic
    let mut local_stream = match tokio::time::timeout_at(
        deadline,
        LocalStream::from_addr(local_addr.as_slice()),
    )
    .await
    {
        Ok(result) => result.context(ConnectLocalStreamSnafu)?,
        Err(_) => {
            return ControlIoTimeoutSnafu {
                action: "connect local service",
                timeout,
            }
            .fail();
        }
    };
    drop(setup_permit);

    pb_mapper_protocol::with_relay_stream!(&mut remote_stream, remote_stream, {
        let (client_reader, client_writer) = remote_stream.split();
        let (server_reader, server_writer) = local_stream.split();

        let forward = <LocalStream::Item as StreamForward>::forward_local_to_remote_with_codec(
            codec_key,
            *credential.key(),
            server_reader,
            server_writer,
            client_reader,
            client_writer,
        );
        let result = match idle_timeout {
            Some(timeout) => {
                pb_mapper_protocol::forward::with_tunnel_idle_timeout(timeout, forward).await
            }
            None => forward.await,
        };
        snafu_error_handle!(result);
    });
    Ok(())
}
