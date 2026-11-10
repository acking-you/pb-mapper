//! Registration leases: register, renew, suspect, confirm, deregister, sweep.

use std::time::Instant;

use pb_mapper_auth::AuthContext;
use pb_mapper_core::conn_id::RemoteConnId;
use pb_mapper_core::snafu_error_handle;
use snafu::ResultExt;
use tokio_util::sync::CancellationToken;

use super::Router;
use crate::error::TaskCenterSendRegisterRespSnafu;
use crate::{
    ConnTask, ConnTaskSender, ImutableKey, ManagerTask, ServerConnHealth, ServerConnInfo,
    record_server_conn_activity, registered_server_conn_count, remove_server_conn,
    service_conn_count, split_scoped_service_key, sweep_server_conn_leases,
};

impl Router {
    #[expect(
        clippy::too_many_arguments,
        reason = "the fields of `ManagerTask::Register`, passed on by `handle`"
    )]
    pub(super) fn on_register(
        &mut self,
        key: ImutableKey,
        conn_id: RemoteConnId,
        conn_sender: ConnTaskSender,
        need_codec: bool,
        is_datagram: bool,
        protocol_version: u16,
        retire_token: CancellationToken,
        direct: Option<AuthContext>,
    ) {
        let direct = direct.filter(|_| self.direct_state.capability.is_some());
        let namespace = split_scoped_service_key(&key).0;
        let existing = self.server_conn_map.get(&key);
        let refusal = if existing.is_some_and(|connections| {
            connections
                .first()
                .is_some_and(|connection| connection.is_datagram != is_datagram)
        }) {
            Some(RegisterRefusal::TransportMismatch)
        } else if existing.is_some_and(|connections| {
            connections.len() >= self.config.max_register_connections_per_service.get()
        }) {
            Some(RegisterRefusal::ServiceConnectionLimit)
        } else if existing.is_none()
            && self
                .server_conn_map
                .keys()
                .filter(|registered| split_scoped_service_key(registered).0 == namespace)
                .count()
                >= self.config.max_services_per_namespace.get()
        {
            Some(RegisterRefusal::NamespaceServiceLimit)
        } else {
            None
        };
        if let Some(refusal) = refusal {
            let _ = conn_sender.try_send(refusal.task());
            return;
        }
        let generation = self.next_server_generation;
        self.next_server_generation = self.next_server_generation.saturating_add(1).max(1);
        let direct_capability = direct.as_ref().and(self.direct_state.capability.clone());
        let info = ServerConnInfo {
            conn_id,
            generation,
            health: ServerConnHealth::Healthy,
            need_codec,
            is_datagram,
            protocol_version,
            last_rx_at: Instant::now(),
            stream_ack_budget: self.config.stream_ack_timeout,
            suspect_check: None,
            retire_token,
            direct,
        };
        // sign up server connection
        self.manager
            .sign_up_conn_sender(conn_id, conn_sender.clone());
        match self.server_conn_map.entry(key.clone()) {
            hashbrown::hash_map::Entry::Occupied(mut o) => o.get_mut().push(info),
            hashbrown::hash_map::Entry::Vacant(v) => {
                v.insert(vec![info]);
            }
        }

        // response registered ok
        tracing::info!(
            event = "server_conn_registered",
            key = %key,
            conn_id = %conn_id,
            generation,
            protocol_version,
            need_codec,
            is_datagram,
            service_connections = service_conn_count(&self.server_conn_map, &key),
            registered_services = self.server_conn_map.len(),
            server_connections = registered_server_conn_count(&self.server_conn_map),
            active_connections = self.manager.active_conn_count(),
            idle_connections = self.manager.idle_conn_count(),
            "server connection registered"
        );
        snafu_error_handle!(
            conn_sender
                .try_send(ConnTask::RegisterResp {
                    generation,
                    protocol_version,
                    lease_ttl_ms: self.config.lease_timeout.as_millis() as u64,
                    direct: direct_capability,
                })
                .context(TaskCenterSendRegisterRespSnafu { key, conn_id })
        );
    }

    pub(super) fn on_deregister_server_conn(&mut self, key: ImutableKey, conn_id: RemoteConnId) {
        let removed_from_service_map =
            remove_server_conn(&mut self.server_conn_map, &key, conn_id).is_some();
        let removed_from_active_map = self.manager.drop_conn_sender(conn_id);
        // Recycled here and only here, whether or not the sender was still
        // registered: this task comes from the socket's own guard, so its
        // arrival is the proof that nothing will speak for the ID again.
        // A retired connection reaches this with its sender already gone,
        // and its ID has been held out of circulation until now.
        self.manager.recycle_conn_id(conn_id);
        self.release_namespace_rate_limit_if_idle(split_scoped_service_key(&key).0);
        tracing::info!(
            event = "server_conn_deregistered",
            key = %key,
            conn_id = %conn_id,
            removed_from_service_map,
            removed_from_active_map,
            registered_services = self.server_conn_map.len(),
            server_connections = registered_server_conn_count(&self.server_conn_map),
            active_connections = self.manager.active_conn_count(),
            idle_connections = self.manager.idle_conn_count(),
            "server connection deregistered"
        );
    }

    pub(super) fn on_server_conn_activity(&mut self, key: ImutableKey, conn_id: RemoteConnId) {
        let recorded = record_server_conn_activity(&mut self.server_conn_map, &key, conn_id);
        tracing::debug!(
            event = "server_conn_lease_renewed",
            key = %key,
            conn_id = %conn_id,
            recorded,
            "server control connection activity recorded"
        );
    }

    pub(super) fn on_suspect(&mut self, key: ImutableKey, conn_id: RemoteConnId, generation: u64) {
        let Some(info) = self.server_conn_map.get_mut(&key).and_then(|infos| {
            infos
                .iter_mut()
                .find(|info| info.conn_id == conn_id && info.generation == generation)
        }) else {
            return;
        };
        info.stream_ack_budget = info
            .stream_ack_budget
            .saturating_mul(2)
            .min(self.config.max_stream_ack_budget());
        // A timeout affects candidate selection immediately, but a late
        // authenticated ACK/heartbeat can restore it without a reconnect.
        info.health = ServerConnHealth::Suspect;
        if info.suspect_check.is_some() {
            return;
        }
        let observed_at = Instant::now();
        info.suspect_check = Some(observed_at);
        let grace = pb_mapper_core::config::control_suspect_grace().max(info.stream_ack_budget);
        tracing::warn!(event = "server_conn_suspected", %key, %conn_id, generation, ?grace, ack_budget = ?info.stream_ack_budget, "stream was slow; waiting for authenticated control activity before retirement");
        let sender = self.manager.get_task_sender();
        self.connection_tasks.spawn(async move {
            tokio::time::sleep(grace).await;
            let _ = sender
                .send(ManagerTask::ConfirmServerConn {
                    key,
                    conn_id,
                    generation,
                    observed_at,
                })
                .await;
        });
    }

    pub(super) fn on_confirm(
        &mut self,
        key: ImutableKey,
        conn_id: RemoteConnId,
        generation: u64,
        observed_at: Instant,
    ) {
        let Some(info) = self.server_conn_map.get_mut(&key).and_then(|infos| {
            infos.iter_mut().find(|info| {
                info.conn_id == conn_id
                    && info.generation == generation
                    && info.suspect_check == Some(observed_at)
            })
        }) else {
            return;
        };
        info.suspect_check = None;
        if info.last_rx_at > observed_at {
            tracing::debug!(event = "server_conn_suspect_cleared", %key, %conn_id, generation, "control activity cancelled retirement");
            return;
        }
        self.retire_server_conn(
            &key,
            conn_id,
            "stream timeout followed by silent control connection",
        );
    }

    pub(super) fn on_sweep(&mut self) {
        self.direct_state
            .sweep(|id| self.manager.get_conn_sender_chan(&id));
        // Retire in place rather than queueing `RetireServerConn` back into
        // the manager channel: this loop is that channel's only consumer, so
        // a full queue would have it waiting on itself.
        for stale in sweep_server_conn_leases(&mut self.server_conn_map, self.config.lease_timeout)
        {
            let reason = format!(
                "lease expired: no control traffic for {:?} (protocol v{})",
                stale.idle_for, stale.protocol_version
            );
            self.retire_server_conn(&stale.key, stale.conn_id, &reason);
        }
    }
}

/// Why a registration was refused.
#[derive(Clone, Copy)]
enum RegisterRefusal {
    TransportMismatch,
    ServiceConnectionLimit,
    NamespaceServiceLimit,
}

impl RegisterRefusal {
    fn task(self) -> ConnTask {
        let (code, reason, retryable) = match self {
            Self::TransportMismatch => (
                "service_transport_mismatch",
                "the service name is already registered with a different transport",
                false,
            ),
            Self::ServiceConnectionLimit => (
                "service_connection_limit_exceeded",
                "the service has reached its register connection limit",
                true,
            ),
            Self::NamespaceServiceLimit => (
                "namespace_service_limit_exceeded",
                "the namespace has reached its service name limit",
                true,
            ),
        };
        ConnTask::RegisterFailed {
            code: code.to_string(),
            reason: reason.to_string(),
            retryable,
        }
    }
}
