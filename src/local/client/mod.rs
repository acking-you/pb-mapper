pub mod error;
mod status;
mod stream;

use std::fmt::Debug;
use std::sync::Arc;

use snafu::ResultExt;
use tokio::net::TcpStream;

use self::error::{AcceptLocalStreamSnafu, BindLocalListenerSnafu};
use self::status::get_status;
use self::stream::handle_local_stream;
use crate::common::config::StatusOp;
use crate::common::listener::{ListenerProvider, StreamAccept};
use crate::common::message::command::{PbConnStatusReq, PbConnStatusResp};
use crate::common::stream::got_one_socket_addr;
use crate::utils::addr::{each_addr, ToSocketAddrs};
use crate::{snafu_error_get_or_return, snafu_error_handle};

pub async fn run_client_side_cli<LocalListener: ListenerProvider, A: ToSocketAddrs>(
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

    let mut stream = snafu_error_get_or_return!(
        each_addr(remote_addr, TcpStream::connect).await,
        "get status stream"
    );

    let status_resp =
        snafu_error_get_or_return!(get_status(&mut stream, PbConnStatusReq::Keys).await);

    let keys = if let PbConnStatusResp::Keys(keys) = &status_resp {
        keys
    } else {
        tracing::error!("We expected status response,but got {status_resp:?}");
        return;
    };

    if !keys.iter().any(|k| k == key.as_ref()) {
        tracing::error!("Not valid key:{key},valid keys:{keys:?}");
        return;
    }

    drop(status_resp);

    tracing::info!("Subcribe server:{key} successful!");

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
    let mut stream = snafu_error_get_or_return!(
        each_addr(remote_addr, TcpStream::connect).await,
        "get status stream"
    );
    let status = snafu_error_get_or_return!(get_status(&mut stream, req).await);
    let status = snafu_error_get_or_return!(serde_json::to_string_pretty(&status));
    println!("Status:{status}");
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
