//! Node-API surface over [`pb_mapper_client::sdk`].

#![allow(clippy::needless_pass_by_value)]

use std::sync::Arc;
use std::time::Duration;

use napi::bindgen_prelude::*;
use napi_derive::napi;
use pb_mapper_client::sdk::{
    self, ClientConfig, ConnectRequest, RegisterRequest, Transport, TunnelStatus,
};

fn to_napi(error: sdk::Error) -> Error {
    Error::from_reason(error.to_string())
}

fn parse_transport(raw: &str) -> Result<Transport> {
    match raw.to_ascii_lowercase().as_str() {
        "tcp" => Ok(Transport::Tcp),
        "udp" => Ok(Transport::Udp),
        _ => Err(Error::from_reason(format!(
            "transport must be \"tcp\" or \"udp\", got {raw}"
        ))),
    }
}

fn status_label(status: &TunnelStatus) -> String {
    match status {
        TunnelStatus::Starting => "starting".into(),
        TunnelStatus::Connected => "connected".into(),
        TunnelStatus::Retrying => "retrying".into(),
        TunnelStatus::Stopped => "stopped".into(),
        TunnelStatus::Failed(reason) => format!("failed:{reason}"),
    }
}

#[napi(object)]
pub struct JsClientConfig {
    pub server: String,
    pub credential: String,
    pub keep_alive: Option<bool>,
    pub namespace: Option<i64>,
}

#[napi(object)]
pub struct JsRegisterRequest {
    pub key: String,
    pub local_addr: String,
    pub transport: String,
    pub codec: Option<bool>,
    pub force_namespace: Option<bool>,
}

#[napi(object)]
pub struct JsConnectRequest {
    pub key: String,
    pub local_addr: String,
    pub transport: String,
}

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
            key_id: value.key_id as i64,
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
    pub server_instance_id: String,
}

impl From<sdk::KeyMetadata> for JsKeyMetadata {
    fn from(value: sdk::KeyMetadata) -> Self {
        Self {
            key_id: value.key_id as i64,
            state: value.state,
            issued_at: value.issued_at as i64,
            expires_at: value.expires_at as i64,
            label: value.label,
        }
    }
}

#[napi(object)]
pub struct JsServiceConnection {
    pub conn_id: u32,
    pub generation: i64,
    pub protocol_version: u32,
    pub healthy: bool,
    pub last_rx_age_ms: i64,
}

impl From<sdk::ServiceConnection> for JsServiceConnection {
    fn from(value: sdk::ServiceConnection) -> Self {
        Self {
            conn_id: value.conn_id,
            generation: value.generation as i64,
            protocol_version: u32::from(value.protocol_version),
            healthy: value.healthy,
            last_rx_age_ms: value.last_rx_age_ms as i64,
        }
    }
}

#[napi(object)]
pub struct JsRemoteId {
    pub server_map: String,
    pub active: String,
    pub idle: String,
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
            key_id: value.key_id as i64,
            namespace: value.namespace as i64,
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
            key_id: value.key_id as i64,
            namespace: value.namespace as i64,
            service_name: value.service_name,
            conn_id: value.conn_id,
            generation: value.generation as i64,
            protocol_version: u32::from(value.protocol_version),
            healthy: value.healthy,
            transport: value.transport,
            codec_enabled: value.codec_enabled,
            last_rx_age_ms: value.last_rx_age_ms as i64,
        }
    }
}

#[napi]
pub struct Client {
    inner: sdk::Client,
}

#[napi]
impl Client {
    #[napi(constructor)]
    pub fn new(config: JsClientConfig) -> Result<Self> {
        let namespace = match config.namespace {
            Some(value) if value < 0 => {
                return Err(Error::from_reason("namespace must be non-negative"));
            }
            Some(value) => Some(value as u64),
            None => None,
        };
        let inner = sdk::Client::new(ClientConfig {
            server: config.server,
            credential: config.credential,
            keep_alive: config.keep_alive.unwrap_or(false),
            namespace,
        })
        .map_err(to_napi)?;
        Ok(Self { inner })
    }

    #[napi]
    pub fn server(&self) -> String {
        self.inner.server().to_string()
    }

    #[napi]
    pub async fn register(&self, request: JsRegisterRequest) -> Result<Registration> {
        let registration = self
            .inner
            .register(RegisterRequest {
                key: request.key,
                local_addr: request.local_addr,
                transport: parse_transport(&request.transport)?,
                codec: request.codec.unwrap_or(false),
                force_namespace: request.force_namespace.unwrap_or(false),
            })
            .await
            .map_err(to_napi)?;
        Ok(Registration {
            inner: Arc::new(registration),
        })
    }

    #[napi]
    pub async fn connect(&self, request: JsConnectRequest) -> Result<Connection> {
        let connection = self
            .inner
            .connect(ConnectRequest {
                key: request.key,
                local_addr: request.local_addr,
                transport: parse_transport(&request.transport)?,
            })
            .await
            .map_err(to_napi)?;
        Ok(Connection {
            inner: Arc::new(connection),
        })
    }

    #[napi]
    pub async fn list_keys(&self) -> Result<Vec<String>> {
        self.inner.list_keys().await.map_err(to_napi)
    }

    #[napi]
    pub async fn service_status(&self, key: String) -> Result<Vec<JsServiceConnection>> {
        Ok(self
            .inner
            .service_status(key)
            .await
            .map_err(to_napi)?
            .into_iter()
            .map(JsServiceConnection::from)
            .collect())
    }

    #[napi]
    pub async fn remote_id(&self) -> Result<JsRemoteId> {
        let remote = self.inner.remote_id().await.map_err(to_napi)?;
        Ok(JsRemoteId {
            server_map: remote.server_map,
            active: remote.active,
            idle: remote.idle,
        })
    }

    #[napi]
    pub fn admin(&self) -> Result<Admin> {
        Ok(Admin {
            inner: self.inner.admin().map_err(to_napi)?,
        })
    }
}

#[napi]
pub struct Registration {
    inner: Arc<sdk::Registration>,
}

#[napi]
impl Registration {
    #[napi]
    pub fn key(&self) -> String {
        self.inner.key().to_string()
    }

    #[napi]
    pub fn status(&self) -> String {
        status_label(&self.inner.status())
    }

    #[napi]
    pub async fn wait_ready(&self, timeout_ms: Option<u32>) -> Result<()> {
        wait_ready_registration(&self.inner, timeout_ms).await
    }

    #[napi]
    pub async fn stop(&self) -> Result<()> {
        self.inner.stop().await.map_err(to_napi)
    }
}

#[napi]
pub struct Connection {
    inner: Arc<sdk::Connection>,
}

#[napi]
impl Connection {
    #[napi]
    pub fn key(&self) -> String {
        self.inner.key().to_string()
    }

    #[napi]
    pub fn status(&self) -> String {
        status_label(&self.inner.status())
    }

    #[napi]
    pub async fn wait_ready(&self, timeout_ms: Option<u32>) -> Result<()> {
        wait_ready_connection(&self.inner, timeout_ms).await
    }

    #[napi]
    pub async fn stop(&self) -> Result<()> {
        self.inner.stop().await.map_err(to_napi)
    }
}

#[napi]
pub struct Admin {
    inner: sdk::Admin,
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
                    .list_keys(page, page_size.unwrap_or(100) as u16)
                    .await
                    .map_err(to_napi)?
                    .items
            }
        };
        Ok(items.into_iter().map(JsKeyMetadata::from).collect())
    }

    #[napi]
    pub async fn revoke_key(&self, key_id: i64) -> Result<JsKeyMetadata> {
        if key_id < 0 {
            return Err(Error::from_reason("key_id must be non-negative"));
        }
        self.inner
            .revoke_key(key_id as u64)
            .await
            .map(JsKeyMetadata::from)
            .map_err(to_napi)
    }

    #[napi]
    pub async fn show_key(&self, key_id: i64) -> Result<JsIssuedKey> {
        if key_id < 0 {
            return Err(Error::from_reason("key_id must be non-negative"));
        }
        self.inner
            .show_key(key_id as u64)
            .await
            .map(JsIssuedKey::from)
            .map_err(to_napi)
    }

    #[napi]
    pub async fn reveal_key(&self, key_id: i64) -> Result<JsIssuedKey> {
        if key_id < 0 {
            return Err(Error::from_reason("key_id must be non-negative"));
        }
        self.inner
            .reveal_key(key_id as u64)
            .await
            .map(JsIssuedKey::from)
            .map_err(to_napi)
    }

    #[napi]
    pub async fn renew_key(&self, key_id: i64, ttl_seconds: u32) -> Result<JsIssuedKey> {
        if key_id < 0 {
            return Err(Error::from_reason("key_id must be non-negative"));
        }
        self.inner
            .renew_key(key_id as u64, Duration::from_secs(u64::from(ttl_seconds)))
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
        let status = self.inner.auth_status().await.map_err(to_napi)?;
        Ok(JsAuthStatus {
            schema_version: status.schema_version as u32,
            safe_mode: status.safe_mode,
            capacity: status.capacity as i64,
            active_keys: status.active_keys as i64,
            expired_keys: status.expired_keys as i64,
            revoked_keys: status.revoked_keys as i64,
            legacy_protocol: match status.legacy_protocol {
                sdk::LegacyProtocol::Allow => "allow".into(),
                sdk::LegacyProtocol::Deny => "deny".into(),
            },
            active_legacy_connections: status.active_legacy_connections as i64,
            server_instance_id: status.server_instance_id,
        })
    }

    #[napi]
    pub async fn set_legacy_protocol(&self, policy: String) -> Result<()> {
        let policy = match policy.to_ascii_lowercase().as_str() {
            "allow" => sdk::LegacyProtocol::Allow,
            "deny" => sdk::LegacyProtocol::Deny,
            _ => {
                return Err(Error::from_reason(
                    "legacy protocol must be \"allow\" or \"deny\"",
                ));
            }
        };
        self.inner
            .set_legacy_protocol(policy)
            .await
            .map_err(to_napi)
    }

    #[napi]
    pub async fn list_services(&self, key_id: Option<i64>) -> Result<Vec<JsServiceInfo>> {
        let key_id = match key_id {
            Some(id) if id < 0 => {
                return Err(Error::from_reason("key_id must be non-negative"));
            }
            Some(id) => Some(id as u64),
            None => None,
        };
        Ok(self
            .inner
            .list_services_all(key_id)
            .await
            .map_err(to_napi)?
            .into_iter()
            .map(JsServiceInfo::from)
            .collect())
    }

    #[napi]
    pub async fn list_connections(&self, key_id: Option<i64>) -> Result<Vec<JsConnectionInfo>> {
        let key_id = match key_id {
            Some(id) if id < 0 => {
                return Err(Error::from_reason("key_id must be non-negative"));
            }
            Some(id) => Some(id as u64),
            None => None,
        };
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

async fn wait_ready_registration(
    registration: &sdk::Registration,
    timeout_ms: Option<u32>,
) -> Result<()> {
    match timeout_ms {
        Some(ms) => registration
            .wait_ready_timeout(Duration::from_millis(u64::from(ms)))
            .await
            .map_err(to_napi),
        None => registration.wait_ready().await.map_err(to_napi),
    }
}

async fn wait_ready_connection(
    connection: &sdk::Connection,
    timeout_ms: Option<u32>,
) -> Result<()> {
    match timeout_ms {
        Some(ms) => connection
            .wait_ready_timeout(Duration::from_millis(u64::from(ms)))
            .await
            .map_err(to_napi),
        None => connection.wait_ready().await.map_err(to_napi),
    }
}
