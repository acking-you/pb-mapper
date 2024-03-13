use std::fmt::Debug;
use std::sync::Arc;

use snafu::ResultExt;
use tokio::net::TcpStream;
use tracing::{info_span, instrument};

use super::error::{
    ConnectRemoteStreamSnafu, DecodeSubcribeRespSnafu, EncodeSubcribeReqSnafu,
    ReadSubcribeRespSnafu, Result, SubcribeRespNotMatchSnafu, WriteSubcribeReqSnafu,
};
use crate::common::forward::{start_forward, NormalForwardReader, NormalForwardWriter};
use crate::common::message::{
    MessageReader, MessageSerializer, MessageWriter, NormalMessageReader, NormalMessageWriter,
    PbConnRequest, PbConnResponse,
};
use crate::common::stream::{set_tcp_keep_alive, NetworkStream};
use crate::snafu_error_handle;
use crate::utils::addr::{each_addr, ToSocketAddrs};

#[instrument(skip(local_stream))]
pub async fn handle_local_stream<
    LocalStream: NetworkStream,
    A: ToSocketAddrs + Debug + Send + 'static,
>(
    mut local_stream: LocalStream,
    key: Arc<str>,
    remote_addr: A,
) -> Result<()> {
    let mut remote_stream = each_addr(remote_addr, TcpStream::connect)
        .await
        .context(ConnectRemoteStreamSnafu)?;
    snafu_error_handle!(
        set_tcp_keep_alive(&remote_stream),
        "remote stream set keepalive"
    );
    // start subcribe
    let (client_id, server_id) = {
        // handle request
        let msg = PbConnRequest::Subcribe {
            key: key.to_string(),
        }
        .encode()
        .context(EncodeSubcribeReqSnafu)?;
        let mut msg_writer = NormalMessageWriter::new(&mut remote_stream);
        msg_writer
            .write_msg(&msg)
            .await
            .context(WriteSubcribeReqSnafu)?;
        // handle response
        let mut msg_reader = NormalMessageReader::new(&mut remote_stream);
        let msg = msg_reader.read_msg().await.context(ReadSubcribeRespSnafu)?;
        let resp = PbConnResponse::decode(msg).context(DecodeSubcribeRespSnafu)?;
        match resp {
            PbConnResponse::Subcribe {
                client_id,
                server_id,
            } => (client_id, server_id),
            resp => SubcribeRespNotMatchSnafu {
                resp: format!("{resp:?}"),
            }
            .fail()?,
        }
    };
    let span = info_span!("forward", "client:{client_id} <-> server_id:{server_id}");
    let _enter = span.enter();
    // start forward
    let (mut client_reader, mut client_writer) = local_stream.split();
    let (mut server_reader, mut server_writer) = remote_stream.split();

    start_forward(
        NormalForwardReader::new(&mut client_reader),
        NormalForwardWriter::new(&mut client_writer),
        NormalForwardReader::new(&mut server_reader),
        NormalForwardWriter::new(&mut server_writer),
    )
    .await;
    Ok(())
}
