pub mod error;
mod stream;

use std::fmt::Debug;
use std::sync::Arc;

use snafu::ResultExt;
use tokio::net::{TcpStream, ToSocketAddrs};

use self::error::{AcceptLocalStreamSnafu, BindLocalListenerSnafu};
use self::stream::handle_local_stream;
use crate::common::config::StatusOp;
use crate::common::listener::{ListenerProvider, StreamAccept};
use crate::common::message::{
    MessageReader, MessageSerializer, MessageWriter, NormalMessageReader, NormalMessageWriter,
    PbConnRequest, PbConnResponse, PbConnStatusReq,
};
use crate::{snafu_error_get_or_return, snafu_error_handle};

pub async fn run_client_side_cli<
    LocalListener: ListenerProvider,
    A: ToSocketAddrs + Debug + Copy + Send + 'static,
>(
    local_addr: A,
    remote_addr: A,
    key: Arc<str>,
) {
    let listener = snafu_error_get_or_return!(LocalListener::bind(local_addr)
        .await
        .context(BindLocalListenerSnafu));
    loop {
        let (stream, _) =
            snafu_error_get_or_return!(listener.accept().await.context(AcceptLocalStreamSnafu));
        let key = key.clone();
        tokio::spawn(async move {
            snafu_error_handle!(handle_local_stream(stream, key, remote_addr).await);
        });
    }
}

pub async fn show_status<A: ToSocketAddrs + Debug + Copy + Send + 'static>(
    remote_addr: A,
    req: PbConnStatusReq,
) {
    let msg = snafu_error_get_or_return!(PbConnRequest::Status(req).encode());
    let mut remote_stream = snafu_error_get_or_return!(TcpStream::connect(remote_addr).await);

    // send status request
    {
        let mut msg_writer = NormalMessageWriter::new(&mut remote_stream);
        snafu_error_get_or_return!(msg_writer.write_msg(&msg).await);
    }

    // get status
    {
        let mut msg_reader = NormalMessageReader::new(&mut remote_stream);
        let msg = snafu_error_get_or_return!(msg_reader.read_msg().await);
        let resp = snafu_error_get_or_return!(PbConnResponse::decode(msg));
        println!(
            "{}",
            snafu_error_get_or_return!(serde_json::to_string_pretty(&resp))
        );
    }
}

#[inline]
pub async fn handle_status_cli<A: ToSocketAddrs + Debug + Copy + Send + 'static>(
    op: StatusOp,
    addr: A,
) {
    match op {
        StatusOp::RemoteId => show_status(addr, PbConnStatusReq::RemoteId).await,
        StatusOp::Keys => show_status(addr, PbConnStatusReq::Keys).await,
    }
}
