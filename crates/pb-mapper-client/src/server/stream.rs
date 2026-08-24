use std::sync::Arc;

use snafu::ResultExt;
use tokio::net::TcpStream;
use tracing::info_span;

use super::error::{
    ConnectLocalStreamSnafu, ConnectRemoteStreamSnafu, ControlIoTimeoutSnafu,
    DecodePbConnStreamRespSnafu, EncodePbConnStreamReqSnafu, PbConnStreamRespNotMatchSnafu, Result,
    WritePbConnStreamReqSnafu,
};
use crate::server::error::CreateHeaderToolSnafu;
use pb_mapper_core::checksum::Credential;
use pb_mapper_core::config::{ResolvedAddrs, control_io_timeout};
use pb_mapper_core::snafu_error_handle;
use pb_mapper_protocol::command::{MessageSerializer, PbConnRequest, PbConnResponse};
use pb_mapper_protocol::forward::StreamForward;
use pb_mapper_protocol::secure::ClientHeaderSession;
use uni_stream::addr::each_addr;
use uni_stream::stream::{StreamProvider, StreamSplit, set_tcp_keep_alive, set_tcp_nodelay};

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
}

/// Handle a stream connection and establish a forward network traffic forwarding.
/// This function handles both local and remote streams, sets up message writers and readers,
/// and starts forwarding network traffic between the two endpoints.
pub async fn handle_stream<LocalStream: StreamProvider>(
    key: Arc<str>,
    client_id: u32,
    server_generation: u64,
    connect: StreamConnect,
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
    } = connect;
    let key_ref = key.as_ref();
    let client_id_span = info_span!("client_id", key_ref, client_id);
    let _enter = client_id_span.enter();

    let request = match namespace {
        Some(namespace) => PbConnRequest::StreamScoped {
            key: key.to_string(),
            namespace,
            dst_id: client_id,
            server_generation,
        },
        None => PbConnRequest::Stream {
            key: key.to_string(),
            dst_id: client_id,
            server_generation,
        },
    };
    let msg = request.encode().context(EncodePbConnStreamReqSnafu)?;

    let timeout = control_io_timeout();
    let mut remote_stream = match tokio::time::timeout(
        timeout,
        each_addr(remote_addr.as_slice(), TcpStream::connect),
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
    if keep_alive {
        snafu_error_handle!(
            set_tcp_keep_alive(&remote_stream),
            "remote stream set keepalive"
        );
    }
    snafu_error_handle!(set_tcp_nodelay(&remote_stream), "remote stream set nodelay");

    // Use the credential captured at registration. A later UI/process key
    // change must not move these streams into another namespace.
    let codec_key = {
        let session = ClientHeaderSession::new_v2(&credential)
            .context(CreateHeaderToolSnafu { action: "session" })?;
        let response = session
            .exchange(&mut remote_stream, &msg, timeout)
            .await
            .context(WritePbConnStreamReqSnafu)?;
        let resp = PbConnResponse::decode(&response).context(DecodePbConnStreamRespSnafu)?;
        match resp {
            PbConnResponse::Stream { codec_key } => codec_key,
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
    let mut local_stream = LocalStream::from_addr(local_addr.as_slice())
        .await
        .context(ConnectLocalStreamSnafu)?;

    let (client_reader, client_writer) = remote_stream.split();
    let (server_reader, server_writer) = local_stream.split();

    snafu_error_handle!(
        <LocalStream::Item as StreamForward>::forward_local_to_remote(
            codec_key,
            *credential.key(),
            server_reader,
            server_writer,
            client_reader,
            client_writer,
        )
        .await
    );

    Ok(())
}
