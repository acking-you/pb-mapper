//! Server-side administrator request execution.
//!
//! ```text
//! authenticated AdminRequest -> revalidated AuthContext -> auth actor / manager
//!                                                        -> AdminResponse
//! ```
//!
//! Credential lifecycle operations go to `AuthRuntime`; service and connection
//! inventory requests go to the routing manager. Read operations are audited without
//! weakening the primary response when only audit emission fails.

use std::time::Duration;

use tokio::net::TcpStream;

use super::error::Error;
use super::{ManagerTask, ManagerTaskSender, Result};
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
            audit_read(
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
            audit_read(&auth, authorization, "auth_status", None, None).await;
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
            audit_read(
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
            audit_read(
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
    let response = tokio::select! {
        biased;
        _ = cancellation.cancelled() => return Err(cancelled_authorization(authorization)),
        result = receiver => result.map_err(|_| {
            AuthFailure::new(
                "server_state_unavailable",
                format!("relay connection manager dropped the {inventory} query"),
                true,
            )
        })?,
    };
    authorization.ensure_active()?;
    Ok(response)
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

async fn audit_read(
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
            "administrator read operation could not be audited"
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
}
