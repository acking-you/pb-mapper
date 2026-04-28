pub mod error;
mod stream;

use std::fmt::Debug;
use std::sync::Arc;
use std::time::Duration;

use snafu::ResultExt;
use tokio::net::TcpStream;
use tracing::instrument;

use self::error::{
    ControlIoTimeoutSnafu, DecodeRegisterRespSnafu, DecodeStreamReqSnafu, EncodePingMsgSnafu,
    EncodeRegisterReqSnafu, EncodeStreamAckMsgSnafu, ReadRegisterRespSnafu, ReadStreamReqSnafu,
    RegisterRespNotMatchSnafu, SendRegisterReqSnafu, WritePingMsgSnafu, WriteStreamAckMsgSnafu,
};
use self::stream::handle_stream;
use crate::common::config::{control_conn_pool_size, control_io_timeout, IS_KEEPALIVE};
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

const PING_INTERVAL: Duration = Duration::from_secs(5 * 60); // 5 minutes

const GLOBAL_RETRY_TIMES: u32 = 16;

fn get_ping_message() -> error::Result<Vec<u8>> {
    PbServerRequest::Ping.encode().context(EncodePingMsgSnafu)
}

#[derive(Debug)]
enum Status {
    ReadMsg,
    SendPing,
    ConnectRemote,
}

enum LocalControlWrite {
    Ping,
    StreamAck {
        client_id: u32,
        server_generation: u64,
    },
}

// Callback for notifying status changes to external systems
pub type StatusCallback = Box<dyn Fn(&str) + Send + Sync>;

#[derive(Debug)]
pub enum ServiceStatus {
    Retrying,
    Connected,
    Failed,
}

#[derive(Clone, Debug)]
struct ServerCliRunConfig<A> {
    local_addr: A,
    remote_addr: A,
    key: Arc<str>,
    need_codec: bool,
    is_datagram: bool,
    worker_index: usize,
}

pub async fn run_server_side_cli<LocalStream, A>(
    local_addr: A,
    remote_addr: A,
    key: Arc<str>,
    need_codec: bool,
    is_datagram: bool,
) where
    LocalStream: StreamProvider + Send + 'static,
    LocalStream::Item: StreamForward,
    A: ToSocketAddrs + Debug + Copy,
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

pub async fn run_server_side_cli_with_callback<LocalStream, A>(
    local_addr: A,
    remote_addr: A,
    key: Arc<str>,
    need_codec: bool,
    is_datagram: bool,
    status_callback: Option<StatusCallback>,
) where
    LocalStream: StreamProvider + Send + 'static,
    LocalStream::Item: StreamForward,
    A: ToSocketAddrs + Debug + Copy,
{
    let local_addr = match got_one_socket_addr(local_addr).await {
        Ok(addr) => addr,
        Err(e) => {
            tracing::error!("parse local addr failed: {e}");
            return;
        }
    };
    let remote_addr = match got_one_socket_addr(remote_addr).await {
        Ok(addr) => addr,
        Err(e) => {
            tracing::error!("parse remote addr failed: {e}");
            return;
        }
    };
    let pool_size = control_conn_pool_size();
    tracing::info!(
        event = "local_server_control_pool_starting",
        key = %key,
        pool_size,
        "starting local server control connection pool"
    );
    let mut worker_handles = Vec::new();
    if pool_size > 1 {
        for worker_index in 1..pool_size {
            let worker_key = key.clone();
            worker_handles.push(tokio::spawn(async move {
                run_server_side_cli_worker::<LocalStream, _>(
                    local_addr,
                    remote_addr,
                    worker_key,
                    need_codec,
                    is_datagram,
                    None,
                    worker_index,
                )
                .await;
            }));
        }
    }
    run_server_side_cli_worker::<LocalStream, _>(
        local_addr,
        remote_addr,
        key,
        need_codec,
        is_datagram,
        status_callback,
        0,
    )
    .await;
    for handle in worker_handles {
        if let Err(e) = handle.await {
            tracing::warn!(
                event = "local_server_control_worker_join_failed",
                error = %e,
                "local server control worker join failed"
            );
        }
    }
}

async fn run_server_side_cli_worker<LocalStream, A>(
    local_addr: A,
    remote_addr: A,
    key: Arc<str>,
    need_codec: bool,
    is_datagram: bool,
    status_callback: Option<StatusCallback>,
    worker_index: usize,
) where
    LocalStream: StreamProvider + Send + 'static,
    LocalStream::Item: StreamForward,
    A: ToSocketAddrs + Debug + Copy + Send + 'static,
{
    let run_config = ServerCliRunConfig {
        local_addr,
        remote_addr,
        key: key.clone(),
        need_codec,
        is_datagram,
        worker_index,
    };
    let mut timeout_count = TimeoutCount::new(GLOBAL_RETRY_TIMES);
    let mut retry_interval = timeout_count.get_interval_by_count();
    while timeout_count.validate() {
        let status = if let Err(status) = run_server_side_cli_inner::<LocalStream, _>(
            &mut timeout_count,
            run_config.clone(),
            status_callback.as_ref(),
        )
        .await
        {
            status
        } else {
            return;
        };
        match status {
            Status::ReadMsg | Status::SendPing | Status::ConnectRemote => {
                tracing::info!(
                    "We will try to re-connect the pb-server:`{:?} <-`{}`-> {:?}` after \
                     {retry_interval}s, global-retry-count:{}",
                    local_addr,
                    key,
                    remote_addr,
                    timeout_count.count(),
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
    config: ServerCliRunConfig<A>,
    status_callback: Option<&StatusCallback>,
) -> std::result::Result<(), Status>
where
    LocalStream::Item: StreamForward,
{
    let ServerCliRunConfig {
        local_addr,
        remote_addr,
        key,
        need_codec,
        is_datagram,
        worker_index,
    } = config;
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
    tracing::info!(
        event = "local_server_connected_remote",
        key = %key,
        worker_index,
        local_addr = %local_addr,
        remote_addr = %remote_addr,
        need_codec,
        is_datagram,
        "local server connected to pb server"
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
        let timeout = control_io_timeout();
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
        match tokio::time::timeout(timeout, msg_writer.write_msg(&msg)).await {
            Ok(result) => snafu_error_get_or_return_ok!(result.context(SendRegisterReqSnafu)),
            Err(_) => snafu_error_get_or_return_ok!(ControlIoTimeoutSnafu {
                action: "send register request",
                timeout,
            }
            .fail()),
        }
    }
    let (mut reader, mut writer) = manager_stream.into_split();
    let mut msg_reader = match get_header_msg_reader(&mut reader) {
        Ok(reader) => reader,
        Err(e) => {
            tracing::error!("create manager header reader failed: {e}");
            return Err(Status::ReadMsg);
        }
    };
    // read register resp to indicate that register has finished
    let (key, conn_id) = {
        let timeout = control_io_timeout();
        let msg = match tokio::time::timeout(timeout, msg_reader.read_msg()).await {
            Ok(result) => snafu_error_get_or_return_ok!(result.context(ReadRegisterRespSnafu)),
            Err(_) => snafu_error_get_or_return_ok!(ControlIoTimeoutSnafu {
                action: "read register response",
                timeout,
            }
            .fail()),
        };
        let resp = snafu_error_get_or_return_ok!(
            PbConnResponse::decode(msg).context(DecodeRegisterRespSnafu)
        );
        let PbConnResponse::Register(conn_id) = resp else {
            snafu_error_get_or_return_ok!(RegisterRespNotMatchSnafu {}.fail())
        };
        tracing::info!(
            event = "local_server_registered",
            key = %key,
            conn_id = %conn_id,
            worker_index,
            local_addr = %local_addr,
            remote_addr = %remote_addr,
            "local server registered with pb server"
        );

        // Notify external systems that connection is established
        if let Some(ref callback) = status_callback {
            callback("connected");
        }
        (key, conn_id)
    };

    // register ok, and reset global timeout count
    global_timeout_cnt.reset();
    let (write_tx, mut write_rx) = tokio::sync::mpsc::unbounded_channel::<LocalControlWrite>();
    let writer_key = key.clone();
    let mut writer_handle = tokio::spawn(async move {
        let mut msg_writer = match get_header_msg_writer(&mut writer) {
            Ok(writer) => writer,
            Err(e) => {
                tracing::error!("create manager header writer failed: {e}");
                return Err(Status::SendPing);
            }
        };
        loop {
            let Some(cmd) = write_rx.recv().await else {
                return Ok(());
            };
            match cmd {
                LocalControlWrite::Ping => {
                    snafu_error_get_or_return!(
                        handle_ping_interval(&mut msg_writer, writer_key.clone(), conn_id).await,
                        "[send ping]",
                        Err(Status::SendPing)
                    );
                    tracing::debug!(
                        event = "local_server_ping_sent",
                        key = %writer_key,
                        conn_id = %conn_id,
                        interval = ?PING_INTERVAL,
                        "local server ping sent"
                    );
                }
                LocalControlWrite::StreamAck {
                    client_id,
                    server_generation,
                } => {
                    snafu_error_get_or_return!(
                        write_stream_ack(&mut msg_writer, client_id, server_generation).await,
                        "[send stream ack]",
                        Err(Status::SendPing)
                    );
                }
            }
        }
    });
    let ping_tx = write_tx.clone();
    let ping_handle = tokio::spawn(async move {
        loop {
            tokio::time::sleep(PING_INTERVAL).await;
            if ping_tx.send(LocalControlWrite::Ping).is_err() {
                break;
            }
        }
    });

    let reader_result = async {
        loop {
            let msg = snafu_error_get_or_return!(
                msg_reader.read_msg().await.context(ReadStreamReqSnafu),
                "[read msg]",
                Err(Status::ReadMsg)
            );
            snafu_error_get_or_continue!(
                handle_request::<LocalStream, _>(
                    msg,
                    local_addr,
                    remote_addr,
                    key.clone(),
                    conn_id,
                    &write_tx
                )
                .await
            );
        }
    };

    let result = tokio::select! {
        result = reader_result => result,
        result = &mut writer_handle => match result {
            Ok(result) => result,
            Err(e) => {
                tracing::error!(
                    event = "local_server_control_writer_join_failed",
                    key = %key,
                    conn_id = %conn_id,
                    error = %e,
                    "local server control writer task failed"
                );
                Err(Status::SendPing)
            }
        },
    };
    ping_handle.abort();
    if !writer_handle.is_finished() {
        writer_handle.abort();
    }
    result
}

#[instrument(skip(writer))]
async fn handle_ping_interval<T: MessageWriter>(
    writer: &mut T,
    key: Arc<str>,
    conn_id: u32,
) -> error::Result<()> {
    let ping_msg = get_ping_message()?;
    let timeout = control_io_timeout();
    match tokio::time::timeout(timeout, writer.write_msg(&ping_msg)).await {
        Ok(result) => result.context(WritePingMsgSnafu),
        Err(_) => ControlIoTimeoutSnafu {
            action: "write ping message",
            timeout,
        }
        .fail(),
    }
}

#[instrument(skip(msg, write_tx))]
async fn handle_request<
    LocalStream: StreamProvider,
    A: ToSocketAddrs + Debug + Copy + Clone + Send + 'static,
>(
    msg: &[u8],
    local_addr: A,
    remote_addr: A,
    key: Arc<str>,
    conn_id: u32,
    write_tx: &tokio::sync::mpsc::UnboundedSender<LocalControlWrite>,
) -> error::Result<()>
where
    LocalStream::Item: StreamForward,
{
    let req = LocalServer::decode(msg).context(DecodeStreamReqSnafu)?;

    match req {
        LocalServer::Stream {
            client_id,
            server_generation,
        } => {
            tracing::debug!(
                event = "local_server_stream_request_received",
                key = %key,
                server_conn_id = %conn_id,
                client_conn_id = client_id,
                server_generation,
                "local server received stream request"
            );
            write_tx
                .send(LocalControlWrite::StreamAck {
                    client_id,
                    server_generation,
                })
                .map_err(|_| error::Error::ControlWriterClosed {
                    action: "stream ack message",
                })?;
            let key = key.clone();
            tokio::spawn(async move {
                snafu_error_handle!(
                    handle_stream::<LocalStream, _>(
                        local_addr,
                        remote_addr,
                        key,
                        client_id,
                        server_generation
                    )
                    .await
                )
            });
        }
        // got pong response
        LocalServer::Pong => {
            tracing::debug!(
                event = "local_server_pong_received",
                key = %key,
                server_conn_id = %conn_id,
                "local server received pong"
            );
        }
    }
    Ok(())
}

async fn write_stream_ack<T: MessageWriter>(
    writer: &mut T,
    client_id: u32,
    server_generation: u64,
) -> error::Result<()> {
    let ack = PbServerRequest::StreamAck {
        client_id,
        server_generation,
    }
    .encode()
    .context(EncodeStreamAckMsgSnafu)?;
    let timeout = control_io_timeout();
    match tokio::time::timeout(timeout, writer.write_msg(&ack)).await {
        Ok(result) => result.context(WriteStreamAckMsgSnafu),
        Err(_) => ControlIoTimeoutSnafu {
            action: "write stream ack message",
            timeout,
        }
        .fail(),
    }
}
