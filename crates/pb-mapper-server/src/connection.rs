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

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use pb_mapper_auth::{ADMIN_KEY_ID, AuthContext};
use pb_mapper_core::config::control_io_timeout;
use pb_mapper_core::conn_id::RemoteConnId;
use pb_mapper_core::snafu_error_handle;
use pb_mapper_core::timeout::RetryBackoff;
use pb_mapper_protocol::MessageWriter;
use pb_mapper_protocol::command::{
    AdminRequest, CONTROL_PROTOCOL_V2, MessageSerializer, PbConnRequest, PbConnResponse,
    PbConnStatusReq, REQUEST_DECODE_FAILED,
};
use pb_mapper_protocol::direct::{SessionId, SessionSecret};
use pb_mapper_protocol::secure::{
    FirstFlightProof, HeaderProtocol, ServerHeaderSession, ServerSecurity,
};
use pb_mapper_protocol::transport::RelayStream;
use snafu::{IntoError, ResultExt};
use tokio::net::{TcpListener, TcpStream};
use tracing::instrument;
use uni_stream::stream::{set_tcp_keep_alive, set_tcp_nodelay};

use crate::admin::handle_admin_request;
use crate::client::handle_client_conn_selected;
use crate::direct;
use crate::error::{
    Result, ServerListenSnafu, TaskCenterInitRequestTimeoutSnafu, TaskCenterSendListenerSnafu,
    TaskCenterSendStreamRespToManagerSnafu, TaskCenterSetKeepAliveSnafu,
};
use crate::routing::ImutableKey;
use crate::server::{ServerRegistration, handle_server_conn};
use crate::service_key::scoped_service_key;
use crate::status::handle_show_status;
use crate::task::{ManagerTask, ManagerTaskSender};

/// Where the relay takes TCP connections from: its listener, or in tests one
/// that fails on cue.
pub(super) trait Accept {
    fn accept(&self) -> impl Future<Output = std::io::Result<(TcpStream, SocketAddr)>> + Send;
}

impl Accept for TcpListener {
    fn accept(&self) -> impl Future<Output = std::io::Result<(TcpStream, SocketAddr)>> + Send {
        TcpListener::accept(self)
    }
}

/// Accept connections until the routing manager is gone.
///
/// A failed accept never ends the loop. A peer that reset before it was
/// accepted costs only itself; anything else, such as a full descriptor
/// table, is retried after a pause that grows to a second, so a relay that
/// ran out of descriptors under a flood serves again once the flood ends.
pub(super) async fn handle_listener(
    task_sender: ManagerTaskSender,
    listener: impl Accept,
    keep_alive: bool,
) -> Result<()> {
    let mut backoff = RetryBackoff::default();
    loop {
        let (stream, addr) = match listener.accept().await {
            Ok(accepted) => {
                backoff.reset();
                accepted
            }
            Err(source) => {
                let peer_gone = matches!(
                    source.kind(),
                    std::io::ErrorKind::ConnectionReset
                        | std::io::ErrorKind::ConnectionAborted
                        | std::io::ErrorKind::ConnectionRefused
                );
                let error = ServerListenSnafu.into_error(source);
                if peer_gone {
                    tracing::debug!(
                        event = "tcp_accept_failed",
                        error = %snafu::Report::from_error(error),
                        "a connection ended before it was accepted"
                    );
                } else {
                    let retry_in = backoff.next_delay();
                    tracing::error!(
                        event = "tcp_accept_failed",
                        error = %snafu::Report::from_error(error),
                        ?retry_in,
                        "accepting tcp connections failed"
                    );
                    tokio::time::sleep(retry_in).await;
                }
                continue;
            }
        };
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
                stream: stream.into(),
                peer_addr: addr,
            })
            .await
            .context(TaskCenterSendListenerSnafu)?
    }
}

/// Count a first frame that failed to authenticate against its peer's log
/// budget, and log it when the budget allows: one record, whichever carrier
/// it arrived on.
pub(super) fn log_auth_failure(
    security: &ServerSecurity,
    peer_addr: SocketAddr,
    conn: impl std::fmt::Display,
    error: &pb_mapper_protocol::secure::ServerInitialError,
) {
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
            conn_id = %conn,
            peer_addr = %peer_addr,
            key_id = key_id.as_u64(),
            reason = %error.failure.code,
            retryable = error.failure.retryable,
            error = %error.failure.message,
            "connection authentication failed"
        );
    }
}

#[instrument(skip(manager_task_sender, conn, security, quic_capability), fields(conn_id = %conn_id, peer_addr = %peer_addr))]
pub(super) async fn handle_conn(
    conn_id: RemoteConnId,
    peer_addr: SocketAddr,
    manager_task_sender: ManagerTaskSender,
    mut conn: RelayStream,
    security: ServerSecurity,
    quic_capability: Option<Arc<pb_mapper_protocol::command::QuicEndpoint>>,
) -> Result<()> {
    let timeout = control_io_timeout();
    let initial = match tokio::time::timeout(timeout, security.read_initial(&mut conn)).await {
        Err(_) => TaskCenterInitRequestTimeoutSnafu { conn_id, timeout }.fail()?,
        Ok(Err(error)) => {
            log_auth_failure(&security, peer_addr, conn_id, &error);
            if let Some(session) = error.response_session {
                write_protocol_error(&mut conn, &session, &error.failure).await;
            }
            return Ok(());
        }
        Ok(Ok(initial)) => initial,
    };
    let first_flight = initial.first_flight;
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
                &pb_mapper_auth::AuthFailure::new(
                    REQUEST_DECODE_FAILED,
                    "authenticated request payload is malformed",
                    false,
                ),
            )
            .await;
            return Ok(());
        }
    };
    let (init_request, scope) = InitRequest::from_wire(init_request);
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
        scope.namespace,
        scope.force_register,
        matches!(init_request, InitRequest::Register(_)),
    ) {
        Ok(namespace) => namespace,
        Err(failure) => {
            write_protocol_error(&mut conn, &session, &failure).await;
            return Ok(());
        }
    };
    let admitted = Admitted {
        conn_id,
        peer_addr,
        conn,
        session,
        auth: auth_context,
        namespace: effective_namespace,
        manager: manager_task_sender,
    };
    match init_request {
        InitRequest::QuicEndpoint => {
            serve_quic_endpoint(admitted, quic_capability, timeout).await;
            Ok(())
        }
        InitRequest::DirectControl => {
            serve_direct_control(admitted).await;
            Ok(())
        }
        InitRequest::DirectRelay {
            id,
            secret,
            data_protocol,
        } => serve_direct_relay(admitted, id, secret, data_protocol, timeout).await,
        InitRequest::Register(request) => serve_register(admitted, request).await,
        InitRequest::Subscribe { key, data_protocol } => {
            serve_subscribe(admitted, key, data_protocol).await
        }
        InitRequest::Stream {
            key,
            data_protocol,
            dst_id,
            server_generation,
        } => serve_stream(admitted, key, data_protocol, dst_id, server_generation).await,
        InitRequest::Status(status) => serve_status(admitted, status).await,
        InitRequest::Admin(request) => {
            serve_admin(admitted, request, &security, first_flight).await
        }
    }
}

/// A first request, with the namespace its scoped form named split off, so each
/// kind of connection is one variant.
enum InitRequest {
    QuicEndpoint,
    DirectControl,
    DirectRelay {
        id: SessionId,
        secret: SessionSecret,
        data_protocol: Option<u16>,
    },
    Register(RegisterRequest),
    Subscribe {
        key: String,
        data_protocol: Option<u16>,
    },
    Stream {
        key: String,
        data_protocol: Option<u16>,
        dst_id: u32,
        server_generation: u64,
    },
    Status(PbConnStatusReq),
    Admin(AdminRequest),
}

struct RegisterRequest {
    key: String,
    need_codec: bool,
    is_datagram: bool,
    protocol_version: Option<u16>,
    client_instance_id: Option<String>,
    heartbeat_interval_ms: Option<u64>,
    heartbeat_tolerance_ms: Option<u64>,
    direct: Option<u16>,
}

/// The namespace a first request named, if any, and whether an administrator's
/// registration insists on it.
struct RequestScope {
    namespace: Option<u64>,
    force_register: bool,
}

impl InitRequest {
    fn from_wire(request: PbConnRequest) -> (Self, RequestScope) {
        let scope = |namespace| RequestScope {
            namespace,
            force_register: false,
        };
        match request {
            PbConnRequest::QuicEndpoint => (Self::QuicEndpoint, scope(None)),
            PbConnRequest::DirectControl { namespace } => (Self::DirectControl, scope(namespace)),
            PbConnRequest::DirectRelay {
                id,
                secret,
                namespace,
                data_protocol,
            } => (
                Self::DirectRelay {
                    id,
                    secret,
                    data_protocol,
                },
                scope(namespace),
            ),
            PbConnRequest::Register {
                need_codec,
                is_datagram,
                key,
                protocol_version,
                client_instance_id,
                heartbeat_interval_ms,
                heartbeat_tolerance_ms,
                direct,
            } => (
                Self::Register(RegisterRequest {
                    key,
                    need_codec,
                    is_datagram,
                    protocol_version,
                    client_instance_id,
                    heartbeat_interval_ms,
                    heartbeat_tolerance_ms,
                    direct,
                }),
                scope(None),
            ),
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
                direct,
            } => (
                Self::Register(RegisterRequest {
                    key,
                    need_codec,
                    is_datagram,
                    protocol_version,
                    client_instance_id,
                    heartbeat_interval_ms,
                    heartbeat_tolerance_ms,
                    direct,
                }),
                RequestScope {
                    namespace: Some(namespace),
                    force_register: force_namespace,
                },
            ),
            PbConnRequest::Subcribe { key, data_protocol } => {
                (Self::Subscribe { key, data_protocol }, scope(None))
            }
            PbConnRequest::SubcribeScoped {
                key,
                namespace,
                data_protocol,
            } => (
                Self::Subscribe { key, data_protocol },
                scope(Some(namespace)),
            ),
            PbConnRequest::Status(status) => (Self::Status(status), scope(None)),
            PbConnRequest::StatusScoped { status, namespace } => {
                (Self::Status(status), scope(Some(namespace)))
            }
            PbConnRequest::Stream {
                key,
                data_protocol,
                dst_id,
                server_generation,
            } => (
                Self::Stream {
                    key,
                    data_protocol,
                    dst_id,
                    server_generation,
                },
                scope(None),
            ),
            PbConnRequest::StreamScoped {
                key,
                data_protocol,
                namespace,
                dst_id,
                server_generation,
            } => (
                Self::Stream {
                    key,
                    data_protocol,
                    dst_id,
                    server_generation,
                },
                scope(Some(namespace)),
            ),
            PbConnRequest::Admin(request) => (Self::Admin(request), scope(None)),
        }
    }
}

/// A connection whose first frame authenticated, ready to serve the role its
/// request named.
struct Admitted {
    conn_id: RemoteConnId,
    peer_addr: SocketAddr,
    conn: RelayStream,
    session: ServerHeaderSession,
    auth: AuthContext,
    /// The namespace the request resolved to.
    namespace: u64,
    manager: ManagerTaskSender,
}

async fn serve_quic_endpoint(
    admitted: Admitted,
    quic_capability: Option<Arc<pb_mapper_protocol::command::QuicEndpoint>>,
    timeout: Duration,
) {
    let Admitted {
        mut conn, session, ..
    } = admitted;
    let response = PbConnResponse::QuicEndpoint(quic_capability.as_deref().cloned()).encode();
    if let Ok(response) = response
        && let Ok(mut writer) = session.response_writer(&mut conn)
    {
        let _ = tokio::time::timeout(timeout, writer.write_msg(&response)).await;
    }
}

async fn serve_direct_control(admitted: Admitted) {
    let Admitted {
        conn_id,
        conn,
        session,
        namespace,
        manager,
        ..
    } = admitted;
    if session.protocol() != HeaderProtocol::V2 {
        return;
    }
    if let Err(error) = direct::handle_control(conn_id, namespace, conn, session, manager).await {
        tracing::debug!(%error, "direct control closed");
    }
}

async fn serve_direct_relay(
    admitted: Admitted,
    id: SessionId,
    secret: SessionSecret,
    data_protocol: Option<u16>,
    timeout: Duration,
) -> Result<()> {
    let Admitted {
        conn_id,
        mut conn,
        session,
        auth,
        namespace,
        manager,
        ..
    } = admitted;
    if session.protocol() != HeaderProtocol::V2 {
        return Ok(());
    }
    let (tx, rx) = tokio::sync::oneshot::channel();
    let sent = manager
        .send(ManagerTask::Direct(direct::DirectTask::Relay {
            auth: auth.clone(),
            namespace,
            id,
            secret,
            response: tx,
        }))
        .await;
    let selected = if sent.is_ok() {
        tokio::time::timeout(timeout, rx)
            .await
            .ok()
            .and_then(|r| r.ok())
            .flatten()
    } else {
        None
    };
    let Some((key, provider, generation)) = selected else {
        write_protocol_error(
            &mut conn,
            &session,
            &pb_mapper_auth::AuthFailure::new(
                "direct_relay_denied",
                "invalid or expired relay selection",
                false,
            ),
        )
        .await;
        return Ok(());
    };
    run_while_credential_active(
        conn,
        session,
        &auth,
        conn_id,
        "direct fallback relay",
        |conn, session| {
            handle_client_conn_selected(
                key,
                conn_id,
                manager,
                conn,
                session,
                data_protocol,
                Some((provider, generation)),
            )
        },
    )
    .await
}

async fn serve_register(admitted: Admitted, request: RegisterRequest) -> Result<()> {
    let Admitted {
        conn_id,
        peer_addr,
        mut conn,
        session,
        auth,
        namespace,
        manager,
    } = admitted;
    let RegisterRequest {
        key,
        need_codec,
        is_datagram,
        protocol_version,
        client_instance_id,
        heartbeat_interval_ms,
        heartbeat_tolerance_ms,
        direct,
    } = request;
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
    let Some(key) = scope_service_or_reject(&mut conn, &session, &auth, namespace, &key).await
    else {
        return Ok(());
    };
    run_while_credential_active(
        conn,
        session,
        &auth,
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
                    direct: direct
                        .filter(|v| {
                            *v == pb_mapper_protocol::direct::DIRECT_VERSION
                                && session.protocol() == HeaderProtocol::V2
                                && protocol_version >= CONTROL_PROTOCOL_V2
                        })
                        .map(|_| auth.clone()),
                },
                manager,
                conn,
                session,
            )
        },
    )
    .await
}

async fn serve_subscribe(
    admitted: Admitted,
    key: String,
    data_protocol: Option<u16>,
) -> Result<()> {
    let Admitted {
        conn_id,
        peer_addr,
        mut conn,
        session,
        auth,
        namespace,
        manager,
    } = admitted;
    tracing::info!(
        event = "init_request",
        request = "subscribe",
        conn_id = %conn_id,
        peer_addr = %peer_addr,
        key = %key,
        "received pb init request"
    );
    let Some(key) = scope_service_or_reject(&mut conn, &session, &auth, namespace, &key).await
    else {
        return Ok(());
    };
    run_while_credential_active(
        conn,
        session,
        &auth,
        conn_id,
        "subscribed data connection",
        |conn, session| {
            handle_client_conn_selected(key, conn_id, manager, conn, session, data_protocol, None)
        },
    )
    .await
}

async fn serve_stream(
    admitted: Admitted,
    key: String,
    data_protocol: Option<u16>,
    dst_id: u32,
    server_generation: u64,
) -> Result<()> {
    let Admitted {
        conn_id,
        peer_addr,
        mut conn,
        session,
        auth,
        namespace,
        manager,
    } = admitted;
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
    let Some(key) = scope_service_or_reject(&mut conn, &session, &auth, namespace, &key).await
    else {
        return Ok(());
    };
    manager
        .send(ManagerTask::Stream {
            data_protocol,
            key: key.clone(),
            stream: conn,
            session,
            server_id: conn_id,
            client_id: dst_id.into(),
            server_generation,
        })
        .await
        .context(TaskCenterSendStreamRespToManagerSnafu { key, conn_id })
}

async fn serve_status(admitted: Admitted, status: PbConnStatusReq) -> Result<()> {
    let Admitted {
        conn_id,
        peer_addr,
        conn,
        session,
        auth,
        namespace,
        manager,
    } = admitted;
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
        &auth,
        conn_id,
        "status request",
        |conn, session| handle_show_status(status, namespace, manager, conn_id, conn, session),
    )
    .await
}

async fn serve_admin(
    admitted: Admitted,
    request: AdminRequest,
    security: &ServerSecurity,
    first_flight: Option<FirstFlightProof>,
) -> Result<()> {
    let Admitted {
        conn_id,
        mut conn,
        session,
        auth,
        manager,
        ..
    } = admitted;
    if !auth.is_admin {
        write_protocol_error(
            &mut conn,
            &session,
            &pb_mapper_auth::AuthFailure::new(
                pb_mapper_auth::codes::ADMIN_PERMISSION_REQUIRED,
                "administrator credential is required for this operation",
                false,
            ),
        )
        .await;
        return Ok(());
    }
    // Only a protocol-v2 session carries a first-flight proof.
    let Some(first_flight) = first_flight else {
        write_protocol_error(
            &mut conn,
            &session,
            &pb_mapper_auth::AuthFailure::new(
                "admin_protocol_v2_required",
                "administrator operations require protocol v2",
                false,
            ),
        )
        .await;
        return Ok(());
    };
    if request.is_mutating() {
        let FirstFlightProof {
            fingerprint,
            client_timestamp,
        } = first_flight;
        if let Err(failure) = security
            .auth()
            .claim_admin_mutation(&auth, fingerprint, client_timestamp)
            .await
        {
            write_protocol_error(&mut conn, &session, &failure).await;
            return Ok(());
        }
    }
    handle_admin_request(
        request,
        auth,
        security.auth().clone(),
        manager,
        conn_id,
        conn,
        session,
    )
    .await
}

/// The routing key for `service_name` in `namespace`, or `None` once the peer
/// has been told why it cannot have one.
async fn scope_service_or_reject(
    conn: &mut RelayStream,
    session: &ServerHeaderSession,
    auth_context: &AuthContext,
    namespace: u64,
    service_name: &str,
) -> Option<ImutableKey> {
    match scoped_service_key(auth_context, namespace, service_name) {
        Ok(key) => Some(key),
        Err(failure) => {
            write_protocol_error(conn, session, &failure).await;
            None
        }
    }
}

async fn run_while_credential_active<F, Fut>(
    mut conn: RelayStream,
    session: ServerHeaderSession,
    auth_context: &AuthContext,
    conn_id: RemoteConnId,
    closed_what: &'static str,
    work: F,
) -> Result<()>
where
    F: FnOnce(RelayStream, ServerHeaderSession) -> Fut,
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
        biased;
        _ = cancellation.cancelled() => {
            tracing::info!(
                event = "connection_auth_expired",
                key_id = auth_context.key_id.as_u64(),
                conn_id = %conn_id,
                "closing {closed_what}"
            );
        }
        result = work(conn, session) => result?,
    }
    Ok(())
}

async fn write_protocol_error(
    conn: &mut RelayStream,
    session: &ServerHeaderSession,
    failure: &pb_mapper_auth::AuthFailure,
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
) -> std::result::Result<u64, pb_mapper_auth::AuthFailure> {
    let namespace = requested.unwrap_or(context.namespace);
    if !context.is_admin && namespace != context.namespace {
        return Err(pb_mapper_auth::AuthFailure::new(
            "namespace_access_denied",
            "temporary credentials can only access their own namespace",
            false,
        ));
    }
    if context.is_admin && is_register && namespace != 0 && !force_register_namespace {
        return Err(pb_mapper_auth::AuthFailure::new(
            "namespace_force_required",
            "administrator registration in a temporary namespace requires --force",
            false,
        ));
    }
    Ok(namespace)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pb_mapper_auth::{AuthConfig, AuthRuntime, LegacyProtocolPolicy};
    use pb_mapper_core::test_support::{
        PROCESS_CREDENTIAL_TEST_LOCK, TEST_ADMIN_KEY_BYTES, TempPath,
    };
    use pb_mapper_protocol::{
        CodecMessageReader, CodecMessageWriter, MessageReader, get_decodec, get_encodec,
    };
    use tokio_util::sync::CancellationToken;

    /// A listener whose next accepts fail with `failures`, in order.
    struct Failing {
        listener: TcpListener,
        failures: std::sync::Mutex<Vec<std::io::Error>>,
    }

    impl Accept for Failing {
        fn accept(&self) -> impl Future<Output = std::io::Result<(TcpStream, SocketAddr)>> + Send {
            let failure = self.failures.lock().unwrap().pop();
            async move {
                match failure {
                    Some(failure) => Err(failure),
                    None => self.listener.accept().await,
                }
            }
        }
    }

    /// A failed accept costs one connection, never the listener: neither a
    /// peer that reset before it was accepted nor a full descriptor table
    /// leaves the relay deaf to TCP.
    #[tokio::test]
    async fn the_listener_outlives_failed_accepts() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let listener = Failing {
            listener,
            failures: std::sync::Mutex::new(vec![
                std::io::Error::other("too many open files"),
                std::io::ErrorKind::ConnectionReset.into(),
            ]),
        };
        let (sender, mut receiver) = crate::manager::task_channel(8);
        let listening = tokio::spawn(handle_listener(sender, listener, false));

        let _client = TcpStream::connect(addr).await.unwrap();
        let task = tokio::time::timeout(Duration::from_secs(5), receiver.recv())
            .await
            .expect("no connection was accepted")
            .expect("the listener stopped");
        assert!(matches!(task, ManagerTask::Accept { .. }));
        listening.abort();
    }

    /// An administrator change needs protocol v2's first-flight proof to claim
    /// against replay. Sent with the administrator key over legacy framing, it
    /// is refused before it reaches auth.
    #[tokio::test]
    async fn a_legacy_administrator_change_is_refused() {
        let _process_credential_guard = PROCESS_CREDENTIAL_TEST_LOCK.lock().await;
        let state_dir = TempPath::new("legacy-admin");
        let auth = AuthRuntime::start(
            TEST_ADMIN_KEY_BYTES,
            AuthConfig {
                state_dir: state_dir.to_path_buf(),
                max_temporary_keys: 4,
                max_temporary_key_ttl: Duration::from_secs(3600),
                legacy_protocol: LegacyProtocolPolicy::Allow,
            },
        )
        .await
        .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let shutdown = CancellationToken::new();
        let relay = tokio::spawn(crate::run_server_on_listener(
            listener,
            shutdown.clone(),
            None,
            false,
            auth.clone(),
        ));

        let key = TEST_ADMIN_KEY_BYTES;
        let mut socket = tokio::net::TcpStream::connect(addr).await.unwrap();
        let request = PbConnRequest::Admin(AdminRequest::LegacyProtocolSet {
            policy: LegacyProtocolPolicy::Deny,
        });
        CodecMessageWriter::for_session_key(&mut socket, get_encodec(&key).unwrap(), key)
            .write_msg(&request.encode().unwrap())
            .await
            .unwrap();
        let mut reader =
            CodecMessageReader::for_session_key(&mut socket, get_decodec(&key).unwrap(), key);
        let response = tokio::time::timeout(Duration::from_secs(10), reader.read_msg())
            .await
            .unwrap()
            .unwrap();
        match PbConnResponse::decode(response).unwrap() {
            PbConnResponse::Error(error) => assert_eq!(error.code, "admin_protocol_v2_required"),
            other => panic!("expected a refusal, got {other:?}"),
        }
        assert!(
            auth.legacy_protocol_allowed().unwrap(),
            "the change was not made"
        );

        shutdown.cancel();
        relay.await.unwrap().unwrap();
    }
}
