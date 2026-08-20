//! Per-connection admission, authentication, namespace resolution, and role dispatch.
//!
//! ```text
//! accepted TCP socket
//!       |
//!       v
//! bounded V2/legacy first frame -> AuthContext -> namespace policy
//!       |                                      |
//!       +-> structured auth error              +-> register / subscribe / stream
//!                                              +-> status / administrator request
//! ```
//!
//! Long-lived register, subscribe, and status futures are raced against the
//! credential's cancellation token here. This outer guard closes a subscriber even
//! when the paired service stream belongs to a different credential.

use super::*;

pub(super) async fn handle_listener(
    task_sender: ManagerTaskSender,
    listener: TcpListener,
    keep_alive: bool,
) -> Result<()> {
    loop {
        let (stream, addr) = listener.accept().await.context(ServerListenSnafu)?;
        tracing::debug!(
            event = "tcp_conn_accepted",
            peer_addr = %addr,
            "accepted tcp connection"
        );
        // set keepalive (optional) and nodelay
        if keep_alive {
            snafu_error_handle!(set_tcp_keep_alive(&stream).context(TaskCenterSetKeepAliveSnafu));
        }
        snafu_error_handle!(set_tcp_nodelay(&stream), "remote stream set nodelay");
        task_sender
            .send(ManagerTask::Accept {
                stream,
                peer_addr: addr,
            })
            .await
            .map_err(|_| kanal::SendError(()))
            .context(TaskCenterSendListenerSnafu)?
    }
}

#[instrument(skip(manager_task_sender, conn, security), fields(conn_id = %conn_id, peer_addr = %peer_addr))]
pub(super) async fn handle_conn(
    conn_id: RemoteConnId,
    peer_addr: SocketAddr,
    manager_task_sender: ManagerTaskSender,
    mut conn: TcpStream,
    security: ServerSecurity,
) -> Result<()> {
    let timeout = control_io_timeout();
    let initial = match tokio::time::timeout(timeout, security.read_initial(&mut conn)).await {
        Err(_) => TaskCenterInitRequestTimeoutSnafu { conn_id, timeout }.fail()?,
        Ok(Err(error)) => {
            let key_id = error
                .response_session
                .as_ref()
                .map(|session| session.key_id())
                .or(error.presented_key_id)
                .unwrap_or(ADMIN_KEY_ID);
            let decision = security.record_failure_log(peer_addr.ip(), key_id, &error.failure.code);
            if decision.suppressed > 0 {
                tracing::warn!(
                    event = "auth_failures_suppressed",
                    peer_ip = %peer_addr.ip(),
                    key_id = key_id.as_u64(),
                    reason = %error.failure.code,
                    suppressed = decision.suppressed,
                    "suppressed repeated authentication failures in the previous window"
                );
            }
            if decision.emit {
                tracing::warn!(
                    event = "auth_failed",
                    auth_stage = "initial_frame",
                    conn_id = %conn_id,
                    peer_addr = %peer_addr,
                    key_id = key_id.as_u64(),
                    reason = %error.failure.code,
                    retryable = error.failure.retryable,
                    error = %error.failure.message,
                    "connection authentication failed"
                );
            }
            if let Some(session) = error.response_session {
                write_protocol_error(&mut conn, &session, &error.failure).await;
            }
            return Ok(());
        }
        Ok(Ok(initial)) => initial,
    };
    let replay_fingerprint = initial.replay_fingerprint;
    let client_timestamp = initial.client_timestamp;
    let init_request = match PbConnRequest::decode(&initial.payload) {
        Ok(request) => request,
        Err(error) => {
            tracing::warn!(
                event = "auth_failed",
                auth_stage = "request_decode",
                conn_id = %conn_id,
                peer_addr = %peer_addr,
                error = %error,
                "authenticated request could not be decoded"
            );
            write_protocol_error(
                &mut conn,
                &initial.session,
                &crate::common::auth::AuthFailure::new(
                    "request_decode_failed",
                    "authenticated request payload is malformed",
                    false,
                ),
            )
            .await;
            return Ok(());
        }
    };
    let mut requested_namespace = None;
    let mut force_register_namespace = false;
    let init_request = match init_request {
        PbConnRequest::RegisterScoped {
            need_codec,
            is_datagram,
            key,
            namespace,
            force_namespace,
            protocol_version,
            client_instance_id,
            heartbeat_interval_ms,
            heartbeat_tolerance_ms,
        } => {
            requested_namespace = Some(namespace);
            force_register_namespace = force_namespace;
            PbConnRequest::Register {
                need_codec,
                is_datagram,
                key,
                protocol_version,
                client_instance_id,
                heartbeat_interval_ms,
                heartbeat_tolerance_ms,
            }
        }
        PbConnRequest::SubcribeScoped { key, namespace } => {
            requested_namespace = Some(namespace);
            PbConnRequest::Subcribe { key }
        }
        PbConnRequest::StatusScoped { status, namespace } => {
            requested_namespace = Some(namespace);
            PbConnRequest::Status(status)
        }
        PbConnRequest::StreamScoped {
            key,
            namespace,
            dst_id,
            server_generation,
        } => {
            requested_namespace = Some(namespace);
            PbConnRequest::Stream {
                key,
                dst_id,
                server_generation,
            }
        }
        request => request,
    };
    let session = initial.session;
    let auth_context = match session.context() {
        Ok(context) => context.clone(),
        Err(error) => {
            tracing::warn!(conn_id = %conn_id, peer_addr = %peer_addr, %error, "missing auth context");
            return Ok(());
        }
    };
    tracing::info!(
        event = "auth_succeeded",
        auth_stage = "session",
        conn_id = %conn_id,
        peer_addr = %peer_addr,
        key_id = auth_context.key_id.as_u64(),
        namespace = auth_context.namespace,
        protocol = ?session.protocol(),
        is_admin = auth_context.is_admin,
        "connection authentication succeeded"
    );
    let effective_namespace = match resolve_namespace(
        &auth_context,
        requested_namespace,
        force_register_namespace,
        matches!(&init_request, PbConnRequest::Register { .. }),
    ) {
        Ok(namespace) => namespace,
        Err(failure) => {
            write_protocol_error(&mut conn, &session, &failure).await;
            return Ok(());
        }
    };
    match init_request {
        PbConnRequest::Register {
            key,
            need_codec,
            is_datagram,
            protocol_version,
            client_instance_id,
            heartbeat_interval_ms,
            heartbeat_tolerance_ms,
        } => {
            let protocol_version = protocol_version.unwrap_or(1);
            tracing::info!(
                event = "init_request",
                request = "register",
                conn_id = %conn_id,
                peer_addr = %peer_addr,
                key = %key,
                protocol_version,
                client_instance_id = ?client_instance_id,
                heartbeat_interval_ms = ?heartbeat_interval_ms,
                heartbeat_tolerance_ms = ?heartbeat_tolerance_ms,
                need_codec,
                is_datagram,
                "received pb init request"
            );
            let Some(key) = scope_service_or_reject(
                &mut conn,
                &session,
                &auth_context,
                effective_namespace,
                &key,
            )
            .await?
            else {
                return Ok(());
            };
            run_while_credential_active(
                conn,
                session,
                &auth_context,
                conn_id,
                "registered service connection",
                |conn, session| {
                    handle_server_conn(
                        ServerRegistration {
                            key,
                            need_codec,
                            is_datagram,
                            protocol_version,
                            conn_id,
                        },
                        manager_task_sender,
                        conn,
                        session,
                    )
                },
            )
            .await?;
        }
        PbConnRequest::Subcribe { key } => {
            tracing::info!(
                event = "init_request",
                request = "subscribe",
                conn_id = %conn_id,
                peer_addr = %peer_addr,
                key = %key,
                "received pb init request"
            );
            let Some(key) = scope_service_or_reject(
                &mut conn,
                &session,
                &auth_context,
                effective_namespace,
                &key,
            )
            .await?
            else {
                return Ok(());
            };
            run_while_credential_active(
                conn,
                session,
                &auth_context,
                conn_id,
                "subscribed data connection",
                |conn, session| {
                    handle_client_conn(key, conn_id, manager_task_sender, conn, session)
                },
            )
            .await?;
        }
        PbConnRequest::Stream {
            key,
            dst_id,
            server_generation,
        } => {
            tracing::debug!(
                event = "init_request",
                request = "stream",
                conn_id = %conn_id,
                peer_addr = %peer_addr,
                key = %key,
                client_conn_id = dst_id,
                server_generation,
                "received pb init request"
            );
            let Some(key) = scope_service_or_reject(
                &mut conn,
                &session,
                &auth_context,
                effective_namespace,
                &key,
            )
            .await?
            else {
                return Ok(());
            };
            manager_task_sender
                .send(ManagerTask::Stream {
                    key: key.clone(),
                    stream: conn,
                    session,
                    server_id: conn_id,
                    client_id: dst_id.into(),
                    server_generation,
                })
                .await
                .map_err(|_| kanal::SendError(()))
                .context(TaskCenterSendStreamRespToManagerSnafu { key, conn_id })?;
        }
        PbConnRequest::Status(status) => {
            tracing::debug!(
                event = "init_request",
                request = "status",
                conn_id = %conn_id,
                peer_addr = %peer_addr,
                status = ?status,
                "received pb init request"
            );
            run_while_credential_active(
                conn,
                session,
                &auth_context,
                conn_id,
                "status request",
                |conn, session| {
                    handle_show_status(
                        status,
                        effective_namespace,
                        manager_task_sender,
                        conn_id,
                        conn,
                        session,
                    )
                },
            )
            .await?;
        }
        PbConnRequest::Admin(request) => {
            if !auth_context.is_admin {
                write_protocol_error(
                    &mut conn,
                    &session,
                    &crate::common::auth::AuthFailure::new(
                        "admin_permission_required",
                        "administrator credential is required for this operation",
                        false,
                    ),
                )
                .await;
                return Ok(());
            }
            if session.protocol() != HeaderProtocol::V2 {
                write_protocol_error(
                    &mut conn,
                    &session,
                    &crate::common::auth::AuthFailure::new(
                        "admin_protocol_v2_required",
                        "administrator operations require protocol v2",
                        false,
                    ),
                )
                .await;
                return Ok(());
            }
            if request.is_mutating() {
                let Some(fingerprint) = replay_fingerprint else {
                    unreachable!("protocol-v2 sessions always carry a replay fingerprint");
                };
                let Some(client_timestamp) = client_timestamp else {
                    unreachable!("protocol-v2 sessions always carry a client timestamp");
                };
                if let Err(failure) = security
                    .auth()
                    .claim_admin_mutation(&auth_context, fingerprint, client_timestamp)
                    .await
                {
                    write_protocol_error(&mut conn, &session, &failure).await;
                    return Ok(());
                }
            }
            handle_admin_request(
                request,
                auth_context,
                security.auth().clone(),
                manager_task_sender,
                conn_id,
                conn,
                session,
            )
            .await?;
        }
        PbConnRequest::RegisterScoped { .. }
        | PbConnRequest::SubcribeScoped { .. }
        | PbConnRequest::StatusScoped { .. }
        | PbConnRequest::StreamScoped { .. } => unreachable!("scoped request was normalized"),
    }
    Ok(())
}

async fn scope_service_or_reject(
    conn: &mut TcpStream,
    session: &ServerHeaderSession,
    auth_context: &AuthContext,
    namespace: u64,
    service_name: &str,
) -> Result<Option<ImutableKey>> {
    match scoped_service_key(auth_context, namespace, service_name) {
        Ok(key) => Ok(Some(key)),
        Err(failure) => {
            write_protocol_error(conn, session, &failure).await;
            Ok(None)
        }
    }
}

async fn run_while_credential_active<F, Fut>(
    mut conn: TcpStream,
    session: ServerHeaderSession,
    auth_context: &AuthContext,
    conn_id: RemoteConnId,
    closed_what: &'static str,
    work: F,
) -> Result<()>
where
    F: FnOnce(TcpStream, ServerHeaderSession) -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
    let cancellation = match auth_context.cancellation_token() {
        Ok(token) => token,
        Err(failure) => {
            write_protocol_error(&mut conn, &session, &failure).await;
            return Ok(());
        }
    };
    tokio::select! {
        result = work(conn, session) => result?,
        _ = cancellation.cancelled() => {
            tracing::info!(
                event = "connection_auth_expired",
                key_id = auth_context.key_id.as_u64(),
                conn_id = %conn_id,
                "closing {closed_what}"
            );
        }
    }
    Ok(())
}

async fn write_protocol_error(
    conn: &mut TcpStream,
    session: &ServerHeaderSession,
    failure: &crate::common::auth::AuthFailure,
) {
    let response = PbConnResponse::error(
        failure.code.clone(),
        failure.message.clone(),
        failure.retryable,
    );
    let Ok(message) = response.encode() else {
        return;
    };
    let Ok(mut writer) = session.response_writer(conn) else {
        return;
    };
    if let Err(error) = writer.write_msg(&message).await {
        tracing::debug!(%error, reason = %failure.code, "failed to write structured protocol error");
    }
}

fn resolve_namespace(
    context: &AuthContext,
    requested: Option<u64>,
    force_register_namespace: bool,
    is_register: bool,
) -> std::result::Result<u64, crate::common::auth::AuthFailure> {
    let namespace = requested.unwrap_or(context.namespace);
    if !context.is_admin && namespace != context.namespace {
        return Err(crate::common::auth::AuthFailure::new(
            "namespace_access_denied",
            "temporary credentials can only access their own namespace",
            false,
        ));
    }
    if context.is_admin && is_register && namespace != 0 && !force_register_namespace {
        return Err(crate::common::auth::AuthFailure::new(
            "namespace_force_required",
            "administrator registration in a temporary namespace requires --force",
            false,
        ));
    }
    Ok(namespace)
}

fn scoped_service_key(
    context: &AuthContext,
    namespace: u64,
    service_name: &str,
) -> std::result::Result<ImutableKey, crate::common::auth::AuthFailure> {
    if service_name.is_empty() || service_name.len() > 1024 || service_name.contains('\0') {
        return Err(crate::common::auth::AuthFailure::new(
            "service_name_invalid",
            "service names must be 1-1024 bytes and must not contain NUL",
            false,
        ));
    }
    if !context.is_admin
        && (service_name.len() > 128
            || !service_name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._:-".contains(&byte)))
    {
        return Err(crate::common::auth::AuthFailure::new(
            "service_name_invalid",
            "temporary-key service names must be 1-128 ASCII bytes from [A-Za-z0-9._:-]",
            false,
        ));
    }
    if namespace == 0 {
        Ok(Arc::from(service_name))
    } else {
        Ok(Arc::from(format!("@{namespace:016x}\u{0}{service_name}")))
    }
}

pub(super) fn split_scoped_service_key(key: &str) -> (u64, &str) {
    let Some((prefix, name)) = key.split_once('\0') else {
        return (0, key);
    };
    let Some(hex) = prefix.strip_prefix('@') else {
        return (0, key);
    };
    match u64::from_str_radix(hex, 16) {
        Ok(namespace) => (namespace, name),
        Err(_) => (0, key),
    }
}

pub(super) fn decrement_namespace_stream_count(
    namespace_stream_counts: &mut hashbrown::HashMap<u64, usize>,
    namespace: u64,
) {
    let Some(count) = namespace_stream_counts.get_mut(&namespace) else {
        return;
    };
    *count = count.saturating_sub(1);
    if *count == 0 {
        namespace_stream_counts.remove(&namespace);
    }
}

pub(super) fn release_namespace_rate_limit_if_idle(
    namespace: u64,
    server_conn_map: &ServerConnMap,
    pending_streams: &hashbrown::HashMap<RemoteConnId, (RemoteConnId, u64, ImutableKey)>,
    namespace_rate_limits: &mut hashbrown::HashMap<u64, NamespaceRateLimit>,
) {
    let has_registered_service = server_conn_map
        .keys()
        .any(|key| split_scoped_service_key(key).0 == namespace);
    let has_pending_stream = pending_streams
        .values()
        .any(|(_, _, key)| split_scoped_service_key(key).0 == namespace);
    if !has_registered_service && !has_pending_stream {
        namespace_rate_limits.remove(&namespace);
    }
}
