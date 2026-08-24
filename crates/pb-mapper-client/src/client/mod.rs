pub mod error;
pub mod status;
mod stream;

use std::fmt::Debug;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use snafu::ResultExt;
use tokio::net::TcpStream;
use tokio::task::JoinSet;
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;
use uni_stream::udp::set_custom_timeout;

use self::error::{AcceptLocalStreamSnafu, BindLocalListenerSnafu};
use self::status::{get_status, get_status_scoped, get_status_with_credential};
use self::stream::handle_local_stream;
use pb_mapper_core::checksum::{Credential, get_process_credential};
use pb_mapper_core::config::{
    StatusOp, client_health_check_interval, client_health_check_timeout,
    client_health_failure_threshold,
};
use pb_mapper_core::snafu_error_get_or_return;
use pb_mapper_core::timeout::RetryBackoff;
use pb_mapper_protocol::command::{PbConnStatusReq, PbConnStatusResp};
use pb_mapper_protocol::forward::StreamForward;
use uni_stream::addr::{ToSocketAddrs, each_addr};
use uni_stream::stream::got_one_socket_addr;
use uni_stream::stream::{ListenerProvider, StreamAccept};

// Callback for notifying status changes to external systems
pub type ClientStatusCallback = Box<dyn Fn(&str) + Send + Sync>;

pub async fn run_client_side_cli<LocalListener: ListenerProvider, A: ToSocketAddrs>(
    local_addr: A,
    remote_addr: A,
    key: Arc<str>,
    keep_alive: bool,
) where
    <LocalListener::Listener as StreamAccept>::Item: StreamForward,
{
    run_client_side_cli_with_callback::<LocalListener, A>(
        local_addr,
        remote_addr,
        key,
        keep_alive,
        None,
    )
    .await
}

pub async fn run_client_side_cli_scoped<LocalListener: ListenerProvider, A: ToSocketAddrs>(
    local_addr: A,
    remote_addr: A,
    key: Arc<str>,
    keep_alive: bool,
    namespace: Option<u64>,
) where
    <LocalListener::Listener as StreamAccept>::Item: StreamForward,
{
    run_client_side_cli_loop::<LocalListener, A>(
        local_addr,
        remote_addr,
        key,
        keep_alive,
        namespace,
        None,
        None,
        CancellationToken::new(),
    )
    .await
}

pub async fn run_client_side_cli_with_callback<LocalListener: ListenerProvider, A: ToSocketAddrs>(
    local_addr: A,
    remote_addr: A,
    key: Arc<str>,
    keep_alive: bool,
    status_callback: Option<ClientStatusCallback>,
) where
    <LocalListener::Listener as StreamAccept>::Item: StreamForward,
{
    run_client_side_cli_loop::<LocalListener, A>(
        local_addr,
        remote_addr,
        key,
        keep_alive,
        None,
        status_callback,
        None,
        CancellationToken::new(),
    )
    .await
}

pub async fn run_client_side_cli_with_pinned_credential<
    LocalListener: ListenerProvider,
    A: ToSocketAddrs,
>(
    local_addr: A,
    remote_addr: A,
    key: Arc<str>,
    keep_alive: bool,
    status_callback: Option<ClientStatusCallback>,
    credential: pb_mapper_core::checksum::Credential,
) where
    <LocalListener::Listener as StreamAccept>::Item: StreamForward,
{
    run_client_side_cli_loop::<LocalListener, A>(
        local_addr,
        remote_addr,
        key,
        keep_alive,
        None,
        status_callback,
        Some(credential),
        CancellationToken::new(),
    )
    .await
}

pub async fn run_client_side_cli_with_callback_scoped<
    LocalListener: ListenerProvider,
    A: ToSocketAddrs,
>(
    local_addr: A,
    remote_addr: A,
    key: Arc<str>,
    keep_alive: bool,
    namespace: Option<u64>,
    status_callback: Option<ClientStatusCallback>,
    pinned_credential: Option<pb_mapper_core::checksum::Credential>,
) where
    <LocalListener::Listener as StreamAccept>::Item: StreamForward,
{
    run_client_side_cli_loop::<LocalListener, A>(
        local_addr,
        remote_addr,
        key,
        keep_alive,
        namespace,
        status_callback,
        pinned_credential,
        CancellationToken::new(),
    )
    .await
}

/// Same as [`run_client_side_cli_with_callback_scoped`], but the retry loop
/// returns when `shutdown` is cancelled.
#[allow(clippy::too_many_arguments)]
pub async fn run_client_side_cli_with_shutdown<LocalListener: ListenerProvider, A: ToSocketAddrs>(
    local_addr: A,
    remote_addr: A,
    key: Arc<str>,
    keep_alive: bool,
    namespace: Option<u64>,
    status_callback: Option<ClientStatusCallback>,
    pinned_credential: Option<pb_mapper_core::checksum::Credential>,
    shutdown: CancellationToken,
) where
    <LocalListener::Listener as StreamAccept>::Item: StreamForward,
{
    run_client_side_cli_loop::<LocalListener, A>(
        local_addr,
        remote_addr,
        key,
        keep_alive,
        namespace,
        status_callback,
        pinned_credential,
        shutdown,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn run_client_side_cli_loop<LocalListener: ListenerProvider, A: ToSocketAddrs>(
    local_addr: A,
    remote_addr: A,
    key: Arc<str>,
    keep_alive: bool,
    namespace: Option<u64>,
    status_callback: Option<ClientStatusCallback>,
    pinned_credential: Option<pb_mapper_core::checksum::Credential>,
    shutdown: CancellationToken,
) where
    <LocalListener::Listener as StreamAccept>::Item: StreamForward,
{
    set_custom_timeout(Duration::from_secs(120));

    let local_addr = match got_one_socket_addr(local_addr).await {
        Ok(addr) => addr,
        Err(e) => {
            tracing::error!("parse local addr failed: {e}");
            if let Some(ref callback) = status_callback {
                callback("failed");
            }
            return;
        }
    };
    let remote_addr = match got_one_socket_addr(remote_addr).await {
        Ok(addr) => addr,
        Err(e) => {
            tracing::error!("parse remote addr failed: {e}");
            if let Some(ref callback) = status_callback {
                callback("failed");
            }
            return;
        }
    };
    let credential = match pinned_credential {
        Some(credential) => credential,
        None => match get_process_credential() {
            Ok(credential) => credential,
            Err(e) => {
                tracing::error!("load client credential failed: {e}");
                if let Some(ref callback) = status_callback {
                    callback("failed");
                }
                return;
            }
        },
    };

    let mut retry_backoff = RetryBackoff::default();
    // Accepted local streams are tracked rather than detached, so a cancelled
    // tunnel takes its in-flight forwarding sessions down with it instead of
    // leaving them forwarding after `stop()` has returned. The set is declared
    // outside the loop: a listener restart is not a reason to drop live sessions.
    let mut stream_tasks = JoinSet::new();

    'outer: loop {
        if shutdown.is_cancelled() {
            break 'outer;
        }
        tracing::debug!(
            event = "client_probe_start",
            key = %key,
            local_addr = %local_addr,
            remote_addr = %remote_addr,
            retry_count = retry_backoff.failures(),
            "client probing remote server"
        );

        if let Err(reason) =
            probe_remote_key(remote_addr, key.as_ref(), namespace, credential).await
        {
            let retry_delay = retry_backoff.next_delay();
            tracing::warn!(
                event = "client_remote_probe_failed",
                key = %key,
                local_addr = %local_addr,
                remote_addr = %remote_addr,
                reason = %reason,
                retry_delay = ?retry_delay,
                retry_count = retry_backoff.failures(),
                "client remote probe failed; retrying"
            );
            if let Some(ref callback) = status_callback {
                callback("retrying");
            }
            tokio::select! {
                () = shutdown.cancelled() => break 'outer,
                () = tokio::time::sleep(retry_delay) => {}
            }
            continue;
        }

        tracing::info!(
            event = "client_key_available",
            key = %key,
            local_addr = %local_addr,
            remote_addr = %remote_addr,
            "remote server key is available; local listener will start"
        );

        retry_backoff.reset();

        // The listener binds before "connected" is reported: that status is what
        // drives readiness for external callers, and a caller told the tunnel is
        // up must be able to reach the local endpoint. Reporting it on the remote
        // probe alone would call an occupied local address ready.
        let listener = match LocalListener::bind(local_addr)
            .await
            .context(BindLocalListenerSnafu)
        {
            Ok(listener) => listener,
            Err(e) => {
                tracing::error!(
                    event = "client_local_bind_failed",
                    key = %key,
                    local_addr = %local_addr,
                    error = %e,
                    "failed to bind local listener"
                );
                if let Some(ref callback) = status_callback {
                    callback("retrying");
                }
                let retry_delay = retry_backoff.next_delay();
                tokio::select! {
                    () = shutdown.cancelled() => break 'outer,
                    () = tokio::time::sleep(retry_delay) => {}
                }
                continue;
            }
        };

        tracing::info!(
            event = "client_local_listener_bound",
            key = %key,
            local_addr = %local_addr,
            remote_addr = %remote_addr,
            "local listener bound; tunnel is ready"
        );

        if let Some(ref callback) = status_callback {
            callback("connected");
        }

        let (stream_failure_tx, mut stream_failure_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut health_interval = tokio::time::interval(client_health_check_interval());
        let health_failure_threshold = client_health_failure_threshold();
        let mut consecutive_health_failures = 0usize;
        health_interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
        health_interval.tick().await;

        loop {
            tokio::select! {
                () = shutdown.cancelled() => {
                    tracing::info!(
                        event = "client_listener_cancelled",
                        key = %key,
                        local_addr = %local_addr,
                        "client listener loop cancelled"
                    );
                    break 'outer;
                }
                accepted = listener.accept() => {
                    let (stream, peer_addr) = match accepted.context(AcceptLocalStreamSnafu) {
                        Ok(result) => result,
                        Err(e) => {
                            tracing::error!(
                                event = "client_local_accept_failed",
                                key = %key,
                                local_addr = %local_addr,
                                error = %e,
                                "failed to accept local stream"
                            );
                            break;
                        }
                    };
                    tracing::debug!(
                        event = "client_local_stream_accepted",
                        key = %key,
                        local_addr = %local_addr,
                        peer_addr = ?peer_addr,
                        "accepted local client stream"
                    );
                    let key = key.clone();
                    let failure_tx = stream_failure_tx.clone();
                    let stream_shutdown = shutdown.clone();
                    stream_tasks.spawn(async move {
                        let forward = handle_local_stream(stream, key, remote_addr, keep_alive, namespace, credential);
                        let forward = tokio::select! {
                            () = stream_shutdown.cancelled() => return,
                            result = forward => result,
                        };
                        if let Err(e) = forward
                        {
                            let reason = snafu::Report::from_error(e).to_string();
                            tracing::warn!(
                                event = "client_local_stream_failed_before_forward",
                                remote_addr = %remote_addr,
                                reason = %reason,
                                "local client stream failed before forwarding started"
                            );
                            let _ = failure_tx.send(reason);
                        }
                    });
                }
                _ = health_interval.tick() => {
                    if let Err(reason) = probe_remote_key(remote_addr, key.as_ref(), namespace, credential).await {
                        consecutive_health_failures = consecutive_health_failures.saturating_add(1);
                        if consecutive_health_failures < health_failure_threshold {
                            tracing::warn!(
                                event = "client_remote_health_check_missed",
                                key = %key,
                                local_addr = %local_addr,
                                remote_addr = %remote_addr,
                                reason = %reason,
                                consecutive_failures = consecutive_health_failures,
                                failure_threshold = health_failure_threshold,
                                "client remote health check failed; listener remains active"
                            );
                            continue;
                        }
                        tracing::warn!(
                            event = "client_remote_health_check_failed",
                            key = %key,
                            local_addr = %local_addr,
                            remote_addr = %remote_addr,
                            reason = %reason,
                            consecutive_failures = consecutive_health_failures,
                            failure_threshold = health_failure_threshold,
                            "client remote health checks failed repeatedly; listener will restart"
                        );
                        if let Some(ref callback) = status_callback {
                            callback("retrying");
                        }
                        break;
                    }
                    consecutive_health_failures = 0;
                    retry_backoff.reset();
                }
                Some(_) = stream_tasks.join_next() => {
                    // Reap finished sessions so the set does not grow for the
                    // lifetime of the process. Failures are already reported
                    // through `stream_failure_tx`.
                }
                Some(stream_failure) = stream_failure_rx.recv() => {
                    tracing::warn!(
                        event = "client_stream_failure_reported",
                        key = %key,
                        local_addr = %local_addr,
                        remote_addr = %remote_addr,
                        stream_failure = %stream_failure,
                        "local stream failure reported; probing remote key"
                    );
                    if let Err(reason) = probe_remote_key(remote_addr, key.as_ref(), namespace, credential).await {
                        tracing::warn!(
                            event = "client_remote_probe_failed_after_stream_error",
                            key = %key,
                            local_addr = %local_addr,
                            remote_addr = %remote_addr,
                            reason = %reason,
                            "remote key probe failed after local stream error; listener will restart"
                        );
                        if let Some(ref callback) = status_callback {
                            callback("retrying");
                        }
                        break;
                    }
                    consecutive_health_failures = 0;
                    retry_backoff.reset();
                }
            }
        }

        if shutdown.is_cancelled() {
            break 'outer;
        }
        let retry_delay = retry_backoff.next_delay();
        tracing::info!(
            event = "client_listener_restart_scheduled",
            key = %key,
            local_addr = %local_addr,
            remote_addr = %remote_addr,
            retry_delay = ?retry_delay,
            retry_count = retry_backoff.failures(),
            "client listener stopped; remote probe will retry"
        );
        tokio::select! {
            () = shutdown.cancelled() => break 'outer,
            () = tokio::time::sleep(retry_delay) => {}
        }
    }

    // Wait for the forwarding sessions to observe the cancellation, so returning
    // from here means no stream of this tunnel is still moving bytes.
    stream_tasks.shutdown().await;
}

async fn probe_remote_key(
    remote_addr: SocketAddr,
    key: &str,
    namespace: Option<u64>,
    credential: Credential,
) -> std::result::Result<(), String> {
    let timeout = client_health_check_timeout();
    match tokio::time::timeout(
        timeout,
        probe_remote_key_once(remote_addr, key, namespace, credential),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => Err(format!("remote key probe timed out after {timeout:?}")),
    }
}

async fn probe_remote_key_once(
    remote_addr: SocketAddr,
    key: &str,
    namespace: Option<u64>,
    credential: Credential,
) -> std::result::Result<(), String> {
    match fetch_remote_status(
        remote_addr,
        PbConnStatusReq::Service {
            key: key.to_string(),
        },
        namespace,
        credential,
    )
    .await
    {
        Ok(PbConnStatusResp::Service { connections, .. }) => {
            if connections.iter().any(|conn| conn.healthy) {
                return Ok(());
            }
            return Err(format!(
                "client key `{key}` has no healthy remote server connections"
            ));
        }
        Ok(status_resp) => {
            return Err(format!(
                "expected service status response, got {status_resp:?}"
            ));
        }
        Err(service_reason) => {
            tracing::debug!(
                event = "client_remote_service_probe_failed",
                key = %key,
                remote_addr = %remote_addr,
                reason = %service_reason,
                "service status probe failed; falling back to key status"
            );
        }
    }

    let status_resp =
        fetch_remote_status(remote_addr, PbConnStatusReq::Keys, namespace, credential).await?;
    let PbConnStatusResp::Keys(keys) = status_resp else {
        return Err(format!(
            "expected keys status response, got {status_resp:?}"
        ));
    };
    if keys.iter().any(|candidate| candidate == key) {
        Ok(())
    } else {
        Err(format!(
            "client key `{key}` is not registered on remote server; valid keys: {keys:?}"
        ))
    }
}

async fn fetch_remote_status(
    remote_addr: SocketAddr,
    req: PbConnStatusReq,
    namespace: Option<u64>,
    credential: Credential,
) -> std::result::Result<PbConnStatusResp, String> {
    let mut stream = each_addr(remote_addr, TcpStream::connect)
        .await
        .map_err(|e| format!("connect remote stream failed: {e}"))?;
    get_status_with_credential(&mut stream, req, namespace, &credential)
        .await
        .map_err(|e| format!("get status failed: {}", snafu::Report::from_error(e)))
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
    if let Err(error) = handle_status_cli_scoped(op, addr, None).await {
        tracing::error!("{error}");
    }
}

pub async fn handle_status_cli_scoped<A: ToSocketAddrs + Debug + Copy + Send + 'static>(
    op: StatusOp,
    addr: A,
    namespace: Option<u64>,
) -> Result<(), Box<dyn std::error::Error>> {
    match op {
        StatusOp::RemoteId => show_status_scoped(addr, PbConnStatusReq::RemoteId, namespace).await,
        StatusOp::Keys => show_status_scoped(addr, PbConnStatusReq::Keys, namespace).await,
    }
}

pub async fn show_status_scoped<A: ToSocketAddrs + Debug + Copy + Send + 'static>(
    remote_addr: A,
    req: PbConnStatusReq,
    namespace: Option<u64>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut stream = each_addr(remote_addr, TcpStream::connect)
        .await
        .map_err(|error| format!("get status stream: {error}"))?;
    let status = get_status_scoped(&mut stream, req, namespace).await?;
    let status = serde_json::to_string_pretty(&status)?;
    println!("Status:{status}");
    Ok(())
}
