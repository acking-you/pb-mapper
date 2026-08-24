pub mod error;
mod stream;

use std::fmt::Debug;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use snafu::ResultExt;
use tokio::net::TcpStream;
use tokio::task::JoinSet;
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;
use tracing::instrument;

use self::error::{
    ControlIoTimeoutSnafu, DecodeRegisterRespSnafu, DecodeStreamReqSnafu, EncodePingMsgSnafu,
    EncodeRegisterReqSnafu, EncodeStreamAckMsgSnafu, ReadRegisterRespSnafu, ReadStreamReqSnafu,
    RegisterRespNotMatchSnafu, SendRegisterReqSnafu, WritePingMsgSnafu, WriteStreamAckMsgSnafu,
};
use self::stream::{StreamConnect, handle_stream};
use pb_mapper_core::checksum::{Credential, get_process_credential};
use pb_mapper_core::config::{
    control_conn_pool_size, control_heartbeat_interval, control_heartbeat_tolerance,
    control_io_timeout, control_suspect_grace, registration_probe_timeout,
};
use pb_mapper_core::timeout::RetryBackoff;
use pb_mapper_core::{
    snafu_error_get_or_continue, snafu_error_get_or_return, snafu_error_get_or_return_ok,
    snafu_error_handle,
};
use pb_mapper_protocol::command::{
    CONTROL_PROTOCOL_V2, LocalServer, MessageSerializer, PbConnRequest, PbConnResponse,
    PbConnStatusReq, PbConnStatusResp, PbServerRequest,
};
use pb_mapper_protocol::forward::StreamForward;
use pb_mapper_protocol::secure::ClientHeaderSession;
use pb_mapper_protocol::{MessageReader, MessageWriter};
use uni_stream::addr::{ToSocketAddrs, each_addr};
use uni_stream::stream::{
    StreamProvider, got_one_socket_addr, set_tcp_keep_alive, set_tcp_nodelay,
};

fn get_ping_message(protocol_version: u16, seq: u64) -> error::Result<Vec<u8>> {
    if protocol_version >= CONTROL_PROTOCOL_V2 {
        PbServerRequest::PingV2 { seq }
            .encode()
            .context(EncodePingMsgSnafu)
    } else {
        PbServerRequest::Ping.encode().context(EncodePingMsgSnafu)
    }
}

#[derive(Debug)]
enum Status {
    ReadMsg,
    SendPing,
    ConnectRemote,
    Cancelled,
    /// The relay refused the registration for a reason reconnecting cannot fix —
    /// a namespace the credential does not own, a malformed service name. Retrying
    /// would loop forever while the caller's `wait_ready` never resolves, so this
    /// ends the worker instead.
    Rejected(String),
}

enum LocalControlWrite {
    Ping {
        seq: u64,
    },
    StreamAck {
        client_id: u32,
        server_generation: u64,
    },
}

#[derive(Debug, Clone, Copy)]
struct ControlRegistration {
    conn_id: u32,
    generation: u64,
    protocol_version: u16,
    lease_ttl_ms: u64,
}

#[derive(Debug)]
struct ControlLeaseState {
    last_rx_at: Instant,
    last_pong_at: Option<Instant>,
}

impl ControlLeaseState {
    fn new() -> Self {
        Self {
            last_rx_at: Instant::now(),
            last_pong_at: None,
        }
    }

    fn record_rx(&mut self) {
        self.last_rx_at = Instant::now();
    }

    fn record_pong(&mut self) {
        let now = Instant::now();
        self.last_rx_at = now;
        self.last_pong_at = Some(now);
    }

    fn last_rx_age(&self) -> Duration {
        self.last_rx_at.elapsed()
    }
}

#[derive(Debug)]
enum RegistrationProbeResult {
    Present,
    Missing,
    Failed(String),
}

// Callback for notifying status changes to external systems
pub type StatusCallback = Box<dyn Fn(&str) + Send + Sync>;

#[derive(Debug)]
pub enum ServiceStatus {
    Retrying,
    Connected,
    Failed,
}

/// How a local-server tunnel forwards: the codec, the transport, and whether
/// its sockets keep alive.
///
/// Grouped rather than passed as three adjacent `bool`s, which is a
/// transposition waiting to happen at a call site — and named fields make the
/// call sites say which is which.
#[derive(Clone, Copy, Debug)]
pub struct ServerTunnelOptions {
    pub need_codec: bool,
    pub is_datagram: bool,
    pub keep_alive: bool,
    pub namespace: Option<u64>,
    pub force_namespace: bool,
}

#[derive(Clone)]
struct ServerCliRunConfig<A> {
    local_addr: A,
    remote_addr: A,
    key: Arc<str>,
    options: ServerTunnelOptions,
    worker_index: usize,
    credential: Credential,
}

impl<A: Debug> Debug for ServerCliRunConfig<A> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ServerCliRunConfig")
            .field("local_addr", &self.local_addr)
            .field("remote_addr", &self.remote_addr)
            .field("key", &self.key)
            .field("options", &self.options)
            .field("worker_index", &self.worker_index)
            .field("credential_key_id", &self.credential.key_id())
            .finish()
    }
}

fn duration_to_millis(duration: Duration) -> u64 {
    duration.as_millis().min(u128::from(u64::MAX)) as u64
}

fn new_client_instance_id(worker_index: usize) -> String {
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(duration_to_millis)
        .unwrap_or_default();
    format!("{}-{worker_index}-{now_ms}", std::process::id())
}

async fn probe_remote_registration(
    remote_addr: SocketAddr,
    key: Arc<str>,
    registration: ControlRegistration,
    namespace: Option<u64>,
    credential: Credential,
) -> RegistrationProbeResult {
    let timeout = registration_probe_timeout();
    let result = tokio::time::timeout(timeout, async {
        let mut stream = each_addr(remote_addr, TcpStream::connect)
            .await
            .map_err(|e| format!("connect remote status stream failed: {e}"))?;
        crate::client::status::get_status_with_credential(
            &mut stream,
            PbConnStatusReq::Service {
                key: key.to_string(),
            },
            namespace,
            &credential,
        )
        .await
        .map_err(|e| {
            format!(
                "get service status failed: {}",
                snafu::Report::from_error(e)
            )
        })
    })
    .await;

    let status = match result {
        Ok(Ok(status)) => status,
        Ok(Err(reason)) => return RegistrationProbeResult::Failed(reason),
        Err(_) => {
            return RegistrationProbeResult::Failed(format!(
                "status probe timed out after {timeout:?}"
            ));
        }
    };

    match status {
        PbConnStatusResp::Service { connections, .. } => {
            let present = connections.iter().any(|conn| {
                conn.conn_id == registration.conn_id
                    && conn.generation == registration.generation
                    && conn.healthy
            });
            if present {
                RegistrationProbeResult::Present
            } else {
                RegistrationProbeResult::Missing
            }
        }
        PbConnStatusResp::Keys(keys) => {
            if keys.iter().any(|candidate| candidate == key.as_ref()) {
                RegistrationProbeResult::Present
            } else {
                RegistrationProbeResult::Missing
            }
        }
        other => RegistrationProbeResult::Failed(format!(
            "unexpected status response while probing registration: {other:?}"
        )),
    }
}

pub async fn run_server_side_cli<LocalStream, A>(
    local_addr: A,
    remote_addr: A,
    key: Arc<str>,
    options: ServerTunnelOptions,
) where
    LocalStream: StreamProvider + Send + 'static,
    LocalStream::Item: StreamForward,
    A: ToSocketAddrs + Debug + Copy,
{
    run_server_side_cli_with_callback::<LocalStream, A>(local_addr, remote_addr, key, options, None)
        .await
}

pub async fn run_server_side_cli_with_pinned_credential<LocalStream, A>(
    local_addr: A,
    remote_addr: A,
    key: Arc<str>,
    options: ServerTunnelOptions,
    status_callback: Option<StatusCallback>,
    credential: Credential,
) where
    LocalStream: StreamProvider + Send + 'static,
    LocalStream::Item: StreamForward,
    A: ToSocketAddrs + Debug + Copy,
{
    run_server_side_cli_pool::<LocalStream, A>(
        local_addr,
        remote_addr,
        key,
        options,
        status_callback,
        Some(credential),
        CancellationToken::new(),
    )
    .await;
}

/// Same as [`run_server_side_cli_with_pinned_credential`], but the retry loop
/// returns when `shutdown` is cancelled.
pub async fn run_server_side_cli_with_shutdown<LocalStream, A>(
    local_addr: A,
    remote_addr: A,
    key: Arc<str>,
    options: ServerTunnelOptions,
    status_callback: Option<StatusCallback>,
    credential: Credential,
    shutdown: CancellationToken,
) where
    LocalStream: StreamProvider + Send + 'static,
    LocalStream::Item: StreamForward,
    A: ToSocketAddrs + Debug + Copy,
{
    run_server_side_cli_pool::<LocalStream, A>(
        local_addr,
        remote_addr,
        key,
        options,
        status_callback,
        Some(credential),
        shutdown,
    )
    .await;
}

pub async fn run_server_side_cli_with_callback<LocalStream, A>(
    local_addr: A,
    remote_addr: A,
    key: Arc<str>,
    options: ServerTunnelOptions,
    status_callback: Option<StatusCallback>,
) where
    LocalStream: StreamProvider + Send + 'static,
    LocalStream::Item: StreamForward,
    A: ToSocketAddrs + Debug + Copy,
{
    run_server_side_cli_pool::<LocalStream, A>(
        local_addr,
        remote_addr,
        key,
        options,
        status_callback,
        None,
        CancellationToken::new(),
    )
    .await;
}

async fn resolve_registration_credential(
    pinned: Option<Credential>,
    shutdown: &CancellationToken,
) -> Option<Credential> {
    if let Some(credential) = pinned {
        return Some(credential);
    }
    let mut retry_backoff = RetryBackoff::default();
    loop {
        if shutdown.is_cancelled() {
            return None;
        }
        match get_process_credential() {
            Ok(credential) => return Some(credential),
            Err(error) => {
                tracing::error!("load registration credential failed: {error}");
                tokio::select! {
                    () = shutdown.cancelled() => return None,
                    () = tokio::time::sleep(retry_backoff.next_delay()) => {}
                }
            }
        }
    }
}

async fn run_server_side_cli_pool<LocalStream, A>(
    local_addr: A,
    remote_addr: A,
    key: Arc<str>,
    options: ServerTunnelOptions,
    status_callback: Option<StatusCallback>,
    pinned_credential: Option<Credential>,
    shutdown: CancellationToken,
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
    let Some(credential) = resolve_registration_credential(pinned_credential, &shutdown).await
    else {
        return;
    };
    let pool_size = control_conn_pool_size().max(1);
    tracing::info!(
        event = "local_server_control_pool_starting",
        key = %key,
        pool_size,
        "starting local server control connection pool"
    );
    let mut workers = JoinSet::new();
    let mut status_callback = status_callback;
    for worker_index in 0..pool_size {
        let worker_key = key.clone();
        let callback = if worker_index == 0 {
            status_callback.take()
        } else {
            None
        };
        let worker_shutdown = shutdown.clone();
        workers.spawn(async move {
            run_server_side_cli_worker::<LocalStream, _>(
                local_addr,
                remote_addr,
                worker_key,
                options,
                callback,
                worker_index,
                credential,
                worker_shutdown,
            )
            .await;
        });
    }
    while let Some(result) = workers.join_next().await {
        if let Err(e) = result {
            tracing::warn!(
                event = "local_server_control_worker_join_failed",
                error = %e,
                "local server control worker join failed"
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_server_side_cli_worker<LocalStream, A>(
    local_addr: A,
    remote_addr: A,
    key: Arc<str>,
    options: ServerTunnelOptions,
    status_callback: Option<StatusCallback>,
    worker_index: usize,
    credential: Credential,
    shutdown: CancellationToken,
) where
    LocalStream: StreamProvider + Send + 'static,
    LocalStream::Item: StreamForward,
    A: ToSocketAddrs + Debug + Copy + Send + 'static,
{
    let mut retry_backoff = RetryBackoff::default();
    let run_config = ServerCliRunConfig {
        local_addr,
        remote_addr,
        key: key.clone(),
        options,
        worker_index,
        credential,
    };
    // Forwarded sessions outlive a single control connection, so the set lives
    // here rather than inside the attempt: a reconnect is not a reason to drop
    // streams that are still healthy. It is drained before this worker returns.
    let mut stream_tasks = JoinSet::new();
    'outer: loop {
        if shutdown.is_cancelled() {
            break 'outer;
        }
        let status = if let Err(status) = run_server_side_cli_inner::<LocalStream, _>(
            &mut retry_backoff,
            run_config.clone(),
            status_callback.as_ref(),
            shutdown.clone(),
            &mut stream_tasks,
        )
        .await
        {
            status
        } else {
            if shutdown.is_cancelled() {
                break 'outer;
            }
            tracing::warn!(
                event = "local_server_control_worker_finished",
                key = %key,
                worker_index,
                "local server control worker finished without an error; reconnecting"
            );
            Status::ReadMsg
        };
        match status {
            Status::Cancelled => break 'outer,
            Status::Rejected(reason) => {
                tracing::error!(
                    event = "local_server_registration_rejected_permanently",
                    key = %key,
                    worker_index,
                    reason = %reason,
                    "pb server permanently rejected this registration; not reconnecting"
                );
                if let Some(ref callback) = status_callback {
                    callback(&format!("failed: {reason}"));
                }
                break 'outer;
            }
            Status::ReadMsg | Status::SendPing | Status::ConnectRemote => {
                let retry_interval = retry_backoff.next_delay();
                tracing::info!(
                    event = "local_server_control_reconnect_scheduled",
                    key = %key,
                    worker_index,
                    local_addr = ?local_addr,
                    remote_addr = ?remote_addr,
                    status = ?status,
                    retry_delay = ?retry_interval,
                    retry_count = retry_backoff.failures(),
                    "local server control connection will reconnect"
                );

                if let Some(ref callback) = status_callback {
                    let status = format!("{status:?}");
                    callback(&status);
                }

                tokio::select! {
                    () = shutdown.cancelled() => break 'outer,
                    () = tokio::time::sleep(retry_interval) => {}
                }
                if shutdown.is_cancelled() {
                    break 'outer;
                }
                if let Some(ref callback) = status_callback {
                    callback("retrying");
                }
            }
        }
    }

    // Cancellation is already signalled by the token the sessions hold; this only
    // waits for them to observe it.
    stream_tasks.shutdown().await;
}

#[instrument(skip(status_callback, shutdown, stream_tasks))]
async fn run_server_side_cli_inner<LocalStream: StreamProvider, A: ToSocketAddrs + Debug + Copy>(
    retry_backoff: &mut RetryBackoff,
    config: ServerCliRunConfig<A>,
    status_callback: Option<&StatusCallback>,
    shutdown: CancellationToken,
    stream_tasks: &mut JoinSet<()>,
) -> std::result::Result<(), Status>
where
    LocalStream::Item: StreamForward,
{
    let ServerCliRunConfig {
        local_addr,
        remote_addr,
        key,
        options:
            ServerTunnelOptions {
                need_codec,
                is_datagram,
                keep_alive,
                namespace,
                force_namespace,
            },
        worker_index,
        credential,
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

    if keep_alive {
        snafu_error_handle!(
            set_tcp_keep_alive(&manager_stream),
            "manager stream set tcp keep alive"
        );
    }
    snafu_error_handle!(
        set_tcp_nodelay(&manager_stream),
        "manager stream set tcp nodelay"
    );

    // Start registration with a protocol-v2 first frame. The session is reused for all
    // subsequent control messages on this TCP connection. The credential is pinned
    // when the worker starts so a later process-key change cannot retarget reconnects.
    let session = match ClientHeaderSession::new_v2(&credential) {
        Ok(session) => session,
        Err(error) => {
            tracing::error!("create manager protocol-v2 session failed: {error}");
            return Err(Status::ConnectRemote);
        }
    };
    let timeout = control_io_timeout();
    let heartbeat_interval = control_heartbeat_interval();
    let heartbeat_tolerance = control_heartbeat_tolerance();
    let request = match namespace {
        Some(namespace) => PbConnRequest::RegisterScoped {
            key: key.to_string(),
            namespace,
            force_namespace,
            need_codec,
            is_datagram,
            protocol_version: Some(CONTROL_PROTOCOL_V2),
            client_instance_id: Some(new_client_instance_id(worker_index)),
            heartbeat_interval_ms: Some(duration_to_millis(heartbeat_interval)),
            heartbeat_tolerance_ms: Some(duration_to_millis(heartbeat_tolerance)),
        },
        None => PbConnRequest::Register {
            key: key.to_string(),
            need_codec,
            is_datagram,
            protocol_version: Some(CONTROL_PROTOCOL_V2),
            client_instance_id: Some(new_client_instance_id(worker_index)),
            heartbeat_interval_ms: Some(duration_to_millis(heartbeat_interval)),
            heartbeat_tolerance_ms: Some(duration_to_millis(heartbeat_tolerance)),
        },
    };
    let msg = snafu_error_get_or_return_ok!(request.encode().context(EncodeRegisterReqSnafu));
    match tokio::time::timeout(timeout, session.write_initial(&mut manager_stream, &msg)).await {
        Ok(result) => snafu_error_get_or_return_ok!(result.context(SendRegisterReqSnafu)),
        Err(_) => snafu_error_get_or_return_ok!(
            ControlIoTimeoutSnafu {
                action: "send register request",
                timeout,
            }
            .fail()
        ),
    }
    let (mut reader, mut writer) = manager_stream.into_split();
    let mut msg_reader = match session.response_reader(&mut reader) {
        Ok(reader) => reader,
        Err(e) => {
            tracing::error!("create manager header reader failed: {e}");
            return Err(Status::ReadMsg);
        }
    };
    // read register resp to indicate that register has finished
    let (key, registration) = {
        let timeout = control_io_timeout();
        let msg = match tokio::time::timeout(timeout, msg_reader.read_msg()).await {
            Ok(result) => snafu_error_get_or_return_ok!(result.context(ReadRegisterRespSnafu)),
            Err(_) => snafu_error_get_or_return_ok!(
                ControlIoTimeoutSnafu {
                    action: "read register response",
                    timeout,
                }
                .fail()
            ),
        };
        let resp = snafu_error_get_or_return_ok!(
            PbConnResponse::decode(msg).context(DecodeRegisterRespSnafu)
        );
        let registration = match resp {
            PbConnResponse::RegisterV2 {
                conn_id,
                generation,
                lease_ttl_ms,
            } => ControlRegistration {
                conn_id,
                generation,
                protocol_version: CONTROL_PROTOCOL_V2,
                lease_ttl_ms,
            },
            PbConnResponse::Register(conn_id) => ControlRegistration {
                conn_id,
                generation: 0,
                protocol_version: 1,
                lease_ttl_ms: 0,
            },
            PbConnResponse::Error(error) => {
                tracing::error!(
                    event = "local_server_registration_rejected",
                    reason = %error.code,
                    retryable = error.retryable,
                    message = %error.message,
                    "pb server rejected service registration"
                );
                if !error.retryable {
                    return Err(Status::Rejected(format!(
                        "{}: {}",
                        error.code, error.message
                    )));
                }
                snafu_error_get_or_return_ok!(RegisterRespNotMatchSnafu {}.fail())
            }
            _ => snafu_error_get_or_return_ok!(RegisterRespNotMatchSnafu {}.fail()),
        };
        tracing::info!(
            event = "local_server_registered",
            key = %key,
            conn_id = %registration.conn_id,
            generation = registration.generation,
            protocol_version = registration.protocol_version,
            lease_ttl_ms = registration.lease_ttl_ms,
            worker_index,
            local_addr = %local_addr,
            remote_addr = %remote_addr,
            "local server registered with pb server"
        );

        // Notify external systems that connection is established
        if let Some(ref callback) = status_callback {
            callback("connected");
        }
        (key, registration)
    };

    retry_backoff.reset();
    let (write_tx, mut write_rx) = tokio::sync::mpsc::unbounded_channel::<LocalControlWrite>();
    let lease_state = Arc::new(tokio::sync::Mutex::new(ControlLeaseState::new()));
    let writer_key = key.clone();
    let writer_registration = registration;
    let mut writer_handle = tokio::spawn(async move {
        let mut msg_writer = match session.continuation_writer(&mut writer) {
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
                LocalControlWrite::Ping { seq } => {
                    snafu_error_get_or_return!(
                        handle_ping_interval(
                            &mut msg_writer,
                            writer_key.clone(),
                            writer_registration,
                            seq
                        )
                        .await,
                        "[send ping]",
                        Err(Status::SendPing)
                    );
                    tracing::debug!(
                        event = "local_server_heartbeat_sent",
                        key = %writer_key,
                        conn_id = %writer_registration.conn_id,
                        generation = writer_registration.generation,
                        protocol_version = writer_registration.protocol_version,
                        seq,
                        interval = ?control_heartbeat_interval(),
                        "local server heartbeat sent"
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

    let heartbeat_interval = control_heartbeat_interval();
    let heartbeat_tolerance = control_heartbeat_tolerance();
    let suspect_grace = control_suspect_grace();
    let mut heartbeat = tokio::time::interval(heartbeat_interval);
    heartbeat.set_missed_tick_behavior(MissedTickBehavior::Delay);
    heartbeat.tick().await;
    let (probe_tx, mut probe_rx) =
        tokio::sync::mpsc::unbounded_channel::<RegistrationProbeResult>();
    let mut probe_in_flight = false;
    let mut ping_seq = 0_u64;

    let result = loop {
        tokio::select! {
            msg = msg_reader.read_msg() => {
                let msg = match msg.context(ReadStreamReqSnafu) {
                    Ok(msg) => msg,
                    Err(e) => {
                        tracing::error!(
                            event = "local_server_control_read_failed",
                            key = %key,
                            conn_id = %registration.conn_id,
                            generation = registration.generation,
                            error = %snafu::Report::from_error(e),
                            "local server control read failed"
                        );
                        break Err(Status::ReadMsg);
                    }
                };
                lease_state.lock().await.record_rx();
                snafu_error_get_or_continue!(
                    handle_request::<LocalStream, _>(
                        msg,
                        StreamConnect {
                            local_addr,
                            remote_addr,
                            keep_alive,
                            namespace,
                            credential,
                        },
                        key.clone(),
                        registration.conn_id,
                        &write_tx,
                        lease_state.clone(),
                        stream_tasks,
                        &shutdown,
                    )
                    .await
                );
            }
            Some(_) = stream_tasks.join_next() => {
                // Reap finished sessions so the set does not grow for the lifetime
                // of the registration. `handle_stream` logs its own failures.
            }
            result = &mut writer_handle => {
                break match result {
                    Ok(result) => result,
                    Err(e) => {
                        tracing::error!(
                            event = "local_server_control_writer_join_failed",
                            key = %key,
                            conn_id = %registration.conn_id,
                            generation = registration.generation,
                            error = %e,
                            "local server control writer task failed"
                        );
                        Err(Status::SendPing)
                    }
                };
            }
            _ = heartbeat.tick() => {
                ping_seq = ping_seq.wrapping_add(1);
                if write_tx.send(LocalControlWrite::Ping { seq: ping_seq }).is_err() {
                    break Err(Status::SendPing);
                }

                let last_rx_age = lease_state.lock().await.last_rx_age();
                if registration.protocol_version >= CONTROL_PROTOCOL_V2
                    && last_rx_age >= heartbeat_tolerance
                    && !probe_in_flight
                {
                    tracing::warn!(
                        event = "local_server_lease_suspect",
                        key = %key,
                        conn_id = %registration.conn_id,
                        generation = registration.generation,
                        worker_index,
                        last_rx_age_ms = duration_to_millis(last_rx_age),
                        heartbeat_tolerance_ms = duration_to_millis(heartbeat_tolerance),
                        "local server control lease is suspect; probing remote registration"
                    );
                    probe_in_flight = true;
                    let probe_tx = probe_tx.clone();
                    let probe_key = key.clone();
                    tokio::spawn(async move {
                        let result = probe_remote_registration(
                            remote_addr,
                            probe_key,
                            registration,
                            namespace,
                            credential,
                        )
                        .await;
                        let _ = probe_tx.send(result);
                    });
                }
            }
            () = shutdown.cancelled() => {
                tracing::info!(
                    event = "local_server_control_cancelled",
                    key = %key,
                    conn_id = %registration.conn_id,
                    generation = registration.generation,
                    worker_index,
                    "local server control loop cancelled"
                );
                break Err(Status::Cancelled);
            }
            Some(probe_result) = probe_rx.recv() => {
                probe_in_flight = false;
                let last_rx_age = lease_state.lock().await.last_rx_age();
                match probe_result {
                    RegistrationProbeResult::Present => {
                        tracing::debug!(
                            event = "local_server_registration_probe_ok",
                            key = %key,
                            conn_id = %registration.conn_id,
                            generation = registration.generation,
                            worker_index,
                            last_rx_age_ms = duration_to_millis(last_rx_age),
                            "remote registration still contains this control connection"
                        );
                    }
                    RegistrationProbeResult::Missing if last_rx_age >= heartbeat_tolerance => {
                        tracing::warn!(
                            event = "local_server_registration_missing",
                            key = %key,
                            conn_id = %registration.conn_id,
                            generation = registration.generation,
                            worker_index,
                            last_rx_age_ms = duration_to_millis(last_rx_age),
                            "remote registration no longer contains this control connection; reconnecting"
                        );
                        break Err(Status::ReadMsg);
                    }
                    RegistrationProbeResult::Missing => {
                        tracing::debug!(
                            event = "local_server_registration_missing_ignored_after_recent_activity",
                            key = %key,
                            conn_id = %registration.conn_id,
                            generation = registration.generation,
                            worker_index,
                            last_rx_age_ms = duration_to_millis(last_rx_age),
                            "remote registration probe was stale after recent control activity"
                        );
                    }
                    RegistrationProbeResult::Failed(reason)
                        if last_rx_age >= heartbeat_tolerance + suspect_grace =>
                    {
                        tracing::warn!(
                            event = "local_server_status_probe_failed",
                            key = %key,
                            conn_id = %registration.conn_id,
                            generation = registration.generation,
                            worker_index,
                            last_rx_age_ms = duration_to_millis(last_rx_age),
                            reason = %reason,
                            "registration probe failed past suspect grace; reconnecting"
                        );
                        break Err(Status::ReadMsg);
                    }
                    RegistrationProbeResult::Failed(reason) => {
                        tracing::warn!(
                            event = "local_server_status_probe_failed",
                            key = %key,
                            conn_id = %registration.conn_id,
                            generation = registration.generation,
                            worker_index,
                            last_rx_age_ms = duration_to_millis(last_rx_age),
                            reason = %reason,
                            "registration probe failed; waiting inside suspect grace"
                        );
                    }
                }
            }
        }
    };
    if !writer_handle.is_finished() {
        writer_handle.abort();
    }
    result
}

#[instrument(skip(writer))]
async fn handle_ping_interval<T: MessageWriter>(
    writer: &mut T,
    _key: Arc<str>,
    registration: ControlRegistration,
    seq: u64,
) -> error::Result<()> {
    let ping_msg = get_ping_message(registration.protocol_version, seq)?;
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

#[instrument(skip(msg, write_tx, lease_state, stream_tasks, shutdown))]
#[allow(clippy::too_many_arguments)]
async fn handle_request<
    LocalStream: StreamProvider,
    A: ToSocketAddrs + Debug + Copy + Clone + Send + 'static,
>(
    msg: &[u8],
    target: StreamConnect<A>,
    key: Arc<str>,
    conn_id: u32,
    write_tx: &tokio::sync::mpsc::UnboundedSender<LocalControlWrite>,
    lease_state: Arc<tokio::sync::Mutex<ControlLeaseState>>,
    stream_tasks: &mut JoinSet<()>,
    shutdown: &CancellationToken,
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
            let stream_shutdown = shutdown.clone();
            // Tracked rather than detached: a cancelled registration must take its
            // in-flight forwarded sessions with it, so `stop()` returning means no
            // stream of this tunnel is still moving bytes.
            stream_tasks.spawn(async move {
                let forward =
                    handle_stream::<LocalStream, _>(key, client_id, server_generation, target);
                tokio::select! {
                    () = stream_shutdown.cancelled() => {}
                    result = forward => snafu_error_handle!(result),
                }
            });
        }
        // got pong response
        LocalServer::Pong => {
            lease_state.lock().await.record_pong();
            tracing::debug!(
                event = "local_server_pong_received",
                key = %key,
                server_conn_id = %conn_id,
                "local server received pong"
            );
        }
        LocalServer::PongV2 { seq } => {
            lease_state.lock().await.record_pong();
            tracing::debug!(
                event = "local_server_pong_received",
                key = %key,
                server_conn_id = %conn_id,
                seq,
                "local server received pong v2"
            );
        }
        LocalServer::Retire {
            reason,
            conn_id: retired_conn_id,
            server_generation,
        } => {
            tracing::warn!(
                event = "local_server_control_retired",
                key = %key,
                server_conn_id = %conn_id,
                retired_conn_id,
                server_generation,
                reason = %reason,
                "remote server retired this local control connection"
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

#[cfg(test)]
mod shutdown_tests {
    use std::time::Duration;

    use super::*;
    use uni_stream::stream::TcpStreamProvider;

    #[tokio::test]
    async fn register_loop_stops_on_cancel() {
        let shutdown = CancellationToken::new();
        let token = shutdown.clone();
        let addr: SocketAddr = "127.0.0.1:1".parse().unwrap();
        let task = tokio::spawn(async move {
            run_server_side_cli_with_shutdown::<TcpStreamProvider, _>(
                addr,
                addr,
                "k".into(),
                ServerTunnelOptions {
                    need_codec: false,
                    is_datagram: false,
                    keep_alive: false,
                    namespace: None,
                    force_namespace: false,
                },
                None,
                Credential::Admin(*b"0123456789abcdefghijklmnopqrstuv"),
                token,
            )
            .await;
        });
        tokio::time::sleep(Duration::from_millis(80)).await;
        shutdown.cancel();
        tokio::time::timeout(Duration::from_secs(3), task)
            .await
            .expect("register loop did not stop after cancel")
            .expect("join");
    }
}
