pub mod error;
pub mod status;
mod stream;

use std::fmt::Debug;
use std::sync::Arc;
use std::time::Duration;

use snafu::ResultExt;
use tokio::net::TcpStream;
use tokio::sync::{Mutex, Semaphore};
use tokio::task::JoinSet;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use uni_stream::udp::set_custom_timeout;

use self::error::{AcceptLocalStreamSnafu, BindLocalListenerSnafu};
use self::status::{get_status_scoped, get_status_with_credential};
use self::stream::{StreamSetup, handle_local_stream};
use crate::addr::{resolve_all, resolve_tunnel_ends};
use crate::recovery::{RecoveryTiming, jitter};
use pb_mapper_core::checksum::{Credential, get_process_credential};
use pb_mapper_core::config::ResolvedAddrs;
use pb_mapper_core::config::{
    StatusOp, client_health_check_interval, client_health_check_timeout,
    client_health_failure_threshold,
};
use pb_mapper_core::timeout::RetryBackoff;
use pb_mapper_protocol::command::{PbConnStatusReq, PbConnStatusResp};
use pb_mapper_protocol::forward::StreamForward;
use uni_stream::addr::{ToSocketAddrs, each_addr};
use uni_stream::stream::{ListenerProvider, StreamAccept};

// Callback for notifying status changes to external systems
pub type ClientStatusCallback = Box<dyn Fn(&str) + Send + Sync>;

/// Resolve both ends, reporting a bad address through `status_callback`.
///
/// The callback is the only channel a caller has here — these entry points return
/// `()` — so an address that never resolves has to settle the tunnel as failed
/// rather than just leaving a log line behind.
async fn resolve_ends_or_fail<A: ToSocketAddrs>(
    local_addr: A,
    remote_addr: A,
    status_callback: Option<&ClientStatusCallback>,
) -> Option<(ResolvedAddrs, ResolvedAddrs)> {
    let resolved = resolve_tunnel_ends(local_addr, remote_addr).await;
    if let (None, Some(callback)) = (&resolved, status_callback) {
        callback("failed");
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
    let Some((local_addr, remote_addr)) =
        resolve_ends_or_fail(local_addr, remote_addr, status_callback.as_ref()).await
    else {
        return;
    };
    run_client_side_cli_loop::<LocalListener>(
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
///
/// Takes both ends already resolved, because its caller — the SDK — resolves them
/// itself in order to report a bad address as an error rather than a log line.
#[allow(clippy::too_many_arguments)]
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
        status_callback,
        pinned_credential,
        shutdown,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn run_client_side_cli_loop<LocalListener: ListenerProvider>(
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
    set_custom_timeout(Duration::from_secs(120));

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

    let mut retry_backoff = RetryBackoff::new(Duration::from_millis(100), Duration::from_secs(2));
    let mut stream_tasks = JoinSet::new();
    let setup_slots = Arc::new(Semaphore::new(64));
    let timing = Arc::new(Mutex::new(RecoveryTiming::default()));

    'outer: loop {
        // Listener lifetime follows the local endpoint, not a remote health
        // snapshot. Only a local bind/accept error requires replacing it.
        let listener = tokio::select! {
            () = shutdown.cancelled() => break,
            result = LocalListener::bind(local_addr.as_slice()) => match result.context(BindLocalListenerSnafu) {
                Ok(listener) => listener,
                Err(error) => {
                    tracing::warn!(event = "client_local_bind_failed", %key, %local_addr, %error);
                    if let Some(callback) = &status_callback { callback("retrying"); }
                    tokio::select! {
                        () = shutdown.cancelled() => break,
                        () = tokio::time::sleep(jitter(retry_backoff.next_delay())) => {}
                    }
                    continue;
                }
            }
        };
        tracing::info!(event = "client_local_listener_bound", %key, %local_addr, "local listener bound; checking remote service");
        let mut probes = JoinSet::new();
        let mut next_probe = Instant::now();
        let mut last_success = None;
        let mut connected = false;
        let mut consecutive_health_failures = 0usize;
        let (stream_event_tx, mut stream_event_rx) = tokio::sync::mpsc::channel(1);

        loop {
            tokio::select! {
                () = shutdown.cancelled() => break 'outer,
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
                    let setup = StreamSetup { permit, timing: timing.clone(), events: event_tx.clone() };
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
                () = tokio::time::sleep_until(next_probe), if probes.is_empty() => {
                    let probe_remote = remote_addr.clone();
                    let probe_key = key.clone();
                    let budget = timing.lock().await.timeout().min(client_health_check_timeout());
                    probes.spawn(async move {
                        let started = Instant::now();
                        let result = probe_remote_key(&probe_remote, &probe_key, namespace, credential, budget).await;
                        (started, started.elapsed(), result)
                    });
                }
                Some(result) = probes.join_next() => {
                    let (started, elapsed, result) = match result {
                        Ok(result) => result,
                        Err(error) => (Instant::now(), Duration::ZERO, Err(ProbeFailure::transient(error.to_string()))),
                    };
                    match result {
                        Ok(()) => {
                            timing.lock().await.record(elapsed);
                            last_success = Some(Instant::now());
                            consecutive_health_failures = 0;
                            retry_backoff.reset();
                            next_probe = Instant::now() + client_health_check_interval();
                            if !connected {
                                connected = true;
                                if let Some(callback) = &status_callback { callback("connected"); }
                            }
                        }
                        Err(failure) if failure.permanent => {
                            if let Some(callback) = &status_callback { callback(&format!("failed: {failure}")); }
                            break 'outer;
                        }
                        Err(_) if last_success.is_some_and(|success| success > started) => {
                            // Actual traffic completed after this probe began.
                            next_probe = Instant::now() + client_health_check_interval();
                        }
                        Err(failure) => {
                            timing.lock().await.timed_out();
                            consecutive_health_failures = consecutive_health_failures.saturating_add(1);
                            if !connected || consecutive_health_failures >= client_health_failure_threshold() {
                                connected = false;
                                if let Some(callback) = &status_callback { callback("retrying"); }
                            }
                            let delay = jitter(retry_backoff.next_delay());
                            next_probe = Instant::now() + delay;
                            tracing::warn!(event = "client_remote_probe_failed", %key, reason = %failure, consecutive_health_failures, retry_delay = ?delay, "remote probe failed; listener remains active");
                        }
                    }
                }
                Some(success) = stream_event_rx.recv() => {
                    if success {
                        last_success = Some(Instant::now());
                        consecutive_health_failures = 0;
                        retry_backoff.reset();
                        if !connected {
                            connected = true;
                            if let Some(callback) = &status_callback { callback("connected"); }
                        }
                    } else if probes.is_empty() {
                        // Coalesce failures and rate limit probes independently
                        // of the number of callers arriving during an outage.
                        next_probe = next_probe.min(Instant::now() + Duration::from_millis(100));
                    }
                }
                Some(_) = stream_tasks.join_next() => {}
            }
        }
        drop(probes);
        if let Some(callback) = &status_callback {
            callback("retrying");
        }
        tokio::select! {
            () = shutdown.cancelled() => break,
            () = tokio::time::sleep(jitter(retry_backoff.next_delay())) => {}
        }
    }

    // Wait for the forwarding sessions to observe the cancellation, so returning
    // from here means no stream of this tunnel is still moving bytes.
    stream_tasks.shutdown().await;
}

/// Why a remote probe did not confirm the service, and whether waiting could
/// change that.
///
/// The distinction is what keeps the retry loop from spinning: a service that has
/// not registered yet may still appear, but a namespace the credential does not
/// own never will.
#[derive(Debug)]
struct ProbeFailure {
    reason: String,
    permanent: bool,
}

impl ProbeFailure {
    /// A failure worth retrying: a transport error, or a service that is simply
    /// not registered yet.
    fn transient(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            permanent: false,
        }
    }

    /// A status failure classified by the relay's own `retryable` verdict, which
    /// is the only party that knows whether the refusal is final.
    fn from_status_error(context: &str, error: crate::client::error::Error) -> Self {
        let permanent = error.remote_retryable() == Some(false);
        Self {
            reason: format!("{context}: {}", snafu::Report::from_error(error)),
            permanent,
        }
    }
}

impl std::fmt::Display for ProbeFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.reason)
    }
}

async fn probe_remote_key(
    remote_addr: &ResolvedAddrs,
    key: &str,
    namespace: Option<u64>,
    credential: Credential,
    timeout: Duration,
) -> std::result::Result<(), ProbeFailure> {
    match tokio::time::timeout(
        timeout,
        probe_remote_key_once(remote_addr, key, namespace, credential),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => Err(ProbeFailure::transient(format!(
            "remote key probe timed out after {timeout:?}"
        ))),
    }
}

async fn probe_remote_key_once(
    remote_addr: &ResolvedAddrs,
    key: &str,
    namespace: Option<u64>,
    credential: Credential,
) -> std::result::Result<(), ProbeFailure> {
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
            return Err(ProbeFailure::transient(format!(
                "client key `{key}` has no healthy remote server connections"
            )));
        }
        Ok(status_resp) => {
            return Err(ProbeFailure::transient(format!(
                "expected service status response, got {status_resp:?}"
            )));
        }
        // A refusal the relay calls final applies to the keys probe just as much,
        // so there is nothing to fall back to.
        Err(failure) if failure.permanent => return Err(failure),
        Err(service_failure) => {
            tracing::debug!(
                event = "client_remote_service_probe_failed",
                key = %key,
                remote_addr = %remote_addr,
                reason = %service_failure,
                "service status probe failed; falling back to key status"
            );
        }
    }

    let status_resp =
        fetch_remote_status(remote_addr, PbConnStatusReq::Keys, namespace, credential).await?;
    let PbConnStatusResp::Keys(keys) = status_resp else {
        return Err(ProbeFailure::transient(format!(
            "expected keys status response, got {status_resp:?}"
        )));
    };
    if keys.iter().any(|candidate| candidate == key) {
        Ok(())
    } else {
        Err(ProbeFailure::transient(format!(
            "client key `{key}` is not registered on remote server; valid keys: {keys:?}"
        )))
    }
}

async fn fetch_remote_status(
    remote_addr: &ResolvedAddrs,
    req: PbConnStatusReq,
    namespace: Option<u64>,
    credential: Credential,
) -> std::result::Result<PbConnStatusResp, ProbeFailure> {
    let mut stream = each_addr(remote_addr.as_slice(), TcpStream::connect)
        .await
        .map_err(|error| {
            ProbeFailure::transient(format!("connect remote stream failed: {error}"))
        })?;
    get_status_with_credential(&mut stream, req, namespace, &credential)
        .await
        .map_err(|error| ProbeFailure::from_status_error("get status failed", error))
}

pub async fn handle_status_cli_scoped<A: ToSocketAddrs>(
    op: StatusOp,
    addr: A,
    namespace: Option<u64>,
) -> Result<(), Box<dyn std::error::Error>> {
    match op {
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
    let mut stream = each_addr(remote_addr.as_slice(), TcpStream::connect)
        .await
        .map_err(|error| format!("get status stream: {error}"))?;
    let status = get_status_scoped(&mut stream, req, namespace).await?;
    let status = serde_json::to_string_pretty(&status)?;
    println!("Status:{status}");
    Ok(())
}
