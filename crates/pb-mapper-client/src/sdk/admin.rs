use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use pb_mapper_auth::{
    AuthStatus, IssuedTemporaryKey, KeyPage, TemporaryKeyMetadata, generate_admin_key,
    write_admin_key_file,
};
use pb_mapper_core::checksum::parse_credential;
use pb_mapper_core::config::control_io_timeout;
use pb_mapper_protocol::MessageReader;
use pb_mapper_protocol::command::{
    AdminConnectionInfo, AdminConnectionPage, AdminRequest, AdminResponse, AdminServiceInfo,
    AdminServicePage, MessageSerializer, PbConnRequest, PbConnResponse,
};
use pb_mapper_protocol::secure::ClientHeaderSession;
use snafu::ResultExt;
use tokio::net::TcpStream;

use super::Error;
use super::client::ClientInner;
use super::error::{ConnectSnafu, Result};
use super::types::LegacyProtocol;

const DEFAULT_PAGE_SIZE: u16 = 100;
const MAX_PAGE_SIZE: u16 = 1000;

/// Administrator RPCs. Constructed via [`super::Client::admin`].
#[derive(Clone)]
pub struct Admin {
    pub(crate) inner: Arc<ClientInner>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IssuedKey {
    pub key_id: u64,
    pub state: String,
    pub issued_at: u64,
    pub expires_at: u64,
    pub label: Option<String>,
    pub credential: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KeyMetadata {
    pub key_id: u64,
    pub state: String,
    pub issued_at: u64,
    pub expires_at: u64,
    pub label: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KeyListPage {
    pub schema_version: u16,
    pub items: Vec<KeyMetadata>,
    pub next_page: Option<u32>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServiceInfo {
    pub key_id: u64,
    pub namespace: u64,
    pub service_name: String,
    pub transport: String,
    pub codec_enabled: bool,
    pub connection_count: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConnectionInfo {
    pub key_id: u64,
    pub namespace: u64,
    pub service_name: String,
    pub conn_id: u32,
    pub generation: u64,
    pub protocol_version: u16,
    pub healthy: bool,
    pub transport: String,
    pub codec_enabled: bool,
    pub last_rx_age_ms: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServicePage {
    pub schema_version: u16,
    pub items: Vec<ServiceInfo>,
    pub next_page: Option<u32>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConnectionPage {
    pub schema_version: u16,
    pub items: Vec<ConnectionInfo>,
    pub next_page: Option<u32>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthStatusInfo {
    pub schema_version: u16,
    pub safe_mode: bool,
    pub capacity: usize,
    pub active_keys: usize,
    pub expired_keys: usize,
    pub revoked_keys: usize,
    pub legacy_protocol: LegacyProtocol,
    pub active_legacy_connections: u64,
    pub last_legacy_connection_at: Option<u64>,
    pub auth_successes: u64,
    pub auth_failures: u64,
    pub server_instance_id: String,
}

impl Admin {
    /// One-shot administrator RPC. CLI output rendering can call this directly.
    pub async fn request(&self, request: AdminRequest) -> Result<AdminResponse> {
        self.request_with_timeout(request, control_io_timeout())
            .await
    }

    pub async fn request_with_timeout(
        &self,
        request: AdminRequest,
        io_timeout: Duration,
    ) -> Result<AdminResponse> {
        send_admin_request(&self.inner.server, self.credential(), request, io_timeout).await
    }

    pub async fn issue_key(&self, ttl: Duration, label: Option<String>) -> Result<IssuedKey> {
        match self
            .request(AdminRequest::KeyIssue {
                ttl_seconds: ttl.as_secs(),
                label,
            })
            .await?
        {
            AdminResponse::KeyIssued(issued) => Ok(IssuedKey::from(issued)),
            other => unexpected("KeyIssued", &other),
        }
    }

    pub async fn list_keys(&self, page: u32, page_size: u16) -> Result<KeyListPage> {
        let page_size = validate_page_size(page_size)?;
        match self
            .request(AdminRequest::KeyList { page, page_size })
            .await?
        {
            AdminResponse::KeyList(page) => Ok(KeyListPage::from(page)),
            other => unexpected("KeyList", &other),
        }
    }

    pub async fn list_keys_all(&self) -> Result<Vec<KeyMetadata>> {
        collect_pages(DEFAULT_PAGE_SIZE, |page, page_size| async move {
            let listed = self.list_keys(page, page_size).await?;
            Ok((listed.items, listed.next_page))
        })
        .await
    }

    pub async fn show_key(&self, key_id: u64) -> Result<IssuedKey> {
        match self.request(AdminRequest::KeyShow { key_id }).await? {
            AdminResponse::KeyShown(issued) => Ok(IssuedKey::from(issued)),
            other => unexpected("KeyShown", &other),
        }
    }

    pub async fn reveal_key(&self, key_id: u64) -> Result<IssuedKey> {
        match self.request(AdminRequest::KeyReveal { key_id }).await? {
            AdminResponse::KeyShown(issued) => Ok(IssuedKey::from(issued)),
            other => unexpected("KeyShown", &other),
        }
    }

    pub async fn renew_key(&self, key_id: u64, ttl: Duration) -> Result<IssuedKey> {
        match self
            .request(AdminRequest::KeyRenew {
                key_id,
                ttl_seconds: ttl.as_secs(),
            })
            .await?
        {
            AdminResponse::KeyRenewed(issued) => Ok(IssuedKey::from(issued)),
            other => unexpected("KeyRenewed", &other),
        }
    }

    pub async fn revoke_key(&self, key_id: u64) -> Result<KeyMetadata> {
        match self.request(AdminRequest::KeyRevoke { key_id }).await? {
            AdminResponse::KeyRevoked(meta) => Ok(KeyMetadata::from(meta)),
            other => unexpected("KeyRevoked", &other),
        }
    }

    pub async fn gc_keys(&self) -> Result<u64> {
        match self.request(AdminRequest::KeyGc).await? {
            AdminResponse::KeyGc { removed } => Ok(removed),
            other => unexpected("KeyGc", &other),
        }
    }

    pub async fn auth_status(&self) -> Result<AuthStatusInfo> {
        match self.request(AdminRequest::AuthStatus).await? {
            AdminResponse::AuthStatus(status) => Ok(AuthStatusInfo::from(status)),
            other => unexpected("AuthStatus", &other),
        }
    }

    pub async fn reset_auth_state(&self) -> Result<()> {
        match self
            .request(AdminRequest::AuthStateReset { confirm: true })
            .await?
        {
            AdminResponse::Ok { .. } => Ok(()),
            other => unexpected("Ok", &other),
        }
    }

    pub async fn rotate_root_key(&self, new_key: Option<String>) -> Result<String> {
        let new_key = new_key.unwrap_or_else(generate_admin_key);
        let parsed = parse_credential(new_key.trim()).map_err(Error::invalid_config)?;
        if !parsed.is_admin() {
            return Err(Error::invalid_config(
                "root rotation requires a 32-byte administrator key",
            ));
        }
        match self
            .request(AdminRequest::RootKeyRotate {
                new_admin_key: new_key.clone(),
            })
            .await?
        {
            AdminResponse::Ok { .. } => {
                *self
                    .inner
                    .credential
                    .write()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = parsed;
                Ok(new_key)
            }
            other => unexpected("Ok", &other),
        }
    }

    /// Rotate the root key and persist it to `path`.
    ///
    /// The candidate is written to a staged sibling file first, and `path` is only
    /// replaced once the relay has accepted the rotation and the new key has passed
    /// a post-rotation status check. Otherwise a failed rotation would leave `path`
    /// holding a key the relay never installed, discarding the still-valid one — the
    /// same staging the `admin root-key rotate` CLI flow performs.
    pub async fn rotate_root_key_to_file(
        &self,
        path: &Path,
        new_key: Option<String>,
    ) -> Result<String> {
        let new_key = new_key.unwrap_or_else(generate_admin_key);
        let staged_path = staged_key_path(path);
        write_admin_key_file(&staged_path, &new_key, true).map_err(|error| Error::AuthFile {
            message: error.to_string(),
        })?;

        let staged_note = || format!("the candidate key remains at `{}`", staged_path.display());
        let rotated = match self.rotate_root_key(Some(new_key)).await {
            Ok(rotated) => rotated,
            Err(error) => {
                return Err(Error::AuthFile {
                    message: format!("root rotation request failed; {}: {error}", staged_note()),
                });
            }
        };
        // `rotate_root_key` already swapped the in-memory credential, so this
        // proves the relay authenticates the key we are about to persist.
        if let Err(error) = self.auth_status().await {
            return Err(Error::AuthFile {
                message: format!(
                    "new administrator key did not pass the post-rotation status check; {}: {error}",
                    staged_note()
                ),
            });
        }
        write_admin_key_file(path, &rotated, true).map_err(|error| Error::AuthFile {
            message: format!(
                "administrator key rotated and verified, but `{}` could not be updated; recover the key from `{}`: {error}",
                path.display(),
                staged_path.display()
            ),
        })?;
        if let Err(error) = std::fs::remove_file(&staged_path) {
            tracing::warn!(
                path = %staged_path.display(),
                %error,
                "administrator key was rotated, but the staged key file could not be removed"
            );
        }
        Ok(rotated)
    }

    pub async fn set_legacy_protocol(&self, policy: LegacyProtocol) -> Result<()> {
        match self
            .request(AdminRequest::LegacyProtocolSet {
                policy: policy.into(),
            })
            .await?
        {
            AdminResponse::Ok { .. } => Ok(()),
            other => unexpected("Ok", &other),
        }
    }

    pub async fn list_services(
        &self,
        key_id: Option<u64>,
        page: u32,
        page_size: u16,
    ) -> Result<ServicePage> {
        let page_size = validate_page_size(page_size)?;
        match self
            .request(AdminRequest::ServiceList {
                key_id,
                page,
                page_size,
            })
            .await?
        {
            AdminResponse::Services(page) => Ok(ServicePage::from(page)),
            other => unexpected("Services", &other),
        }
    }

    pub async fn list_services_all(&self, key_id: Option<u64>) -> Result<Vec<ServiceInfo>> {
        collect_pages(DEFAULT_PAGE_SIZE, |page, page_size| async move {
            let listed = self.list_services(key_id, page, page_size).await?;
            Ok((listed.items, listed.next_page))
        })
        .await
    }

    pub async fn list_connections(
        &self,
        key_id: Option<u64>,
        page: u32,
        page_size: u16,
    ) -> Result<ConnectionPage> {
        let page_size = validate_page_size(page_size)?;
        match self
            .request(AdminRequest::ConnectionList {
                key_id,
                page,
                page_size,
            })
            .await?
        {
            AdminResponse::Connections(page) => Ok(ConnectionPage::from(page)),
            other => unexpected("Connections", &other),
        }
    }

    pub async fn list_connections_all(&self, key_id: Option<u64>) -> Result<Vec<ConnectionInfo>> {
        collect_pages(DEFAULT_PAGE_SIZE, |page, page_size| async move {
            let listed = self.list_connections(key_id, page, page_size).await?;
            Ok((listed.items, listed.next_page))
        })
        .await
    }

    fn credential(&self) -> pb_mapper_core::checksum::Credential {
        *self
            .inner
            .credential
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// The sibling path a rotation candidate is staged at, matching the CLI's naming.
fn staged_key_path(path: &Path) -> std::path::PathBuf {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("admin.key");
    path.with_file_name(format!(".{name}.next"))
}

fn validate_page_size(page_size: u16) -> Result<u16> {
    if !(1..=MAX_PAGE_SIZE).contains(&page_size) {
        return Err(Error::invalid_config(format!(
            "page_size must be between 1 and {MAX_PAGE_SIZE}"
        )));
    }
    Ok(page_size)
}

fn unexpected<T>(expected: &str, actual: &AdminResponse) -> Result<T> {
    Err(Error::protocol(format!(
        "expected {expected}, got {actual:?}"
    )))
}

async fn collect_pages<T, F, Fut>(page_size: u16, mut fetch: F) -> Result<Vec<T>>
where
    F: FnMut(u32, u16) -> Fut,
    Fut: std::future::Future<Output = Result<(Vec<T>, Option<u32>)>>,
{
    let mut page = 0_u32;
    let mut items = Vec::new();
    for _ in 0..10_000 {
        let (chunk, next) = fetch(page, page_size).await?;
        items.extend(chunk);
        match next {
            Some(next_page) => page = next_page,
            None => return Ok(items),
        }
    }
    Err(Error::protocol("pagination exceeded 10000 pages"))
}

async fn send_admin_request(
    server: &str,
    credential: pb_mapper_core::checksum::Credential,
    request: AdminRequest,
    io_timeout: Duration,
) -> Result<AdminResponse> {
    let addr = pb_mapper_core::config::get_sockaddr_async(server)
        .await
        .map_err(|source| Error::Address {
            addr: server.to_string(),
            source,
        })?;
    let encoded = PbConnRequest::Admin(request)
        .encode()
        .map_err(|error| Error::protocol(error.to_string()))?;
    for attempt in 0..2 {
        let sent = std::sync::atomic::AtomicBool::new(false);
        let attempt_result = tokio::time::timeout(io_timeout, async {
            let mut stream = TcpStream::connect(addr).await.context(ConnectSnafu {
                addr: addr.to_string(),
            })?;
            let session = ClientHeaderSession::new_v2(&credential)
                .map_err(|error| Error::protocol(error.to_string()))?;
            session
                .write_initial(&mut stream, &encoded)
                .await
                .map_err(|error| Error::protocol(error.to_string()))?;
            sent.store(true, std::sync::atomic::Ordering::Release);
            let mut reader = session
                .response_reader(&mut stream)
                .map_err(|error| Error::protocol(error.to_string()))?;
            let message = reader
                .read_msg()
                .await
                .map_err(|error| Error::protocol(error.to_string()))?;
            PbConnResponse::decode(message).map_err(|error| Error::protocol(error.to_string()))
        })
        .await
        .map_err(|_| Error::TimedOut {
            timeout: io_timeout,
        });
        let pre_send = !sent.load(std::sync::atomic::Ordering::Acquire);
        let response = match attempt_result {
            Ok(Ok(response)) => response,
            Ok(Err(_)) if attempt == 0 && pre_send => continue,
            Ok(Err(error)) => return Err(error),
            Err(_) if attempt == 0 && pre_send => continue,
            Err(error) => return Err(error),
        };
        match response {
            PbConnResponse::Admin(response) => return Ok(response),
            PbConnResponse::Error(error)
                if error.code == "connection_salt_replayed" && error.retryable =>
            {
                if attempt == 0 {
                    continue;
                }
                return Err(Error::from_remote(error));
            }
            PbConnResponse::Error(error) => return Err(Error::from_remote(error)),
            response => {
                return Err(Error::protocol(format!(
                    "unexpected administrator response: {response:?}"
                )));
            }
        }
    }
    Err(Error::protocol(
        "connection salt replay retry was exhausted",
    ))
}

impl From<IssuedTemporaryKey> for IssuedKey {
    fn from(value: IssuedTemporaryKey) -> Self {
        Self {
            key_id: value.metadata.key_id.as_u64(),
            state: value.metadata.state,
            issued_at: value.metadata.issued_at,
            expires_at: value.metadata.expires_at,
            label: value.metadata.label,
            credential: value.credential,
        }
    }
}

impl From<TemporaryKeyMetadata> for KeyMetadata {
    fn from(value: TemporaryKeyMetadata) -> Self {
        Self {
            key_id: value.key_id.as_u64(),
            state: value.state,
            issued_at: value.issued_at,
            expires_at: value.expires_at,
            label: value.label,
        }
    }
}

impl From<KeyPage> for KeyListPage {
    fn from(value: KeyPage) -> Self {
        Self {
            schema_version: value.schema_version,
            items: value.items.into_iter().map(KeyMetadata::from).collect(),
            next_page: value.next_page,
        }
    }
}

impl From<AdminServiceInfo> for ServiceInfo {
    fn from(value: AdminServiceInfo) -> Self {
        Self {
            key_id: value.key_id,
            namespace: value.namespace,
            service_name: value.service_name,
            transport: value.transport,
            codec_enabled: value.codec_enabled,
            connection_count: value.connection_count,
        }
    }
}

impl From<AdminServicePage> for ServicePage {
    fn from(value: AdminServicePage) -> Self {
        Self {
            schema_version: value.schema_version,
            items: value.items.into_iter().map(ServiceInfo::from).collect(),
            next_page: value.next_page,
        }
    }
}

impl From<AdminConnectionInfo> for ConnectionInfo {
    fn from(value: AdminConnectionInfo) -> Self {
        Self {
            key_id: value.key_id,
            namespace: value.namespace,
            service_name: value.service_name,
            conn_id: value.conn_id,
            generation: value.generation,
            protocol_version: value.protocol_version,
            healthy: value.healthy,
            transport: value.transport,
            codec_enabled: value.codec_enabled,
            last_rx_age_ms: value.last_rx_age_ms,
        }
    }
}

impl From<AdminConnectionPage> for ConnectionPage {
    fn from(value: AdminConnectionPage) -> Self {
        Self {
            schema_version: value.schema_version,
            items: value.items.into_iter().map(ConnectionInfo::from).collect(),
            next_page: value.next_page,
        }
    }
}

impl From<AuthStatus> for AuthStatusInfo {
    fn from(value: AuthStatus) -> Self {
        Self {
            schema_version: value.schema_version,
            safe_mode: value.safe_mode,
            capacity: value.capacity,
            active_keys: value.active_keys,
            expired_keys: value.expired_keys,
            revoked_keys: value.revoked_keys,
            legacy_protocol: value.legacy_protocol.into(),
            active_legacy_connections: value.active_legacy_connections,
            last_legacy_connection_at: value.last_legacy_connection_at,
            auth_successes: value.auth_successes,
            auth_failures: value.auth_failures,
            server_instance_id: value.server_instance_id,
        }
    }
}
