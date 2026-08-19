use std::fmt::Debug;
use std::sync::Arc;

use snafu::ResultExt;
use tokio::net::TcpStream;
use tracing::info_span;

use super::error::{
    ConnectLocalStreamSnafu, ConnectRemoteStreamSnafu, ControlIoTimeoutSnafu,
    DecodePbConnStreamRespSnafu, EncodePbConnStreamReqSnafu, PbConnStreamRespNotMatchSnafu, Result,
    WritePbConnStreamReqSnafu,
};
use crate::common::checksum::Credential;
use crate::common::config::control_io_timeout;
use crate::common::message::command::{MessageSerializer, PbConnRequest, PbConnResponse};
use crate::common::message::forward::StreamForward;
use crate::common::message::secure::ClientHeaderSession;
use crate::local::server::error::CreateHeaderToolSnafu;
use crate::snafu_error_handle;
use uni_stream::addr::{each_addr, ToSocketAddrs};
use uni_stream::stream::{set_tcp_keep_alive, set_tcp_nodelay, StreamProvider, StreamSplit};

#[derive(Clone, Copy, Debug)]
pub struct StreamConnect<A> {
    pub local_addr: A,
    pub remote_addr: A,
    pub keep_alive: bool,
    pub namespace: Option<u64>,
    pub credential: Credential,
}

/// Handle a stream connection and establish a forward network traffic forwarding.
/// This function handles both local and remote streams, sets up message writers and readers,
/// and starts forwarding network traffic between the two endpoints.
pub async fn handle_stream<
    LocalStream: StreamProvider,
    A: ToSocketAddrs + Debug + Copy + Clone + Send,
>(
    key: Arc<str>,
    client_id: u32,
    server_generation: u64,
    connect: StreamConnect<A>,
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
    let mut remote_stream =
        match tokio::time::timeout(timeout, each_addr(remote_addr, TcpStream::connect)).await {
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
    let mut local_stream = LocalStream::from_addr(local_addr)
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
