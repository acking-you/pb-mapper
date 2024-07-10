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
use crate::common::message::command::{MessageSerializer, PbConnRequest, PbConnResponse};
use crate::common::message::forward::{
    start_forward, CodecForwardReader, CodecForwardWriter, NormalForwardReader, NormalForwardWriter,
};
use crate::common::message::{
    get_decodec, get_encodec, get_header_msg_reader, get_header_msg_writer, MessageReader,
    MessageWriter,
};
use crate::common::stream::{set_tcp_keep_alive, StreamProvider, StreamSplit};
use crate::local::server::error::CreateHeaderToolSnafu;
use crate::utils::addr::{each_addr, ToSocketAddrs};
use crate::{
    create_component, snafu_error_get_or_return_ok, snafu_error_handle,
    start_forward_with_codec_key,
};

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
) -> Result<()> {
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
    snafu_error_handle!(
        set_tcp_keep_alive(&remote_stream),
        "remote stream set keepalive"
    );

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

    let (mut client_reader, mut client_writer) = remote_stream.split();
    let (mut server_reader, mut server_writer) = local_stream.split();

    start_forward_with_codec_key!(
        codec_key,
        &mut client_reader,
        &mut client_writer,
        &mut server_reader,
        &mut server_writer,
        true,
        true,
        false,
        false
    );

    Ok(())
}
