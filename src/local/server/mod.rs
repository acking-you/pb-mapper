pub mod error;
mod stream;

use std::fmt::Debug;
use std::sync::Arc;

use snafu::ResultExt;
use tokio::net::TcpStream;
use tracing::instrument;

use self::error::{
    DecodeRegisterRespSnafu, DecodeStreamReqSnafu, EncodeRegisterReqSnafu, ReadRegisterRespSnafu,
    ReadStreamReqSnafu, RegisterRespNotMatchSnafu, SendRegisterReqSnafu,
};
use self::stream::handle_stream;
use crate::common::message::{
    LocalServerRequest, MessageReader, MessageSerializer, MessageWriter, NormalMessageReader,
    NormalMessageWriter, PbConnRequest, PbConnResponse,
};
use crate::common::stream::{got_one_socket_addr, set_tcp_keep_alive, StreamProvider};
use crate::utils::addr::{each_addr, ToSocketAddrs};
use crate::{snafu_error_get_or_continue, snafu_error_get_or_return, snafu_error_handle};

#[instrument]
pub async fn run_server_side_cli<LocalStream: StreamProvider, A: ToSocketAddrs + Debug>(
    local_addr: A,
    remote_addr: A,
    key: Arc<str>,
) {
    let local_addr = got_one_socket_addr(local_addr)
        .await
        .expect("at least one socket addr be parsed from `local_addr`");
    let remote_addr = got_one_socket_addr(remote_addr)
        .await
        .expect("at least one socket addr be parsed from `remote_addr`");

    let mut manager_stream = each_addr(remote_addr, TcpStream::connect)
        .await
        .expect("connect remote pb server never fails");

    snafu_error_handle!(
        set_tcp_keep_alive(&manager_stream),
        "manager stream set tcp keep alive"
    );

    // start register server with key
    {
        let msg = snafu_error_get_or_return!(PbConnRequest::Register {
            key: key.to_string(),
        }
        .encode()
        .context(EncodeRegisterReqSnafu));
        let mut msg_writer = NormalMessageWriter::new(&mut manager_stream);
        snafu_error_get_or_return!(msg_writer
            .write_msg(&msg)
            .await
            .context(SendRegisterReqSnafu));
    }

    let mut msg_reader = NormalMessageReader::new(&mut manager_stream);
    // read register resp to indicate that register has finished
    {
        let msg =
            snafu_error_get_or_return!(msg_reader.read_msg().await.context(ReadRegisterRespSnafu));
        let resp = snafu_error_get_or_return!(
            PbConnResponse::decode(msg).context(DecodeRegisterRespSnafu)
        );
        let PbConnResponse::Register(conn_id) = resp else {
            snafu_error_get_or_return!(RegisterRespNotMatchSnafu {}.fail())
        };
        tracing::info!("Server Register Ok: key:{key}, conn_id:{conn_id}");
    }

    // start listen stream request
    loop {
        let msg =
            snafu_error_get_or_return!(msg_reader.read_msg().await.context(ReadStreamReqSnafu));
        let req = snafu_error_get_or_continue!(
            LocalServerRequest::decode(msg).context(DecodeStreamReqSnafu)
        );

        match req {
            LocalServerRequest::Stream { client_id } => {
                let key = key.clone();
                tokio::spawn(async move {
                    snafu_error_handle!(
                        handle_stream::<LocalStream, _>(local_addr, remote_addr, key, client_id)
                            .await
                    )
                });
            }
        }
    }
}
