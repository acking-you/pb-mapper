//! The `Admin` class: credential and relay administration with the
//! administrator key.

use std::time::Duration;

use napi::bindgen_prelude::*;
use napi_derive::napi;
use pb_mapper_client::sdk;

use crate::convert::{as_opt_u64, as_u64, id, narrow_page_size, parse_legacy_protocol, to_napi};

#[napi(object)]
pub struct JsIssuedKey {
    pub key_id: i64,
    pub state: String,
    pub issued_at: i64,
    pub expires_at: i64,
    pub label: Option<String>,
    pub credential: String,
}

impl From<sdk::IssuedKey> for JsIssuedKey {
    fn from(value: sdk::IssuedKey) -> Self {
        Self {
            key_id: id(value.key_id),
            state: value.state,
            issued_at: value.issued_at as i64,
            expires_at: value.expires_at as i64,
            label: value.label,
            credential: value.credential,
        }
    }
}

#[napi(object)]
pub struct JsKeyMetadata {
    pub key_id: i64,
    pub state: String,
    pub issued_at: i64,
    pub expires_at: i64,
    pub label: Option<String>,
}

#[napi(object)]
pub struct JsAuthStatus {
    pub schema_version: u32,
    pub safe_mode: bool,
    pub capacity: i64,
    pub active_keys: i64,
    pub expired_keys: i64,
    pub revoked_keys: i64,
    pub legacy_protocol: String,
    pub active_legacy_connections: i64,
    /// Unix seconds of the most recent legacy (protocol-v1) connection, or
    /// `null` if the server has never seen one.
    pub last_legacy_connection_at: Option<i64>,
    pub auth_successes: i64,
    pub auth_failures: i64,
    pub server_instance_id: String,
}

impl From<sdk::AuthStatusInfo> for JsAuthStatus {
    fn from(value: sdk::AuthStatusInfo) -> Self {
        Self {
            schema_version: u32::from(value.schema_version),
            safe_mode: value.safe_mode,
            capacity: value.capacity as i64,
            active_keys: value.active_keys as i64,
            expired_keys: value.expired_keys as i64,
            revoked_keys: value.revoked_keys as i64,
            legacy_protocol: value.legacy_protocol.as_str().into(),
            active_legacy_connections: value.active_legacy_connections as i64,
            last_legacy_connection_at: value.last_legacy_connection_at.map(|at| at as i64),
            auth_successes: value.auth_successes as i64,
            auth_failures: value.auth_failures as i64,
            server_instance_id: value.server_instance_id,
        }
    }
}

impl From<sdk::KeyMetadata> for JsKeyMetadata {
    fn from(value: sdk::KeyMetadata) -> Self {
        Self {
            key_id: id(value.key_id),
            state: value.state,
            issued_at: value.issued_at as i64,
            expires_at: value.expires_at as i64,
            label: value.label,
        }
    }
}

#[napi(object)]
pub struct JsServiceInfo {
    pub key_id: i64,
    pub namespace: i64,
    pub service_name: String,
    pub transport: String,
    pub codec_enabled: bool,
    pub connection_count: u32,
}

impl From<sdk::ServiceInfo> for JsServiceInfo {
    fn from(value: sdk::ServiceInfo) -> Self {
        Self {
            key_id: id(value.key_id),
            namespace: id(value.namespace),
            service_name: value.service_name,
            transport: value.transport,
            codec_enabled: value.codec_enabled,
            connection_count: value.connection_count,
        }
    }
}

#[napi(object)]
pub struct JsConnectionInfo {
    pub key_id: i64,
    pub namespace: i64,
    pub service_name: String,
    pub conn_id: u32,
    pub generation: i64,
    pub protocol_version: u32,
    pub healthy: bool,
    pub transport: String,
    pub codec_enabled: bool,
    pub last_rx_age_ms: i64,
}

impl From<sdk::ConnectionInfo> for JsConnectionInfo {
    fn from(value: sdk::ConnectionInfo) -> Self {
        Self {
            key_id: id(value.key_id),
            namespace: id(value.namespace),
            service_name: value.service_name,
            conn_id: value.conn_id,
            generation: id(value.generation),
            protocol_version: u32::from(value.protocol_version),
            healthy: value.healthy,
            transport: value.transport,
            codec_enabled: value.codec_enabled,
            last_rx_age_ms: value.last_rx_age_ms as i64,
        }
    }
}

#[napi]
pub struct Admin {
    pub(crate) inner: sdk::Admin,
}

#[napi]
impl Admin {
    #[napi]
    pub async fn issue_key(&self, ttl_seconds: u32, label: Option<String>) -> Result<JsIssuedKey> {
        self.inner
            .issue_key(Duration::from_secs(u64::from(ttl_seconds)), label)
            .await
            .map(JsIssuedKey::from)
            .map_err(to_napi)
    }

    /// Temporary credentials. Pages through the whole inventory when neither
    /// argument is given, matching `listServices` and `listConnections`; a
    /// truncated credential list with no way to see that it was truncated is
    /// worse than a slower call. Pass `page` to fetch exactly one page.
    #[napi]
    pub async fn list_keys(
        &self,
        page: Option<u32>,
        page_size: Option<u32>,
    ) -> Result<Vec<JsKeyMetadata>> {
        let items = match page {
            None => self.inner.list_keys_all().await.map_err(to_napi)?,
            Some(page) => {
                self.inner
                    .list_keys(page, narrow_page_size(page_size)?)
                    .await
                    .map_err(to_napi)?
                    .items
            }
        };
        Ok(items.into_iter().map(JsKeyMetadata::from).collect())
    }

    #[napi]
    pub async fn revoke_key(&self, key_id: i64) -> Result<JsKeyMetadata> {
        let key_id = as_u64("keyId", key_id)?;
        self.inner
            .revoke_key(key_id)
            .await
            .map(JsKeyMetadata::from)
            .map_err(to_napi)
    }

    #[napi]
    pub async fn show_key(&self, key_id: i64) -> Result<JsIssuedKey> {
        let key_id = as_u64("keyId", key_id)?;
        self.inner
            .show_key(key_id)
            .await
            .map(JsIssuedKey::from)
            .map_err(to_napi)
    }

    #[napi]
    pub async fn reveal_key(&self, key_id: i64) -> Result<JsIssuedKey> {
        let key_id = as_u64("keyId", key_id)?;
        self.inner
            .reveal_key(key_id)
            .await
            .map(JsIssuedKey::from)
            .map_err(to_napi)
    }

    #[napi]
    pub async fn renew_key(&self, key_id: i64, ttl_seconds: u32) -> Result<JsIssuedKey> {
        let key_id = as_u64("keyId", key_id)?;
        self.inner
            .renew_key(key_id, Duration::from_secs(u64::from(ttl_seconds)))
            .await
            .map(JsIssuedKey::from)
            .map_err(to_napi)
    }

    #[napi]
    pub async fn gc_keys(&self) -> Result<i64> {
        self.inner
            .gc_keys()
            .await
            .map(|removed| removed as i64)
            .map_err(to_napi)
    }

    #[napi]
    pub async fn reset_auth_state(&self) -> Result<()> {
        self.inner.reset_auth_state().await.map_err(to_napi)
    }

    #[napi]
    pub async fn rotate_root_key(&self, new_key: Option<String>) -> Result<String> {
        self.inner.rotate_root_key(new_key).await.map_err(to_napi)
    }

    #[napi]
    pub async fn auth_status(&self) -> Result<JsAuthStatus> {
        self.inner
            .auth_status()
            .await
            .map(JsAuthStatus::from)
            .map_err(to_napi)
    }

    #[napi]
    pub async fn set_legacy_protocol(&self, policy: String) -> Result<()> {
        let policy = parse_legacy_protocol(&policy)?;
        self.inner
            .set_legacy_protocol(policy)
            .await
            .map_err(to_napi)
    }

    #[napi]
    pub async fn list_services(&self, key_id: Option<i64>) -> Result<Vec<JsServiceInfo>> {
        let key_id = as_opt_u64("keyId", key_id)?;
        Ok(self
            .inner
            .list_services_all(key_id)
            .await
            .map_err(to_napi)?
            .into_iter()
            .map(JsServiceInfo::from)
            .collect())
    }

    /// Drops registered control connections the relay is still holding for a
    /// service, and resolves with how many it dropped.
    ///
    /// Omit `connId` to retire every connection the service has, which is what
    /// frees a connection quota filled by connections that should have gone away.
    #[napi]
    pub async fn retire_connections(
        &self,
        service_name: String,
        key_id: Option<i64>,
        conn_id: Option<u32>,
    ) -> Result<u32> {
        let key_id = as_opt_u64("keyId", key_id)?;
        self.inner
            .retire_connections(key_id, service_name, conn_id)
            .await
            .map_err(to_napi)
    }

    #[napi]
    pub async fn list_connections(&self, key_id: Option<i64>) -> Result<Vec<JsConnectionInfo>> {
        let key_id = as_opt_u64("keyId", key_id)?;
        Ok(self
            .inner
            .list_connections_all(key_id)
            .await
            .map_err(to_napi)?
            .into_iter()
            .map(JsConnectionInfo::from)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A key id past 2^53, after 2^21 reissues of one slot, cannot reach JS
    /// as itself. It reaches it as the largest integer JS holds exactly,
    /// which names no key, rather than rounded to a neighbouring slot's id,
    /// which `revokeKey` would then reach.
    #[test]
    fn a_key_id_js_cannot_hold_names_no_key() {
        let reissued = ((1_u64 << 21) << 32) | 1;
        let listed = JsKeyMetadata::from(sdk::KeyMetadata {
            key_id: reissued,
            namespace: reissued,
            state: "active".into(),
            issued_at: 0,
            expires_at: 0,
            label: None,
        });
        assert_eq!(listed.key_id, (1 << 53) - 1);
        let listed = JsKeyMetadata::from(sdk::KeyMetadata {
            key_id: 7,
            namespace: 7,
            state: "active".into(),
            issued_at: 0,
            expires_at: 0,
            label: None,
        });
        assert_eq!(listed.key_id, 7);
    }
}
