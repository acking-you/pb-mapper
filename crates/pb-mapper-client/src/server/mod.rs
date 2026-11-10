//! The register side of a tunnel: a pool of control connections that keep a
//! local service registered with the relay, and a forwarded session for each
//! stream the relay asks for.

mod attempt;
mod control;
pub mod error;
mod probe;
mod status;
mod stream;
mod worker;

use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::addr::resolve_tunnel_ends;
use crate::diagnostics::Diagnostics;
use crate::endpoint::RelayEndpoint;
use pb_mapper_core::checksum::Credential;
use pb_mapper_core::config::ResolvedAddrs;
use pb_mapper_protocol::forward::StreamForward;
use uni_stream::addr::ToSocketAddrs;
use uni_stream::stream::StreamProvider;

#[cfg(feature = "p2p")]
pub(crate) use self::control::LocalControlWrite;
pub(crate) use self::worker::run_server_side_cli_recovering;
pub use crate::worker_status::StatusCallback;
use crate::worker_status::text_sink;

/// How a local-server tunnel forwards and where it registers: the codec, the
/// transport, whether its sockets keep alive, and its namespace.
///
/// Grouped rather than passed as adjacent `bool`s, which is a transposition
/// waiting to happen at a call site — and named fields make the call sites say
/// which is which.
#[derive(Clone, Copy, Debug)]
pub struct ServerTunnelOptions {
    pub need_codec: bool,
    pub is_datagram: bool,
    pub keep_alive: bool,
    pub namespace: Option<u64>,
    pub force_namespace: bool,
}

pub async fn run_server_side_cli<LocalStream, A>(
    local_addr: A,
    remote_addr: A,
    key: Arc<str>,
    options: ServerTunnelOptions,
) where
    LocalStream: StreamProvider + Send + 'static,
    LocalStream::Item: StreamForward,
    A: ToSocketAddrs,
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
    A: ToSocketAddrs,
{
    let Some((local_addr, remote_addr)) = resolve_tunnel_ends(local_addr, remote_addr).await else {
        return;
    };
    run_server_side_cli_pool::<LocalStream>(
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
///
/// Takes both ends already resolved, because its caller — the SDK — resolves them
/// itself in order to report a bad address as an error rather than a log line.
pub async fn run_server_side_cli_with_shutdown<LocalStream>(
    local_addr: ResolvedAddrs,
    remote_addr: ResolvedAddrs,
    key: Arc<str>,
    options: ServerTunnelOptions,
    status_callback: Option<StatusCallback>,
    credential: Credential,
    shutdown: CancellationToken,
) where
    LocalStream: StreamProvider + Send + 'static,
    LocalStream::Item: StreamForward,
{
    run_server_side_cli_pool::<LocalStream>(
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
    A: ToSocketAddrs,
{
    let Some((local_addr, remote_addr)) = resolve_tunnel_ends(local_addr, remote_addr).await else {
        return;
    };
    run_server_side_cli_pool::<LocalStream>(
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

async fn run_server_side_cli_pool<LocalStream>(
    local_addr: ResolvedAddrs,
    remote_addr: ResolvedAddrs,
    key: Arc<str>,
    options: ServerTunnelOptions,
    status_callback: Option<StatusCallback>,
    pinned_credential: Option<Credential>,
    shutdown: CancellationToken,
) where
    LocalStream: StreamProvider + Send + 'static,
    LocalStream::Item: StreamForward,
{
    run_server_side_cli_recovering::<LocalStream>(
        local_addr,
        RelayEndpoint::fixed(remote_addr),
        key,
        options,
        status_callback.map(text_sink),
        pinned_credential,
        shutdown,
        Diagnostics::default(),
        None,
        false,
        crate::sdk::DirectPolicy::RELAY,
    )
    .await;
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
        let addr = ResolvedAddrs::from("127.0.0.1:1".parse::<std::net::SocketAddr>().unwrap());
        let task = tokio::spawn(async move {
            run_server_side_cli_with_shutdown::<TcpStreamProvider>(
                addr.clone(),
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
        // Let the pool start its workers, so cancelling stops running ones.
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        shutdown.cancel();
        tokio::time::timeout(Duration::from_secs(3), task)
            .await
            .expect("register loop did not stop after cancel")
            .expect("join");
    }
}
