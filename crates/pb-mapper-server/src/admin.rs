//! Server-side administrator request execution.
//!
//! ```text
//! authenticated AdminRequest -> revalidated AuthContext -> auth actor / manager
//!                                                        -> AdminResponse
//! ```
//!
//! Credential lifecycle operations go to `AuthRuntime`; service and connection
//! requests go to the routing manager. Every audited operation keeps its primary
//! response when only audit emission fails.
//!
//! Reads and mutations differ in where the authorization point sits: see
//! [`query_inventory`] and [`apply_mutation`].

use std::time::Duration;

use tokio::net::TcpStream;
use tokio_util::sync::CancellationToken;

use super::error::Error;
use super::{ManagerTask, ManagerTaskSender, Result, compose_service_key, validate_service_name};
use pb_mapper_auth::{AuthContext, AuthFailure, AuthRuntime, KeyId};
use pb_mapper_core::checksum::{Credential, parse_credential};
use pb_mapper_core::conn_id::RemoteConnId;
use pb_mapper_protocol::MessageWriter;
use pb_mapper_protocol::command::{AdminRequest, AdminResponse, MessageSerializer, PbConnResponse};
use pb_mapper_protocol::secure::ServerHeaderSession;

pub async fn handle_admin_request(
    request: AdminRequest,
    authorization: AuthContext,
    auth: AuthRuntime,
    manager: ManagerTaskSender,
    conn_id: RemoteConnId,
    mut conn: TcpStream,
    session: ServerHeaderSession,
) -> Result<()> {
    let result = execute(request, &authorization, auth, manager).await;
    let response = match result {
        Ok(response) => PbConnResponse::Admin(response),
        Err(failure) => {
            tracing::warn!(
                event = "admin_operation_failed",
                auth_stage = "permission_or_state",
                conn_id = %conn_id,
                reason = %failure.code,
                retryable = failure.retryable,
                error = %failure.message,
                "administrator operation failed"
            );
            PbConnResponse::error(failure.code, failure.message, failure.retryable)
        }
    };
    let message = response.encode().map_err(|error| Error::AdminOperation {
        detail: format!("failed to encode response: {error}"),
    })?;
    let mut writer = session
        .response_writer(&mut conn)
        .map_err(|error| Error::AdminOperation {
            detail: format!("failed to create response writer: {error}"),
        })?;
    writer
        .write_msg(&message)
        .await
        .map_err(|error| Error::AdminOperation {
            detail: format!("failed to write response: {error}"),
        })
}

async fn execute(
    request: AdminRequest,
    authorization: &AuthContext,
    auth: AuthRuntime,
    manager: ManagerTaskSender,
) -> std::result::Result<AdminResponse, AuthFailure> {
    match request {
        AdminRequest::KeyIssue { ttl_seconds, label } => auth
            .issue(authorization, Duration::from_secs(ttl_seconds), label)
            .await
            .map(AdminResponse::KeyIssued),
        AdminRequest::KeyList { page, page_size } => {
            audit_action(
                &auth,
                authorization,
                "temporary_key_list",
                None,
                Some(format!("page={page},page_size={page_size}")),
            )
            .await;
            auth.list(authorization, page, page_size)
                .await
                .map(AdminResponse::KeyList)
        }
        AdminRequest::KeyShow { key_id } => auth
            .show(authorization, KeyId::from_u64(key_id), false)
            .await
            .map(AdminResponse::KeyShown),
        AdminRequest::KeyReveal { key_id } => auth
            .show(authorization, KeyId::from_u64(key_id), true)
            .await
            .map(AdminResponse::KeyShown),
        AdminRequest::KeyRenew {
            key_id,
            ttl_seconds,
        } => auth
            .renew(
                authorization,
                KeyId::from_u64(key_id),
                Duration::from_secs(ttl_seconds),
            )
            .await
            .map(AdminResponse::KeyRenewed),
        AdminRequest::KeyRevoke { key_id } => auth
            .revoke(authorization, KeyId::from_u64(key_id))
            .await
            .map(AdminResponse::KeyRevoked),
        AdminRequest::KeyGc => auth
            .gc(authorization)
            .await
            .map(|removed| AdminResponse::KeyGc { removed }),
        AdminRequest::AuthStatus => {
            audit_action(&auth, authorization, "auth_status", None, None).await;
            auth.status(authorization)
                .await
                .map(AdminResponse::AuthStatus)
        }
        AdminRequest::AuthStateReset { confirm } => {
            if !confirm {
                return Err(AuthFailure::new(
                    "confirmation_required",
                    "auth-state reset requires explicit confirmation",
                    false,
                ));
            }
            auth.reset(authorization).await?;
            Ok(AdminResponse::Ok {
                action: "auth_state_reset".to_string(),
            })
        }
        AdminRequest::RootKeyRotate { new_admin_key } => {
            let Credential::Admin(new_key) = parse_credential(new_admin_key.trim())
                .map_err(|error| AuthFailure::new("administrator_key_invalid", error, false))?
            else {
                return Err(AuthFailure::new(
                    "administrator_key_invalid",
                    "root rotation requires a 32-byte administrator key",
                    false,
                ));
            };
            auth.rotate_root(authorization, new_key).await?;
            Ok(AdminResponse::Ok {
                action: "administrator_key_rotated".to_string(),
            })
        }
        AdminRequest::LegacyProtocolSet { policy } => {
            auth.set_legacy_protocol(authorization, policy).await?;
            Ok(AdminResponse::Ok {
                action: "legacy_protocol_updated".to_string(),
            })
        }
        AdminRequest::ServiceList {
            key_id,
            page,
            page_size,
        } => {
            audit_action(
                &auth,
                authorization,
                "service_list",
                key_id.map(KeyId::from_u64),
                Some(format!("page={page},page_size={page_size}")),
            )
            .await;
            let (response_sender, receiver) = tokio::sync::oneshot::channel();
            let response = query_inventory(
                authorization,
                &manager,
                ManagerTask::AdminServiceList {
                    key_id,
                    page,
                    page_size,
                    response_sender,
                },
                receiver,
                "service",
            )
            .await?;
            Ok(AdminResponse::Services(response))
        }
        AdminRequest::ConnectionList {
            key_id,
            page,
            page_size,
        } => {
            audit_action(
                &auth,
                authorization,
                "connection_list",
                key_id.map(KeyId::from_u64),
                Some(format!("page={page},page_size={page_size}")),
            )
            .await;
            let (response_sender, receiver) = tokio::sync::oneshot::channel();
            let response = query_inventory(
                authorization,
                &manager,
                ManagerTask::AdminConnectionList {
                    key_id,
                    page,
                    page_size,
                    response_sender,
                },
                receiver,
                "connection",
            )
            .await?;
            Ok(AdminResponse::Connections(response))
        }
        AdminRequest::ConnectionRetire {
            key_id,
            service_name,
            conn_id,
        } => {
            // The name arrives from the wire, so it gets the same check a
            // registration's does. Without it a name carrying the NUL separator
            // would compose another namespace's routing key, retiring a service
            // the audit record does not name.
            validate_service_name(&service_name)?;
            let namespace = key_id.unwrap_or_default();
            let key = compose_service_key(namespace, &service_name);
            let (response_sender, receiver) = tokio::sync::oneshot::channel();
            let retired = apply_mutation(
                authorization,
                &manager,
                ManagerTask::AdminConnectionRetire {
                    key,
                    conn_id: conn_id.map(RemoteConnId::from),
                    response_sender,
                },
                receiver,
                "connection retirement",
            )
            .await?;
            // Audited after the fact, not before: the interesting record is what
            // was actually dropped. A `retired=0` entry says the operator named
            // something the relay no longer had. Reached unconditionally, since
            // `apply_mutation` does not re-check the lease — a change that
            // happened gets recorded even if the credential rotated meanwhile.
            audit_action(
                &auth,
                authorization,
                "connection_retire",
                key_id.map(KeyId::from_u64),
                Some(format!(
                    "service={service_name},conn_id={},retired={retired}",
                    conn_id.map_or_else(|| "all".to_string(), |id| id.to_string())
                )),
            )
            .await;
            Ok(AdminResponse::ConnectionsRetired { retired })
        }
    }
}

/// Dispatch an inventory read only while the administrator lease remains current.
///
/// Root rotation cancels the old lease. Racing both channel operations against that
/// cancellation prevents a request authenticated under the old root from waiting for or
/// returning relay inventory after the rotation has taken effect. The final revalidation
/// establishes the successful read's authorization point after the manager produced its page.
async fn query_inventory<T>(
    authorization: &AuthContext,
    manager: &ManagerTaskSender,
    task: ManagerTask,
    receiver: tokio::sync::oneshot::Receiver<T>,
    inventory: &'static str,
) -> std::result::Result<T, AuthFailure> {
    let cancellation = send_manager_task(authorization, manager, task).await?;
    let response = tokio::select! {
        biased;
        _ = cancellation.cancelled() => return Err(cancelled_authorization(authorization)),
        result = receiver => result.map_err(|_| manager_dropped(inventory))?,
    };
    authorization.ensure_active()?;
    Ok(response)
}

/// Run a manager task that changes relay state, and report what it did.
///
/// Differs from [`query_inventory`] in where the authorization point sits. A read
/// re-checks the lease afterwards, because a snapshot taken under a credential
/// that has since rotated should not be handed back. A mutation cannot: by the
/// time the manager answers, the change is already applied, so racing the
/// cancellation or re-checking the lease would report failure for a mutation that
/// happened — and skip auditing it. The authorization is therefore established
/// before the task is sent, and the outcome reported unconditionally after.
async fn apply_mutation<T>(
    authorization: &AuthContext,
    manager: &ManagerTaskSender,
    task: ManagerTask,
    receiver: tokio::sync::oneshot::Receiver<T>,
    mutation: &'static str,
) -> std::result::Result<T, AuthFailure> {
    let _cancellation = send_manager_task(authorization, manager, task).await?;
    receiver.await.map_err(|_| manager_dropped(mutation))
}

/// Authorize the caller, then hand the task to the manager.
///
/// Returns the administrator lease's cancellation token, since a caller that
/// still has waiting left to do needs it. Racing the cancellation here is safe
/// for a mutation too: a task that was never sent was never applied.
async fn send_manager_task(
    authorization: &AuthContext,
    manager: &ManagerTaskSender,
    task: ManagerTask,
) -> std::result::Result<CancellationToken, AuthFailure> {
    let cancellation = authorization.admin_cancellation_token()?;
    tokio::select! {
        biased;
        _ = cancellation.cancelled() => return Err(cancelled_authorization(authorization)),
        result = manager.send(task) => result.map_err(|_| {
            AuthFailure::new(
                "server_state_unavailable",
                "relay connection manager is unavailable",
                true,
            )
        })?,
    }
    Ok(cancellation)
}

fn manager_dropped(what: &'static str) -> AuthFailure {
    AuthFailure::new(
        "server_state_unavailable",
        format!("relay connection manager dropped the {what} query"),
        true,
    )
}

fn cancelled_authorization(authorization: &AuthContext) -> AuthFailure {
    match authorization.ensure_active() {
        Err(error) => error,
        Ok(_) => AuthFailure::new(
            "administrator_key_rotated",
            "administrator credential lease was cancelled during the inventory query",
            false,
        ),
    }
}

/// Record an administrator action, and never fail the action if recording fails.
///
/// The audit itself needs a live administrator lease, so a root rotation between
/// the action and this call loses the record. For a mutation that record is the
/// only trace the change leaves, which is why the failure is logged rather than
/// swallowed: the log line is what an operator reconstructs the gap from.
async fn audit_action(
    auth: &AuthRuntime,
    authorization: &AuthContext,
    action: &str,
    key_id: Option<KeyId>,
    detail: Option<String>,
) {
    if let Err(error) = auth
        .audit_admin(authorization, action, key_id, detail)
        .await
    {
        tracing::warn!(
            event = "admin_audit_failed",
            auth_stage = "audit",
            action,
            reason = %error.code,
            error = %error.message,
            "administrator operation could not be audited"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pb_mapper_auth::ADMIN_KEY_ID;
    use pb_mapper_auth::{AuthConfig, LegacyProtocolPolicy};

    fn temp_state_dir(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "pb-mapper-admin-{name}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or_default()
        ))
    }

    async fn inventory_query_rejects_rotation(connection_query: bool) {
        let _process_credential_guard = pb_mapper_core::test_support::PROCESS_CREDENTIAL_TEST_LOCK
            .lock()
            .await;
        let state_dir = temp_state_dir(if connection_query {
            "connections"
        } else {
            "services"
        });
        let old_key = *b"0123456789abcdefghijklmnopqrstuv";
        let new_key = *b"abcdefghijklmnopqrstuvwxyz012345";
        let runtime = AuthRuntime::start(
            old_key,
            AuthConfig {
                state_dir: state_dir.clone(),
                max_temporary_keys: 4,
                max_temporary_key_ttl: Duration::from_secs(3600),
                legacy_protocol: LegacyProtocolPolicy::Allow,
            },
        )
        .await
        .expect("authentication runtime should start");
        let admin = runtime
            .authenticate_presented(ADMIN_KEY_ID, &old_key)
            .expect("old administrator key should authenticate");
        let request = if connection_query {
            AdminRequest::ConnectionList {
                key_id: None,
                page: 0,
                page_size: 100,
            }
        } else {
            AdminRequest::ServiceList {
                key_id: None,
                page: 0,
                page_size: 100,
            }
        };
        let (manager, receiver) = kanal::unbounded_async();
        let request_admin = admin.clone();
        let request_runtime = runtime.clone();
        let pending = tokio::spawn(async move {
            execute(request, &request_admin, request_runtime, manager).await
        });

        let manager_task = receiver
            .recv()
            .await
            .expect("inventory request should reach the manager");
        runtime
            .rotate_root(&admin, new_key)
            .await
            .expect("root rotation should succeed");
        match manager_task {
            ManagerTask::AdminServiceList {
                response_sender, ..
            } => {
                let _ = response_sender.send(pb_mapper_protocol::command::AdminServicePage {
                    schema_version: 1,
                    items: Vec::new(),
                    next_page: None,
                });
            }
            ManagerTask::AdminConnectionList {
                response_sender, ..
            } => {
                let _ = response_sender.send(pb_mapper_protocol::command::AdminConnectionPage {
                    schema_version: 1,
                    items: Vec::new(),
                    next_page: None,
                });
            }
            _ => panic!("expected an administrator inventory manager task"),
        }

        let failure = pending
            .await
            .expect("inventory task should not panic")
            .expect_err("rotated administrator must not receive inventory");
        assert_eq!(failure.code, "administrator_key_rotated");

        drop(runtime);
        tokio::time::sleep(Duration::from_millis(20)).await;
        let _ = std::fs::remove_dir_all(state_dir);
    }

    #[tokio::test]
    async fn service_inventory_rejects_root_rotation_after_dispatch() {
        inventory_query_rejects_rotation(false).await;
    }

    #[tokio::test]
    async fn connection_inventory_rejects_root_rotation_after_dispatch() {
        inventory_query_rejects_rotation(true).await;
    }

    /// A retirement that has already happened is reported, not disowned.
    ///
    /// The mirror image of the two cases above: a read taken under a credential
    /// that has since rotated must be refused, but a mutation cannot be, because
    /// refusing it would tell the operator nothing was retired while leaving the
    /// connections gone and the action unaudited.
    #[tokio::test]
    async fn connection_retire_reports_a_mutation_that_outlived_its_credential() {
        let _process_credential_guard = pb_mapper_core::test_support::PROCESS_CREDENTIAL_TEST_LOCK
            .lock()
            .await;
        let state_dir = temp_state_dir("retire-rotation");
        let old_key = *b"0123456789abcdefghijklmnopqrstuv";
        let new_key = *b"abcdefghijklmnopqrstuvwxyz012345";
        let runtime = AuthRuntime::start(
            old_key,
            AuthConfig {
                state_dir: state_dir.clone(),
                max_temporary_keys: 4,
                max_temporary_key_ttl: Duration::from_secs(3600),
                legacy_protocol: LegacyProtocolPolicy::Allow,
            },
        )
        .await
        .expect("authentication runtime should start");
        let admin = runtime
            .authenticate_presented(ADMIN_KEY_ID, &old_key)
            .expect("old administrator key should authenticate");
        let (manager, receiver) = kanal::unbounded_async();
        let request_admin = admin.clone();
        let request_runtime = runtime.clone();
        let pending = tokio::spawn(async move {
            execute(
                AdminRequest::ConnectionRetire {
                    key_id: None,
                    service_name: "echo".to_string(),
                    conn_id: None,
                },
                &request_admin,
                request_runtime,
                manager,
            )
            .await
        });

        let manager_task = receiver
            .recv()
            .await
            .expect("retire request should reach the manager");
        runtime
            .rotate_root(&admin, new_key)
            .await
            .expect("root rotation should succeed");
        let ManagerTask::AdminConnectionRetire {
            response_sender, ..
        } = manager_task
        else {
            panic!("expected an administrator retire manager task");
        };
        let _ = response_sender.send(3);

        let response = pending
            .await
            .expect("retire task should not panic")
            .expect("a retirement that happened must be reported");
        assert!(matches!(
            response,
            AdminResponse::ConnectionsRetired { retired: 3 }
        ));

        drop(runtime);
        tokio::time::sleep(Duration::from_millis(20)).await;
        let _ = std::fs::remove_dir_all(state_dir);
    }

    /// A retire name carrying the scoped-key separator never reaches the manager.
    ///
    /// `@{namespace:016x}\0{name}` is the routing key of a namespaced service, so
    /// a name spelling one out would retire a service in a namespace the request
    /// never named — and the audit record would name the unscoped one.
    #[tokio::test]
    async fn connection_retire_rejects_a_name_that_spells_out_a_scoped_key() {
        let _process_credential_guard = pb_mapper_core::test_support::PROCESS_CREDENTIAL_TEST_LOCK
            .lock()
            .await;
        let state_dir = temp_state_dir("retire-nul");
        let key = *b"0123456789abcdefghijklmnopqrstuv";
        let runtime = AuthRuntime::start(
            key,
            AuthConfig {
                state_dir: state_dir.clone(),
                max_temporary_keys: 4,
                max_temporary_key_ttl: Duration::from_secs(3600),
                legacy_protocol: LegacyProtocolPolicy::Allow,
            },
        )
        .await
        .expect("authentication runtime should start");
        let admin = runtime
            .authenticate_presented(ADMIN_KEY_ID, &key)
            .expect("administrator key should authenticate");
        let (manager, receiver) = kanal::unbounded_async();

        let failure = execute(
            AdminRequest::ConnectionRetire {
                key_id: None,
                service_name: "@0000000000000001\u{0}echo".to_string(),
                conn_id: None,
            },
            &admin,
            runtime.clone(),
            manager.clone(),
        )
        .await
        .expect_err("a NUL-carrying service name must be refused");
        assert_eq!(failure.code, "service_name_invalid");
        assert!(
            receiver.is_empty(),
            "a refused retirement must not reach the manager"
        );

        drop(runtime);
        tokio::time::sleep(Duration::from_millis(20)).await;
        let _ = std::fs::remove_dir_all(state_dir);
    }
}
