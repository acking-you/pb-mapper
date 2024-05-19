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
use crate::common::forward::{start_forward, NormalForwardReader, NormalForwardWriter};
use crate::common::message::{
    MessageReader, MessageSerializer, MessageWriter, NormalMessageReader, NormalMessageWriter,
    PbConnRequest, PbConnResponse,
};
use crate::common::stream::{set_tcp_keep_alive, StreamProvider, StreamSplit};
use crate::snafu_error_handle;
use crate::utils::addr::{each_addr, ToSocketAddrs};

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
    {
        let mut msg_writer = NormalMessageWriter::new(&mut remote_stream);
        msg_writer
            .write_msg(&msg)
            .await
            .context(WritePbConnStreamReqSnafu)?;
        let mut msg_reader = NormalMessageReader::new(&mut remote_stream);
        let msg = msg_reader
            .read_msg()
            .await
            .context(ReadPbConnStreamRespSnafu)?;
        let resp = PbConnResponse::decode(msg).context(DecodePbConnStreamRespSnafu)?;
        if !matches!(resp, PbConnResponse::Stream) {
            PbConnStreamRespNotMatchSnafu {
                resp: format!("{resp:?}"),
            }
            .fail()?
        }
    }

    // start forward network traffic
    let mut local_stream = LocalStream::from_addr(local_addr)
        .await
        .context(ConnectLocalStreamSnafu)?;

    let (mut client_reader, mut client_writer) = remote_stream.split();
    let (mut server_reader, mut server_writer) = local_stream.split();

    start_forward(
        NormalForwardReader::new(&mut client_reader),
        NormalForwardWriter::new(&mut client_writer),
        NormalForwardReader::new(&mut server_reader),
        NormalForwardWriter::new(&mut server_writer),
    )
    .await;

    Ok(())
}
