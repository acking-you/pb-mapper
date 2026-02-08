pub mod error;
mod stream;

use std::fmt::Debug;
use std::sync::Arc;
use std::time::Duration;

use snafu::ResultExt;
use tokio::net::TcpStream;
use tokio::time::Instant;
use tracing::instrument;

use self::error::{
    DecodeRegisterRespSnafu, DecodeStreamReqSnafu, EncodePingMsgSnafu, EncodeRegisterReqSnafu,
    ReadRegisterRespSnafu, ReadStreamReqSnafu, RegisterRespNotMatchSnafu, SendRegisterReqSnafu,
    WritePingMsgSnafu,
};
use self::stream::handle_stream;
use crate::common::config::IS_KEEPALIVE;
use crate::common::message::command::{
    LocalServer, MessageSerializer, PbConnRequest, PbConnResponse, PbServerRequest,
};
use crate::common::message::forward::StreamForward;
use crate::common::message::{
    get_header_msg_reader, get_header_msg_writer, MessageReader, MessageWriter,
};
use crate::utils::timeout::TimeoutCount;
use crate::{
    snafu_error_get_or_continue, snafu_error_get_or_return, snafu_error_get_or_return_ok,
    snafu_error_handle,
};
use uni_stream::addr::{each_addr, ToSocketAddrs};
use uni_stream::stream::{
    got_one_socket_addr, set_tcp_keep_alive, set_tcp_nodelay, StreamProvider,
};

const LOCAL_SERVER_TIMEOUT: Duration = Duration::from_secs(64);

const PING_INTERVAL: Duration = Duration::from_secs(5 * 60); // 5 minutes

const GLOBAL_RETRY_TIMES: u32 = 16;

const LOCAL_RETRY_TIMES: u32 = 8;

fn get_ping_message() -> error::Result<Vec<u8>> {
    PbServerRequest::Ping.encode().context(EncodePingMsgSnafu)
}

#[derive(Debug)]
enum Status {
    Timeout,
    ReadMsg,
    SendPing,
    ConnectRemote,
}

// Callback for notifying status changes to external systems
pub type StatusCallback = Box<dyn Fn(&str) + Send + Sync>;

#[derive(Debug)]
pub enum ServiceStatus {
    Retrying,
    Connected,
    Failed,
}

pub async fn run_server_side_cli<LocalStream: StreamProvider, A: ToSocketAddrs + Debug + Copy>(
    local_addr: A,
    remote_addr: A,
    key: Arc<str>,
    need_codec: bool,
    is_datagram: bool,
) where
    LocalStream::Item: StreamForward,
{
    run_server_side_cli_with_callback::<LocalStream, A>(
        local_addr,
        remote_addr,
        key,
        need_codec,
        is_datagram,
        None,
    )
    .await
}

pub async fn run_server_side_cli_with_callback<
    LocalStream: StreamProvider,
    A: ToSocketAddrs + Debug + Copy,
>(
    local_addr: A,
    remote_addr: A,
    key: Arc<str>,
    need_codec: bool,
    is_datagram: bool,
    status_callback: Option<StatusCallback>,
) where
    LocalStream::Item: StreamForward,
{
    let mut timeout_count = TimeoutCount::new(GLOBAL_RETRY_TIMES);
    let mut retry_interval = timeout_count.get_interval_by_count();
    while timeout_count.validate() {
        let status = if let Err(status) = run_server_side_cli_inner::<LocalStream, _>(
            &mut timeout_count,
            local_addr,
            remote_addr,
            key.clone(),
            need_codec,
            is_datagram,
            status_callback.as_ref(),
        )
        .await
        {
            status
        } else {
            return;
        };
        match status {
            Status::Timeout | Status::ReadMsg | Status::SendPing | Status::ConnectRemote => {
                tracing::info!(
                    "We will try to re-connect the pb-server:`{:?} <-`{}`-> {:?}` after \
                     {retry_interval}s, global-retry-count:{}",
                    local_addr,
                    key,
                    remote_addr,
                    timeout_count.count()
                );

                // Notify external systems
                if let Some(ref callback) = status_callback {
                    let status = format!("{status:?}");
                    callback(&status);
                }

                tokio::time::sleep(Duration::from_secs(retry_interval)).await;
                retry_interval = timeout_count.get_interval_by_count();
                // Notify external systems that we're in retry mode
                if let Some(ref callback) = status_callback {
                    callback("retrying");
                }
            }
        }
    }
}

#[instrument(skip(status_callback))]
async fn run_server_side_cli_inner<LocalStream: StreamProvider, A: ToSocketAddrs + Debug + Copy>(
    global_timeout_cnt: &mut TimeoutCount,
    local_addr: A,
    remote_addr: A,
    key: Arc<str>,
    need_codec: bool,
    is_datagram: bool,
    status_callback: Option<&StatusCallback>,
) -> std::result::Result<(), Status>
where
    LocalStream::Item: StreamForward,
{
    let local_addr = match got_one_socket_addr(local_addr).await {
        Ok(addr) => addr,
        Err(e) => {
            tracing::error!("parse local addr failed: {e}");
            return Err(Status::ConnectRemote);
        }
    };
    let remote_addr = match got_one_socket_addr(remote_addr).await {
        Ok(addr) => addr,
        Err(e) => {
            tracing::error!("parse remote addr failed: {e}");
            return Err(Status::ConnectRemote);
        }
    };

    let mut manager_stream = snafu_error_get_or_return!(
        each_addr(remote_addr, TcpStream::connect).await,
        "[connect remote stream]",
        Err(Status::ConnectRemote)
    );

    if *IS_KEEPALIVE {
        snafu_error_handle!(
            set_tcp_keep_alive(&manager_stream),
            "manager stream set tcp keep alive"
        );
    }
    snafu_error_handle!(
        set_tcp_nodelay(&manager_stream),
        "manager stream set tcp nodelay"
    );

    // start register server with key
    {
        let msg = snafu_error_get_or_return_ok!(PbConnRequest::Register {
            key: key.to_string(),
            need_codec,
            is_datagram,
        }
        .encode()
        .context(EncodeRegisterReqSnafu));
        let mut msg_writer = match get_header_msg_writer(&mut manager_stream) {
            Ok(writer) => writer,
            Err(e) => {
                tracing::error!("create manager header writer failed: {e}");
                return Err(Status::ConnectRemote);
            }
        };
        snafu_error_get_or_return_ok!(msg_writer
            .write_msg(&msg)
            .await
            .context(SendRegisterReqSnafu));
    }
    let (mut reader, mut writer) = manager_stream.split();
    let mut msg_reader = match get_header_msg_reader(&mut reader) {
        Ok(reader) => reader,
        Err(e) => {
            tracing::error!("create manager header reader failed: {e}");
            return Err(Status::ReadMsg);
        }
    };
    let mut msg_writer = match get_header_msg_writer(&mut writer) {
        Ok(writer) => writer,
        Err(e) => {
            tracing::error!("create manager header writer failed: {e}");
            return Err(Status::SendPing);
        }
    };
    // read register resp to indicate that register has finished
    let (key, conn_id) = {
        let msg = snafu_error_get_or_return_ok!(msg_reader
            .read_msg()
            .await
            .context(ReadRegisterRespSnafu));
        let resp = snafu_error_get_or_return_ok!(
            PbConnResponse::decode(msg).context(DecodeRegisterRespSnafu)
        );
        let PbConnResponse::Register(conn_id) = resp else {
            snafu_error_get_or_return_ok!(RegisterRespNotMatchSnafu {}.fail())
        };
        tracing::info!("Server Register Ok: key:{key}, conn_id:{conn_id}");

        // Notify external systems that connection is established
        if let Some(ref callback) = status_callback {
            callback("connected");
        }
        (key, conn_id)
    };

    // start listen stream request
    let mut timeout = Instant::now() + LOCAL_SERVER_TIMEOUT;
    let mut timeout_count = TimeoutCount::new(LOCAL_RETRY_TIMES);
    // regiseter ok,and reset global timeout count
    global_timeout_cnt.reset();
    loop {
        tokio::select! {
            ret = msg_reader.read_msg() =>{
                let msg = snafu_error_get_or_return!(ret.context(ReadStreamReqSnafu),"[read msg]",Err(Status::ReadMsg));
                // timeout will reset by this function
                snafu_error_get_or_continue!(
                    handle_request::<LocalStream,_>(msg,local_addr,remote_addr,key.clone(),conn_id,
                    TimeoutContext {
                        timeout_count: &mut timeout_count,
                         timeout: &mut timeout
                    }).await
                );
            }
            // handle ping interval
            _ = tokio::time::sleep(PING_INTERVAL) =>{
                snafu_error_get_or_return!(
                    handle_ping_interval(&mut msg_writer,key.clone(),conn_id).await,
                    "[send ping]",
                    Err(Status::SendPing)
                );
                tracing::info!("ping trigger:{PING_INTERVAL:?}");
            }
            // handle timeout
            _ = tokio::time::sleep_until(timeout) =>{
                if timeout_count.validate(){
                    tracing::info!("[timeout retry] local retry count:{}",timeout_count.count());
                    timeout = Instant::now() + LOCAL_SERVER_TIMEOUT;
                }else{
                    tracing::warn!("Timeout traggier! `{timeout:?}`");
                    return Err(Status::Timeout);
                }

            }
        }
    }
}

#[instrument(skip(writer))]
async fn handle_ping_interval<T: MessageWriter>(
    writer: &mut T,
    key: Arc<str>,
    conn_id: u32,
) -> error::Result<()> {
    let ping_msg = get_ping_message()?;
    writer.write_msg(&ping_msg).await.context(WritePingMsgSnafu)
}

struct TimeoutContext<'a, 'b> {
    timeout_count: &'a mut TimeoutCount,
    timeout: &'b mut Instant,
}

#[instrument(skip(msg, timeout_ctx))]
async fn handle_request<
    LocalStream: StreamProvider,
    A: ToSocketAddrs + Debug + Copy + Clone + Send + 'static,
>(
    msg: &[u8],
    local_addr: A,
    remote_addr: A,
    key: Arc<str>,
    conn_id: u32,
    timeout_ctx: TimeoutContext<'_, '_>,
) -> error::Result<()>
where
    LocalStream::Item: StreamForward,
{
    let req = LocalServer::decode(msg).context(DecodeStreamReqSnafu)?;

    match req {
        LocalServer::Stream { client_id } => {
            let key = key.clone();
            tokio::spawn(async move {
                snafu_error_handle!(
                    handle_stream::<LocalStream, _>(local_addr, remote_addr, key, client_id).await
                )
            });
        }
        // got pong response
        LocalServer::Pong => {
            tracing::info!("got pong message! we will reset timeout");
        }
    }
    timeout_ctx.timeout_count.reset();
    *timeout_ctx.timeout = Instant::now() + LOCAL_SERVER_TIMEOUT;
    Ok(())
}
