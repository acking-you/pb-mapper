//! The control pool and each worker's reconnect loop.

use std::sync::Arc;
use std::time::Duration;

use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use super::ServerTunnelOptions;
use super::attempt::{ServerCliRunConfig, Status, run_server_side_cli_inner};
use super::control::DirectProvider;
use super::status::PoolStatus;
use super::stream::spawn_stream;
use crate::diagnostics::{Diagnostics, RecoveryFailure};
use crate::direct_policy::DirectPolicy;
use crate::endpoint::RelayEndpoint;
use crate::recovery::{REVOKED_DIRECT_RECHECK, RecoveryTiming, jitter, transport_backoff};
use crate::worker_status::{StatusSink, WorkerStatus};
use pb_mapper_core::checksum::{Credential, get_process_credential};
use pb_mapper_core::config::{ResolvedAddrs, control_conn_pool_size, registration_reject_backoff};
use pb_mapper_core::timeout::RetryBackoff;
use pb_mapper_protocol::forward::StreamForward;
use uni_stream::stream::StreamProvider;

/// The two retry ladders a control worker climbs, and the reason they are two.
///
/// A transport failure and a retryable rejection are not the same kind of
/// problem, so they must not share a counter. Sharing one meant a reconnect —
/// which happens on its own schedule — reset the wait a full quota was serving,
/// leaving the worker hammering the relay with a request it had just refused.
#[derive(Debug)]
pub(super) struct ControlBackoff {
    /// Could not reach or hold the relay. Retry quickly; that is how the tunnel
    /// comes back.
    transport: RetryBackoff,
    /// The relay answered and refused, for something that may clear. Retry
    /// slowly; see [`registration_reject_backoff`].
    reject: RetryBackoff,
    pub(super) timing: RecoveryTiming,
}

impl ControlBackoff {
    fn new() -> Self {
        let (reject_min, reject_max) = registration_reject_backoff();
        Self {
            transport: transport_backoff(),
            timing: RecoveryTiming::default(),
            reject: RetryBackoff::new(reject_min, reject_max),
        }
    }

    /// Clear both ladders. Called once a registration is established: it is the
    /// only evidence that either condition has passed.
    pub(super) fn reset(&mut self) {
        self.transport.reset();
        self.reject.reset();
    }
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

#[expect(
    clippy::too_many_arguments,
    reason = "the SDK starts a registration with each of its settings"
)]
pub(crate) async fn run_server_side_cli_recovering<LocalStream>(
    local_addr: ResolvedAddrs,
    remote_addr: RelayEndpoint,
    key: Arc<str>,
    options: ServerTunnelOptions,
    status: Option<StatusSink>,
    pinned_credential: Option<Credential>,
    shutdown: CancellationToken,
    diagnostics: Diagnostics,
    idle_timeout: Option<Duration>,
    direct: Option<DirectPolicy>,
) where
    LocalStream: StreamProvider + Send + 'static,
    LocalStream::Item: StreamForward,
{
    let Some(credential) = resolve_registration_credential(pinned_credential, &shutdown).await
    else {
        return;
    };
    remote_addr.start();
    let pool_size = control_conn_pool_size().max(1);
    tracing::info!(
        event = "local_server_control_pool_starting",
        key = %key,
        pool_size,
        "starting local server control connection pool"
    );
    #[cfg(feature = "p2p")]
    let direct = DirectProvider(direct.map(|policy| {
        crate::direct::Provider::spawn::<LocalStream>(
            local_addr.clone(),
            key.clone(),
            options.is_datagram,
            shutdown.clone(),
            idle_timeout,
            diagnostics.clone(),
            policy,
        )
    }));
    #[cfg(not(feature = "p2p"))]
    let direct = {
        let _ = direct;
        DirectProvider::default()
    };
    let config = ServerCliRunConfig {
        local_addr,
        remote_addr,
        diagnostics,
        key,
        options,
        worker_index: 0,
        credential,
        idle_timeout,
        direct,
        spawn_stream: spawn_stream::<LocalStream>,
    };
    run_control_pool(config, pool_size, status, shutdown).await;
}

/// Run `pool_size` control workers for the registration `config` describes,
/// and return once every one of them has.
///
/// Not generic: the local stream type is reached only through
/// `config.spawn_stream`, so this and everything below it is compiled once.
async fn run_control_pool(
    config: ServerCliRunConfig,
    pool_size: usize,
    status: Option<StatusSink>,
    shutdown: CancellationToken,
) {
    let mut workers = JoinSet::new();
    // Every worker reports here, and the pool's aggregate is what the caller is
    // told: one healthy control connection is a reachable service, whichever
    // worker holds it.
    let pool_status = status.map(|sink| Arc::new(PoolStatus::new(sink, pool_size)));
    for worker_index in 0..pool_size {
        let worker = ServerCliRunConfig {
            worker_index,
            ..config.clone()
        };
        workers.spawn(run_server_side_cli_worker(
            worker,
            pool_status.clone(),
            shutdown.clone(),
        ));
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
    config.direct.stop().await;
}

/// One control worker: hold a registration with the relay, reconnecting on
/// whichever ladder the last attempt's outcome calls for, until it is
/// cancelled or permanently rejected.
async fn run_server_side_cli_worker(
    config: ServerCliRunConfig,
    pool_status: Option<Arc<PoolStatus>>,
    shutdown: CancellationToken,
) {
    let worker_index = config.worker_index;
    let ServerCliRunConfig {
        local_addr,
        remote_addr,
        diagnostics,
        key,
        ..
    } = &config;
    let report = |status: WorkerStatus| {
        if let Some(ref pool_status) = pool_status {
            pool_status.report(worker_index, status);
        }
    };
    let mut backoff = ControlBackoff::new();
    // Forwarded sessions outlive a single control connection, so the set lives
    // here rather than inside the attempt: a reconnect is not a reason to drop
    // streams that are still healthy. It is drained before this worker returns.
    let mut stream_tasks = JoinSet::new();
    let setup_slots = remote_addr.data_slots();
    let mut wake = remote_addr.wake();
    diagnostics.network_seen(remote_addr.network_generation());
    'outer: loop {
        if shutdown.is_cancelled() {
            break 'outer;
        }
        wake.acknowledge();
        diagnostics.attempt();
        let mut carrier = None;
        let status = match run_server_side_cli_inner(
            &mut backoff,
            config.clone(),
            &report,
            shutdown.clone(),
            &mut stream_tasks,
            &setup_slots,
            &mut wake,
            &mut carrier,
        )
        .await
        {
            Status::Ended => {
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
            }
            status => status,
        };
        // Every non-terminal outcome retries; only the ladder differs.
        let Some(failure) = status.failure() else {
            break 'outer;
        };
        if matches!(
            failure,
            RecoveryFailure::Transport | RecoveryFailure::Timeout | RecoveryFailure::Dns
        ) {
            remote_addr.transport_failed(carrier);
        }
        let (retry_interval, retry_count) = match status {
            Status::Cancelled => break 'outer,
            Status::Rejected(_) if config.direct.has_active() => {
                // Revocation closes the control plane, not endpoint-owned direct sessions.
                (REVOKED_DIRECT_RECHECK, 0)
            }
            Status::Rejected(reason) => {
                diagnostics.failed(RecoveryFailure::Rejected, Duration::ZERO);
                tracing::error!(
                    event = "local_server_registration_rejected_permanently",
                    key = %key,
                    worker_index,
                    reason = %reason,
                    "pb server permanently rejected this registration; not reconnecting"
                );
                report(WorkerStatus::Failed(Some(reason)));
                break 'outer;
            }
            Status::RejectedRetryable(ref reason) => {
                let interval = backoff.reject.next_delay();
                tracing::debug!(
                    event = "local_server_registration_rejected_retryable",
                    key = %key,
                    worker_index,
                    reason = %reason,
                    retry_delay = ?interval,
                    retry_count = backoff.reject.failures(),
                    "pb server rejected this registration for a condition that may clear"
                );
                (interval, backoff.reject.failures())
            }
            Status::NetworkChanged => {
                diagnostics.network_changed(remote_addr.network_generation());
                backoff.transport.reset();
                (Duration::ZERO, 0)
            }
            Status::ReadMsg
            | Status::Ended
            | Status::SendPing
            | Status::ConnectRemote
            | Status::Resolve
            | Status::Timeout => (
                jitter(backoff.transport.next_delay()),
                backoff.transport.failures(),
            ),
        };
        if diagnostics.failed(failure, retry_interval) {
            tracing::info!(
                event = "local_server_control_reconnect_scheduled",
                key = %key,
                worker_index,
                local_addr = ?local_addr,
                remote_addr = ?remote_addr,
                status = ?status,
                retry_delay = ?retry_interval,
                retry_count,
                "local server control connection will reconnect"
            );
        }
        report(WorkerStatus::Retrying);

        tokio::select! {
            () = shutdown.cancelled() => break 'outer,
            () = tokio::time::sleep(retry_interval) => {},
            woke = wake.changed(), if !matches!(failure, RecoveryFailure::Rejected) => {
                if let crate::endpoint::Wake::Network(generation) = woke {
                    diagnostics.network_changed(generation);
                }
                backoff.transport.reset();
            }
        }
        if shutdown.is_cancelled() {
            break 'outer;
        }
    }

    // Cancellation is already signalled by the token the sessions hold; this only
    // waits for them to observe it.
    stream_tasks.shutdown().await;
}
