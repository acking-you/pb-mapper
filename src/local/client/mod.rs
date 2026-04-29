pub mod error;
pub mod status;
mod stream;

use std::fmt::Debug;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use snafu::ResultExt;
use tokio::net::TcpStream;
use tokio::time::MissedTickBehavior;
use uni_stream::udp::set_custom_timeout;

use self::error::{AcceptLocalStreamSnafu, BindLocalListenerSnafu};
use self::status::get_status;
use self::stream::handle_local_stream;
use crate::common::config::{client_health_check_interval, client_health_check_timeout, StatusOp};
use crate::common::message::command::{PbConnStatusReq, PbConnStatusResp};
use crate::common::message::forward::StreamForward;
use crate::snafu_error_get_or_return;
use crate::utils::timeout::RetryBackoff;
use uni_stream::addr::{each_addr, ToSocketAddrs};
use uni_stream::stream::got_one_socket_addr;
use uni_stream::stream::{ListenerProvider, StreamAccept};

// Callback for notifying status changes to external systems
pub type ClientStatusCallback = Box<dyn Fn(&str) + Send + Sync>;

pub async fn run_client_side_cli<LocalListener: ListenerProvider, A: ToSocketAddrs>(
    local_addr: A,
    remote_addr: A,
    key: Arc<str>,
) where
    <LocalListener::Listener as StreamAccept>::Item: StreamForward,
{
    run_client_side_cli_with_callback::<LocalListener, A>(local_addr, remote_addr, key, None).await
}

pub async fn run_client_side_cli_with_callback<LocalListener: ListenerProvider, A: ToSocketAddrs>(
    local_addr: A,
    remote_addr: A,
    key: Arc<str>,
    status_callback: Option<ClientStatusCallback>,
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

    let mut retry_backoff = RetryBackoff::default();

    loop {
        tracing::debug!(
            event = "client_probe_start",
            key = %key,
            local_addr = %local_addr,
            remote_addr = %remote_addr,
            retry_count = retry_backoff.failures(),
            "client probing remote server"
        );

        if let Err(reason) = probe_remote_key(remote_addr, key.as_ref()).await {
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
            tokio::time::sleep(retry_delay).await;
            continue;
        }

        tracing::info!(
            event = "client_key_available",
            key = %key,
            local_addr = %local_addr,
            remote_addr = %remote_addr,
            "remote server key is available; local listener will start"
        );

        if let Some(ref callback) = status_callback {
            callback("connected");
        }

        retry_backoff.reset();

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
                let retry_delay = retry_backoff.next_delay();
                tokio::time::sleep(retry_delay).await;
                continue;
            }
        };

        let (stream_failure_tx, mut stream_failure_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut health_interval = tokio::time::interval(client_health_check_interval());
        health_interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
        health_interval.tick().await;

        loop {
            tokio::select! {
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
                    tokio::spawn(async move {
                        if let Err(e) = handle_local_stream(stream, key, remote_addr).await {
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
                    if let Err(reason) = probe_remote_key(remote_addr, key.as_ref()).await {
                        tracing::warn!(
                            event = "client_remote_health_check_failed",
                            key = %key,
                            local_addr = %local_addr,
                            remote_addr = %remote_addr,
                            reason = %reason,
                            "client remote health check failed; listener will restart"
                        );
                        if let Some(ref callback) = status_callback {
                            callback("retrying");
                        }
                        break;
                    }
                    retry_backoff.reset();
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
                    if let Err(reason) = probe_remote_key(remote_addr, key.as_ref()).await {
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
                }
            }
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
        tokio::time::sleep(retry_delay).await;
    }
}

async fn probe_remote_key(remote_addr: SocketAddr, key: &str) -> std::result::Result<(), String> {
    let timeout = client_health_check_timeout();
    match tokio::time::timeout(timeout, probe_remote_key_once(remote_addr, key)).await {
        Ok(result) => result,
        Err(_) => Err(format!("remote key probe timed out after {timeout:?}")),
    }
}

async fn probe_remote_key_once(
    remote_addr: SocketAddr,
    key: &str,
) -> std::result::Result<(), String> {
    match fetch_remote_status(
        remote_addr,
        PbConnStatusReq::Service {
            key: key.to_string(),
        },
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

    let status_resp = fetch_remote_status(remote_addr, PbConnStatusReq::Keys).await?;
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
) -> std::result::Result<PbConnStatusResp, String> {
    let mut stream = each_addr(remote_addr, TcpStream::connect)
        .await
        .map_err(|e| format!("connect remote stream failed: {e}"))?;
    get_status(&mut stream, req)
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
    match op {
        StatusOp::RemoteId => show_status(addr, PbConnStatusReq::RemoteId).await,
        StatusOp::Keys => show_status(addr, PbConnStatusReq::Keys).await,
    }
}
