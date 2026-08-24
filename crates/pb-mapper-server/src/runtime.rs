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

struct RemoteIdProvider {
    next_id: RemoteConnId,
}

impl RemoteIdProvider {
    fn new() -> Self {
        Self {
            next_id: RemoteConnId::default(),
        }
    }
}

impl ConnIdProvider<RemoteConnId> for RemoteIdProvider {
    fn get_next_id(&mut self) -> RemoteConnId {
        let ret = self.next_id;
        self.next_id += 1;
        ret
    }

    fn is_valid_id(&self, id: &RemoteConnId) -> bool {
        id < &self.next_id
    }
}
type ServerMananger = TaskManager<ManagerTask, ConnTask, RemoteConnId, RemoteIdProvider>;

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
        .map_err(|error| std::io::Error::other(error.to_string()))?;
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
    let security = ServerSecurity::new(auth);
    let mut manager = ServerMananger::new(RemoteIdProvider::new());
    // represent the mapping of the `key` to the id of the server-side conn
    let mut server_conn_map = ServerConnMap::new();
    let mut pending_streams =
        hashbrown::HashMap::<RemoteConnId, (RemoteConnId, u64, ImutableKey)>::new();
    let mut namespace_stream_counts = hashbrown::HashMap::<u64, usize>::new();
    let mut namespace_rate_limits = hashbrown::HashMap::<u64, NamespaceRateLimit>::new();
    let max_services_per_namespace = env_limit("PB_MAPPER_MAX_SERVICES_PER_NAMESPACE", 256);
    let max_register_connections_per_service =
        env_limit("PB_MAPPER_MAX_REGISTER_CONNECTIONS_PER_SERVICE", 16);
    let max_streams_per_namespace = env_limit("PB_MAPPER_MAX_STREAMS_PER_NAMESPACE", 1024);
    let new_streams_per_second = env_limit("PB_MAPPER_NEW_STREAMS_PER_SECOND", 100);
    let new_streams_burst = env_limit("PB_MAPPER_NEW_STREAMS_BURST", 200);
    let mut next_server_generation = 1_u64;
    let mut connection_tasks = tokio::task::JoinSet::new();

    let listen_addr = listener.local_addr()?;
    tracing::info!(
        event = "pb_server_listening",
        listen_addr = %listen_addr,
        control_timeout = ?control_io_timeout(),
        "pb-mapper server is listening"
    );

    let task_sender = manager.get_task_sender();
    let shutdown_token_clone = shutdown_token.clone();

    let listener_handle = tokio::spawn(async move {
        tokio::select! {
            result = handle_listener(task_sender, listener, keep_alive) => {
                if let Err(e) = result {
                    tracing::error!("Listener error: {}", e);
                }
            }
            _ = shutdown_token_clone.cancelled() => {
                tracing::info!("Listener shutdown requested");
            }
        }
    });

    let start_time = std::time::Instant::now();

    let status_forward_handle = status_channel.map(|mut receiver| {
        let status_sender = manager.get_task_sender();
        tokio::spawn(async move {
            while let Some(response_sender) = receiver.recv().await {
                if status_sender
                    .send(ManagerTask::StatusQuery { response_sender })
                    .await
                    .is_err()
                {
                    break;
                }
            }
        })
    });

    let shutdown_handle = {
        let shutdown_sender = manager.get_task_sender();
        tokio::spawn(async move {
            shutdown_token.cancelled().await;
            let _ = shutdown_sender.send(ManagerTask::Shutdown).await;
        })
    };

    // Drives lease expiry. A tick is a request to sweep, not a deadline: it is
    // dropped rather than queued behind a busy manager, since the next tick asks
    // for the same thing and a backlog of sweeps would only pile up while the loop
    // was already too busy to serve them.
    let sweep_handle = {
        let sweep_sender = manager.get_task_sender();
        let sweep_interval = server_lease_sweep_interval();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(sweep_interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                ticker.tick().await;
                // A tick that finds the queue full is dropped, not retried: the
                // next tick asks for the same sweep, and a backlog of them would
                // only grow while the manager was already behind.
                if let Err(kanal::SendTimeoutError::Closed(_)) =
                    sweep_sender.try_send(ManagerTask::SweepServerLeases)
                {
                    break;
                }
            }
        })
    };

    loop {
        let task = match manager.wait_for_task().await {
            Ok(task) => task,
            Err(e) => {
                tracing::error!("Manager task error: {}", e);
                break;
            }
        };

        match task {
            ManagerTask::AdminServiceList {
                key_id,
                page,
                page_size,
                response_sender,
            } => {
                let page_size = page_size.clamp(1, 1000) as usize;
                let start = (page as usize).saturating_mul(page_size);
                let mut all = server_conn_map
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
                let items = all.iter().skip(start).take(page_size).cloned().collect();
                let next_page =
                    (start.saturating_add(page_size) < all.len()).then_some(page.saturating_add(1));
                let _ = response_sender.send(AdminServicePage {
                    schema_version: 1,
                    items,
                    next_page,
                });
            }
            ManagerTask::AdminConnectionList {
                key_id,
                page,
                page_size,
                response_sender,
            } => {
                let now = Instant::now();
                let page_size = page_size.clamp(1, 1000) as usize;
                let start = (page as usize).saturating_mul(page_size);
                let mut all = server_conn_map
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
                                transport: if connection.is_datagram { "udp" } else { "tcp" }
                                    .to_string(),
                                codec_enabled: connection.need_codec,
                                last_rx_age_ms: now
                                    .duration_since(connection.last_rx_at)
                                    .as_millis()
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
                let items = all.iter().skip(start).take(page_size).cloned().collect();
                let next_page =
                    (start.saturating_add(page_size) < all.len()).then_some(page.saturating_add(1));
                let _ = response_sender.send(AdminConnectionPage {
                    schema_version: 1,
                    items,
                    next_page,
                });
            }
            ManagerTask::AdminConnectionRetire {
                key,
                conn_id,
                response_sender,
            } => {
                // Snapshot the targets before retiring any of them: `retire_server_conn`
                // mutates the very map that names them, and dropping the last connection
                // of a service removes its entry outright.
                let targets: Vec<RemoteConnId> = server_conn_map
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
                    retire_server_conn(
                        &mut manager,
                        &mut server_conn_map,
                        &pending_streams,
                        &mut namespace_rate_limits,
                        &key,
                        *target,
                        "retired by administrator",
                    );
                }
                let _ = response_sender.send(u32::try_from(targets.len()).unwrap_or(u32::MAX));
            }
            ManagerTask::StatusQuery { response_sender } => {
                let total_connections = server_conn_map
                    .values()
                    .map(|conns| conns.len() as u32)
                    .sum();

                let status_info = ServerStatusInfo {
                    active_connections: total_connections,
                    registered_services: server_conn_map.len() as u32,
                    uptime_seconds: start_time.elapsed().as_secs(),
                };

                // Send response back (ignore if receiver dropped)
                let _ = response_sender.send(status_info);
                tracing::debug!(
                    event = "status_query_served",
                    registered_services = server_conn_map.len(),
                    server_connections = total_connections,
                    active_connections = manager.active_conn_count(),
                    idle_connections = manager.idle_conn_count(),
                    "server status query served"
                );
            }
            ManagerTask::Status {
                conn_sender,
                status,
                namespace,
                conn_id,
            } => {
                let resp = match status {
                    PbConnStatusReq::RemoteId => {
                        let scoped = server_conn_map
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
                        let client_ids = pending_streams
                            .iter()
                            .filter_map(|(client_id, (_, _, key))| {
                                (split_scoped_service_key(key).0 == namespace).then_some(*client_id)
                            })
                            .collect::<Vec<_>>();
                        PbConnResponse::Status(PbConnStatusResp::RemoteId {
                            server_map: format!("{scoped:?}"),
                            active: format!(
                                "registered={registered_ids:?}, clients={client_ids:?}"
                            ),
                            idle: "namespace scoped; use `pb-mapper admin connection list` for global inspection"
                                .to_string(),
                        })
                    }
                    PbConnStatusReq::Keys => PbConnResponse::Status(PbConnStatusResp::Keys(
                        server_conn_map
                            .keys()
                            .filter_map(|key| {
                                let (key_namespace, service_name) = split_scoped_service_key(key);
                                (key_namespace == namespace).then(|| service_name.to_string())
                            })
                            .collect(),
                    )),
                    PbConnStatusReq::Service { key } => {
                        let display_key = key.clone();
                        let key: ImutableKey = if namespace == 0 {
                            key.into()
                        } else {
                            Arc::from(format!("@{namespace:016x}\u{0}{key}"))
                        };
                        PbConnResponse::Status(PbConnStatusResp::Service {
                            key: display_key,
                            connections: service_status_connections(&server_conn_map, &key),
                        })
                    }
                };
                snafu_error_get_or_continue!(
                    conn_sender
                        .send(ConnTask::StatusResp(resp))
                        .await
                        .map_err(|_| kanal::SendError(()))
                        .context(TaskCenterSendStatusRespSnafu { conn_id })
                );
            }
            ManagerTask::Accept { stream, peer_addr } => {
                let conn_id = manager.get_conn_id(
                    server_conn_map
                        .iter()
                        .flat_map(|(_, ids)| ids.iter().map(|v| v.conn_id)),
                );
                tracing::info!(
                    event = "conn_accepted",
                    conn_id = %conn_id,
                    peer_addr = %peer_addr,
                    registered_services = server_conn_map.len(),
                    server_connections = registered_server_conn_count(&server_conn_map),
                    active_connections = manager.active_conn_count(),
                    idle_connections = manager.idle_conn_count(),
                    "accepted pb connection"
                );
                let manager_task_sender = manager.get_task_sender();
                let security = security.clone();
                while connection_tasks.try_join_next().is_some() {}
                connection_tasks.spawn(async move {
                    snafu_error_handle!(
                        handle_conn(conn_id, peer_addr, manager_task_sender, stream, security)
                            .await
                    );
                });
            }
            ManagerTask::DeRegisterServerConn { key, conn_id } => {
                let removed_from_service_map =
                    remove_server_conn(&mut server_conn_map, &key, conn_id);
                let removed_from_active_map = manager.deregister_conn(conn_id);
                release_namespace_rate_limit_if_idle(
                    split_scoped_service_key(&key).0,
                    &server_conn_map,
                    &pending_streams,
                    &mut namespace_rate_limits,
                );
                tracing::info!(
                    event = "server_conn_deregistered",
                    key = %key,
                    conn_id = %conn_id,
                    removed_from_service_map,
                    removed_from_active_map,
                    registered_services = server_conn_map.len(),
                    server_connections = registered_server_conn_count(&server_conn_map),
                    active_connections = manager.active_conn_count(),
                    idle_connections = manager.idle_conn_count(),
                    "server connection deregistered"
                );
            }
            ManagerTask::ServerConnActivity { key, conn_id } => {
                let recorded = record_server_conn_activity(&mut server_conn_map, &key, conn_id);
                tracing::debug!(
                    event = "server_conn_lease_renewed",
                    key = %key,
                    conn_id = %conn_id,
                    recorded,
                    "server control connection activity recorded"
                );
            }
            ManagerTask::RetireServerConn {
                key,
                conn_id,
                reason,
            } => {
                retire_server_conn(
                    &mut manager,
                    &mut server_conn_map,
                    &pending_streams,
                    &mut namespace_rate_limits,
                    &key,
                    conn_id,
                    &reason,
                );
            }
            ManagerTask::DeRegisterClientConn {
                server_id,
                client_id,
            } => {
                let removed_namespace = pending_streams.remove(&client_id).map(|(_, _, key)| {
                    let namespace = split_scoped_service_key(&key).0;
                    decrement_namespace_stream_count(&mut namespace_stream_counts, namespace);
                    namespace
                });
                let removed_server_conn = if let Some(server_id) = server_id {
                    manager.deregister_conn(server_id)
                } else {
                    false
                };
                let removed_client_conn = manager.deregister_conn(client_id);
                if let Some(namespace) = removed_namespace {
                    release_namespace_rate_limit_if_idle(
                        namespace,
                        &server_conn_map,
                        &pending_streams,
                        &mut namespace_rate_limits,
                    );
                }
                if removed_server_conn || removed_client_conn {
                    tracing::info!(
                        event = "client_conn_deregistered",
                        server_conn_id = ?server_id,
                        client_conn_id = %client_id,
                        removed_server_conn,
                        removed_client_conn,
                        registered_services = server_conn_map.len(),
                        server_connections = registered_server_conn_count(&server_conn_map),
                        active_connections = manager.active_conn_count(),
                        idle_connections = manager.idle_conn_count(),
                        "client connection deregistered"
                    );
                } else {
                    tracing::debug!(
                        event = "client_conn_deregister_skipped",
                        server_conn_id = ?server_id,
                        client_conn_id = %client_id,
                        registered_services = server_conn_map.len(),
                        server_connections = registered_server_conn_count(&server_conn_map),
                        active_connections = manager.active_conn_count(),
                        idle_connections = manager.idle_conn_count(),
                        "client connection was already inactive"
                    );
                }
            }
            ManagerTask::Register {
                key,
                conn_id,
                conn_sender,
                need_codec,
                is_datagram,
                protocol_version,
            } => {
                let namespace = split_scoped_service_key(&key).0;
                let existing = server_conn_map.get(&key);
                let failure = if existing.is_some_and(|connections| {
                    connections
                        .first()
                        .is_some_and(|connection| connection.is_datagram != is_datagram)
                }) {
                    Some((
                        "service_transport_mismatch",
                        "the service name is already registered with a different transport",
                        false,
                    ))
                } else if existing.is_some_and(|connections| {
                    connections.len() >= max_register_connections_per_service
                }) {
                    Some((
                        "service_connection_limit_exceeded",
                        "the service has reached its register connection limit",
                        true,
                    ))
                } else if existing.is_none()
                    && server_conn_map
                        .keys()
                        .filter(|registered| split_scoped_service_key(registered).0 == namespace)
                        .count()
                        >= max_services_per_namespace
                {
                    Some((
                        "namespace_service_limit_exceeded",
                        "the namespace has reached its service name limit",
                        true,
                    ))
                } else {
                    None
                };
                if let Some((code, reason, retryable)) = failure {
                    let _ = conn_sender
                        .send(ConnTask::RegisterFailed {
                            code: code.to_string(),
                            reason: reason.to_string(),
                            retryable,
                        })
                        .await;
                    continue;
                }
                let generation = next_server_generation;
                next_server_generation = next_server_generation.saturating_add(1).max(1);
                let now = Instant::now();
                // sign up server connection
                manager.sign_up_conn_sender(conn_id, conn_sender.clone());
                match server_conn_map.entry(key.clone()) {
                    hashbrown::hash_map::Entry::Occupied(mut o) => {
                        o.get_mut().push(ServerConnInfo {
                            conn_id,
                            generation,
                            health: ServerConnHealth::Healthy,
                            need_codec,
                            is_datagram,
                            protocol_version,
                            last_rx_at: now,
                        });
                    }
                    hashbrown::hash_map::Entry::Vacant(v) => {
                        v.insert(vec![ServerConnInfo {
                            conn_id,
                            generation,
                            health: ServerConnHealth::Healthy,
                            need_codec,
                            is_datagram,
                            protocol_version,
                            last_rx_at: now,
                        }]);
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
                    service_connections = service_conn_count(&server_conn_map, &key),
                    registered_services = server_conn_map.len(),
                    server_connections = registered_server_conn_count(&server_conn_map),
                    active_connections = manager.active_conn_count(),
                    idle_connections = manager.idle_conn_count(),
                    "server connection registered"
                );
                snafu_error_get_or_continue!(
                    conn_sender
                        .send(ConnTask::RegisterResp {
                            generation,
                            protocol_version,
                            lease_ttl_ms: server_lease_timeout().as_millis() as u64,
                        })
                        .await
                        .map_err(|_| kanal::SendError(()))
                        .context(TaskCenterSendRegisterRespSnafu { key, conn_id })
                );
            }
            ManagerTask::Stream {
                key,
                stream,
                session,
                server_id,
                client_id,
                server_generation,
            } => {
                let Some((expected_control_conn_id, expected_generation, expected_key)) =
                    pending_streams.get(&client_id).cloned()
                else {
                    tracing::warn!(
                        event = "stale_stream_without_pending_client",
                        server_conn_id = %server_id,
                        client_conn_id = %client_id,
                        server_generation,
                        "dropping stream for client without pending subscribe"
                    );
                    continue;
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
                    continue;
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
                    continue;
                }
                if let Some(info) = server_conn_map
                    .values_mut()
                    .flat_map(|infos| infos.iter_mut())
                    .find(|info| {
                        info.conn_id == expected_control_conn_id
                            && info.generation == expected_generation
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
                    active_connections = manager.active_conn_count(),
                    "server stream ready for client"
                );
                let client_sender = snafu_error_get_or_continue!(
                    manager
                        .get_conn_sender_chan(&client_id)
                        .context(TaskCenterStreamConnIdNotExistSnafu { conn_id: client_id })
                );
                snafu_error_handle!(
                    client_sender
                        .send(ConnTask::StreamResp {
                            server_id,
                            server_generation: expected_generation,
                            stream,
                            session,
                        })
                        .await
                        .map_err(|_| kanal::SendError(()))
                        .context(TaskCenterSendStreamRespToClientSnafu { conn_id: client_id })
                );
            }
            ManagerTask::StreamAck {
                server_id,
                client_id,
                server_generation,
            } => {
                let recorded_activity =
                    record_server_conn_activity_by_conn_id(&mut server_conn_map, server_id);
                let Some((expected_server_id, expected_generation, _)) =
                    pending_streams.get(&client_id).cloned()
                else {
                    tracing::warn!(
                        event = "stale_stream_ack_without_pending_client",
                        server_conn_id = %server_id,
                        client_conn_id = %client_id,
                        server_generation,
                        recorded_activity,
                        "dropping stream ack for client without pending subscribe"
                    );
                    continue;
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
                    continue;
                }
                if let Some(info) = server_conn_map
                    .values_mut()
                    .flat_map(|infos| infos.iter_mut())
                    .find(|info| info.conn_id == server_id && info.generation == server_generation)
                {
                    info.health = ServerConnHealth::Healthy;
                }
                let client_sender = snafu_error_get_or_continue!(
                    manager
                        .get_conn_sender_chan(&client_id)
                        .context(TaskCenterStreamConnIdNotExistSnafu { conn_id: client_id })
                );
                snafu_error_handle!(
                    client_sender
                        .send(ConnTask::StreamAck {
                            server_id,
                            server_generation,
                        })
                        .await
                        .map_err(|_| kanal::SendError(()))
                        .context(TaskCenterSendStreamRespToClientSnafu { conn_id: client_id })
                );
            }
            ManagerTask::Subcribe {
                key,
                conn_id,
                conn_sender,
                excluded_server_conns,
            } => {
                let namespace = split_scoped_service_key(&key).0;
                if namespace_stream_counts
                    .get(&namespace)
                    .copied()
                    .unwrap_or_default()
                    >= max_streams_per_namespace
                {
                    let _ = conn_sender
                        .send(ConnTask::SubcribeFailed {
                            code: "namespace_stream_limit_exceeded".to_string(),
                            reason: "the namespace has reached its active stream limit".to_string(),
                            retryable: true,
                        })
                        .await;
                    continue;
                }
                let Some(server_conn_id_list) = server_conn_map.get(&key).cloned() else {
                    let reason = format!("server key `{key}` is not registered");
                    tracing::warn!(
                        event = "subscribe_key_missing",
                        key = %key,
                        client_conn_id = %conn_id,
                        excluded_server_conns = ?excluded_server_conns,
                        registered_services = server_conn_map.len(),
                        server_connections = registered_server_conn_count(&server_conn_map),
                        "subscribe key is not registered"
                    );
                    if excluded_server_conns.is_empty() {
                        send_subcribe_failed(&conn_sender, &key, conn_id, reason).await;
                    } else {
                        send_subcribe_retry(&conn_sender, &key, conn_id, reason).await;
                    }
                    continue;
                };
                if !namespace_rate_limits
                    .entry(namespace)
                    .or_insert_with(|| {
                        NamespaceRateLimit::new(new_streams_per_second, new_streams_burst)
                    })
                    .allow()
                {
                    let _ = conn_sender
                        .send(ConnTask::SubcribeFailed {
                            code: "namespace_stream_rate_exceeded".to_string(),
                            reason: "the namespace new-stream rate limit was exceeded".to_string(),
                            retryable: true,
                        })
                        .await;
                    continue;
                }
                let mut selected = false;
                let mut candidates = Vec::new();
                candidates.extend(server_conn_id_list.iter().rev().copied().filter(|info| {
                    info.health == ServerConnHealth::Healthy
                        && !excluded_server_conns.contains(&(info.conn_id, info.generation))
                }));
                for server_info in candidates {
                    let ServerConnInfo {
                        conn_id: server_conn_id,
                        generation: server_generation,
                        health,
                        need_codec,
                        is_datagram,
                        protocol_version: _,
                        last_rx_at: _,
                    } = server_info;
                    let Some(server_conn_sender) = manager.get_conn_sender_chan(&server_conn_id)
                    else {
                        tracing::warn!(
                            event = "subscribe_stale_server_conn",
                            key = %key,
                            client_conn_id = %conn_id,
                            server_conn_id = %server_conn_id,
                            reason = "sender_not_found",
                            "subscribe skipped stale server connection"
                        );
                        remove_server_conn(&mut server_conn_map, &key, server_conn_id);
                        let _ = manager.deregister_conn(server_conn_id);
                        continue;
                    };
                    // 1. Send a request to get server stream
                    if let Err(e) = server_conn_sender
                        .send(ConnTask::StreamReq {
                            client_id: conn_id,
                            server_generation,
                        })
                        .await
                        .map_err(|_| kanal::SendError(()))
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
                        remove_server_conn(&mut server_conn_map, &key, server_conn_id);
                        let _ = manager.deregister_conn(server_conn_id);
                        continue;
                    }
                    // sign up client connection after a server accepted the stream request
                    if manager.get_conn_sender_chan(&conn_id).is_none() {
                        manager.sign_up_conn_sender(conn_id, conn_sender.clone());
                    }
                    let is_new_stream = pending_streams
                        .insert(conn_id, (server_conn_id, server_generation, key.clone()))
                        .is_none();
                    if is_new_stream {
                        *namespace_stream_counts.entry(namespace).or_default() += 1;
                    }
                    // 2. Response subcribe ok
                    if let Err(e) = conn_sender
                        .send(ConnTask::SubcribeResp {
                            server_conn_id,
                            server_generation,
                            need_codec,
                            is_datagram,
                        })
                        .await
                        .map_err(|_| kanal::SendError(()))
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
                        manager.deregister_conn(conn_id);
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
                        service_connections = service_conn_count(&server_conn_map, &key),
                        active_connections = manager.active_conn_count(),
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
                        registered_services = server_conn_map.len(),
                        server_connections = registered_server_conn_count(&server_conn_map),
                        "no usable server connection for subscribe"
                    );
                    if excluded_server_conns.is_empty() {
                        send_subcribe_failed(&conn_sender, &key, conn_id, reason).await;
                    } else {
                        send_subcribe_retry(&conn_sender, &key, conn_id, reason).await;
                    }
                }
            }
            ManagerTask::SweepServerLeases => {
                // Retire in place rather than queueing `RetireServerConn` back into
                // the manager channel: this loop is that channel's only consumer, so
                // a full queue would have it waiting on itself.
                for stale in sweep_server_conn_leases(&mut server_conn_map) {
                    let reason = format!(
                        "lease expired: no control traffic for {:?} (protocol v{})",
                        stale.idle_for, stale.protocol_version
                    );
                    retire_server_conn(
                        &mut manager,
                        &mut server_conn_map,
                        &pending_streams,
                        &mut namespace_rate_limits,
                        &stale.key,
                        stale.conn_id,
                        &reason,
                    );
                }
            }
            ManagerTask::Shutdown => {
                tracing::info!("Server shutdown requested, stopping main loop");
                break;
            }
        }
    }

    // Abort first, then wait. Dropping a JoinHandle after abort() does not
    // wait for the task to drop its AuthRuntime clone, so a UI restart can
    // still see auth.lock held.
    connection_tasks.abort_all();
    while connection_tasks.join_next().await.is_some() {}
    abort_and_wait(
        [listener_handle, shutdown_handle, sweep_handle]
            .into_iter()
            .chain(status_forward_handle),
    )
    .await;
    security.auth().shutdown_actor().await;
    tracing::info!("Server shutdown completed");
    Ok(())
}

/// Drop one registered control connection from every map that tracks it, and tell
/// its socket task to unwind.
///
/// Shared by every caller that retires a connection — a subscriber that found it
/// unresponsive, and the lease sweep — so an operator reads one `server_conn_retired`
/// record with one shape regardless of which noticed.
///
/// The `ConnTask::Retire` notification is best-effort: a task whose queue is full or
/// already gone is a task that is not going to serve traffic either way, and the maps
/// have already been unwound by the time it is sent.
fn retire_server_conn(
    manager: &mut ServerMananger,
    server_conn_map: &mut ServerConnMap,
    pending_streams: &hashbrown::HashMap<RemoteConnId, (RemoteConnId, u64, ImutableKey)>,
    namespace_rate_limits: &mut hashbrown::HashMap<u64, NamespaceRateLimit>,
    key: &ImutableKey,
    conn_id: RemoteConnId,
    reason: &str,
) {
    let conn_sender = manager.get_conn_sender_chan(&conn_id);
    let removed_from_service_map = remove_server_conn(server_conn_map, key, conn_id);
    let removed_from_active_map = manager.deregister_conn(conn_id);
    release_namespace_rate_limit_if_idle(
        split_scoped_service_key(key).0,
        server_conn_map,
        pending_streams,
        namespace_rate_limits,
    );
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
    tracing::warn!(
        event = "server_conn_retired",
        key = %key,
        conn_id = %conn_id,
        reason = %reason,
        removed_from_service_map,
        removed_from_active_map,
        retire_notified,
        registered_services = server_conn_map.len(),
        server_connections = registered_server_conn_count(server_conn_map),
        active_connections = manager.active_conn_count(),
        idle_connections = manager.idle_conn_count(),
        "server connection retired"
    );
}

async fn abort_and_wait(handles: impl IntoIterator<Item = tokio::task::JoinHandle<()>>) {
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
        let _client = TcpStream::connect(addr).await.unwrap();
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
