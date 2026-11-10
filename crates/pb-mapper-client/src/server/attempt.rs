use std::fmt::Debug;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use snafu::ResultExt;
use tokio::task::JoinSet;
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;
use tracing::instrument;

use super::ServerTunnelOptions;
use super::control::{
    ControlLeaseState, ControlRegistration, LocalControlWrite, handle_ping_interval,
    handle_request, write_stream_ack,
};
use super::error::{
    DecodeRegisterRespSnafu, EncodeRegisterReqSnafu, ReadRegisterRespSnafu, ReadStreamReqSnafu,
    RegisterRespNotMatchSnafu, SendRegisterReqSnafu,
};
use super::probe::{RegistrationProbeResult, probe_remote_registration};
use super::status::WorkerStatus;
use super::stream::StreamConnect;
use super::worker::ControlBackoff;
use crate::diagnostics::{Diagnostics, RecoveryPhase};
use crate::endpoint::{RecoveryWake, RelayEndpoint};
use crate::relay::CarrierId;
use pb_mapper_core::checksum::Credential;
#[cfg(feature = "p2p")]
use pb_mapper_core::config::control_io_timeout;
use pb_mapper_core::config::{
    ResolvedAddrs, control_heartbeat_interval, control_heartbeat_tolerance, control_suspect_grace,
};
use pb_mapper_core::{
    snafu_error_get_or_continue, snafu_error_get_or_return, snafu_error_get_or_return_ok,
    snafu_error_handle,
};
use pb_mapper_protocol::MessageReader;
#[cfg(feature = "p2p")]
use pb_mapper_protocol::MessageWriter;
#[cfg(feature = "p2p")]
use pb_mapper_protocol::command::PbServerRequest;
use pb_mapper_protocol::command::{
    CONTROL_PROTOCOL_V2, MessageSerializer, PbConnRequest, PbConnResponse,
};
use pb_mapper_protocol::forward::StreamForward;
use pb_mapper_protocol::secure::ClientHeaderSession;
use uni_stream::stream::StreamProvider;

/// Why one attempt at holding a control connection ended, and therefore how the
/// worker should treat it.
///
/// The three transport variants are interchangeable as far as the retry decision
/// goes — they differ only in what they report — while the two rejection variants
/// each mean something the transport ones do not:
///
/// * [`Status::Rejected`] is terminal, so the worker stops.
/// * [`Status::RejectedRetryable`] is not, but the relay answered and refused, so
///   the worker waits on the much slower [`registration_reject_backoff`](pb_mapper_core::config::registration_reject_backoff) ladder
///   instead of the transport one.
#[derive(Debug)]
pub(super) enum Status {
    ReadMsg,
    SendPing,
    ConnectRemote,
    Resolve,
    Timeout,
    NetworkChanged,
    Cancelled,
    /// The relay refused the registration for a reason reconnecting cannot fix —
    /// a namespace the credential does not own, a malformed service name. Retrying
    /// would loop forever while the caller's `wait_ready` never resolves, so this
    /// ends the worker instead.
    Rejected(String),
    /// The relay refused the registration for a condition that can clear on its
    /// own — a full per-service connection quota, a namespace at its service
    /// limit. Worth retrying, but slowly: the relay is reachable and answering,
    /// and reconnecting hard only adds load to what has to drain.
    RejectedRetryable(String),
}

impl Status {
    fn rejected(error: pb_mapper_protocol::command::PbErrorResponse) -> Self {
        let reason = format!("{}: {}", error.code, error.message);
        if error.retryable {
            Self::RejectedRetryable(reason)
        } else {
            Self::Rejected(reason)
        }
    }
}

#[derive(Clone)]
pub(super) struct ServerCliRunConfig {
    pub(super) local_addr: ResolvedAddrs,
    pub(super) remote_addr: RelayEndpoint,
    pub(super) diagnostics: Diagnostics,
    pub(super) key: Arc<str>,
    pub(super) options: ServerTunnelOptions,
    pub(super) worker_index: usize,
    pub(super) credential: Credential,
    pub(super) idle_timeout: Option<Duration>,
    #[cfg(feature = "p2p")]
    pub(super) direct: Option<Arc<crate::direct::Provider>>,
}

impl Debug for ServerCliRunConfig {
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

// `backoff` is skipped: it is mutable retry state, and recording it would
// print two ladders' internals on every span the worker enters.
#[allow(clippy::too_many_arguments)]
#[instrument(skip(backoff, report, shutdown, stream_tasks, setup_slots, wake, carrier))]
pub(super) async fn run_server_side_cli_inner<LocalStream: StreamProvider>(
    backoff: &mut ControlBackoff,
    config: ServerCliRunConfig,
    report: &impl Fn(WorkerStatus),
    shutdown: CancellationToken,
    stream_tasks: &mut JoinSet<()>,
    setup_slots: &Arc<tokio::sync::Semaphore>,
    wake: &mut RecoveryWake,
    carrier: &mut Option<CarrierId>,
) -> std::result::Result<(), Status>
where
    LocalStream::Item: StreamForward,
{
    let ServerCliRunConfig {
        local_addr,
        remote_addr,
        diagnostics,
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
        idle_timeout,
        #[cfg(feature = "p2p")]
        direct,
    } = config;
    let control_permit = tokio::select! {
        () = shutdown.cancelled() => return Err(Status::Cancelled),
        () = wake.network_changed() => return Err(Status::NetworkChanged),
        permit = remote_addr.control_permit() => permit,
    };
    let started = tokio::time::Instant::now();
    let timeout = backoff.timing.timeout();
    let deadline = started + timeout;
    diagnostics.phase(RecoveryPhase::Resolving);
    let addresses = tokio::select! {
        () = shutdown.cancelled() => return Err(Status::Cancelled),
        () = wake.network_changed() => return Err(Status::NetworkChanged),
        result = tokio::time::timeout_at(deadline, remote_addr.addresses()) => match result {
            Ok(Ok(addresses)) => addresses,
            _ => return Err(Status::Resolve),
        },
    };
    diagnostics.phase(RecoveryPhase::Connecting);
    let mut manager_stream = tokio::select! {
        () = shutdown.cancelled() => return Err(Status::Cancelled),
        () = wake.network_changed() => return Err(Status::NetworkChanged),
        result = tokio::time::timeout_at(deadline, remote_addr.connect_addresses(&addresses, credential, carrier)) => {
            match result {
                Ok(Ok(stream)) => stream,
                Ok(Err(error)) => {
                    tracing::debug!(event = "local_server_dial_failed", %error, %key, worker_index);
                    if let crate::relay::ConnectError::Rejected { response } = error {
                        remote_addr.protocol_succeeded();
                        diagnostics.responded(started.elapsed());
                        backoff.timing.record(started.elapsed());
                        return Err(Status::rejected(response));
                    }
                    return Err(Status::ConnectRemote);
                }
                Err(_) => {
                    backoff.timing.timed_out();
                    tracing::debug!(event = "local_server_setup_timeout", %key, worker_index, ?timeout, phase = "dial");
                    return Err(Status::Timeout);
                }
            }
        }
    };
    tracing::debug!(
        event = "local_server_connected_remote",
        key = %key,
        worker_index,
        local_addr = %local_addr,
        remote_addr = %remote_addr,
        need_codec,
        is_datagram,
        "local server connected to pb server"
    );

    snafu_error_handle!(manager_stream.configure(keep_alive));

    let registered_addr = manager_stream
        .relay_addr()
        .map(ResolvedAddrs::from)
        .unwrap_or(addresses);
    diagnostics.phase(RecoveryPhase::Handshake);
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
    let heartbeat_interval = control_heartbeat_interval();
    let heartbeat_tolerance = control_heartbeat_tolerance();
    #[cfg(feature = "p2p")]
    let direct_version = direct
        .as_ref()
        .map(|_| pb_mapper_protocol::direct::DIRECT_VERSION);
    #[cfg(not(feature = "p2p"))]
    let direct_version = None;
    let request = match namespace {
        Some(namespace) => PbConnRequest::RegisterScoped {
            direct: direct_version,
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
            direct: direct_version,
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
    tokio::select! {
        () = shutdown.cancelled() => return Err(Status::Cancelled),
        () = wake.network_changed() => return Err(Status::NetworkChanged),
        result = tokio::time::timeout_at(deadline, session.write_initial(&mut manager_stream, &msg)) => {
            match result {
                Ok(result) => snafu_error_get_or_return_ok!(result.context(SendRegisterReqSnafu)),
                Err(_) => {
                    backoff.timing.timed_out();
                    tracing::debug!(event = "local_server_setup_timeout", %key, worker_index, ?timeout, phase = "write");
                    return Err(Status::Timeout);
                }
            }
        }
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
        let msg = tokio::select! {
            () = shutdown.cancelled() => return Err(Status::Cancelled),
            () = wake.network_changed() => return Err(Status::NetworkChanged),
            result = tokio::time::timeout_at(deadline, msg_reader.read_msg()) => {
                match result {
                    Ok(result) => snafu_error_get_or_return_ok!(result.context(ReadRegisterRespSnafu)),
                    Err(_) => {
                        backoff.timing.timed_out();
                        tracing::debug!(event = "local_server_setup_timeout", %key, worker_index, ?timeout, phase = "response");
                        return Err(Status::Timeout);
                    }
                }
            }
        };
        let resp = snafu_error_get_or_return_ok!(
            PbConnResponse::decode(msg).context(DecodeRegisterRespSnafu)
        );
        remote_addr.protocol_succeeded();
        diagnostics.responded(started.elapsed());
        backoff.timing.record(started.elapsed());
        let registration = match resp {
            PbConnResponse::RegisterV2 {
                conn_id,
                generation,
                lease_ttl_ms,
                ..
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
            // A rejection is an answer, not a failure to get one, so it never
            // goes through `snafu_error_get_or_return_ok!`: that would report the
            // attempt as having "finished without an error" and retry it on the
            // transport ladder, which is how a refused registration turned into
            // thousands of reject lines a minute.
            PbConnResponse::Error(error) => {
                tracing::debug!(
                    event = "local_server_registration_rejected",
                    key = %key,
                    worker_index,
                    reason = %error.code,
                    retryable = error.retryable,
                    message = %error.message,
                    "pb server rejected service registration"
                );
                return Err(Status::rejected(error));
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

        // This worker holds a registered control connection, which is on its own
        // enough to make the service reachable.
        report(WorkerStatus::Connected);
        (key, registration)
    };

    drop(control_permit);
    diagnostics.succeeded(started.elapsed());
    remote_addr.protocol_succeeded();
    backoff.timing.record(started.elapsed());
    tracing::debug!(event = "local_server_setup_latency", elapsed_ms = duration_to_millis(started.elapsed()), next_timeout_ms = duration_to_millis(backoff.timing.timeout()), %key, worker_index);
    backoff.reset();
    let (write_tx, mut write_rx) = tokio::sync::mpsc::channel::<LocalControlWrite>(64);
    #[cfg(feature = "p2p")]
    if let Some(direct) = &direct {
        direct.connected(write_tx.clone());
    }
    let lease_state = Arc::new(tokio::sync::Mutex::new(ControlLeaseState::new()));
    let writer_key = key.clone();
    let writer_registration = registration;
    let writer_lease = lease_state.clone();
    let mut writer_handle = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
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
                #[cfg(feature = "p2p")]
                LocalControlWrite::Direct(message) => {
                    let payload = PbServerRequest::Direct(*message)
                        .encode()
                        .map_err(|_| Status::SendPing)?;
                    tokio::time::timeout(control_io_timeout(), msg_writer.write_msg(&payload))
                        .await
                        .map_err(|_| Status::SendPing)?
                        .map_err(|_| Status::SendPing)?;
                }
                LocalControlWrite::Ping { seq } => {
                    // Timed from the write, not the tick: the round trip
                    // leaves out this queue.
                    writer_lease.lock().await.ping_sent(seq);
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
    }));

    let heartbeat_interval = control_heartbeat_interval();
    let heartbeat_tolerance = control_heartbeat_tolerance();
    let suspect_grace = control_suspect_grace();
    let mut heartbeat = tokio::time::interval(heartbeat_interval);
    heartbeat.set_missed_tick_behavior(MissedTickBehavior::Delay);
    heartbeat.tick().await;
    let mut probes = JoinSet::new();
    let mut ping_seq = 0_u64;
    let mut control_deadline = tokio::time::Instant::now() + heartbeat_tolerance + suspect_grace;

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
                remote_addr.protocol_succeeded();
                diagnostics.heard();
                lease_state.lock().await.record_rx();
                control_deadline = tokio::time::Instant::now() + heartbeat_tolerance + suspect_grace;
                snafu_error_get_or_continue!(
                    handle_request::<LocalStream>(
                        msg,
                        StreamConnect {
                            local_addr: local_addr.clone(),
                            remote_addr: registered_addr.clone(),
                            keep_alive,
                            namespace,
                            credential,
                            idle_timeout,
                        },
                        remote_addr.clone(),
                        &diagnostics,
                        key.clone(),
                        registration.conn_id,
                        &write_tx,
                        lease_state.clone(),
                        stream_tasks,
                        &shutdown,
                        setup_slots,
                        #[cfg(feature = "p2p")]
                        direct.as_ref(),
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
                if write_tx.try_send(LocalControlWrite::Ping { seq: ping_seq }).is_err() {
                    break Err(Status::SendPing);
                }

                let last_rx_age = lease_state.lock().await.last_rx_age();
                if registration.protocol_version >= CONTROL_PROTOCOL_V2
                    && last_rx_age >= heartbeat_tolerance
                    && probes.is_empty()
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
                    let probe_key = key.clone();
                    let probe_remote = registered_addr.clone();
                    let probe_endpoint = remote_addr.clone();
                    probes.spawn(async move {
                        let _permit = probe_endpoint.control_permit().await;
                        probe_remote_registration(
                            probe_remote,
                            probe_endpoint.clone(),
                            probe_key,
                            registration,
                            namespace,
                            credential,
                        )
                        .await
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
            () = tokio::time::sleep_until(control_deadline) => {
                tracing::warn!(event = "local_server_control_unresponsive", %key, worker_index, "control socket received no reply past grace; reconnecting");
                break Err(Status::ReadMsg);
            }
            Some(probe_result) = probes.join_next() => {
                let probe_result = probe_result.unwrap_or_else(|error| RegistrationProbeResult::Failed(error.to_string()));
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
