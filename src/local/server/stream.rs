use std::fmt::Debug;
use std::sync::Arc;

use snafu::ResultExt;
use tokio::net::TcpStream;
use tracing::info_span;

use super::error::{
    ConnectLocalStreamSnafu, ConnectRemoteStreamSnafu, DecodePbConnStreamRespSnafu,
    EncodePbConnStreamReqSnafu, PbConnStreamRespNotMatchSnafu, ReadPbConnStreamRespSnafu, Result,
    WritePbConnStreamReqSnafu,
};
use crate::common::config::IS_KEEPALIVE;
use crate::common::message::command::{MessageSerializer, PbConnRequest, PbConnResponse};
use crate::common::message::forward::StreamForward;
use crate::common::message::{
    get_header_msg_reader, get_header_msg_writer, MessageReader, MessageWriter,
};
use crate::local::server::error::CreateHeaderToolSnafu;
use crate::snafu_error_handle;
use uni_stream::addr::{each_addr, ToSocketAddrs};
use uni_stream::stream::{set_tcp_keep_alive, set_tcp_nodelay, StreamProvider, StreamSplit};

/// Handle a stream connection and establish a forward network traffic forwarding.
/// This function handles both local and remote streams, sets up message writers and readers,
/// and starts forwarding network traffic between the two endpoints.
pub async fn handle_stream<
    LocalStream: StreamProvider,
    A: ToSocketAddrs + Debug + Copy + Clone + Send,
>(
    local_addr: A,
    remote_addr: A,
    key: Arc<str>,
    client_id: u32,
) -> Result<()>
where
    LocalStream::Item: StreamForward,
{
    let key_ref = key.as_ref();
    let client_id_span = info_span!("client_id", key_ref, client_id);
    let _enter = client_id_span.enter();

    let msg = PbConnRequest::Stream {
        key: key.to_string(),
        dst_id: client_id,
    }
    .encode()
    .context(EncodePbConnStreamReqSnafu)?;

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

    // write stream request and read response
    let codec_key = {
        let mut msg_writer = get_header_msg_writer(&mut remote_stream)
            .context(CreateHeaderToolSnafu { action: "writer" })?;
        msg_writer
            .write_msg(&msg)
            .await
            .context(WritePbConnStreamReqSnafu)?;
        let mut msg_reader = get_header_msg_reader(&mut remote_stream)
            .context(CreateHeaderToolSnafu { action: "reader" })?;
        let msg = msg_reader
            .read_msg()
            .await
            .context(ReadPbConnStreamRespSnafu)?;
        let resp = PbConnResponse::decode(msg).context(DecodePbConnStreamRespSnafu)?;
        match resp {
            PbConnResponse::Stream { codec_key } => codec_key,
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
            server_reader,
            server_writer,
            client_reader,
            client_writer,
        )
        .await
    );

    Ok(())
}
