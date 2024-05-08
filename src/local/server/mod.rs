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
    DecodeRegisterRespSnafu, DecodeStreamReqSnafu, EncodeRegisterReqSnafu, ReadRegisterRespSnafu,
    ReadStreamReqSnafu, RegisterRespNotMatchSnafu, SendRegisterReqSnafu, WritePingMsgSnafu,
};
use self::stream::handle_stream;
use crate::common::message::{
    LocalServer, MessageReader, MessageSerializer, MessageWriter, NormalMessageReader,
    NormalMessageWriter, PbConnRequest, PbConnResponse, PbServerRequest,
};
use crate::common::stream::{got_one_socket_addr, set_tcp_keep_alive, StreamProvider};
use crate::utils::addr::{each_addr, ToSocketAddrs};
use crate::utils::timeout::TimeoutCount;
use crate::{
    snafu_error_get_or_continue, snafu_error_get_or_return, snafu_error_get_or_return_ok,
    snafu_error_handle,
};

/// Each time a request is received from the public server, the connection time out will be increase
/// `LOCAL_SERVER_TIMEOUT`
const LOCAL_SERVER_TIMEOUT: Duration = Duration::from_secs(30);

const PING_INTERVAL: Duration = Duration::from_secs(10);

const RETRY_INTERVAL: Duration = Duration::from_secs(5);

const RETRY_TIMES: usize = 5;

enum Status {
    Timeout,
    ReadMsg,
    SendPing,
    ConnectRemote,
}

pub async fn run_server_side_cli<LocalStream: StreamProvider, A: ToSocketAddrs + Debug + Copy>(
    local_addr: A,
    remote_addr: A,
    key: Arc<str>,
) {
    let mut timeout_count = TimeoutCount::new(RETRY_TIMES);
    while timeout_count.validate() {
        let status = if let Err(status) = run_server_side_cli_inner::<LocalStream, _>(
            &mut timeout_count,
            local_addr,
            remote_addr,
            key.clone(),
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
                     {RETRY_INTERVAL:?}, global-retry-Count:{}",
                    local_addr,
                    key,
                    remote_addr,
                    timeout_count.count()
                );
                tokio::time::sleep(RETRY_INTERVAL).await;
            }
        }
    }
}

#[instrument]
async fn run_server_side_cli_inner<LocalStream: StreamProvider, A: ToSocketAddrs + Debug + Copy>(
    global_timeout_cnt: &mut TimeoutCount,
    local_addr: A,
    remote_addr: A,
    key: Arc<str>,
) -> std::result::Result<(), Status> {
    let local_addr = got_one_socket_addr(local_addr)
        .await
        .expect("at least one socket addr be parsed from `local_addr`");
    let remote_addr = got_one_socket_addr(remote_addr)
        .await
        .expect("at least one socket addr be parsed from `remote_addr`");

    let mut manager_stream = snafu_error_get_or_return!(
        each_addr(remote_addr, TcpStream::connect).await,
        "[connect remote stream]",
        Err(Status::ConnectRemote)
    );

    snafu_error_handle!(
        set_tcp_keep_alive(&manager_stream),
        "manager stream set tcp keep alive"
    );

    // start register server with key
    {
        let msg = snafu_error_get_or_return_ok!(PbConnRequest::Register {
            key: key.to_string(),
        }
        .encode()
        .context(EncodeRegisterReqSnafu));
        let mut msg_writer = NormalMessageWriter::new(&mut manager_stream);
        snafu_error_get_or_return_ok!(msg_writer
            .write_msg(&msg)
            .await
            .context(SendRegisterReqSnafu));
    }
    let (mut reader, mut writer) = manager_stream.split();
    let mut msg_reader = NormalMessageReader::new(&mut reader);
    let mut msg_writer = NormalMessageWriter::new(&mut writer);
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
        (key, conn_id)
    };

    // start listen stream request
    let mut timeout = Instant::now() + LOCAL_SERVER_TIMEOUT;
    let mut timeout_count = TimeoutCount::new(RETRY_TIMES);
    let ping_msg = snafu_error_get_or_return_ok!(PbServerRequest::Ping.encode());
    // regiseter ok,and reset global timeout count
    global_timeout_cnt.reset(RETRY_TIMES);
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
                    handle_ping_interval(&ping_msg,&mut msg_writer,key.clone(),conn_id).await,
                    "[read msg]",
                    Err(Status::SendPing)
                );
                tracing::info!("ping trigger:{PING_INTERVAL:?}");
            }
            // handle timeout
            _ = tokio::time::sleep_until(timeout) =>{
                if timeout_count.validate(){
                    tracing::info!("[timeout retry] local retry count:{}",timeout_count.count());
                }else{
                    tracing::warn!("Timeout traggier! `{timeout:?}`");
                    return Err(Status::Timeout);
                }

            }
        }
    }
}

#[instrument(skip(ping_msg, writer))]
async fn handle_ping_interval<T: MessageWriter>(
    ping_msg: &[u8],
    writer: &mut T,
    key: Arc<str>,
    conn_id: u32,
) -> error::Result<()> {
    writer.write_msg(ping_msg).await.context(WritePingMsgSnafu)
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
) -> error::Result<()> {
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
    timeout_ctx.timeout_count.reset(RETRY_TIMES);
    *timeout_ctx.timeout = Instant::now() + LOCAL_SERVER_TIMEOUT;
    Ok(())
}
