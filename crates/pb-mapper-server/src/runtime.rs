//! Relay orchestration and the serialized routing-manager event loop.
//!
//! ```text
//! listener task ---- Accept -------+
//! control tasks ---- Register -----+-> ManagerTask loop -> routing maps / quotas
//! subscriber ------- Subcribe -----+                    -> ConnTask responses
//! provider stream -- Stream/Ack ---+
//! ```
//!
//! The manager loop is the single writer for connection IDs, registrations, pending
//! streams, per-namespace counts, and rate limits. Socket I/O runs in spawned connection
//! tasks and communicates with this state only through typed tasks.

use super::*;
use std::ops::ControlFlow;

use crate::manager::TrySendError;

/// Run a server that takes its keep-alive setting from the environment.
///
/// Callers that own the setting — the binary, and the UI, which has a toggle for
/// it — should use [`run_server_with_shutdown`] and pass it explicitly.
pub async fn run_server<A: ToSocketAddrs>(addr: A) -> std::io::Result<()> {
    run_server_with_shutdown(addr, CancellationToken::new(), None, keep_alive_from_env()).await
}

pub async fn run_server_with_shutdown<A: ToSocketAddrs>(
    addr: A,
    shutdown_token: CancellationToken,
    status_channel: Option<
        tokio::sync::mpsc::UnboundedReceiver<tokio::sync::oneshot::Sender<ServerStatusInfo>>,
    >,
    keep_alive: bool,
) -> std::io::Result<()> {
    run_server_with_auth_config(
        addr,
        shutdown_token,
        status_channel,
        keep_alive,
        AuthConfig::default(),
    )
    .await
}

pub async fn run_server_with_auth_config<A: ToSocketAddrs>(
    addr: A,
    shutdown_token: CancellationToken,
    status_channel: Option<
        tokio::sync::mpsc::UnboundedReceiver<tokio::sync::oneshot::Sender<ServerStatusInfo>>,
    >,
    keep_alive: bool,
    auth_config: AuthConfig,
) -> std::io::Result<()> {
    let auth = AuthRuntime::from_process(auth_config)
        .await
        .map_err(std::io::Error::other)?;
    let listener = TcpListener::bind(addr).await?;
    run_server_on_listener(listener, shutdown_token, status_channel, keep_alive, auth).await
}

pub async fn run_server_on_listener(
    listener: TcpListener,
    shutdown_token: CancellationToken,
    status_channel: Option<
        tokio::sync::mpsc::UnboundedReceiver<tokio::sync::oneshot::Sender<ServerStatusInfo>>,
    >,
    keep_alive: bool,
    auth: AuthRuntime,
) -> std::io::Result<()> {
    run_server_on_listener_with_direct(
        listener,
        shutdown_token,
        status_channel,
        keep_alive,
        auth,
        None,
    )
    .await
}

pub async fn run_server_on_listener_with_direct(
    listener: TcpListener,
    shutdown_token: CancellationToken,
    status_channel: Option<
        tokio::sync::mpsc::UnboundedReceiver<tokio::sync::oneshot::Sender<ServerStatusInfo>>,
    >,
    keep_alive: bool,
    auth: AuthRuntime,
    direct_config: Option<DirectServerConfig>,
) -> std::io::Result<()> {
    run_server_on_listener_with_options(
        listener,
        shutdown_token,
        status_channel,
        keep_alive,
        auth,
        RelayServerOptions {
            direct: direct_config,
            quic: None,
        },
    )
    .await
}

pub async fn run_server_on_listener_with_options(
    listener: TcpListener,
    shutdown_token: CancellationToken,
    status_channel: Option<
        tokio::sync::mpsc::UnboundedReceiver<tokio::sync::oneshot::Sender<ServerStatusInfo>>,
    >,
    keep_alive: bool,
    auth: AuthRuntime,
    options: RelayServerOptions,
) -> std::io::Result<()> {
    let (capability, stun_task) = direct::bind(options.direct).await?;
    let direct_state = direct::DirectState::new(capability);
    let security = ServerSecurity::new(auth);
    let manager = TaskManager::new();
    let config = RoutingConfig::from_env();

    let listen_addr = listener.local_addr()?;
    tracing::info!(
        event = "pb_server_listening",
        listen_addr = %listen_addr,
        control_timeout = ?control_io_timeout(),
        "pb-mapper server is listening"
    );

    // Clients reach QUIC at the IP they reached TCP at, so a listener on the
    // other address family is one some of them cannot use.
    if let Some(quic) = &options.quic
        && quic.listen.is_ipv6() != listen_addr.is_ipv6()
    {
        tracing::warn!(
            event = "relay_quic_family_mismatch",
            tcp = %listen_addr,
            quic = %quic.listen,
            "QUIC and TCP listen on different address families; clients on TCP's cannot use QUIC"
        );
    }
    let quic_stop = CancellationToken::new();
    #[cfg(feature = "relay-quic")]
    let (quic_task, quic_capability) = quic::start(
        options.quic,
        security.clone(),
        manager.get_task_sender(),
        quic_stop.clone(),
    )
    .await?;
    #[cfg(not(feature = "relay-quic"))]
    let (quic_task, quic_capability): (
        Option<tokio_util::task::AbortOnDropHandle<()>>,
        Option<Arc<pb_mapper_protocol::command::QuicEndpoint>>,
    ) = {
        if options.quic.is_some() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "binary built without relay-quic",
            ));
        }
        (None, None)
    };
    let task_sender = manager.get_task_sender();
    let shutdown_token_clone = shutdown_token.clone();

    let listener_handle = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
        tokio::select! {
            biased;
            _ = shutdown_token_clone.cancelled() => {
                tracing::info!("Listener shutdown requested");
            }
            result = handle_listener(task_sender, listener, keep_alive) => {
                if let Err(e) = result {
                    tracing::error!("Listener error: {}", snafu::Report::from_error(e));
                }
            }
        }
    }));

    let start_time = std::time::Instant::now();

    let status_forward_handle = status_channel.map(|mut receiver| {
        let status_sender = manager.get_task_sender();
        tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
            while let Some(response_sender) = receiver.recv().await {
                if status_sender
                    .send(ManagerTask::StatusQuery { response_sender })
                    .await
                    .is_err()
                {
                    break;
                }
            }
        }))
    });

    let shutdown_handle = {
        let shutdown_sender = manager.get_task_sender();
        tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
            shutdown_token.cancelled().await;
            let _ = shutdown_sender.send(ManagerTask::Shutdown).await;
        }))
    };

    // Drives lease expiry. A tick is a request to sweep, not a deadline: it is
    // dropped rather than queued behind a busy manager, since the next tick asks
    // for the same thing and a backlog of sweeps would only pile up while the loop
    // was already too busy to serve them.
    let sweep_handle = {
        let sweep_sender = manager.get_task_sender();
        let sweep_interval = server_lease_sweep_interval();
        tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
            let mut ticker = tokio::time::interval(sweep_interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                ticker.tick().await;
                // A tick that finds the queue full is dropped, not retried: the
                // next tick asks for the same sweep, and a backlog of them would
                // only grow while the manager was already behind.
                if let Err(TrySendError::Closed(_)) =
                    sweep_sender.try_send(ManagerTask::SweepServerLeases)
                {
                    break;
                }
            }
        }))
    };

    let mut router = Router::new(
        manager,
        direct_state,
        security,
        quic_capability,
        config,
        start_time,
    );
    loop {
        // `None` cannot happen while the manager holds its own sender.
        let Some(task) = router.manager.wait_for_task().await else {
            break;
        };

        router.reap_finished_tasks();
        if stun_task.as_ref().is_some_and(|task| task.is_finished()) {
            router.direct_state.capability = None;
        }
        if router.handle(task).is_break() {
            break;
        }
    }

    // Abort first, then wait. Dropping a JoinHandle after abort() does not
    // wait for the task to drop its AuthRuntime clone, so a UI restart can
    // still see auth.lock held.
    router.connection_tasks.abort_all();
    while router.connection_tasks.join_next().await.is_some() {}
    // The QUIC listener drains its own connection tasks before it ends.
    quic_stop.cancel();
    if let Some(task) = quic_task {
        let _ = task.await;
    }
    abort_and_wait(
        [listener_handle, shutdown_handle, sweep_handle]
            .into_iter()
            .chain(status_forward_handle),
    )
    .await;
    router.security.auth().shutdown_actor().await;
    tracing::info!("Server shutdown completed");
    Ok(())
}

/// The routing manager's state, and the one place that changes it.
///
/// Connection IDs, registrations, pending streams, per-namespace counts and rate
/// limits have a single writer: the relay's routing loop, which hands each task to
/// [`Router::handle`]. Nothing here awaits. The loop's only await is the receive
/// that produced the task, and a handler that waited on a connection's mailbox
/// would stall every other service, heartbeat and retirement behind it.
struct Router {
    manager: TaskManager,
    /// Registered control connections, by service key.
    server_conn_map: ServerConnMap,
    pending_streams: PendingStreams,
    namespace_rate_limits: NamespaceRateLimits,
    config: RoutingConfig,
    next_server_generation: u64,
    direct_state: direct::DirectState,
    /// Connection tasks and suspicion timers, aborted when the relay stops.
    connection_tasks: tokio::task::JoinSet<()>,
    security: ServerSecurity,
    quic_capability: Option<Arc<pb_mapper_protocol::command::QuicEndpoint>>,
    start_time: Instant,
}

impl Router {
    fn new(
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

    fn reap_finished_tasks(&mut self) {
        while self.connection_tasks.try_join_next().is_some() {}
    }

    /// Apply one task; `Break` once the relay is shutting down.
    fn handle(&mut self, task: ManagerTask) -> ControlFlow<()> {
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
                server_id,
                client_id,
                server_generation,
            } => self.on_stream(
                data_protocol,
                key,
                stream,
                session,
                server_id,
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

    fn on_accept(&mut self, stream: RelayStream, peer_addr: SocketAddr) {
        let conn_id = self.manager.get_conn_id(
            self.server_conn_map
                .iter()
                .flat_map(|(_, ids)| ids.iter().map(|v| v.conn_id)),
        );
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
        let security = self.security.clone();
        let quic_capability = self.quic_capability.clone();
        self.connection_tasks.spawn(async move {
            snafu_error_handle!(
                handle_conn(
                    conn_id,
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

    fn on_status_query(&self, response_sender: tokio::sync::oneshot::Sender<ServerStatusInfo>) {
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
    fn retire_server_conn(&mut self, key: &ImutableKey, conn_id: RemoteConnId, reason: &str) {
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
    fn release_namespace_rate_limit_if_idle(&mut self, namespace: u64) {
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

    // Registration leases: register, renew, suspect, confirm, retire, sweep.

    #[expect(
        clippy::too_many_arguments,
        reason = "the fields of `ManagerTask::Register`, passed on by `handle`"
    )]
    fn on_register(
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

    fn on_deregister_server_conn(&mut self, key: ImutableKey, conn_id: RemoteConnId) {
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

    fn on_server_conn_activity(&mut self, key: ImutableKey, conn_id: RemoteConnId) {
        let recorded = record_server_conn_activity(&mut self.server_conn_map, &key, conn_id);
        tracing::debug!(
            event = "server_conn_lease_renewed",
            key = %key,
            conn_id = %conn_id,
            recorded,
            "server control connection activity recorded"
        );
    }

    fn on_suspect(&mut self, key: ImutableKey, conn_id: RemoteConnId, generation: u64) {
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

    fn on_confirm(
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

    fn on_sweep(&mut self) {
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

    // Subscriptions and the streams that serve them, relayed or direct.

    fn on_subscribe(
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
    fn on_stream(
        &mut self,
        data_protocol: Option<u16>,
        key: ImutableKey,
        stream: RelayStream,
        session: ServerHeaderSession,
        server_id: RemoteConnId,
        client_id: RemoteConnId,
        server_generation: u64,
    ) {
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
                    server_id,
                    server_generation: expected_generation,
                    stream,
                    session,
                })
                .context(TaskCenterSendStreamRespToClientSnafu { conn_id: client_id })
        );
    }

    fn on_stream_ack(
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

    fn on_deregister_client_conn(
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

    fn on_direct(&mut self, task: direct::DirectTask) {
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

    // Read-only views for status and administrator requests, and the
    // administrator's retirement.

    fn on_status(
        &self,
        response_sender: tokio::sync::oneshot::Sender<PbConnResponse>,
        status: PbConnStatusReq,
        namespace: u64,
        conn_id: RemoteConnId,
    ) {
        let resp = match status {
            PbConnStatusReq::Sessions => PbConnResponse::Status(PbConnStatusResp::Sessions(
                self.direct_state.observations(namespace),
            )),
            PbConnStatusReq::RemoteId => {
                let scoped = self
                    .server_conn_map
                    .iter()
                    .filter(|(key, _)| split_scoped_service_key(key).0 == namespace)
                    .map(|(key, value)| (split_scoped_service_key(key).1, value))
                    .collect::<Vec<_>>();
                let registered_ids = scoped
                    .iter()
                    .flat_map(|(_, connections)| {
                        connections.iter().map(|connection| connection.conn_id)
                    })
                    .collect::<Vec<_>>();
                let client_ids = self
                    .pending_streams
                    .iter()
                    .filter_map(|(client_id, stream)| {
                        (split_scoped_service_key(&stream.key).0 == namespace).then_some(*client_id)
                    })
                    .collect::<Vec<_>>();
                PbConnResponse::Status(PbConnStatusResp::RemoteId {
                    server_map: format!("{scoped:?}"),
                    active: format!("registered={registered_ids:?}, clients={client_ids:?}"),
                    idle: "namespace scoped; use `pb-mapper admin connection list` for global inspection"
                        .to_string(),
                })
            }
            PbConnStatusReq::Keys => PbConnResponse::Status(PbConnStatusResp::Keys(
                self.server_conn_map
                    .keys()
                    .filter_map(|key| {
                        let (key_namespace, service_name) = split_scoped_service_key(key);
                        (key_namespace == namespace).then(|| service_name.to_string())
                    })
                    .collect(),
            )),
            PbConnStatusReq::Service { key, direct_query } => {
                let connections = service_status_connections(
                    &self.server_conn_map,
                    &compose_service_key(namespace, &key),
                );
                PbConnResponse::Status(PbConnStatusResp::Service {
                    key,
                    connections,
                    direct: direct_query
                        .filter(|q| *q)
                        .and(self.direct_state.capability.clone()),
                })
            }
        };
        snafu_error_handle!(
            response_sender
                .send(resp)
                .map_err(|_| MailboxError::Closed)
                .context(TaskCenterSendStatusRespSnafu { conn_id })
        );
    }

    fn on_admin_service_list(
        &self,
        key_id: Option<u64>,
        page: u32,
        page_size: u16,
        response_sender: tokio::sync::oneshot::Sender<AdminServicePage>,
    ) {
        let mut all = self
            .server_conn_map
            .iter()
            .filter_map(|(key, connections)| {
                let (namespace, service_name) = split_scoped_service_key(key);
                if key_id.is_some_and(|key_id| key_id != namespace) {
                    return None;
                }
                let first = connections.first()?;
                Some(AdminServiceInfo {
                    key_id: namespace,
                    namespace,
                    service_name: service_name.to_string(),
                    transport: if first.is_datagram { "udp" } else { "tcp" }.to_string(),
                    codec_enabled: first.need_codec,
                    connection_count: connections.len() as u32,
                })
            })
            .collect::<Vec<_>>();
        all.sort_by(|left, right| {
            left.namespace
                .cmp(&right.namespace)
                .then_with(|| left.service_name.cmp(&right.service_name))
        });
        let (items, next_page) = paginate(&all, page, page_size);
        let _ = response_sender.send(AdminServicePage {
            schema_version: 1,
            items,
            next_page,
        });
    }

    fn on_admin_connection_list(
        &self,
        key_id: Option<u64>,
        page: u32,
        page_size: u16,
        response_sender: tokio::sync::oneshot::Sender<AdminConnectionPage>,
    ) {
        let now = Instant::now();
        let mut all = self
            .server_conn_map
            .iter()
            .flat_map(|(key, connections)| {
                let (namespace, service_name) = split_scoped_service_key(key);
                connections.iter().filter_map(move |connection| {
                    if key_id.is_some_and(|key_id| key_id != namespace) {
                        return None;
                    }
                    Some(AdminConnectionInfo {
                        key_id: namespace,
                        namespace,
                        service_name: service_name.to_string(),
                        conn_id: connection.conn_id.into(),
                        generation: connection.generation,
                        protocol_version: connection.protocol_version,
                        healthy: connection.health == ServerConnHealth::Healthy,
                        transport: if connection.is_datagram { "udp" } else { "tcp" }.to_string(),
                        codec_enabled: connection.need_codec,
                        last_rx_age_ms: now.duration_since(connection.last_rx_at).as_millis()
                            as u64,
                    })
                })
            })
            .collect::<Vec<_>>();
        all.sort_by(|left, right| {
            left.namespace
                .cmp(&right.namespace)
                .then_with(|| left.service_name.cmp(&right.service_name))
                .then_with(|| left.conn_id.cmp(&right.conn_id))
        });
        let (items, next_page) = paginate(&all, page, page_size);
        let _ = response_sender.send(AdminConnectionPage {
            schema_version: 1,
            items,
            next_page,
        });
    }

    fn on_admin_connection_retire(
        &mut self,
        key: ImutableKey,
        conn_id: Option<RemoteConnId>,
        response_sender: tokio::sync::oneshot::Sender<u32>,
    ) {
        // Snapshot the targets before retiring any of them: `retire_server_conn`
        // mutates the very map that names them, and dropping the last connection
        // of a service removes its entry outright.
        let targets: Vec<RemoteConnId> = self
            .server_conn_map
            .get(&key)
            .map(|connections| {
                connections
                    .iter()
                    .map(|connection| connection.conn_id)
                    .filter(|candidate| conn_id.is_none_or(|wanted| wanted == *candidate))
                    .collect()
            })
            .unwrap_or_default();
        for target in &targets {
            self.retire_server_conn(&key, *target, "retired by administrator");
        }
        let _ = response_sender.send(u32::try_from(targets.len()).unwrap_or(u32::MAX));
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

async fn abort_and_wait(
    handles: impl IntoIterator<Item = tokio_util::task::AbortOnDropHandle<()>>,
) {
    let handles: Vec<_> = handles.into_iter().collect();
    for handle in &handles {
        handle.abort();
    }
    for handle in handles {
        let _ = handle.await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pb_mapper_auth::{AuthConfig, AuthRuntime, LegacyProtocolPolicy};
    use pb_mapper_core::test_support::PROCESS_CREDENTIAL_TEST_LOCK;
    use rand::RngExt;
    use std::path::PathBuf;
    use std::time::Duration;

    fn temp_state_dir(name: &str) -> PathBuf {
        let mut suffix = [0_u8; 8];
        let mut rng = rand::rng();
        for byte in &mut suffix {
            *byte = rng.random();
        }
        std::env::temp_dir().join(format!(
            "pb-mapper-{name}-{}",
            suffix
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        ))
    }

    fn server_conn_info(conn_id: u32, generation: u64) -> ServerConnInfo {
        ServerConnInfo {
            direct: None,
            conn_id: RemoteConnId::from(conn_id),
            generation,
            health: ServerConnHealth::Healthy,
            need_codec: false,
            is_datagram: false,
            protocol_version: CONTROL_PROTOCOL_V2,
            last_rx_at: Instant::now(),
            stream_ack_budget: pb_mapper_core::config::stream_ack_timeout(),
            suspect_check: None,
            retire_token: CancellationToken::new(),
        }
    }

    /// A retired connection's ID must not be handed out again until its own socket
    /// task has deregistered.
    ///
    /// The race this pins: retirement drops the routing entry while the socket task
    /// is still unwinding, and that task still owns a guard that will send
    /// `DeRegisterServerConn` for the same ID. If the ID were recycled at
    /// retirement, the next `Accept` would take it, a replacement could register
    /// under it, and the old guard's deregistration — which matches on ID alone —
    /// would remove the replacement's sender and its service-map entry instead.
    #[tokio::test]
    async fn a_retired_conn_id_is_withheld_until_its_socket_task_deregisters() {
        let state_dir = temp_state_dir("retired-id");
        let auth = AuthRuntime::from_isolated_state(AuthConfig {
            state_dir: state_dir.clone(),
            ..AuthConfig::default()
        })
        .await
        .unwrap();
        let mut router = Router::new(
            TaskManager::new(),
            direct::DirectState::new(None),
            ServerSecurity::new(auth.clone()),
            None,
            RoutingConfig::from_env(),
            Instant::now(),
        );
        let key: ImutableKey = Arc::from("echo");

        // One registered control connection, holding the first ID the manager hands out.
        let retired = router.manager.get_conn_id(std::iter::empty());
        let (conn_sender, _conn_receiver) = kanal::bounded_async(MAILBOX_CAPACITY);
        router.manager.sign_up_conn_sender(retired, conn_sender);
        router
            .server_conn_map
            .insert(key.clone(), vec![server_conn_info(retired.into(), 1)]);

        assert!(
            router
                .handle(ManagerTask::RetireServerConn {
                    key: key.clone(),
                    conn_id: retired,
                    reason: "retired by administrator".to_string(),
                })
                .is_continue()
        );
        assert!(
            !router.server_conn_map.contains_key(&key),
            "retirement unwinds the routing entry immediately"
        );
        assert!(
            router.manager.get_conn_sender_chan(&retired).is_none(),
            "the sender goes at retirement"
        );

        // The retired socket task has not unwound yet, so its ID is still spoken for.
        let replacement = router.manager.get_conn_id(
            router
                .server_conn_map
                .iter()
                .flat_map(|(_, ids)| ids.iter().map(|info| info.conn_id)),
        );
        assert_ne!(
            replacement, retired,
            "the retired id was handed to a new connection while its guard was still live"
        );

        // The replacement registers, and only then does the old guard's
        // deregistration arrive. It must not disturb the replacement.
        let (replacement_sender, _replacement_receiver) = kanal::bounded_async(MAILBOX_CAPACITY);
        router
            .manager
            .sign_up_conn_sender(replacement, replacement_sender);
        router
            .server_conn_map
            .insert(key.clone(), vec![server_conn_info(replacement.into(), 2)]);

        assert!(
            router
                .handle(ManagerTask::DeRegisterServerConn {
                    key: key.clone(),
                    conn_id: retired,
                })
                .is_continue()
        );

        assert!(
            router.manager.get_conn_sender_chan(&replacement).is_some(),
            "the replacement kept its sender"
        );
        assert_eq!(
            service_conn_count(&router.server_conn_map, &key),
            1,
            "the replacement kept its service-map entry"
        );

        // And the ID is available again now that its task is gone.
        assert_eq!(router.manager.get_conn_id(std::iter::empty()), retired);

        drop(router);
        auth.shutdown_actor().await;
        let _ = std::fs::remove_dir_all(state_dir);
    }

    /// The capacity of a connection's `ConnTask` mailbox; the value does not
    /// matter here.
    const MAILBOX_CAPACITY: usize = 8;

    #[tokio::test]
    async fn shutdown_releases_auth_lock_while_a_connection_is_open() {
        let _process_credential_guard = PROCESS_CREDENTIAL_TEST_LOCK.lock().await;
        let state_dir = temp_state_dir("shutdown-lock");
        let admin_key = *b"0123456789abcdefghijklmnopqrstuv";
        let config = AuthConfig {
            state_dir: state_dir.clone(),
            max_temporary_keys: 4,
            max_temporary_key_ttl: Duration::from_secs(3600),
            legacy_protocol: LegacyProtocolPolicy::Allow,
        };
        let auth = AuthRuntime::start(admin_key, config.clone()).await.unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let shutdown_token = CancellationToken::new();
        let server = tokio::spawn({
            let shutdown_token = shutdown_token.clone();
            async move { run_server_on_listener(listener, shutdown_token, None, false, auth).await }
        });
        let _client = tokio::net::TcpStream::connect(addr).await.unwrap();
        tokio::time::sleep(Duration::from_millis(80)).await;
        shutdown_token.cancel();
        tokio::time::timeout(Duration::from_secs(2), server)
            .await
            .expect("server shutdown should finish after aborting connections")
            .unwrap()
            .unwrap();
        let restarted = AuthRuntime::start(admin_key, config).await.unwrap();
        drop(restarted);
        tokio::time::sleep(Duration::from_millis(20)).await;
        let _ = std::fs::remove_dir_all(state_dir);
    }
}

#[cfg(test)]
mod cancellation_audit {
    use super::*;
    use std::{
        future::{Future, poll_fn},
        task::Poll,
    };
    #[tokio::test]
    async fn dropping_relay_future_must_release_its_listener() {
        let config = AuthConfig {
            state_dir: std::env::temp_dir()
                .join(format!("pb-audit-abort-{}", rand::random::<u64>())),
            ..AuthConfig::default()
        };
        let auth = AuthRuntime::from_isolated_state(config).await.unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let shutdown = CancellationToken::new();
        let mut relay = Box::pin(run_server_on_listener(
            listener,
            shutdown.clone(),
            None,
            false,
            auth.clone(),
        ));
        poll_fn(|cx| {
            assert!(relay.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        drop(relay);
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
        let rebound = TcpListener::bind(addr).await;
        let leaked = rebound.is_err();
        shutdown.cancel();
        auth.shutdown_actor().await;
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
        assert!(
            !leaked,
            "relay future dropped, but detached listener task still owns {addr}"
        );
    }
}
