//! Subscriptions and the streams that serve them, relayed or direct.

use std::time::{Duration, Instant};

use pb_mapper_core::conn_id::RemoteConnId;
use pb_mapper_core::{snafu_error_get_or_return, snafu_error_handle};
use pb_mapper_protocol::secure::ServerHeaderSession;
use pb_mapper_protocol::transport::RelayStream;
use snafu::{OptionExt, ResultExt};

use super::Router;
use crate::direct;
use crate::error::{
    TaskCenterClientSendStreamSnafu, TaskCenterSendStreamRespToClientSnafu,
    TaskCenterSendSubcribeRespSnafu, TaskCenterStreamConnIdNotExistSnafu,
};
use crate::guard::ConnLease;
use crate::routing::{
    ImutableKey, PendingStream, ServerConnHealth, ServerConnInfo, cancel_and_remove_server_conn,
    record_server_conn_activity_by_conn_id, registered_server_conn_count, service_conn_count,
};
use crate::service_key::split_scoped_service_key;
use crate::task::{ConnTask, ConnTaskSender};

impl Router {
    pub(super) fn on_subscribe(
        &mut self,
        key: ImutableKey,
        conn_id: RemoteConnId,
        conn_sender: ConnTaskSender,
        excluded_server_conns: Vec<(RemoteConnId, u64)>,
        pinned_provider: Option<(RemoteConnId, u64)>,
    ) {
        let namespace = split_scoped_service_key(&key).0;
        // Fallback belongs to an already admitted subscription. It must
        // keep its slot/token even when the namespace is now full. A
        // pinned DirectRelay is the same: its grant held a setup slot
        // and a rate token until this subscription replaced it.
        let is_new_subscription =
            !self.pending_streams.contains(&conn_id) && pinned_provider.is_none();
        if is_new_subscription
            && !self
                .pending_streams
                .has_room(namespace, 0, self.config.max_streams_per_namespace)
        {
            let _ = conn_sender.try_send(SubscribeRefusal::NamespaceStreamLimit.task());
            return;
        }
        let Some(registrations) = self.server_conn_map.get(&key) else {
            let reason = format!("server key `{key}` is not registered");
            tracing::warn!(
                event = "subscribe_key_missing",
                key = %key,
                client_conn_id = %conn_id,
                excluded_server_conns = ?excluded_server_conns,
                registered_services = self.server_conn_map.len(),
                server_connections = registered_server_conn_count(&self.server_conn_map),
                "subscribe key is not registered"
            );
            reject_subscribe(
                &conn_sender,
                &key,
                conn_id,
                reason,
                !excluded_server_conns.is_empty(),
            );
            return;
        };
        // A direct setup that ended before its grant already paid for
        // the ordinary subscription its consumer falls back to.
        if is_new_subscription
            && !self.direct_state.take_fallback_credit(&key)
            && !self.namespace_rate_limits.try_take(namespace)
        {
            let _ = conn_sender.try_send(SubscribeRefusal::NamespaceStreamRate.task());
            return;
        }
        // Newest first. A snapshot, because an unusable candidate is
        // removed from the map while the rest are still to be tried.
        let candidates: Vec<Candidate> = registrations
            .iter()
            .rev()
            .filter(|info| {
                info.health == ServerConnHealth::Healthy
                    && !excluded_server_conns.contains(&(info.conn_id, info.generation))
                    && pinned_provider.is_none_or(|p| p == (info.conn_id, info.generation))
            })
            .map(Candidate::from)
            .collect();
        let mut selected = false;
        for candidate in candidates {
            let Candidate {
                conn_id: server_conn_id,
                generation: server_generation,
                health,
                need_codec,
                is_datagram,
                stream_ack_budget,
            } = candidate;
            let Some(server_conn_sender) = self.manager.get_conn_sender_chan(&server_conn_id)
            else {
                tracing::warn!(
                    event = "subscribe_stale_server_conn",
                    key = %key,
                    client_conn_id = %conn_id,
                    server_conn_id = %server_conn_id,
                    reason = "sender_not_found",
                    "subscribe skipped stale server connection"
                );
                cancel_and_remove_server_conn(&mut self.server_conn_map, &key, server_conn_id);
                let _ = self.manager.drop_conn_sender(server_conn_id);
                continue;
            };
            // The routing manager must never await a stalled socket's
            // bounded mailbox: that would also block every other service,
            // heartbeat and retirement timer.
            // 1. Send a request to get server stream
            if let Err(e) = server_conn_sender
                .try_send(ConnTask::StreamReq {
                    client_id: conn_id,
                    server_generation,
                })
                .context(TaskCenterClientSendStreamSnafu {
                    key: key.clone(),
                    conn_id,
                })
            {
                let report = snafu::Report::from_error(e);
                tracing::error!(
                    event = "subscribe_stream_request_failed",
                    key = %key,
                    client_conn_id = %conn_id,
                    server_conn_id = %server_conn_id,
                    error = %report,
                    "failed to send stream request to registered server"
                );
                cancel_and_remove_server_conn(&mut self.server_conn_map, &key, server_conn_id);
                let _ = self.manager.drop_conn_sender(server_conn_id);
                continue;
            }
            // sign up client connection after a server accepted the stream request
            if !self.manager.has_conn_sender(&conn_id) {
                self.manager
                    .sign_up_conn_sender(conn_id, conn_sender.clone());
            }
            self.pending_streams.insert(
                conn_id,
                PendingStream {
                    server_conn_id,
                    server_generation,
                    key: key.clone(),
                    requested_at: Instant::now(),
                },
            );
            // 2. Response subcribe ok
            if let Err(e) = conn_sender
                .try_send(ConnTask::SubcribeResp {
                    ack_timeout: stream_ack_budget,
                    server_conn_id,
                    server_generation,
                    need_codec,
                    is_datagram,
                })
                .context(TaskCenterSendSubcribeRespSnafu {
                    key: key.clone(),
                    conn_id,
                })
            {
                let report = snafu::Report::from_error(e);
                tracing::error!(
                    event = "subscribe_response_failed",
                    key = %key,
                    client_conn_id = %conn_id,
                    server_conn_id = %server_conn_id,
                    error = %report,
                    "failed to send subscribe response to client"
                );
                self.manager.drop_conn_sender(conn_id);
                selected = true;
                break;
            }
            tracing::info!(
                event = "subscribe_server_selected",
                key = %key,
                client_conn_id = %conn_id,
                server_conn_id = %server_conn_id,
                server_generation,
                health = ?health,
                need_codec,
                is_datagram,
                service_connections = service_conn_count(&self.server_conn_map, &key),
                active_connections = self.manager.active_conn_count(),
                "selected server connection for client subscribe"
            );
            selected = true;
            break;
        }
        if !selected {
            let reason = format!("no usable server connection for key `{key}`");
            tracing::warn!(
                event = "subscribe_no_usable_server_conn",
                key = %key,
                client_conn_id = %conn_id,
                excluded_server_conns = ?excluded_server_conns,
                registered_services = self.server_conn_map.len(),
                server_connections = registered_server_conn_count(&self.server_conn_map),
                "no usable server connection for subscribe"
            );
            reject_subscribe(
                &conn_sender,
                &key,
                conn_id,
                reason,
                !excluded_server_conns.is_empty(),
            );
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "the fields of `ManagerTask::Stream`, passed on by `handle`"
    )]
    pub(super) fn on_stream(
        &mut self,
        data_protocol: Option<u16>,
        key: ImutableKey,
        stream: RelayStream,
        session: ServerHeaderSession,
        server: ConnLease,
        client_id: RemoteConnId,
        server_generation: u64,
    ) {
        // A stream dropped here returns its ID as `server` drops with it.
        let server_id = server.conn_id();
        let Some(PendingStream {
            server_conn_id: expected_control_conn_id,
            server_generation: expected_generation,
            key: expected_key,
            requested_at: _,
        }) = self.pending_streams.get(&client_id).cloned()
        else {
            tracing::warn!(
                event = "stale_stream_without_pending_client",
                server_conn_id = %server_id,
                client_conn_id = %client_id,
                server_generation,
                "dropping stream for client without pending subscribe"
            );
            return;
        };
        if key != expected_key {
            tracing::warn!(
                event = "stream_namespace_mismatch",
                stream_conn_id = %server_id,
                client_conn_id = %client_id,
                expected_key = %expected_key,
                actual_key = %key,
                "dropping stream that does not belong to the pending namespace and service"
            );
            return;
        }
        if server_generation != 0 && expected_generation != server_generation {
            tracing::warn!(
                event = "stale_stream_generation_mismatch",
                stream_conn_id = %server_id,
                client_conn_id = %client_id,
                expected_control_conn_id = %expected_control_conn_id,
                expected_generation,
                server_generation,
                "dropping stale stream for a previous subscribe attempt"
            );
            return;
        }
        if let Some(info) = self
            .server_conn_map
            .values_mut()
            .flat_map(|infos| infos.iter_mut())
            .find(|info| {
                info.conn_id == expected_control_conn_id && info.generation == expected_generation
            })
        {
            info.health = ServerConnHealth::Healthy;
        }
        tracing::debug!(
            event = "stream_ready_for_client",
            stream_conn_id = %server_id,
            control_conn_id = %expected_control_conn_id,
            client_conn_id = %client_id,
            server_generation = expected_generation,
            active_connections = self.manager.active_conn_count(),
            "server stream ready for client"
        );
        let client_sender = snafu_error_get_or_return!(
            self.manager
                .get_conn_sender_chan(&client_id)
                .context(TaskCenterStreamConnIdNotExistSnafu { conn_id: client_id })
        );
        snafu_error_handle!(
            client_sender
                .try_send(ConnTask::StreamResp {
                    data_protocol,
                    server,
                    server_generation: expected_generation,
                    stream,
                    session,
                })
                .context(TaskCenterSendStreamRespToClientSnafu { conn_id: client_id })
        );
    }

    pub(super) fn on_stream_ack(
        &mut self,
        server_id: RemoteConnId,
        client_id: RemoteConnId,
        server_generation: u64,
    ) {
        let recorded_activity =
            record_server_conn_activity_by_conn_id(&mut self.server_conn_map, server_id);
        let Some(&PendingStream {
            server_conn_id: expected_server_id,
            server_generation: expected_generation,
            key: _,
            requested_at,
        }) = self.pending_streams.get(&client_id)
        else {
            tracing::warn!(
                event = "stale_stream_ack_without_pending_client",
                server_conn_id = %server_id,
                client_conn_id = %client_id,
                server_generation,
                recorded_activity,
                "dropping stream ack for client without pending subscribe"
            );
            return;
        };
        if expected_server_id != server_id || expected_generation != server_generation {
            tracing::warn!(
                event = "stale_stream_ack_generation_mismatch",
                server_conn_id = %server_id,
                client_conn_id = %client_id,
                expected_server_conn_id = %expected_server_id,
                expected_generation,
                server_generation,
                recorded_activity,
                "dropping stale stream ack for a previous subscribe attempt"
            );
            return;
        }
        if let Some(info) = self
            .server_conn_map
            .values_mut()
            .flat_map(|infos| infos.iter_mut())
            .find(|info| info.conn_id == server_id && info.generation == server_generation)
        {
            info.health = ServerConnHealth::Healthy;
            let sample_budget = requested_at.elapsed().saturating_mul(3);
            info.stream_ack_budget = (info.stream_ack_budget.mul_f64(0.75)
                + sample_budget.mul_f64(0.25))
            .max(self.config.stream_ack_timeout)
            .min(self.config.max_stream_ack_budget());
        }
        let client_sender = snafu_error_get_or_return!(
            self.manager
                .get_conn_sender_chan(&client_id)
                .context(TaskCenterStreamConnIdNotExistSnafu { conn_id: client_id })
        );
        snafu_error_handle!(
            client_sender
                .try_send(ConnTask::StreamAck {
                    server_id,
                    server_generation,
                })
                .context(TaskCenterSendStreamRespToClientSnafu { conn_id: client_id })
        );
    }

    pub(super) fn on_deregister_client_conn(
        &mut self,
        server_id: Option<RemoteConnId>,
        client_id: RemoteConnId,
    ) {
        let removed_namespace = self
            .pending_streams
            .remove(&client_id)
            .map(|stream| split_scoped_service_key(&stream.key).0);
        // Same rule as `DeRegisterServerConn`: the guard's arrival is what
        // makes recycling safe, so both IDs come back here even if their
        // senders were dropped earlier.
        let removed_server_conn = if let Some(server_id) = server_id {
            let removed = self.manager.drop_conn_sender(server_id);
            self.manager.recycle_conn_id(server_id);
            removed
        } else {
            false
        };
        let removed_client_conn = self.manager.drop_conn_sender(client_id);
        self.manager.recycle_conn_id(client_id);
        if let Some(namespace) = removed_namespace {
            self.release_namespace_rate_limit_if_idle(namespace);
        }
        if removed_server_conn || removed_client_conn {
            tracing::info!(
                event = "client_conn_deregistered",
                server_conn_id = ?server_id,
                client_conn_id = %client_id,
                removed_server_conn,
                removed_client_conn,
                registered_services = self.server_conn_map.len(),
                server_connections = registered_server_conn_count(&self.server_conn_map),
                active_connections = self.manager.active_conn_count(),
                idle_connections = self.manager.idle_conn_count(),
                "client connection deregistered"
            );
        } else {
            tracing::debug!(
                event = "client_conn_deregister_skipped",
                server_conn_id = ?server_id,
                client_conn_id = %client_id,
                registered_services = self.server_conn_map.len(),
                server_connections = registered_server_conn_count(&self.server_conn_map),
                active_connections = self.manager.active_conn_count(),
                idle_connections = self.manager.idle_conn_count(),
                "client connection was already inactive"
            );
        }
    }

    pub(super) fn on_direct(&mut self, task: direct::DirectTask) {
        use direct::DirectTask;
        use pb_mapper_protocol::direct::DirectClientMessage;
        // Closures name the fields they use rather than calling methods on `self`,
        // so each borrows only those fields and they can be used alongside
        // `self.direct_state`.
        let route = |id| self.manager.get_conn_sender_chan(&id);
        match task {
            DirectTask::Connect {
                connection,
                namespace,
                auth,
                sender,
            } => self
                .direct_state
                .connect(connection, namespace, auth, sender),
            DirectTask::Client {
                connection,
                message:
                    DirectClientMessage::Open {
                        request,
                        key,
                        is_datagram,
                        peer,
                    },
            } => {
                let admit = |namespace, pending| {
                    self.pending_streams.has_room(
                        namespace,
                        pending,
                        self.config.max_streams_per_namespace,
                    ) && self.namespace_rate_limits.try_take(namespace)
                };
                self.direct_state.open(
                    connection,
                    request,
                    key,
                    is_datagram,
                    peer,
                    &self.server_conn_map,
                    route,
                    admit,
                );
            }
            DirectTask::Client {
                connection,
                message,
            } => self.direct_state.client(connection, message, route),
            DirectTask::Provider {
                connection,
                key,
                message,
            } => {
                let auth = self
                    .server_conn_map
                    .get(&key)
                    .and_then(|all| all.iter().find(|p| p.conn_id == connection))
                    .and_then(|p| p.direct.clone());
                self.direct_state
                    .provider(connection, &key, message, auth, route);
            }
            DirectTask::Disconnect { connection } => {
                self.direct_state.disconnect(connection, route)
            }
            DirectTask::Relay {
                auth,
                namespace,
                id,
                secret,
                response,
            } => {
                let _ = response.send(self.direct_state.relay(&auth, namespace, id, secret));
            }
        }
    }
}

/// What a subscribe needs of a registration it may route to.
#[derive(Clone, Copy)]
struct Candidate {
    conn_id: RemoteConnId,
    generation: u64,
    health: ServerConnHealth,
    need_codec: bool,
    is_datagram: bool,
    stream_ack_budget: Duration,
}

impl From<&ServerConnInfo> for Candidate {
    fn from(info: &ServerConnInfo) -> Self {
        Self {
            conn_id: info.conn_id,
            generation: info.generation,
            health: info.health,
            need_codec: info.need_codec,
            is_datagram: info.is_datagram,
            stream_ack_budget: info.stream_ack_budget,
        }
    }
}

/// Why a new subscription was refused before any registration was tried.
#[derive(Clone, Copy)]
enum SubscribeRefusal {
    NamespaceStreamLimit,
    NamespaceStreamRate,
}

impl SubscribeRefusal {
    fn task(self) -> ConnTask {
        let (code, reason) = match self {
            Self::NamespaceStreamLimit => (
                "namespace_stream_limit_exceeded",
                "the namespace has reached its active stream limit",
            ),
            Self::NamespaceStreamRate => (
                "namespace_stream_rate_exceeded",
                "the namespace new-stream rate limit was exceeded",
            ),
        };
        ConnTask::SubcribeFailed {
            code: code.to_string(),
            reason: reason.to_string(),
            retryable: true,
        }
    }
}

/// Tell a subscriber that no registration can serve it: a retry when it is
/// recovering from registrations it already excluded, a failure otherwise.
fn reject_subscribe(
    conn_sender: &ConnTaskSender,
    key: &ImutableKey,
    conn_id: RemoteConnId,
    reason: String,
    retry: bool,
) {
    if retry {
        if conn_sender
            .try_send(ConnTask::SubcribeRetry {
                reason: reason.clone(),
            })
            .is_err()
        {
            tracing::debug!(
                event = "subscribe_retry_receiver_dropped",
                key = %key,
                client_conn_id = %conn_id,
                reason = %reason,
                "subscribe retry receiver dropped"
            );
        }
    } else if conn_sender
        .try_send(ConnTask::SubcribeFailed {
            code: "service_not_available".to_string(),
            reason: reason.clone(),
            retryable: true,
        })
        .is_err()
    {
        tracing::debug!(
            event = "subscribe_failure_receiver_dropped",
            key = %key,
            client_conn_id = %conn_id,
            reason = %reason,
            "subscribe failure receiver dropped"
        );
    }
}
