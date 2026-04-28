pub mod error;
pub mod status;
mod stream;

use std::fmt::Debug;
use std::sync::Arc;

use snafu::ResultExt;
use tokio::net::TcpStream;
use uni_stream::udp::set_custom_timeout;

use self::error::{AcceptLocalStreamSnafu, BindLocalListenerSnafu};
use self::status::get_status;
use self::stream::handle_local_stream;
use crate::common::config::StatusOp;
use crate::common::message::command::{PbConnStatusReq, PbConnStatusResp};
use crate::common::message::forward::StreamForward;
use crate::{snafu_error_get_or_return, snafu_error_handle};
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
    use crate::utils::timeout::TimeoutCount;
    use std::time::Duration;

    const CLIENT_RETRY_TIMES: u32 = 8;

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

    let mut timeout_count = TimeoutCount::new(CLIENT_RETRY_TIMES);

    loop {
        tracing::debug!(
            event = "client_probe_start",
            key = %key,
            local_addr = %local_addr,
            remote_addr = %remote_addr,
            retry_count = timeout_count.count(),
            "client probing remote server"
        );
        // Notify that we're trying to connect
        if let Some(ref callback) = status_callback {
            if timeout_count.count() > 0 {
                callback("retrying");
            }
        }

        let mut stream = match each_addr(remote_addr, TcpStream::connect).await {
            Ok(stream) => stream,
            Err(e) => {
                tracing::error!(
                    event = "client_remote_connect_failed",
                    key = %key,
                    remote_addr = %remote_addr,
                    error = %e,
                    "failed to connect to remote server"
                );
                if !timeout_count.validate() {
                    if let Some(ref callback) = status_callback {
                        callback("failed");
                    }
                    return;
                }
                let interval = timeout_count.get_interval_by_count();
                tokio::time::sleep(Duration::from_secs(interval)).await;
                continue;
            }
        };

        let status_resp = match get_status(&mut stream, PbConnStatusReq::Keys).await {
            Ok(resp) => resp,
            Err(e) => {
                tracing::error!(
                    event = "client_status_probe_failed",
                    key = %key,
                    remote_addr = %remote_addr,
                    error = %e,
                    "failed to get status from server"
                );
                if !timeout_count.validate() {
                    if let Some(ref callback) = status_callback {
                        callback("failed");
                    }
                    return;
                }
                let interval = timeout_count.get_interval_by_count();
                tokio::time::sleep(Duration::from_secs(interval)).await;
                continue;
            }
        };

        let keys = if let PbConnStatusResp::Keys(keys) = &status_resp {
            keys
        } else {
            tracing::error!(
                event = "client_status_unexpected_response",
                key = %key,
                remote_addr = %remote_addr,
                response = ?status_resp,
                "expected keys status response"
            );
            if !timeout_count.validate() {
                if let Some(ref callback) = status_callback {
                    callback("failed");
                }
                return;
            }
            let interval = timeout_count.get_interval_by_count();
            tokio::time::sleep(Duration::from_secs(interval)).await;
            continue;
        };

        if !keys.iter().any(|k| k == key.as_ref()) {
            tracing::error!(
                event = "client_key_not_registered",
                key = %key,
                remote_addr = %remote_addr,
                valid_keys = ?keys,
                "client key is not registered on remote server"
            );
            if !timeout_count.validate() {
                if let Some(ref callback) = status_callback {
                    callback("failed");
                }
                return;
            }
            let interval = timeout_count.get_interval_by_count();
            tokio::time::sleep(Duration::from_secs(interval)).await;
            continue;
        }

        drop(status_resp);
        // The status probe is one-shot. Close it before the long-lived listener starts,
        // otherwise the server closes its side while this process keeps a stale fd open.
        drop(stream);
        tracing::info!(
            event = "client_key_available",
            key = %key,
            local_addr = %local_addr,
            remote_addr = %remote_addr,
            "remote server key is available; local listener will start"
        );

        // Notify successful connection
        if let Some(ref callback) = status_callback {
            callback("connected");
        }

        // Reset timeout count after successful connection
        timeout_count.reset();

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
                if !timeout_count.validate() {
                    if let Some(ref callback) = status_callback {
                        callback("failed");
                    }
                    return;
                }
                let interval = timeout_count.get_interval_by_count();
                tokio::time::sleep(Duration::from_secs(interval)).await;
                continue;
            }
        };

        loop {
            let (stream, peer_addr) = match listener.accept().await.context(AcceptLocalStreamSnafu)
            {
                Ok(result) => result,
                Err(e) => {
                    tracing::error!(
                        event = "client_local_accept_failed",
                        key = %key,
                        local_addr = %local_addr,
                        error = %e,
                        "failed to accept local stream"
                    );
                    break; // Break inner loop to retry connection
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
            tokio::spawn(async move {
                snafu_error_handle!(handle_local_stream(stream, key, remote_addr).await);
            });
        }

        // If we reach here, the inner loop broke due to error
        if !timeout_count.validate() {
            if let Some(ref callback) = status_callback {
                callback("failed");
            }
            return;
        }

        let interval = timeout_count.get_interval_by_count();
        tokio::time::sleep(Duration::from_secs(interval)).await;
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
