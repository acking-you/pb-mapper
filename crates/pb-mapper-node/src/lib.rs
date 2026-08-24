//! Node-API surface over [`pb_mapper_client::sdk`].
//!
//! Three things happen at this boundary and nowhere else:
//!
//! * **Numbers narrow.** JavaScript has no `u64`, so napi maps the SDK's
//!   unsigned ids and timestamps to `i64` and its page sizes to `u32`. Every
//!   inbound narrowing is checked — see [`as_u64`] and [`narrow_page_size`] —
//!   because a silent `as` cast turns a caller's mistake into wrong data
//!   instead of an error.
//! * **Enums become strings.** `"tcp"`, `"connected"`, `"allow"`: idiomatic on
//!   the JS side, and parsed back into the SDK's enums here.
//! * **Errors flatten.** Every SDK error becomes a JS `Error` carrying its
//!   rendered message, since napi has no structured error channel.
//!
//! Beyond that this crate holds no logic: anything worth testing belongs in the
//! SDK, which is exercised by `crates/pb-mapper-cli/tests/sdk_e2e.rs`.

#![allow(clippy::needless_pass_by_value)]

use std::sync::Arc;
use std::time::Duration;

use napi::bindgen_prelude::*;
use napi_derive::napi;
use pb_mapper_client::sdk::{
    self, ClientConfig, ConnectRequest, RegisterRequest, Transport, TunnelStatus,
};

/// Default page size for the `list*` calls that take an explicit page.
const DEFAULT_PAGE_SIZE: u32 = 100;

fn to_napi(error: sdk::Error) -> Error {
    Error::from_reason(error.to_string())
}

fn invalid_argument(message: impl Into<String>) -> Error {
    Error::from_reason(message.into())
}

/// Narrow a JS integer to the `u64` the SDK expects.
///
/// `field` names the argument, so a caller sees which one was negative rather
/// than a bare type complaint.
fn as_u64(field: &str, value: i64) -> Result<u64> {
    u64::try_from(value).map_err(|_| invalid_argument(format!("{field} must be non-negative")))
}

/// Same as [`as_u64`], for an optional argument.
fn as_opt_u64(field: &str, value: Option<i64>) -> Result<Option<u64>> {
    value.map(|value| as_u64(field, value)).transpose()
}

/// Narrow a JS page size to the `u16` the SDK expects.
///
/// Checked rather than cast: `as u16` silently wraps, so `pageSize: 65_537`
/// would have asked for a single-item page instead of failing.
fn narrow_page_size(value: Option<u32>) -> Result<u16> {
    let value = value.unwrap_or(DEFAULT_PAGE_SIZE);
    u16::try_from(value).map_err(|_| invalid_argument(format!("pageSize {value} is out of range")))
}

fn parse_transport(raw: &str) -> Result<Transport> {
    match raw.to_ascii_lowercase().as_str() {
        "tcp" => Ok(Transport::Tcp),
        "udp" => Ok(Transport::Udp),
        _ => Err(invalid_argument(format!(
            "transport must be \"tcp\" or \"udp\", got {raw}"
        ))),
    }
}

/// Render a tunnel status for JS.
///
/// `Failed` keeps its reason after the colon: it is the only description of a
/// permanent rejection a JS caller ever receives.
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
            legacy_protocol: match value.legacy_protocol {
                sdk::LegacyProtocol::Allow => "allow".into(),
                sdk::LegacyProtocol::Deny => "deny".into(),
            },
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
        let inner = sdk::Client::new(ClientConfig {
            server: config.server,
            credential: config.credential,
            keep_alive: config.keep_alive.unwrap_or(false),
            namespace: as_opt_u64("namespace", config.namespace)?,
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
        Ok(Registration::new(registration))
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
        Ok(Connection::new(connection))
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

/// A live `register` tunnel: a local service published on the relay.
#[napi]
pub struct Registration {
    inner: Arc<sdk::Registration>,
}

/// A live `connect` tunnel: a local listener forwarding to a registered service.
#[napi]
pub struct Connection {
    inner: Arc<sdk::Connection>,
}

/// Implements the JS surface of one tunnel handle.
///
/// [`Registration`] and [`Connection`] stay distinct types on both sides of the
/// boundary — neither may be passed where the other is meant — but their JS
/// surface is identical down to the last method, so it is written once here.
///
/// The structs themselves are declared above rather than generated: napi copies
/// doc comments into `index.d.ts`, and a comment forwarded through a macro
/// argument arrives there mangled.
///
/// Both hold the handle in an `Arc`, because napi passes `&self` to an
/// `async fn` and the returned future must not borrow the wrapper.
macro_rules! tunnel_class {
    ($name:ident wrapping $handle:ty) => {
        impl $name {
            fn new(inner: $handle) -> Self {
                Self {
                    inner: Arc::new(inner),
                }
            }
        }

        #[napi]
        impl $name {
            /// The service key this tunnel is bound to.
            #[napi]
            pub fn key(&self) -> String {
                self.inner.key().to_string()
            }

            /// The tunnel's latest status: `starting`, `connected`, `retrying`,
            /// `stopped`, or `failed:<reason>`.
            #[napi]
            pub fn status(&self) -> String {
                status_label(&self.inner.status())
            }

            /// Resolves once the tunnel is connected, and rejects if it fails or
            /// is stopped first.
            ///
            /// Without `timeoutMs` this waits indefinitely, which is only safe
            /// because a tunnel the relay refuses permanently settles as failed
            /// rather than retrying forever.
            #[napi]
            pub async fn wait_ready(&self, timeout_ms: Option<u32>) -> Result<()> {
                match timeout_ms {
                    Some(ms) => self
                        .inner
                        .wait_ready_timeout(Duration::from_millis(u64::from(ms)))
                        .await
                        .map_err(to_napi),
                    None => self.inner.wait_ready().await.map_err(to_napi),
                }
            }

            /// Cancels the tunnel's worker and waits for it to unwind.
            #[napi]
            pub async fn stop(&self) -> Result<()> {
                self.inner.stop().await.map_err(to_napi)
            }
        }
    };
}

tunnel_class!(Registration wrapping sdk::Registration);
tunnel_class!(Connection wrapping sdk::Connection);

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
