//! What a connect tunnel knows about the remote service, and the probes that
//! find out.

use std::ops::ControlFlow;
use std::sync::Arc;
use std::time::Duration;

use tokio::task::JoinSet;
use tokio::time::Instant;

use super::status::get_status_with_credential;
use crate::diagnostics::{Diagnostics, RecoveryFailure, RecoveryPhase};
use crate::endpoint::RelayEndpoint;
use crate::recovery::{REVOKED_DIRECT_RECHECK, SharedTiming};
use crate::relay::CarrierId;
use crate::worker_status::{StatusSink, WorkerStatus};
use pb_mapper_core::checksum::Credential;
use pb_mapper_core::config::{
    ResolvedAddrs, client_health_check_interval, client_health_check_timeout,
    client_health_failure_threshold,
};
use pb_mapper_core::timeout::RetryBackoff;
use pb_mapper_protocol::command::{PbConnStatusReq, PbConnStatusResp};

/// Why a remote probe did not confirm the service, and whether waiting could
/// change that.
///
/// The distinction is what keeps the retry loop from spinning: a service that has
/// not registered yet may still appear, but a namespace the credential does not
/// own never will.
#[derive(Debug)]
pub(super) struct ProbeFailure {
    reason: String,
    permanent: bool,
    kind: RecoveryFailure,
    protocol_replied: bool,
}

impl ProbeFailure {
    fn update_timing(&self, timing: &SharedTiming, elapsed: Duration) {
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
pub(super) struct ProbeTrace {
    carrier: Option<CarrierId>,
    rtt: Option<Duration>,
}

/// One probe's result: when it started, how long it took, its verdict and
/// what it learned on the way.
pub(super) type ProbeOutcome = (
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
pub(super) struct Health<'a> {
    pub(super) remote_addr: &'a RelayEndpoint,
    pub(super) key: &'a Arc<str>,
    pub(super) namespace: Option<u64>,
    pub(super) credential: Credential,
    pub(super) diagnostics: &'a Diagnostics,
    pub(super) status: Option<&'a StatusSink>,
    pub(super) timing: &'a SharedTiming,
    /// Shared with the listener's own rebinds, which wait on it too.
    pub(super) retry_backoff: &'a mut RetryBackoff,
    pub(super) probes: JoinSet<ProbeOutcome>,
    pub(super) next_probe: Instant,
    pub(super) last_success: Option<Instant>,
    pub(super) connected: bool,
    pub(super) consecutive_health_failures: usize,
}

impl Health<'_> {
    fn report(&self, status: WorkerStatus) {
        if let Some(sink) = self.status {
            sink(&status);
        }
    }

    /// Something changed that a waiting probe would not see: drop the ones in
    /// flight and ask again now.
    pub(super) fn recheck_now(&mut self) {
        self.probes.abort_all();
        self.next_probe = Instant::now();
        self.retry_backoff.reset();
    }

    pub(super) fn spawn_probe(&mut self) {
        self.diagnostics.attempt();
        self.diagnostics.phase(RecoveryPhase::Handshake);
        let probe_remote = self.remote_addr.clone();
        let probe_key = self.key.clone();
        let (namespace, credential) = (self.namespace, self.credential);
        let budget = self.timing.timeout().min(client_health_check_timeout());
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
    pub(super) fn on_probe_result(
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
                self.timing.record(elapsed);
                self.remote_addr.protocol_succeeded();
                self.diagnostics.succeeded(elapsed);
                self.last_success = Some(Instant::now());
                self.consecutive_health_failures = 0;
                self.retry_backoff.reset();
                self.next_probe = Instant::now() + client_health_check_interval();
                if !self.connected {
                    self.connected = true;
                    self.report(WorkerStatus::Connected);
                }
            }
            Err(failure) if failure.permanent && self.diagnostics.has_active_direct() => {
                self.diagnostics.failed(failure.kind, Duration::ZERO);
                self.report(WorkerStatus::Retrying);
                self.next_probe = Instant::now() + REVOKED_DIRECT_RECHECK;
            }
            Err(failure) if failure.permanent => {
                if failure.protocol_replied {
                    self.remote_addr.protocol_succeeded();
                    self.diagnostics.responded(elapsed);
                }
                self.diagnostics.failed(failure.kind, Duration::ZERO);
                self.report(WorkerStatus::Failed(Some(failure.reason)));
                return ControlFlow::Break(());
            }
            Err(_) if self.last_success.is_some_and(|success| success > started) => {
                // Actual traffic completed after this probe began.
                self.next_probe = Instant::now() + client_health_check_interval();
            }
            Err(failure) => {
                failure.update_timing(self.timing, elapsed);
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
                    self.report(WorkerStatus::Retrying);
                }
                let delay = self.retry_backoff.next_delay();
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
    pub(super) fn on_stream_event(&mut self, success: bool) {
        if success {
            self.last_success = Some(Instant::now());
            self.consecutive_health_failures = 0;
            self.retry_backoff.reset();
            if !self.connected {
                self.connected = true;
                self.report(WorkerStatus::Connected);
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

#[cfg(test)]
mod recovery_tests {
    use super::*;
    #[test]
    fn absent_service_learns_response_latency_without_inflating_timeout() {
        let timing = SharedTiming::default();
        let missing = ProbeFailure {
            reason: String::new(),
            permanent: false,
            kind: RecoveryFailure::ServiceUnavailable,
            protocol_replied: true,
        };
        for _ in 0..20 {
            missing.update_timing(&timing, Duration::from_millis(40));
        }
        assert_eq!(timing.timeout(), Duration::from_secs(1));
        let refusal = ProbeFailure::transient("connection refused");
        refusal.update_timing(&timing, Duration::from_millis(1));
        assert_eq!(timing.timeout(), Duration::from_secs(1));
        let timeout = ProbeFailure {
            kind: RecoveryFailure::Timeout,
            ..refusal
        };
        timeout.update_timing(&timing, Duration::from_secs(1));
        assert_eq!(timing.timeout(), Duration::from_secs(2));
    }
}
