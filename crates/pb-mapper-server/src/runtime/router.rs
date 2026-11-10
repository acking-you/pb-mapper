//! Dispatch of manager tasks, and the steps several kinds share: accepting a
//! connection, retiring a registration, releasing a namespace's rate limiter.

use std::net::SocketAddr;
use std::ops::ControlFlow;
use std::sync::Arc;
use std::time::Instant;

use pb_mapper_core::conn_id::RemoteConnId;
use pb_mapper_core::snafu_error_handle;
use pb_mapper_protocol::secure::ServerSecurity;
use pb_mapper_protocol::transport::RelayStream;

use super::Router;
use crate::connection::handle_conn;
use crate::guard::ConnLease;
use crate::manager::TaskManager;
use crate::routing::{
    ImutableKey, NamespaceRateLimits, PendingStreams, RoutingConfig, ServerConnMap,
    registered_server_conn_count, remove_server_conn,
};
use crate::service_key::split_scoped_service_key;
use crate::task::{ConnTask, ManagerTask};
use crate::{ServerStatusInfo, direct};

impl Router {
    pub(super) fn new(
        manager: TaskManager,
        direct_state: direct::DirectState,
        security: ServerSecurity,
        quic_capability: Option<Arc<pb_mapper_protocol::command::QuicEndpoint>>,
        config: RoutingConfig,
        start_time: Instant,
    ) -> Self {
        Self {
            manager,
            server_conn_map: ServerConnMap::new(),
            pending_streams: PendingStreams::default(),
            namespace_rate_limits: NamespaceRateLimits::new(
                config.new_streams_per_second,
                config.new_streams_burst,
            ),
            config,
            next_server_generation: 1,
            direct_state,
            connection_tasks: tokio::task::JoinSet::new(),
            security,
            quic_capability,
            start_time,
        }
    }

    pub(super) fn reap_finished_tasks(&mut self) {
        while self.connection_tasks.try_join_next().is_some() {}
    }

    /// Apply one task; `Break` once the relay is shutting down.
    pub(super) fn handle(&mut self, task: ManagerTask) -> ControlFlow<()> {
        match task {
            ManagerTask::Direct(task) => self.on_direct(task),
            ManagerTask::AdminServiceList {
                key_id,
                page,
                page_size,
                response_sender,
            } => self.on_admin_service_list(key_id, page, page_size, response_sender),
            ManagerTask::AdminConnectionList {
                key_id,
                page,
                page_size,
                response_sender,
            } => self.on_admin_connection_list(key_id, page, page_size, response_sender),
            ManagerTask::AdminConnectionRetire {
                key,
                conn_id,
                response_sender,
            } => self.on_admin_connection_retire(key, conn_id, response_sender),
            ManagerTask::StatusQuery { response_sender } => self.on_status_query(response_sender),
            ManagerTask::Status {
                response_sender,
                status,
                namespace,
                conn_id,
            } => self.on_status(response_sender, status, namespace, conn_id),
            ManagerTask::Accept { stream, peer_addr } => self.on_accept(stream, peer_addr),
            ManagerTask::DeRegisterServerConn { key, conn_id } => {
                self.on_deregister_server_conn(key, conn_id)
            }
            ManagerTask::ServerConnActivity { key, conn_id } => {
                self.on_server_conn_activity(key, conn_id)
            }
            ManagerTask::RetireServerConn {
                key,
                conn_id,
                reason,
            } => self.retire_server_conn(&key, conn_id, &reason),
            ManagerTask::SuspectServerConn {
                key,
                conn_id,
                generation,
            } => self.on_suspect(key, conn_id, generation),
            ManagerTask::ConfirmServerConn {
                key,
                conn_id,
                generation,
                observed_at,
            } => self.on_confirm(key, conn_id, generation, observed_at),
            ManagerTask::DeRegisterClientConn {
                server_id,
                client_id,
            } => self.on_deregister_client_conn(server_id, client_id),
            ManagerTask::Register {
                key,
                conn_id,
                conn_sender,
                need_codec,
                is_datagram,
                protocol_version,
                retire_token,
                direct,
            } => self.on_register(
                key,
                conn_id,
                conn_sender,
                need_codec,
                is_datagram,
                protocol_version,
                retire_token,
                direct,
            ),
            ManagerTask::Stream {
                data_protocol,
                key,
                stream,
                session,
                server,
                client_id,
                server_generation,
            } => self.on_stream(
                data_protocol,
                key,
                stream,
                session,
                server,
                client_id,
                server_generation,
            ),
            ManagerTask::StreamAck {
                server_id,
                client_id,
                server_generation,
            } => self.on_stream_ack(server_id, client_id, server_generation),
            ManagerTask::Subcribe {
                key,
                conn_id,
                conn_sender,
                excluded_server_conns,
                selected,
            } => self.on_subscribe(key, conn_id, conn_sender, excluded_server_conns, selected),
            ManagerTask::SweepServerLeases => self.on_sweep(),
            ManagerTask::Shutdown => {
                tracing::info!("Server shutdown requested, stopping main loop");
                return ControlFlow::Break(());
            }
        }
        ControlFlow::Continue(())
    }

    pub(super) fn on_accept(&mut self, stream: RelayStream, peer_addr: SocketAddr) {
        let Some(conn_id) = self.manager.get_conn_id(
            self.server_conn_map
                .iter()
                .flat_map(|(_, ids)| ids.iter().map(|v| v.conn_id)),
        ) else {
            // Dropping the stream closes it.
            tracing::error!(
                event = "conn_refused",
                peer_addr = %peer_addr,
                active_connections = self.manager.active_conn_count(),
                "refused a connection: every connection id is in use"
            );
            return;
        };
        tracing::info!(
            event = "conn_accepted",
            conn_id = %conn_id,
            peer_addr = %peer_addr,
            registered_services = self.server_conn_map.len(),
            server_connections = registered_server_conn_count(&self.server_conn_map),
            active_connections = self.manager.active_conn_count(),
            idle_connections = self.manager.idle_conn_count(),
            "accepted pb connection"
        );
        let manager_task_sender = self.manager.get_task_sender().for_connection();
        // Made where the ID is taken, so that it comes back however the
        // connection ends, even as a task aborted before it first ran.
        let lease = ConnLease::new(conn_id, manager_task_sender.clone());
        let security = self.security.clone();
        let quic_capability = self.quic_capability.clone();
        self.connection_tasks.spawn(async move {
            snafu_error_handle!(
                handle_conn(
                    lease,
                    peer_addr,
                    manager_task_sender,
                    stream,
                    security,
                    quic_capability
                )
                .await
            );
        });
    }

    pub(super) fn on_status_query(
        &self,
        response_sender: tokio::sync::oneshot::Sender<ServerStatusInfo>,
    ) {
        let total_connections = self
            .server_conn_map
            .values()
            .map(|conns| conns.len() as u32)
            .sum();

        let status_info = ServerStatusInfo {
            active_connections: total_connections,
            registered_services: self.server_conn_map.len() as u32,
            uptime_seconds: self.start_time.elapsed().as_secs(),
        };

        // Send response back (ignore if receiver dropped)
        let _ = response_sender.send(status_info);
        tracing::debug!(
            event = "status_query_served",
            registered_services = self.server_conn_map.len(),
            server_connections = total_connections,
            active_connections = self.manager.active_conn_count(),
            idle_connections = self.manager.idle_conn_count(),
            "server status query served"
        );
    }

    /// Drop one registered control connection from every map that tracks it, and
    /// tell its socket task to unwind.
    ///
    /// Shared by every caller that retires a connection — a subscriber that found
    /// it unresponsive, and the lease sweep — so an operator reads one
    /// `server_conn_retired` record with one shape regardless of which noticed.
    ///
    /// Teardown happens twice over, because the work queue alone cannot be trusted
    /// to deliver it. `ConnTask::Retire` is the graceful path: the writer sees it
    /// between writes and unwinds, its guard deregisters, and the ID is recycled.
    /// But a registration that has stopped reading leaves the writer blocked inside
    /// a single `write_msg` with no timeout, and a task queued behind that is never
    /// observed — so the connection's `retire_token` is cancelled as well, which
    /// interrupts the write itself and drops the socket. The notification stays
    /// best-effort on top of that: a task whose queue is full is not going to
    /// serve traffic either way.
    pub(super) fn retire_server_conn(
        &mut self,
        key: &ImutableKey,
        conn_id: RemoteConnId,
        reason: &str,
    ) {
        let conn_sender = self.manager.get_conn_sender_chan(&conn_id);
        let removed = remove_server_conn(&mut self.server_conn_map, key, conn_id);
        let removed_from_service_map = removed.is_some();
        // The sender goes, but the ID does not come back yet: the socket task is
        // still running and still holds a guard that will deregister when it
        // unwinds. Recycling here would let a newly accepted socket register under
        // the same ID, and that pending deregistration — which matches on ID alone
        // — would unwind the replacement instead. The `DeRegisterServerConn` arm
        // recycles it when the guard's task finally arrives.
        let removed_from_active_map = self.manager.drop_conn_sender(conn_id);
        self.release_namespace_rate_limit_if_idle(split_scoped_service_key(key).0);
        let retire_notified = conn_sender
            .as_ref()
            .and_then(|sender| {
                sender
                    .try_send(ConnTask::Retire {
                        reason: reason.to_string(),
                    })
                    .ok()
            })
            .is_some();
        // Cancelled unconditionally, and after the notification: a writer that is
        // responsive takes the graceful path first, and one that is wedged
        // mid-write needs this to ever unwind at all. Without it a half-open
        // registration keeps its socket task alive forever, and with it the ID it
        // is holding.
        if let Some(info) = &removed {
            info.retire_token.cancel();
        }
        tracing::warn!(
            event = "server_conn_retired",
            key = %key,
            conn_id = %conn_id,
            reason = %reason,
            removed_from_service_map,
            removed_from_active_map,
            retire_notified,
            socket_cancelled = removed_from_service_map,
            registered_services = self.server_conn_map.len(),
            server_connections = registered_server_conn_count(&self.server_conn_map),
            active_connections = self.manager.active_conn_count(),
            idle_connections = self.manager.idle_conn_count(),
            "server connection retired"
        );
    }

    /// Drop `namespace`'s rate limiter once it has no registered service and no
    /// pending stream.
    pub(super) fn release_namespace_rate_limit_if_idle(&mut self, namespace: u64) {
        if self.pending_streams.count_in(namespace) > 0
            || self
                .server_conn_map
                .keys()
                .any(|key| split_scoped_service_key(key).0 == namespace)
        {
            return;
        }
        self.namespace_rate_limits.release(namespace);
    }
}
