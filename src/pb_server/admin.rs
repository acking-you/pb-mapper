use std::time::Duration;

use tokio::net::TcpStream;

use super::error::Error;
use super::{ManagerTask, ManagerTaskSender, Result};
use crate::common::auth::{AuthFailure, AuthRuntime};
use crate::common::checksum::{parse_credential, Credential};
use crate::common::conn_id::RemoteConnId;
use crate::common::message::command::{
    AdminRequest, AdminResponse, MessageSerializer, PbConnResponse,
};
use crate::common::message::secure::ServerHeaderSession;
use crate::common::message::MessageWriter;

pub async fn handle_admin_request(
    request: AdminRequest,
    auth: AuthRuntime,
    manager: ManagerTaskSender,
    conn_id: RemoteConnId,
    mut conn: TcpStream,
    session: ServerHeaderSession,
) -> Result<()> {
    let result = execute(request, auth, manager).await;
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
    auth: AuthRuntime,
    manager: ManagerTaskSender,
) -> std::result::Result<AdminResponse, AuthFailure> {
    match request {
        AdminRequest::KeyIssue { ttl_seconds, label } => auth
            .issue(Duration::from_secs(ttl_seconds), label)
            .await
            .map(AdminResponse::KeyIssued),
        AdminRequest::KeyList { page, page_size } => {
            audit_read(
                &auth,
                "temporary_key_list",
                None,
                Some(format!("page={page},page_size={page_size}")),
            )
            .await;
            auth.list(page, page_size).await.map(AdminResponse::KeyList)
        }
        AdminRequest::KeyShow { key_id } => {
            auth.show(key_id, false).await.map(AdminResponse::KeyShown)
        }
        AdminRequest::KeyReveal { key_id } => {
            auth.show(key_id, true).await.map(AdminResponse::KeyShown)
        }
        AdminRequest::KeyRenew {
            key_id,
            ttl_seconds,
        } => auth
            .renew(key_id, Duration::from_secs(ttl_seconds))
            .await
            .map(AdminResponse::KeyRenewed),
        AdminRequest::KeyRevoke { key_id } => {
            auth.revoke(key_id).await.map(AdminResponse::KeyRevoked)
        }
        AdminRequest::KeyGc => auth
            .gc()
            .await
            .map(|removed| AdminResponse::KeyGc { removed }),
        AdminRequest::AuthStatus => {
            audit_read(&auth, "auth_status", None, None).await;
            auth.status().await.map(AdminResponse::AuthStatus)
        }
        AdminRequest::AuthStateReset { confirm } => {
            if !confirm {
                return Err(AuthFailure::new(
                    "confirmation_required",
                    "auth-state reset requires explicit confirmation",
                    false,
                ));
            }
            auth.reset().await?;
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
            auth.rotate_root(new_key).await?;
            Ok(AdminResponse::Ok {
                action: "administrator_key_rotated".to_string(),
            })
        }
        AdminRequest::LegacyProtocolSet { policy } => {
            auth.set_legacy_protocol(policy).await?;
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
                "service_list",
                key_id,
                Some(format!("page={page},page_size={page_size}")),
            )
            .await;
            let (response_sender, receiver) = tokio::sync::oneshot::channel();
            manager
                .send(ManagerTask::AdminServiceList {
                    key_id,
                    page,
                    page_size,
                    response_sender,
                })
                .await
                .map_err(|_| {
                    AuthFailure::new(
                        "server_state_unavailable",
                        "relay connection manager is unavailable",
                        true,
                    )
                })?;
            receiver.await.map(AdminResponse::Services).map_err(|_| {
                AuthFailure::new(
                    "server_state_unavailable",
                    "relay connection manager dropped the service query",
                    true,
                )
            })
        }
        AdminRequest::ConnectionList {
            key_id,
            page,
            page_size,
        } => {
            audit_read(
                &auth,
                "connection_list",
                key_id,
                Some(format!("page={page},page_size={page_size}")),
            )
            .await;
            let (response_sender, receiver) = tokio::sync::oneshot::channel();
            manager
                .send(ManagerTask::AdminConnectionList {
                    key_id,
                    page,
                    page_size,
                    response_sender,
                })
                .await
                .map_err(|_| {
                    AuthFailure::new(
                        "server_state_unavailable",
                        "relay connection manager is unavailable",
                        true,
                    )
                })?;
            receiver.await.map(AdminResponse::Connections).map_err(|_| {
                AuthFailure::new(
                    "server_state_unavailable",
                    "relay connection manager dropped the connection query",
                    true,
                )
            })
        }
    }
}

async fn audit_read(auth: &AuthRuntime, action: &str, key_id: Option<u64>, detail: Option<String>) {
    if let Err(error) = auth.audit_admin(action, key_id, detail).await {
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
