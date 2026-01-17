use std::fmt::Debug;
use std::sync::Arc;

use snafu::ResultExt;
use tokio::net::TcpStream;
use tracing::{info_span, instrument};

use super::error::{
    ConnectRemoteStreamSnafu, DecodeSubcribeRespSnafu, EncodeSubcribeReqSnafu,
    ReadSubcribeRespSnafu, Result, SubcribeRespNotMatchSnafu, WriteSubcribeReqSnafu,
};
use crate::common::config::IS_KEEPALIVE;
use crate::common::message::command::{MessageSerializer, PbConnRequest, PbConnResponse};
use crate::common::message::forward::StreamForward;
use crate::common::message::{
    get_header_msg_reader, get_header_msg_writer, MessageReader, MessageWriter,
};
use crate::local::client::error::CreateHeaderToolSnafu;
use crate::snafu_error_handle;
use uni_stream::addr::{each_addr, ToSocketAddrs};
use uni_stream::stream::{set_tcp_keep_alive, set_tcp_nodelay, NetworkStream};

#[instrument(skip(local_stream))]
pub async fn handle_local_stream<
    LocalStream: NetworkStream + StreamForward,
    A: ToSocketAddrs + Debug + Send + 'static,
>(
    mut local_stream: LocalStream,
    key: Arc<str>,
    remote_addr: A,
) -> Result<()> {
    let mut remote_stream = each_addr(remote_addr, TcpStream::connect)
        .await
        .context(ConnectRemoteStreamSnafu)?;

    if *IS_KEEPALIVE {
        snafu_error_handle!(
            set_tcp_keep_alive(&remote_stream),
            "remote stream set keepalive"
        );
    }
    snafu_error_handle!(set_tcp_nodelay(&remote_stream), "remote stream set nodelay");

    // start subcribe
    let (codec_key, client_id, server_id) = {
        // handle request
        let msg = PbConnRequest::Subcribe {
            key: key.to_string(),
        }
        .encode()
        .context(EncodeSubcribeReqSnafu)?;
        let mut msg_writer = get_header_msg_writer(&mut remote_stream)
            .context(CreateHeaderToolSnafu { action: "writer" })?;
        msg_writer
            .write_msg(&msg)
            .await
            .context(WriteSubcribeReqSnafu)?;
        // handle response
        let mut msg_reader = get_header_msg_reader(&mut remote_stream)
            .context(CreateHeaderToolSnafu { action: "reader" })?;
        let msg = msg_reader.read_msg().await.context(ReadSubcribeRespSnafu)?;
        let resp = PbConnResponse::decode(msg).context(DecodeSubcribeRespSnafu)?;
        match resp {
            PbConnResponse::Subcribe {
                codec_key,
                client_id,
                server_id,
            } => (codec_key, client_id, server_id),
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
            client_reader,
            client_writer,
            server_reader,
            server_writer,
        )
        .await
    );

    Ok(())
}
