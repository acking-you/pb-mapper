use std::fmt::Debug;
use std::sync::Arc;

use snafu::ResultExt;
use tokio::net::TcpStream;
use tracing::{info_span, instrument};

use super::error::{
    ConnectRemoteStreamSnafu, DecodeSubcribeRespSnafu, EncodeSubcribeReqSnafu, Result,
    SubcribeRespNotMatchSnafu, WriteSubcribeReqSnafu,
};
use crate::common::checksum::Credential;
use crate::common::config::control_io_timeout;
use crate::common::message::command::{MessageSerializer, PbConnRequest, PbConnResponse};
use crate::common::message::forward::StreamForward;
use crate::common::message::secure::ClientHeaderSession;
use crate::local::client::error::CreateHeaderToolSnafu;
use crate::snafu_error_handle;
use uni_stream::addr::{ToSocketAddrs, each_addr};
use uni_stream::stream::{NetworkStream, set_tcp_keep_alive, set_tcp_nodelay};

#[instrument(skip(local_stream))]
pub async fn handle_local_stream<
    LocalStream: NetworkStream + StreamForward,
    A: ToSocketAddrs + Debug + Send + 'static,
>(
    mut local_stream: LocalStream,
    key: Arc<str>,
    remote_addr: A,
    keep_alive: bool,
    namespace: Option<u64>,
    credential: Credential,
) -> Result<()> {
    let mut remote_stream = each_addr(remote_addr, TcpStream::connect)
        .await
        .context(ConnectRemoteStreamSnafu)?;

    if keep_alive {
        snafu_error_handle!(
            set_tcp_keep_alive(&remote_stream),
            "remote stream set keepalive"
        );
    }
    snafu_error_handle!(set_tcp_nodelay(&remote_stream), "remote stream set nodelay");

    // start subcribe
    let (codec_key, client_id, server_id) = {
        let timeout = control_io_timeout();
        // handle request
        let request = match namespace {
            Some(namespace) => PbConnRequest::SubcribeScoped {
                key: key.to_string(),
                namespace,
            },
            None => PbConnRequest::Subcribe {
                key: key.to_string(),
            },
        };
        let msg = request.encode().context(EncodeSubcribeReqSnafu)?;
        let session = ClientHeaderSession::new_v2(&credential)
            .context(CreateHeaderToolSnafu { action: "session" })?;
        let response = session
            .exchange(&mut remote_stream, &msg, timeout)
            .await
            .context(WriteSubcribeReqSnafu)?;
        let resp = PbConnResponse::decode(&response).context(DecodeSubcribeRespSnafu)?;
        match resp {
            PbConnResponse::Subcribe {
                codec_key,
                client_id,
                server_id,
            } => (codec_key, client_id, server_id),
            PbConnResponse::Error(error) => SubcribeRespNotMatchSnafu {
                resp: format!("{}: {}", error.code, error.message),
            }
            .fail()?,
            resp => SubcribeRespNotMatchSnafu {
                resp: format!("{resp:?}"),
            }
            .fail()?,
        }
    };
    let span = info_span!("forward", "client:{client_id} <-> server_id:{server_id}");
    let _enter = span.enter();
    // start forward
    let (client_reader, client_writer) = local_stream.split();
    let (server_reader, server_writer) = remote_stream.split();

    snafu_error_handle!(
        <LocalStream as StreamForward>::forward_local_to_remote(
            codec_key,
            *credential.key(),
            client_reader,
            client_writer,
            server_reader,
            server_writer,
        )
        .await
    );

    Ok(())
}
