pub mod error;
pub mod status;
pub(crate) mod stream;

use crate::diagnostics::{Diagnostics, RecoveryFailure, RecoveryPhase};
use crate::endpoint::RelayEndpoint;
use crate::relay::CarrierId;
use std::fmt::Debug;
use std::ops::ControlFlow;
use std::sync::Arc;
use std::time::Duration;

use snafu::ResultExt;
use tokio::sync::{Mutex, Semaphore};
use tokio::task::JoinSet;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use uni_stream::udp::set_custom_timeout;

use self::error::{AcceptLocalStreamSnafu, BindLocalListenerSnafu};
use self::status::{get_status_scoped, get_status_with_credential};
use self::stream::{StreamSetup, handle_local_stream};
use crate::addr::{resolve_all, resolve_tunnel_ends};
use crate::recovery::{REVOKED_DIRECT_RECHECK, RecoveryTiming, jitter, transport_backoff};
use pb_mapper_core::checksum::{Credential, get_process_credential};
use pb_mapper_core::config::ResolvedAddrs;
use pb_mapper_core::config::{
    StatusOp, client_health_check_interval, client_health_check_timeout,
    client_health_failure_threshold,
};
use pb_mapper_core::timeout::RetryBackoff;
use pb_mapper_protocol::command::{PbConnStatusReq, PbConnStatusResp};
use pb_mapper_protocol::forward::StreamForward;
use uni_stream::addr::ToSocketAddrs;
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
    run_client_side_cli_recovering_with_path::<LocalListener>(
        local_addr,
        RelayEndpoint::fixed(remote_addr),
        key,
        keep_alive,
        namespace,
        status_callback,
        pinned_credential,
        shutdown,
        Diagnostics::default(),
        crate::sdk::PathPolicy::RelayOnly,
        false,
        crate::sdk::DirectPolicy::RELAY,
    )
    .await;
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_client_side_cli_recovering_with_path<LocalListener: ListenerProvider>(
    local_addr: ResolvedAddrs,
    remote_addr: RelayEndpoint,
    key: Arc<str>,
    keep_alive: bool,
    namespace: Option<u64>,
    status_callback: Option<ClientStatusCallback>,
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
        let mut health = Health {
            remote_addr: &remote_addr,
            key: &key,
            namespace,
            credential,
            diagnostics: &diagnostics,
            status_callback: status_callback.as_ref(),
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
                    health.spawn_probe().await;
                }
                Some(result) = health.probes.join_next() => {
                    if health.on_probe_result(result).await.is_break() {
                        break 'outer;
                    }
                }
                Some(success) = stream_event_rx.recv() => health.on_stream_event(success),
                Some(_) = stream_tasks.join_next() => {}
            }
        }
        drop(health);
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
    #[cfg(feature = "p2p")]
    if let Some(direct) = direct {
        direct.stop().await;
    }
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
    kind: RecoveryFailure,
    protocol_replied: bool,
}

impl ProbeFailure {
    fn update_timing(&self, timing: &mut RecoveryTiming, elapsed: Duration) {
        if self.kind == RecoveryFailure::Timeout {
            timing.timed_out();
        } else if self.protocol_replied {
            timing.record(elapsed);
        }
    }

    /// A failure worth retrying: a transport error, or a service that is simply
    /// not registered yet.
    fn transient(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            permanent: false,
            kind: RecoveryFailure::Transport,
            protocol_replied: false,
        }
    }

    /// A status failure classified by the relay's own `retryable` verdict, which
    /// is the only party that knows whether the refusal is final.
    fn from_status_error(context: &str, error: crate::client::error::Error) -> Self {
        let permanent = error.remote_retryable() == Some(false);
        let protocol_replied = error.remote_retryable().is_some();
        Self {
            reason: format!("{context}: {}", snafu::Report::from_error(error)),
            permanent,
            kind: if protocol_replied {
                RecoveryFailure::Rejected
            } else {
                RecoveryFailure::Transport
            },
            protocol_replied,
        }
    }
}

impl std::fmt::Display for ProbeFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.reason)
    }
}

/// What a probe learned besides its verdict: the carrier it went over, to
/// blame if the relay did not answer, and the round trip of its latest
/// exchange that the relay did answer.
#[derive(Default)]
struct ProbeTrace {
    carrier: Option<CarrierId>,
    rtt: Option<Duration>,
}

/// One probe's result: when it started, how long it took, its verdict and
/// what it learned on the way.
type ProbeOutcome = (
    Instant,
    Duration,
    std::result::Result<(), ProbeFailure>,
    ProbeTrace,
);

/// What a bound listener knows about the remote service, and the probes that
/// find out.
///
/// Lives as long as one listener. Not generic: only binding and accepting name
/// the local listener type, so the health bookkeeping is compiled once.
struct Health<'a> {
    remote_addr: &'a RelayEndpoint,
    key: &'a Arc<str>,
    namespace: Option<u64>,
    credential: Credential,
    diagnostics: &'a Diagnostics,
    status_callback: Option<&'a ClientStatusCallback>,
    timing: &'a Mutex<RecoveryTiming>,
    /// Shared with the listener's own rebinds, which wait on it too.
    retry_backoff: &'a mut RetryBackoff,
    probes: JoinSet<ProbeOutcome>,
    next_probe: Instant,
    last_success: Option<Instant>,
    connected: bool,
    consecutive_health_failures: usize,
}

impl Health<'_> {
    fn report(&self, status: &str) {
        if let Some(callback) = self.status_callback {
            callback(status);
        }
    }

    /// Something changed that a waiting probe would not see: drop the ones in
    /// flight and ask again now.
    fn recheck_now(&mut self) {
        self.probes.abort_all();
        self.next_probe = Instant::now();
        self.retry_backoff.reset();
    }

    async fn spawn_probe(&mut self) {
        self.diagnostics.attempt();
        self.diagnostics.phase(RecoveryPhase::Handshake);
        let probe_remote = self.remote_addr.clone();
        let probe_key = self.key.clone();
        let (namespace, credential) = (self.namespace, self.credential);
        let budget = self
            .timing
            .lock()
            .await
            .timeout()
            .min(client_health_check_timeout());
        self.probes.spawn(async move {
            let _permit = probe_remote.control_permit().await;
            let started = Instant::now();
            let mut trace = ProbeTrace::default();
            let result = probe_remote_key(
                &probe_remote,
                &probe_key,
                namespace,
                credential,
                budget,
                &mut trace,
            )
            .await;
            (started, started.elapsed(), result, trace)
        });
    }

    /// Act on a finished probe. Breaks when the relay refused for good, which
    /// ends the tunnel.
    async fn on_probe_result(
        &mut self,
        result: std::result::Result<ProbeOutcome, tokio::task::JoinError>,
    ) -> ControlFlow<()> {
        let (started, elapsed, result, ProbeTrace { carrier, rtt }) = match result {
            Ok(result) => result,
            Err(error) if error.is_cancelled() => return ControlFlow::Continue(()),
            Err(error) => (
                Instant::now(),
                Duration::ZERO,
                Err(ProbeFailure::transient(error.to_string())),
                ProbeTrace::default(),
            ),
        };
        if let Some(rtt) = rtt {
            self.diagnostics.relay_rtt(self.remote_addr, rtt);
        }
        match result {
            Ok(()) => {
                self.timing.lock().await.record(elapsed);
                self.remote_addr.protocol_succeeded();
                self.diagnostics.succeeded(elapsed);
                self.last_success = Some(Instant::now());
                self.consecutive_health_failures = 0;
                self.retry_backoff.reset();
                self.next_probe = Instant::now() + client_health_check_interval();
                if !self.connected {
                    self.connected = true;
                    self.report("connected");
                }
            }
            Err(failure) if failure.permanent && self.diagnostics.has_active_direct() => {
                self.diagnostics.failed(failure.kind, Duration::ZERO);
                self.report("retrying");
                self.next_probe = Instant::now() + REVOKED_DIRECT_RECHECK;
            }
            Err(failure) if failure.permanent => {
                if failure.protocol_replied {
                    self.remote_addr.protocol_succeeded();
                    self.diagnostics.responded(elapsed);
                }
                self.diagnostics.failed(failure.kind, Duration::ZERO);
                self.report(&format!("failed: {failure}"));
                return ControlFlow::Break(());
            }
            Err(_) if self.last_success.is_some_and(|success| success > started) => {
                // Actual traffic completed after this probe began.
                self.next_probe = Instant::now() + client_health_check_interval();
            }
            Err(failure) => {
                failure.update_timing(&mut *self.timing.lock().await, elapsed);
                if failure.protocol_replied {
                    self.remote_addr.protocol_succeeded();
                    self.diagnostics.responded(elapsed);
                } else {
                    self.remote_addr.transport_failed(carrier);
                }
                self.consecutive_health_failures =
                    self.consecutive_health_failures.saturating_add(1);
                if !self.connected
                    || self.consecutive_health_failures >= client_health_failure_threshold()
                {
                    self.connected = false;
                    self.report("retrying");
                }
                let delay = jitter(self.retry_backoff.next_delay());
                self.next_probe = Instant::now() + delay;
                if self.diagnostics.failed(failure.kind, delay) {
                    tracing::warn!(
                        event = "client_remote_probe_failed",
                        key = %self.key,
                        reason = %failure,
                        consecutive_health_failures = self.consecutive_health_failures,
                        retry_delay = ?delay,
                        "remote probe failed; listener remains active"
                    );
                }
            }
        }
        ControlFlow::Continue(())
    }

    /// A local stream finished its setup: proof the service is reachable when
    /// it succeeded, a reason to probe soon when it did not.
    fn on_stream_event(&mut self, success: bool) {
        if success {
            self.last_success = Some(Instant::now());
            self.consecutive_health_failures = 0;
            self.retry_backoff.reset();
            if !self.connected {
                self.connected = true;
                self.report("connected");
            }
        } else if self.probes.is_empty() {
            // Coalesce failures and rate limit probes independently
            // of the number of callers arriving during an outage.
            self.next_probe = self
                .next_probe
                .min(Instant::now() + Duration::from_millis(100));
        }
    }
}

async fn probe_remote_key(
    remote_addr: &RelayEndpoint,
    key: &str,
    namespace: Option<u64>,
    credential: Credential,
    timeout: Duration,
    trace: &mut ProbeTrace,
) -> std::result::Result<(), ProbeFailure> {
    match tokio::time::timeout(timeout, async {
        // Resolved once, here, so a name that does not resolve is reported
        // as such rather than as the relay being unreachable.
        let addresses = remote_addr
            .addresses()
            .await
            .map_err(|error| ProbeFailure {
                reason: error.to_string(),
                permanent: false,
                kind: RecoveryFailure::Dns,
                protocol_replied: false,
            })?;
        probe_remote_key_once(remote_addr, &addresses, key, namespace, credential, trace).await
    })
    .await
    {
        Ok(result) => result,
        Err(_) => Err(ProbeFailure {
            reason: format!("remote key probe timed out after {timeout:?}"),
            permanent: false,
            kind: RecoveryFailure::Timeout,
            protocol_replied: false,
        }),
    }
}

async fn probe_remote_key_once(
    remote_addr: &RelayEndpoint,
    addresses: &ResolvedAddrs,
    key: &str,
    namespace: Option<u64>,
    credential: Credential,
    trace: &mut ProbeTrace,
) -> std::result::Result<(), ProbeFailure> {
    match fetch_remote_status(
        remote_addr,
        addresses,
        PbConnStatusReq::Service {
            direct_query: None,
            key: key.to_string(),
        },
        namespace,
        credential,
        trace,
    )
    .await
    {
        Ok(PbConnStatusResp::Service { connections, .. }) => {
            if connections.iter().any(|conn| conn.healthy) {
                return Ok(());
            }
            return Err(ProbeFailure {
                reason: format!("client key `{key}` has no healthy remote server connections"),
                permanent: false,
                kind: RecoveryFailure::ServiceUnavailable,
                protocol_replied: true,
            });
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

    let status_resp = fetch_remote_status(
        remote_addr,
        addresses,
        PbConnStatusReq::Keys,
        namespace,
        credential,
        trace,
    )
    .await?;
    let PbConnStatusResp::Keys(keys) = status_resp else {
        return Err(ProbeFailure::transient(format!(
            "expected keys status response, got {status_resp:?}"
        )));
    };
    if keys.iter().any(|candidate| candidate == key) {
        Ok(())
    } else {
        Err(ProbeFailure {
            reason: format!("client key `{key}` is not registered on remote server"),
            permanent: false,
            kind: RecoveryFailure::ServiceUnavailable,
            protocol_replied: true,
        })
    }
}

async fn fetch_remote_status(
    remote_addr: &RelayEndpoint,
    addresses: &ResolvedAddrs,
    req: PbConnStatusReq,
    namespace: Option<u64>,
    credential: Credential,
    trace: &mut ProbeTrace,
) -> std::result::Result<PbConnStatusResp, ProbeFailure> {
    let mut stream = remote_addr
        .connect_addresses(addresses, credential, &mut trace.carrier)
        .await
        .map_err(|error| match error {
            crate::relay::ConnectError::Rejected { response } => ProbeFailure {
                reason: format!("{}: {}", response.code, response.message),
                permanent: !response.retryable,
                kind: RecoveryFailure::Rejected,
                protocol_replied: true,
            },
            error => ProbeFailure::transient(format!("connect remote stream failed: {error}")),
        })?;
    // The exchange alone, on a connected stream: one round trip and the
    // relay's handling of it.
    let asked = Instant::now();
    let status = get_status_with_credential(&mut stream, req, namespace, &credential).await;
    let answered = match &status {
        Ok(_) => true,
        Err(error) => error.remote_retryable().is_some(),
    };
    if answered {
        trace.rtt = Some(asked.elapsed());
    }
    status.map_err(|error| ProbeFailure::from_status_error("get status failed", error))
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
#[allow(clippy::unwrap_used, clippy::expect_used)]
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

#[cfg(test)]
mod recovery_tests {
    use super::*;
    #[test]
    fn absent_service_learns_response_latency_without_inflating_timeout() {
        let mut timing = RecoveryTiming::default();
        let missing = ProbeFailure {
            reason: String::new(),
            permanent: false,
            kind: RecoveryFailure::ServiceUnavailable,
            protocol_replied: true,
        };
        for _ in 0..20 {
            missing.update_timing(&mut timing, Duration::from_millis(40));
        }
        assert_eq!(timing.timeout(), Duration::from_secs(1));
        let refusal = ProbeFailure::transient("connection refused");
        refusal.update_timing(&mut timing, Duration::from_millis(1));
        assert_eq!(timing.timeout(), Duration::from_secs(1));
        let timeout = ProbeFailure {
            kind: RecoveryFailure::Timeout,
            ..refusal
        };
        timeout.update_timing(&mut timing, Duration::from_secs(1));
        assert_eq!(timing.timeout(), Duration::from_secs(2));
    }
}
