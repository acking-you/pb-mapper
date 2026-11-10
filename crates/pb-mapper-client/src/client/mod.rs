pub mod error;
mod health;
pub mod status;
pub(crate) mod stream;

use crate::diagnostics::Diagnostics;
use crate::endpoint::RelayEndpoint;
use std::sync::{Arc, Once};
use std::time::Duration;

use snafu::ResultExt;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use uni_stream::udp::set_custom_timeout;

use self::error::{AcceptLocalStreamSnafu, BindLocalListenerSnafu};
use self::health::Health;
use self::status::get_status_scoped;
use self::stream::{StreamSetup, handle_local_stream};
use crate::addr::{resolve_all, resolve_tunnel_ends};
use crate::recovery::{SharedTiming, jitter, transport_backoff};
use crate::worker_status::{StatusSink, WorkerStatus, text_sink};
use pb_mapper_core::checksum::{Credential, get_process_credential};
use pb_mapper_core::config::{ResolvedAddrs, StatusOp};
use pb_mapper_protocol::command::PbConnStatusReq;
use pb_mapper_protocol::forward::StreamForward;
use uni_stream::addr::ToSocketAddrs;
use uni_stream::stream::{ListenerProvider, StreamAccept};

/// Local streams of one connect tunnel in setup at once. More wait in the
/// listener's backlog; a stream that starts forwarding gives its slot back.
const LOCAL_SETUPS: usize = 64;

pub use crate::worker_status::StatusCallback as ClientStatusCallback;

/// Resolve both ends, reporting a bad address through `status`.
///
/// The status is the only channel a caller has here — these entry points return
/// `()` — so an address that never resolves has to settle the tunnel as failed
/// rather than just leaving a log line behind.
async fn resolve_ends_or_fail<A: ToSocketAddrs>(
    local_addr: A,
    remote_addr: A,
    status: Option<&StatusSink>,
) -> Option<(ResolvedAddrs, ResolvedAddrs)> {
    let resolved = resolve_tunnel_ends(local_addr, remote_addr).await;
    if let (None, Some(sink)) = (&resolved, status) {
        sink(&WorkerStatus::Failed(None));
    }
    resolved
}

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

pub async fn run_client_side_cli_with_callback<LocalListener: ListenerProvider, A: ToSocketAddrs>(
    local_addr: A,
    remote_addr: A,
    key: Arc<str>,
    keep_alive: bool,
    status_callback: Option<ClientStatusCallback>,
) where
    <LocalListener::Listener as StreamAccept>::Item: StreamForward,
{
    run_client_side_cli_with_callback_scoped::<LocalListener, A>(
        local_addr,
        remote_addr,
        key,
        keep_alive,
        None,
        status_callback,
        None,
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
    run_client_side_cli_with_callback_scoped::<LocalListener, A>(
        local_addr,
        remote_addr,
        key,
        keep_alive,
        None,
        status_callback,
        Some(credential),
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
    let status = status_callback.map(text_sink);
    let Some((local_addr, remote_addr)) =
        resolve_ends_or_fail(local_addr, remote_addr, status.as_ref()).await
    else {
        return;
    };
    run_client_side_cli_loop::<LocalListener>(
        local_addr,
        remote_addr,
        key,
        keep_alive,
        namespace,
        status,
        pinned_credential,
        CancellationToken::new(),
    )
    .await
}

/// Same as [`run_client_side_cli_with_callback_scoped`], but the retry loop
/// returns when `shutdown` is cancelled.
///
/// Takes both ends already resolved, because its caller — the SDK — resolves them
/// itself in order to report a bad address as an error rather than a log line.
#[expect(clippy::too_many_arguments, reason = "a published signature")]
pub async fn run_client_side_cli_with_shutdown<LocalListener: ListenerProvider>(
    local_addr: ResolvedAddrs,
    remote_addr: ResolvedAddrs,
    key: Arc<str>,
    keep_alive: bool,
    namespace: Option<u64>,
    status_callback: Option<ClientStatusCallback>,
    pinned_credential: Option<pb_mapper_core::checksum::Credential>,
    shutdown: CancellationToken,
) where
    <LocalListener::Listener as StreamAccept>::Item: StreamForward,
{
    run_client_side_cli_loop::<LocalListener>(
        local_addr,
        remote_addr,
        key,
        keep_alive,
        namespace,
        status_callback.map(text_sink),
        pinned_credential,
        shutdown,
    )
    .await
}

#[expect(
    clippy::too_many_arguments,
    reason = "takes what both relay-only entry points pass through"
)]
async fn run_client_side_cli_loop<LocalListener: ListenerProvider>(
    local_addr: ResolvedAddrs,
    remote_addr: ResolvedAddrs,
    key: Arc<str>,
    keep_alive: bool,
    namespace: Option<u64>,
    status: Option<StatusSink>,
    pinned_credential: Option<pb_mapper_core::checksum::Credential>,
    shutdown: CancellationToken,
) where
    <LocalListener::Listener as StreamAccept>::Item: StreamForward,
{
    run_client_side_cli_recovering_with_path::<LocalListener>(
        local_addr,
        RelayEndpoint::fixed(remote_addr),
        key,
        keep_alive,
        namespace,
        status,
        pinned_credential,
        shutdown,
        Diagnostics::default(),
        crate::sdk::PathPolicy::RelayOnly,
        false,
        crate::sdk::DirectPolicy::RELAY,
    )
    .await;
}

#[expect(
    clippy::too_many_arguments,
    reason = "the SDK starts a connection with each of its settings"
)]
pub(crate) async fn run_client_side_cli_recovering_with_path<LocalListener: ListenerProvider>(
    local_addr: ResolvedAddrs,
    remote_addr: RelayEndpoint,
    key: Arc<str>,
    keep_alive: bool,
    namespace: Option<u64>,
    status: Option<StatusSink>,
    pinned_credential: Option<Credential>,
    shutdown: CancellationToken,
    diagnostics: Diagnostics,
    path: crate::sdk::PathPolicy,
    is_datagram: bool,
    policy: crate::sdk::DirectPolicy,
) where
    <LocalListener::Listener as StreamAccept>::Item: StreamForward,
{
    remote_addr.start();
    let mut wake = remote_addr.wake();
    diagnostics.network_seen(remote_addr.network_generation());
    // uni-stream keeps its UDP session timeout in a `static mut` that it reads
    // once. Write it once per process, so tunnels starting together never
    // write it at the same time; every write was this same value.
    static UDP_TIMEOUT: Once = Once::new();
    UDP_TIMEOUT.call_once(|| set_custom_timeout(Duration::from_secs(120)));

    let credential = match pinned_credential {
        Some(credential) => credential,
        None => match get_process_credential() {
            Ok(credential) => credential,
            Err(e) => {
                tracing::error!("load client credential failed: {e}");
                if let Some(sink) = &status {
                    sink(&WorkerStatus::Failed(None));
                }
                return;
            }
        },
    };

    #[cfg(feature = "p2p")]
    let direct = (path == crate::sdk::PathPolicy::Auto).then(|| {
        crate::direct::Consumer::new(
            remote_addr.clone(),
            credential,
            namespace,
            is_datagram,
            keep_alive,
            diagnostics.clone(),
            policy,
        )
    });
    #[cfg(not(feature = "p2p"))]
    let _ = (path, is_datagram, policy);
    let mut retry_backoff = transport_backoff();
    let mut stream_tasks = JoinSet::new();
    let setup_slots = Arc::new(Semaphore::new(LOCAL_SETUPS));
    let timing = Arc::new(SharedTiming::default());

    'outer: loop {
        // Listener lifetime follows the local endpoint, not a remote health
        // snapshot. Only a local bind/accept error requires replacing it.
        let listener = tokio::select! {
            () = shutdown.cancelled() => break,
            result = LocalListener::bind(local_addr.as_slice()) => match result.context(BindLocalListenerSnafu) {
                Ok(listener) => listener,
                Err(error) => {
                    tracing::warn!(event = "client_local_bind_failed", %key, %local_addr, %error);
                    if let Some(sink) = &status { sink(&WorkerStatus::Retrying); }
                    tokio::select! {
                        () = shutdown.cancelled() => break,
                        () = tokio::time::sleep(jitter(retry_backoff.next_delay())) => {}
                    }
                    continue;
                }
            }
        };
        tracing::info!(event = "client_local_listener_bound", %key, %local_addr, "local listener bound; checking remote service");
        let mut health = Health {
            remote_addr: &remote_addr,
            key: &key,
            namespace,
            credential,
            diagnostics: &diagnostics,
            status: status.as_ref(),
            timing: &timing,
            retry_backoff: &mut retry_backoff,
            probes: JoinSet::new(),
            next_probe: Instant::now(),
            last_success: None,
            connected: false,
            consecutive_health_failures: 0,
        };
        let (stream_event_tx, mut stream_event_rx) = tokio::sync::mpsc::channel(1);

        loop {
            tokio::select! {
                () = shutdown.cancelled() => break 'outer,
                woke = wake.changed() => {
                    if let crate::endpoint::Wake::Network(generation) = woke {
                        diagnostics.network_changed(generation);
                        #[cfg(feature = "p2p")]
                        if let Some(direct) = &direct { direct.invalidate(); }
                    }
                    health.recheck_now();
                },
                accepted = async {
                    // Backpressure stays in the listener backlog while setup is
                    // saturated. Established forwarding releases its permit.
                    let permit = setup_slots.clone().acquire_owned().await;
                    (permit, listener.accept().await)
                } => {
                    let (Ok(permit), accepted) = accepted else { break 'outer; };
                    let (stream, _) = match accepted.context(AcceptLocalStreamSnafu) {
                        Ok(accepted) => accepted,
                        Err(error) => {
                            tracing::warn!(event = "client_local_accept_failed", %key, %local_addr, %error);
                            break;
                        }
                    };
                    let stream_key = key.clone();
                    let stream_remote = remote_addr.clone();
                    let stream_shutdown = shutdown.clone();
                    let event_tx = stream_event_tx.clone();
                    let setup = StreamSetup { permit, timing: timing.clone(), events: event_tx.clone(), diagnostics: diagnostics.clone(), #[cfg(feature = "p2p")] direct: direct.clone() };
                    stream_tasks.spawn(async move {
                        let result = tokio::select! {
                            () = stream_shutdown.cancelled() => return,
                            result = handle_local_stream(stream, stream_key, stream_remote, keep_alive, namespace, credential, setup) => result,
                        };
                        if let Err(error) = result {
                            tracing::warn!(event = "client_local_stream_failed_before_forward", reason = %snafu::Report::from_error(error));
                            let _ = event_tx.try_send(false);
                        }
                    });
                }
                () = tokio::time::sleep_until(health.next_probe), if health.probes.is_empty() => {
                    health.spawn_probe();
                }
                Some(result) = health.probes.join_next() => {
                    if health.on_probe_result(result).is_break() {
                        break 'outer;
                    }
                }
                Some(success) = stream_event_rx.recv() => health.on_stream_event(success),
                Some(_) = stream_tasks.join_next() => {}
            }
        }
        drop(health);
        if let Some(sink) = &status {
            sink(&WorkerStatus::Retrying);
        }
        tokio::select! {
            () = shutdown.cancelled() => break,
            () = tokio::time::sleep(jitter(retry_backoff.next_delay())) => {}
        }
    }

    // Wait for the forwarding sessions to observe the cancellation, so returning
    // from here means no stream of this tunnel is still moving bytes.
    stream_tasks.shutdown().await;
    #[cfg(feature = "p2p")]
    if let Some(direct) = direct {
        direct.stop().await;
    }
}

pub async fn handle_status_cli_scoped<A: ToSocketAddrs>(
    op: StatusOp,
    addr: A,
    namespace: Option<u64>,
) -> Result<(), Box<dyn std::error::Error>> {
    match op {
        StatusOp::Sessions => show_status_scoped(addr, PbConnStatusReq::Sessions, namespace).await,
        StatusOp::RemoteId => show_status_scoped(addr, PbConnStatusReq::RemoteId, namespace).await,
        StatusOp::Keys => show_status_scoped(addr, PbConnStatusReq::Keys, namespace).await,
    }
}

pub async fn show_status_scoped<A: ToSocketAddrs>(
    remote_addr: A,
    req: PbConnStatusReq,
    namespace: Option<u64>,
) -> Result<(), Box<dyn std::error::Error>> {
    let remote_addr = resolve_all(remote_addr).await?;
    let mut stream = crate::addr::connect_tcp(&remote_addr)
        .await
        .map_err(|error| format!("get status stream: {error}"))?;
    let status = get_status_scoped(&mut stream, req, namespace).await?;
    let status = serde_json::to_string_pretty(&status)?;
    println!("Status:{status}");
    Ok(())
}

#[cfg(test)]
mod cancellation_audit {
    use std::{
        future::Future,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        task::{Context, Wake, Waker},
        time::Duration,
    };
    struct Notified(AtomicBool);
    impl Wake for Notified {
        fn wake(self: Arc<Self>) {
            self.0.store(true, Ordering::SeqCst);
        }
        fn wake_by_ref(self: &Arc<Self>) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    #[tokio::test]
    async fn cancelled_udp_accept_must_preserve_the_delivered_peer() {
        let listener = uni_stream::udp::UdpListener::bind("127.0.0.1:0")
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let client = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let notified = Arc::new(Notified(AtomicBool::new(false)));
        let waker = Waker::from(notified.clone());
        let mut accept = Box::pin(listener.accept());
        assert!(
            accept
                .as_mut()
                .poll(&mut Context::from_waker(&waker))
                .is_pending()
        );
        client.send_to(b"first datagram", addr).await.unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            while !notified.0.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        drop(accept);
        let delivered = tokio::time::timeout(Duration::from_millis(100), listener.accept()).await;
        assert!(
            delivered.is_ok(),
            "UDP peer/first datagram disappeared when a competing select branch cancelled accept"
        );
    }
}
