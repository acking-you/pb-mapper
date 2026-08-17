//! Authentication state for protocol-v2 connections and administrator operations.
//!
//! The root administrator key is never copied into a temporary credential. Temporary
//! keys are derived from `(root key, server instance id, key id)` and the hot slot table
//! stores only lifecycle metadata plus a weak lease reference. The background actor owns
//! the strong leases through a hierarchical timing wheel.

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, RwLock, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rand::RngExt;
use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM};
use ring::hkdf::{Salt, HKDF_SHA256};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use super::checksum::{
    encode_temporary_credential, get_process_credential, parse_credential,
    set_process_msg_header_key, AesKeyType, Credential, MACHINE_MSG_HEADER_KEY_PATH,
};

pub const ADMIN_NAMESPACE: u64 = 0;
pub const DEFAULT_AUTH_STATE_DIR: &str = "/var/lib/pb-mapper/auth";
pub const DEFAULT_TEMP_KEY_CAPACITY: usize = 65_536;
pub const DEFAULT_MAX_TEMP_KEY_TTL: Duration = Duration::from_secs(30 * 24 * 60 * 60);
pub const MIN_TEMP_KEY_TTL: Duration = Duration::from_secs(10);
const TOMBSTONE_RETENTION: Duration = Duration::from_secs(60);
const SNAPSHOT_COMPACTION_INTERVAL: Duration = Duration::from_secs(5 * 60);
const SNAPSHOT_SCHEMA_VERSION: u16 = 1;
const STATE_BLOB_MAGIC: &[u8; 5] = b"PBAS1";
const STATE_AAD: &[u8] = b"pb-mapper-auth-state-v1";
const INSTANCE_ID_LEN: usize = 16;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LegacyProtocolPolicy {
    Allow,
    Deny,
}

impl LegacyProtocolPolicy {
    pub fn is_allowed(self) -> bool {
        matches!(self, Self::Allow)
    }
}

#[derive(Clone, Debug)]
pub struct AuthConfig {
    pub state_dir: PathBuf,
    pub max_temporary_keys: usize,
    pub max_temporary_key_ttl: Duration,
    pub legacy_protocol: LegacyProtocolPolicy,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            state_dir: std::env::var_os("PB_MAPPER_AUTH_STATE_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(DEFAULT_AUTH_STATE_DIR)),
            max_temporary_keys: env_usize(
                "PB_MAPPER_AUTH_MAX_TEMP_KEYS",
                DEFAULT_TEMP_KEY_CAPACITY,
                1,
                1_048_576,
            ),
            max_temporary_key_ttl: Duration::from_secs(env_u64(
                "PB_MAPPER_AUTH_MAX_TEMP_TTL_SECS",
                DEFAULT_MAX_TEMP_KEY_TTL.as_secs(),
                MIN_TEMP_KEY_TTL.as_secs(),
                365 * 24 * 60 * 60,
            )),
            legacy_protocol: match std::env::var("PB_MAPPER_LEGACY_PROTOCOL")
                .unwrap_or_else(|_| "allow".to_string())
                .to_ascii_lowercase()
                .as_str()
            {
                "deny" => LegacyProtocolPolicy::Deny,
                _ => LegacyProtocolPolicy::Allow,
            },
        }
    }
}

fn env_usize(name: &str, default: usize, min: usize, max: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| (*value >= min) && (*value <= max))
        .unwrap_or(default)
}

fn env_u64(name: &str, default: u64, min: u64, max: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| (*value >= min) && (*value <= max))
        .unwrap_or(default)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AuthFailure {
    pub code: String,
    pub message: String,
    pub retryable: bool,
}

impl AuthFailure {
    pub fn new(code: impl Into<String>, message: impl Into<String>, retryable: bool) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            retryable,
        }
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new("auth_internal_error", message, false)
    }
}

impl fmt::Display for AuthFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for AuthFailure {}

#[derive(Debug)]
pub struct AuthLease {
    key_id: u64,
    expires_at: AtomicU64,
    wheel_version: AtomicU64,
    cancellation: CancellationToken,
}

impl AuthLease {
    fn new(key_id: u64, expires_at: u64) -> Self {
        Self {
            key_id,
            expires_at: AtomicU64::new(expires_at),
            wheel_version: AtomicU64::new(1),
            cancellation: CancellationToken::new(),
        }
    }

    pub fn key_id(&self) -> u64 {
        self.key_id
    }

    pub fn expires_at(&self) -> u64 {
        self.expires_at.load(Ordering::Acquire)
    }

    pub fn cancellation_token(&self) -> CancellationToken {
        self.cancellation.clone()
    }
}

#[derive(Clone, Debug)]
pub struct AuthContext {
    pub key_id: u64,
    pub namespace: u64,
    pub is_admin: bool,
    lease: Weak<AuthLease>,
}

impl AuthContext {
    fn from_lease(key_id: u64, is_admin: bool, lease: &Arc<AuthLease>) -> Self {
        Self {
            key_id,
            namespace: if is_admin { ADMIN_NAMESPACE } else { key_id },
            is_admin,
            lease: Arc::downgrade(lease),
        }
    }

    pub fn ensure_active(&self) -> Result<Arc<AuthLease>, AuthFailure> {
        let lease = self.lease.upgrade().ok_or_else(|| {
            AuthFailure::new(
                if self.is_admin {
                    "administrator_key_rotated"
                } else {
                    "temporary_key_inactive"
                },
                "credential lease is no longer active",
                false,
            )
        })?;
        if lease.cancellation.is_cancelled() {
            return Err(AuthFailure::new(
                if self.is_admin {
                    "administrator_key_rotated"
                } else {
                    "temporary_key_revoked"
                },
                "credential lease has been cancelled",
                false,
            ));
        }
        if !self.is_admin && lease.expires_at() <= unix_seconds() {
            lease.cancellation.cancel();
            return Err(AuthFailure::new(
                "temporary_key_expired",
                "temporary key has expired",
                false,
            ));
        }
        Ok(lease)
    }

    pub fn cancellation_token(&self) -> Result<CancellationToken, AuthFailure> {
        Ok(self.ensure_active()?.cancellation_token())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SlotState {
    Free,
    Active,
    Expired,
    Revoked,
}

#[derive(Debug)]
struct SlotHot {
    generation: u32,
    state: SlotState,
    expires_at: u64,
    lease: Weak<AuthLease>,
}

impl Default for SlotHot {
    fn default() -> Self {
        Self {
            generation: 0,
            state: SlotState::Free,
            expires_at: 0,
            lease: Weak::new(),
        }
    }
}

#[derive(Debug)]
struct AuthStateInner {
    admin_key: RwLock<AesKeyType>,
    admin_lease: RwLock<Weak<AuthLease>>,
    instance_id: RwLock<[u8; INSTANCE_ID_LEN]>,
    slots: RwLock<Box<[SlotHot]>>,
    safe_mode: AtomicBool,
    legacy_protocol_allowed: AtomicBool,
    active_legacy_connections: AtomicU64,
    last_legacy_connection_at: AtomicU64,
    auth_successes: AtomicU64,
    auth_failures: AtomicU64,
}

impl AuthStateInner {
    fn admin_key(&self) -> AesKeyType {
        *self
            .admin_key
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn instance_id(&self) -> [u8; INSTANCE_ID_LEN] {
        *self
            .instance_id
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[derive(Clone)]
pub struct AuthRuntime {
    inner: Weak<AuthStateInner>,
    command_tx: mpsc::Sender<AuthCommand>,
    config: AuthConfig,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TemporaryKeyMetadata {
    pub key_id: u64,
    pub state: String,
    pub issued_at: u64,
    pub expires_at: u64,
    pub label: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IssuedTemporaryKey {
    #[serde(flatten)]
    pub metadata: TemporaryKeyMetadata,
    pub credential: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KeyPage {
    pub schema_version: u16,
    pub items: Vec<TemporaryKeyMetadata>,
    pub next_page: Option<u32>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AuthStatus {
    pub schema_version: u16,
    pub safe_mode: bool,
    pub capacity: usize,
    pub active_keys: usize,
    pub expired_keys: usize,
    pub revoked_keys: usize,
    pub legacy_protocol: LegacyProtocolPolicy,
    pub active_legacy_connections: u64,
    pub last_legacy_connection_at: Option<u64>,
    pub auth_successes: u64,
    pub auth_failures: u64,
    pub server_instance_id: String,
}

#[derive(Clone, Debug)]
struct ColdMetadata {
    issued_at: u64,
    label: Option<String>,
}

enum AuthCommand {
    Issue {
        ttl: Duration,
        label: Option<String>,
        response: oneshot::Sender<Result<IssuedTemporaryKey, AuthFailure>>,
    },
    List {
        page: u32,
        page_size: u16,
        response: oneshot::Sender<Result<KeyPage, AuthFailure>>,
    },
    Show {
        key_id: u64,
        reveal: bool,
        response: oneshot::Sender<Result<IssuedTemporaryKey, AuthFailure>>,
    },
    Renew {
        key_id: u64,
        ttl: Duration,
        response: oneshot::Sender<Result<IssuedTemporaryKey, AuthFailure>>,
    },
    Revoke {
        key_id: u64,
        response: oneshot::Sender<Result<TemporaryKeyMetadata, AuthFailure>>,
    },
    Gc {
        response: oneshot::Sender<Result<u64, AuthFailure>>,
    },
    Reset {
        response: oneshot::Sender<Result<(), AuthFailure>>,
    },
    RotateRoot {
        new_key: AesKeyType,
        response: oneshot::Sender<Result<(), AuthFailure>>,
    },
    SetLegacyProtocol {
        policy: LegacyProtocolPolicy,
        response: oneshot::Sender<Result<(), AuthFailure>>,
    },
    Status {
        response: oneshot::Sender<Result<AuthStatus, AuthFailure>>,
    },
    Audit {
        action: String,
        key_id: Option<u64>,
        detail: Option<String>,
        response: oneshot::Sender<Result<(), AuthFailure>>,
    },
}

impl AuthRuntime {
    pub async fn from_process(config: AuthConfig) -> Result<Self, AuthFailure> {
        prepare_state_dir(&config.state_dir)?;
        let credential = load_server_admin_credential(&config.state_dir)?;
        let Credential::Admin(admin_key) = credential else {
            return Err(AuthFailure::new(
                "administrator_key_required",
                "the relay server must start with the administrator credential",
                false,
            ));
        };
        Self::start(admin_key, config).await
    }

    pub async fn start(admin_key: AesKeyType, config: AuthConfig) -> Result<Self, AuthFailure> {
        prepare_state_dir(&config.state_dir)?;
        let instance_id = load_or_create_instance_id(&config.state_dir)?;
        let (loaded, safe_mode) = load_persisted_state(&config, &admin_key, instance_id);
        let mut slots = (0..config.max_temporary_keys)
            .map(|_| SlotHot::default())
            .collect::<Vec<_>>()
            .into_boxed_slice();
        let mut cold = HashMap::new();
        let mut wheel = TimingWheel::new(unix_seconds());
        let now = unix_seconds();

        let admin_lease = Arc::new(AuthLease::new(0, u64::MAX));
        if let Some(state) = loaded.as_ref() {
            for (index, generation) in state.generations.iter().copied().enumerate() {
                if let Some(slot) = slots.get_mut(index) {
                    slot.generation = generation;
                }
            }
            for entry in &state.entries {
                let index = key_slot(entry.key_id) as usize;
                let Some(slot) = slots.get_mut(index) else {
                    continue;
                };
                if slot.generation != key_generation(entry.key_id) {
                    continue;
                }
                let state = if entry.state == SlotState::Active && entry.expires_at <= now {
                    SlotState::Expired
                } else {
                    entry.state
                };
                slot.state = state;
                slot.expires_at = entry.expires_at;
                cold.insert(
                    entry.key_id,
                    ColdMetadata {
                        issued_at: entry.issued_at,
                        label: entry.label.clone(),
                    },
                );
                if state == SlotState::Active {
                    let lease = Arc::new(AuthLease::new(entry.key_id, entry.expires_at));
                    slot.lease = Arc::downgrade(&lease);
                    wheel.insert(lease);
                }
            }
        }

        let legacy_protocol = loaded
            .as_ref()
            .map(|state| state.legacy_protocol)
            .unwrap_or(config.legacy_protocol);
        let inner = Arc::new(AuthStateInner {
            admin_key: RwLock::new(admin_key),
            admin_lease: RwLock::new(Arc::downgrade(&admin_lease)),
            instance_id: RwLock::new(instance_id),
            slots: RwLock::new(slots),
            safe_mode: AtomicBool::new(safe_mode),
            legacy_protocol_allowed: AtomicBool::new(legacy_protocol.is_allowed()),
            active_legacy_connections: AtomicU64::new(0),
            last_legacy_connection_at: AtomicU64::new(0),
            auth_successes: AtomicU64::new(0),
            auth_failures: AtomicU64::new(0),
        });
        let (command_tx, command_rx) = mpsc::channel(256);
        let runtime = Self {
            inner: Arc::downgrade(&inner),
            command_tx,
            config: config.clone(),
        };

        tokio::spawn(run_auth_actor(
            inner,
            admin_lease,
            command_rx,
            config,
            cold,
            wheel,
        ));
        Ok(runtime)
    }

    pub fn config(&self) -> &AuthConfig {
        &self.config
    }

    fn inner(&self) -> Result<Arc<AuthStateInner>, AuthFailure> {
        self.inner.upgrade().ok_or_else(|| {
            AuthFailure::new(
                "auth_state_unavailable",
                "authentication state manager is not running",
                true,
            )
        })
    }

    pub fn admin_key(&self) -> Result<AesKeyType, AuthFailure> {
        Ok(self.inner()?.admin_key())
    }

    pub fn derive_key(&self, key_id: u64) -> Result<AesKeyType, AuthFailure> {
        let inner = self.inner()?;
        if key_id == 0 {
            return Ok(inner.admin_key());
        }
        derive_temporary_key(&inner.admin_key(), &inner.instance_id(), key_id)
    }

    pub fn authenticate(&self, key_id: u64) -> Result<AuthContext, AuthFailure> {
        let inner = self.inner()?;
        if key_id == 0 {
            let lease = inner
                .admin_lease
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .upgrade()
                .ok_or_else(|| {
                    AuthFailure::new(
                        "administrator_key_rotated",
                        "administrator credential was rotated",
                        false,
                    )
                })?;
            inner.auth_successes.fetch_add(1, Ordering::Relaxed);
            return Ok(AuthContext::from_lease(0, true, &lease));
        }
        if inner.safe_mode.load(Ordering::Acquire) {
            inner.auth_failures.fetch_add(1, Ordering::Relaxed);
            return Err(AuthFailure::new(
                "temporary_key_store_unavailable",
                "temporary key state is unavailable; administrator reset is required",
                false,
            ));
        }

        let index = key_slot(key_id) as usize;
        let generation = key_generation(key_id);
        let slots = inner
            .slots
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(slot) = slots.get(index) else {
            inner.auth_failures.fetch_add(1, Ordering::Relaxed);
            return Err(AuthFailure::new(
                "temporary_key_not_found",
                "temporary key id is outside the configured slot table",
                false,
            ));
        };
        if slot.generation != generation {
            inner.auth_failures.fetch_add(1, Ordering::Relaxed);
            return Err(AuthFailure::new(
                "temporary_key_generation_mismatch",
                "temporary key generation does not match the current slot",
                false,
            ));
        }
        let failure = match slot.state {
            SlotState::Free => Some(AuthFailure::new(
                "temporary_key_not_found",
                "temporary key does not exist",
                false,
            )),
            SlotState::Expired => Some(AuthFailure::new(
                "temporary_key_expired",
                "temporary key has expired",
                false,
            )),
            SlotState::Revoked => Some(AuthFailure::new(
                "temporary_key_revoked",
                "temporary key was revoked",
                false,
            )),
            SlotState::Active if slot.expires_at <= unix_seconds() => {
                if let Some(lease) = slot.lease.upgrade() {
                    lease.cancellation.cancel();
                }
                Some(AuthFailure::new(
                    "temporary_key_expired",
                    "temporary key has expired",
                    false,
                ))
            }
            SlotState::Active => None,
        };
        if let Some(failure) = failure {
            inner.auth_failures.fetch_add(1, Ordering::Relaxed);
            return Err(failure);
        }
        let lease = slot.lease.upgrade().ok_or_else(|| {
            inner.auth_failures.fetch_add(1, Ordering::Relaxed);
            AuthFailure::new(
                "temporary_key_inactive",
                "temporary key lease is no longer active",
                true,
            )
        })?;
        inner.auth_successes.fetch_add(1, Ordering::Relaxed);
        Ok(AuthContext::from_lease(key_id, false, &lease))
    }

    pub fn legacy_protocol_allowed(&self) -> Result<bool, AuthFailure> {
        Ok(self
            .inner()?
            .legacy_protocol_allowed
            .load(Ordering::Acquire))
    }

    pub fn record_legacy_connection(&self) -> Result<LegacyConnectionGuard, AuthFailure> {
        let inner = self.inner()?;
        inner
            .active_legacy_connections
            .fetch_add(1, Ordering::AcqRel);
        inner
            .last_legacy_connection_at
            .store(unix_seconds(), Ordering::Release);
        Ok(LegacyConnectionGuard {
            inner: Arc::downgrade(&inner),
        })
    }

    async fn request<T>(
        &self,
        build: impl FnOnce(oneshot::Sender<Result<T, AuthFailure>>) -> AuthCommand,
    ) -> Result<T, AuthFailure> {
        let (response, receiver) = oneshot::channel();
        self.command_tx.send(build(response)).await.map_err(|_| {
            AuthFailure::new(
                "auth_state_unavailable",
                "authentication state manager is not running",
                true,
            )
        })?;
        receiver.await.map_err(|_| {
            AuthFailure::new(
                "auth_state_unavailable",
                "authentication state manager dropped the response",
                true,
            )
        })?
    }

    pub async fn issue(
        &self,
        ttl: Duration,
        label: Option<String>,
    ) -> Result<IssuedTemporaryKey, AuthFailure> {
        self.request(|response| AuthCommand::Issue {
            ttl,
            label,
            response,
        })
        .await
    }

    pub async fn list(&self, page: u32, page_size: u16) -> Result<KeyPage, AuthFailure> {
        self.request(|response| AuthCommand::List {
            page,
            page_size,
            response,
        })
        .await
    }

    pub async fn show(&self, key_id: u64, reveal: bool) -> Result<IssuedTemporaryKey, AuthFailure> {
        self.request(|response| AuthCommand::Show {
            key_id,
            reveal,
            response,
        })
        .await
    }

    pub async fn renew(
        &self,
        key_id: u64,
        ttl: Duration,
    ) -> Result<IssuedTemporaryKey, AuthFailure> {
        self.request(|response| AuthCommand::Renew {
            key_id,
            ttl,
            response,
        })
        .await
    }

    pub async fn revoke(&self, key_id: u64) -> Result<TemporaryKeyMetadata, AuthFailure> {
        self.request(|response| AuthCommand::Revoke { key_id, response })
            .await
    }

    pub async fn gc(&self) -> Result<u64, AuthFailure> {
        self.request(|response| AuthCommand::Gc { response }).await
    }

    pub async fn reset(&self) -> Result<(), AuthFailure> {
        self.request(|response| AuthCommand::Reset { response })
            .await
    }

    pub async fn rotate_root(&self, new_key: AesKeyType) -> Result<(), AuthFailure> {
        self.request(|response| AuthCommand::RotateRoot { new_key, response })
            .await
    }

    pub async fn set_legacy_protocol(
        &self,
        policy: LegacyProtocolPolicy,
    ) -> Result<(), AuthFailure> {
        self.request(|response| AuthCommand::SetLegacyProtocol { policy, response })
            .await
    }

    pub async fn status(&self) -> Result<AuthStatus, AuthFailure> {
        self.request(|response| AuthCommand::Status { response })
            .await
    }

    pub async fn audit_admin(
        &self,
        action: impl Into<String>,
        key_id: Option<u64>,
        detail: Option<String>,
    ) -> Result<(), AuthFailure> {
        let action = action.into();
        self.request(|response| AuthCommand::Audit {
            action,
            key_id,
            detail,
            response,
        })
        .await
    }
}

pub struct LegacyConnectionGuard {
    inner: Weak<AuthStateInner>,
}

impl Drop for LegacyConnectionGuard {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.upgrade() {
            inner
                .active_legacy_connections
                .fetch_sub(1, Ordering::AcqRel);
        }
    }
}

fn load_server_admin_credential(state_dir: &Path) -> Result<Credential, AuthFailure> {
    let path = state_dir.join("admin.key");
    let raw = if path.exists() {
        #[cfg(unix)]
        {
            let metadata = std::fs::metadata(&path).map_err(|error| {
                AuthFailure::new(
                    "administrator_key_required",
                    format!(
                        "administrator key file `{}` metadata could not be read: {error}",
                        path.display()
                    ),
                    false,
                )
            })?;
            if metadata.permissions().mode() & 0o077 != 0 {
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).map_err(
                    |error| {
                        AuthFailure::new(
                            "administrator_key_required",
                            format!(
                                "administrator key file `{}` permissions could not be secured: {error}",
                                path.display()
                            ),
                            false,
                        )
                    },
                )?;
                tracing::warn!(
                    event = "administrator_key_permissions_repaired",
                    path = %path.display(),
                    "restricted administrator key file permissions to 0600"
                );
            }
        }
        std::fs::read_to_string(&path).map_err(|error| {
            AuthFailure::new(
                "administrator_key_required",
                format!(
                    "administrator key file `{}` could not be read: {error}",
                    path.display()
                ),
                false,
            )
        })?
    } else if let Ok(credential) = get_process_credential() {
        let Credential::Admin(key) = credential else {
            return Err(AuthFailure::new(
                "administrator_key_required",
                "the relay server cannot start with a temporary credential",
                false,
            ));
        };
        let key = String::from_utf8(key.to_vec()).map_err(|_| {
            AuthFailure::new(
                "administrator_key_invalid",
                "the relay administrator key must be printable UTF-8 so it can be persisted",
                false,
            )
        })?;
        write_admin_key(state_dir, &key)?;
        key
    } else if Path::new(MACHINE_MSG_HEADER_KEY_PATH).is_file() {
        let key = std::fs::read_to_string(MACHINE_MSG_HEADER_KEY_PATH).map_err(|error| {
            AuthFailure::new(
                "administrator_key_required",
                format!(
                    "legacy administrator key file `{MACHINE_MSG_HEADER_KEY_PATH}` could not be read: {error}"
                ),
                false,
            )
        })?;
        let Credential::Admin(_) = parse_credential(key.trim())
            .map_err(|error| AuthFailure::new("administrator_key_invalid", error, false))?
        else {
            return Err(AuthFailure::new(
                "administrator_key_required",
                "the legacy server key file contains a temporary credential",
                false,
            ));
        };
        write_admin_key(state_dir, key.trim())?;
        tracing::warn!(
            event = "administrator_key_migrated",
            source = MACHINE_MSG_HEADER_KEY_PATH,
            destination = %path.display(),
            "migrated the legacy administrator key into the v0.4 authentication state directory"
        );
        key
    } else {
        let key = initialize_admin_key(&path, false)?;
        tracing::warn!(
            event = "administrator_key_initialized",
            path = %path.display(),
            "no administrator credential was configured; generated a random key file"
        );
        key
    };
    let credential = parse_credential(raw.trim())
        .map_err(|error| AuthFailure::new("administrator_key_invalid", error, false))?;
    if !credential.is_admin() {
        return Err(AuthFailure::new(
            "administrator_key_required",
            "the server key file contains a temporary credential",
            false,
        ));
    }
    set_process_msg_header_key(Some(raw.trim())).map_err(AuthFailure::internal)?;
    Ok(credential)
}

pub fn make_key_id(generation: u32, slot: u32) -> u64 {
    (u64::from(generation) << 32) | u64::from(slot)
}

pub fn key_generation(key_id: u64) -> u32 {
    (key_id >> 32) as u32
}

pub fn key_slot(key_id: u64) -> u32 {
    key_id as u32
}

pub fn derive_temporary_key(
    admin_key: &AesKeyType,
    instance_id: &[u8; INSTANCE_ID_LEN],
    key_id: u64,
) -> Result<AesKeyType, AuthFailure> {
    let salt = Salt::new(HKDF_SHA256, instance_id);
    let pseudo_random_key = salt.extract(admin_key);
    let key_id_bytes = key_id.to_be_bytes();
    let info = [b"pb-mapper-temp-key-v1".as_slice(), key_id_bytes.as_slice()];
    let output = pseudo_random_key
        .expand(&info, HkdfLen(32))
        .map_err(|_| AuthFailure::internal("failed to expand temporary key"))?;
    let mut key = [0_u8; 32];
    output
        .fill(&mut key)
        .map_err(|_| AuthFailure::internal("failed to fill temporary key"))?;
    Ok(key)
}

struct HkdfLen(usize);

impl ring::hkdf::KeyType for HkdfLen {
    fn len(&self) -> usize {
        self.0
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct PersistedEntry {
    key_id: u64,
    state: SlotState,
    issued_at: u64,
    expires_at: u64,
    label: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct PersistedSnapshot {
    schema_version: u16,
    instance_id: [u8; INSTANCE_ID_LEN],
    generations: Vec<u32>,
    entries: Vec<PersistedEntry>,
    legacy_protocol: LegacyProtocolPolicy,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
enum StateMutation {
    Issue(PersistedEntry),
    Renew { key_id: u64, expires_at: u64 },
    Revoke { key_id: u64, at: u64 },
    LegacyProtocol(LegacyProtocolPolicy),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct AuditRecord {
    at: u64,
    action: String,
    key_id: Option<u64>,
    label: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
enum WalRecord {
    Mutation {
        mutation: StateMutation,
        audit: AuditRecord,
    },
    Audit(AuditRecord),
}

async fn run_auth_actor(
    inner: Arc<AuthStateInner>,
    mut admin_lease: Arc<AuthLease>,
    mut command_rx: mpsc::Receiver<AuthCommand>,
    config: AuthConfig,
    mut cold: HashMap<u64, ColdMetadata>,
    mut wheel: TimingWheel,
) {
    let now = unix_seconds();
    let mut tombstones = inner
        .slots
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .iter()
        .enumerate()
        .filter_map(|(index, slot)| {
            matches!(slot.state, SlotState::Expired | SlotState::Revoked).then_some((
                now.saturating_add(TOMBSTONE_RETENTION.as_secs()),
                make_key_id(slot.generation, index as u32),
            ))
        })
        .collect::<VecDeque<_>>();
    let mut last_snapshot_at = unix_seconds();
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = tick.tick() => {
                let now = unix_seconds();
                for lease in wheel.advance(now) {
                    let key_id = lease.key_id();
                    let version = lease.wheel_version.load(Ordering::Acquire);
                    if lease.expires_at() > now {
                        wheel.insert_with_version(lease, version);
                        continue;
                    }
                    let mut slots = inner.slots.write().unwrap_or_else(|poisoned| poisoned.into_inner());
                    if let Some(slot) = slots.get_mut(key_slot(key_id) as usize) {
                        if slot.generation == key_generation(key_id) && slot.state == SlotState::Active {
                            slot.state = SlotState::Expired;
                            lease.cancellation.cancel();
                            tombstones.push_back((now.saturating_add(TOMBSTONE_RETENTION.as_secs()), key_id));
                            tracing::info!(
                                event = "temporary_key_expired",
                                auth_stage = "expiry",
                                key_id,
                                expires_at = lease.expires_at(),
                                "temporary key expired and active work was cancelled"
                            );
                        }
                    }
                }
                while let Some((cleanup_at, key_id)) = tombstones.front().copied() {
                    if cleanup_at > now {
                        break;
                    }
                    tombstones.pop_front();
                    let mut slots = inner.slots.write().unwrap_or_else(|poisoned| poisoned.into_inner());
                    if let Some(slot) = slots.get_mut(key_slot(key_id) as usize) {
                        if slot.generation == key_generation(key_id) && matches!(slot.state, SlotState::Expired | SlotState::Revoked) {
                            slot.state = SlotState::Free;
                            slot.expires_at = 0;
                            slot.lease = Weak::new();
                            cold.remove(&key_id);
                        }
                    }
                }
                if now.saturating_sub(last_snapshot_at) >= SNAPSHOT_COMPACTION_INTERVAL.as_secs() {
                    let snapshot = build_snapshot(&inner, &cold);
                    if let Err(error) = write_snapshot_and_truncate_wal(
                        &config,
                        &inner.admin_key(),
                        &snapshot,
                    ) {
                        inner.safe_mode.store(true, Ordering::Release);
                        cancel_all_temporary_leases(&inner);
                        tracing::error!(
                            event = "auth_state_safe_mode",
                            auth_stage = "snapshot_compaction",
                            reason = %error.code,
                            error = %error,
                            "authentication state compaction failed closed"
                        );
                    } else {
                        last_snapshot_at = now;
                    }
                }
            }
            command = command_rx.recv() => {
                let Some(command) = command else {
                    admin_lease.cancellation.cancel();
                    cancel_all_temporary_leases(&inner);
                    break;
                };
                match command {
                    AuthCommand::Issue { ttl, label, response } => {
                        let result = actor_issue(&inner, &config, &mut cold, &mut wheel, ttl, label);
                        let _ = response.send(result);
                    }
                    AuthCommand::List { page, page_size, response } => {
                        let _ = response.send(actor_list(&inner, &cold, page, page_size));
                    }
                    AuthCommand::Show { key_id, reveal, response } => {
                        let result = actor_show(&inner, &config, &cold, key_id, reveal);
                        let _ = response.send(result);
                    }
                    AuthCommand::Renew { key_id, ttl, response } => {
                        let result = actor_renew(&inner, &config, &cold, &mut wheel, key_id, ttl);
                        let _ = response.send(result);
                    }
                    AuthCommand::Revoke { key_id, response } => {
                        let result = actor_revoke(&inner, &config, &cold, &mut tombstones, key_id);
                        let _ = response.send(result);
                    }
                    AuthCommand::Gc { response } => {
                        let result = actor_gc(&inner, &config, &mut cold, &mut tombstones);
                        let _ = response.send(result);
                    }
                    AuthCommand::Reset { response } => {
                        let result = actor_reset(&inner, &config, &mut cold, &mut wheel, "auth_state_reset");
                        let _ = response.send(result);
                    }
                    AuthCommand::RotateRoot { new_key, response } => {
                        let result = actor_rotate_root(&inner, &config, &mut cold, &mut wheel, &mut admin_lease, new_key);
                        let _ = response.send(result);
                    }
                    AuthCommand::SetLegacyProtocol { policy, response } => {
                        let result = actor_set_legacy_protocol(&inner, &config, policy);
                        let _ = response.send(result);
                    }
                    AuthCommand::Status { response } => {
                        let _ = response.send(Ok(actor_status(&inner)));
                    }
                    AuthCommand::Audit { action, key_id, detail, response } => {
                        let result = append_audit(
                            &config,
                            &inner.admin_key(),
                            audit(&action, key_id, detail),
                        );
                        let _ = response.send(result);
                    }
                }
            }
        }
    }
}

fn validate_ttl(config: &AuthConfig, ttl: Duration) -> Result<u64, AuthFailure> {
    if ttl < MIN_TEMP_KEY_TTL {
        return Err(AuthFailure::new(
            "temporary_key_ttl_too_short",
            format!(
                "temporary key TTL must be at least {} seconds",
                MIN_TEMP_KEY_TTL.as_secs()
            ),
            false,
        ));
    }
    if ttl > config.max_temporary_key_ttl {
        return Err(AuthFailure::new(
            "temporary_key_ttl_too_long",
            format!(
                "temporary key TTL exceeds the configured maximum of {} seconds",
                config.max_temporary_key_ttl.as_secs()
            ),
            false,
        ));
    }
    Ok(unix_seconds().saturating_add(ttl.as_secs()))
}

fn validate_label(label: Option<String>) -> Result<Option<String>, AuthFailure> {
    let label = label
        .map(|label| label.trim().to_string())
        .filter(|label| !label.is_empty());
    if label.as_ref().is_some_and(|label| label.len() > 64) {
        return Err(AuthFailure::new(
            "temporary_key_label_too_long",
            "temporary key label must not exceed 64 UTF-8 bytes",
            false,
        ));
    }
    Ok(label)
}

fn actor_issue(
    inner: &Arc<AuthStateInner>,
    config: &AuthConfig,
    cold: &mut HashMap<u64, ColdMetadata>,
    wheel: &mut TimingWheel,
    ttl: Duration,
    label: Option<String>,
) -> Result<IssuedTemporaryKey, AuthFailure> {
    ensure_store_available(inner)?;
    let expires_at = validate_ttl(config, ttl)?;
    let label = validate_label(label)?;
    let issued_at = unix_seconds();
    let mut slots = inner
        .slots
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some((index, slot)) = slots
        .iter_mut()
        .enumerate()
        .find(|(_, slot)| slot.state == SlotState::Free && slot.generation < u32::MAX)
    else {
        return Err(AuthFailure::new(
            "temporary_key_capacity_exhausted",
            "temporary key slot table is full",
            true,
        ));
    };
    let generation = slot.generation + 1;
    let key_id = make_key_id(generation, index as u32);
    let entry = PersistedEntry {
        key_id,
        state: SlotState::Active,
        issued_at,
        expires_at,
        label: label.clone(),
    };
    append_mutation(
        config,
        &inner.admin_key(),
        StateMutation::Issue(entry.clone()),
        audit("temporary_key_issue", Some(key_id), label.clone()),
    )?;
    let lease = Arc::new(AuthLease::new(key_id, expires_at));
    slot.generation = generation;
    slot.state = SlotState::Active;
    slot.expires_at = expires_at;
    slot.lease = Arc::downgrade(&lease);
    cold.insert(key_id, ColdMetadata { issued_at, label });
    wheel.insert(lease);
    drop(slots);
    metadata_with_credential(inner, cold, key_id, true)
}

fn actor_list(
    inner: &Arc<AuthStateInner>,
    cold: &HashMap<u64, ColdMetadata>,
    page: u32,
    page_size: u16,
) -> Result<KeyPage, AuthFailure> {
    let page_size = page_size.clamp(1, 1000) as usize;
    let start = (page as usize).saturating_mul(page_size);
    let slots = inner
        .slots
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut all = slots
        .iter()
        .enumerate()
        .filter_map(|(index, slot)| {
            if slot.state == SlotState::Free {
                return None;
            }
            let key_id = make_key_id(slot.generation, index as u32);
            let cold = cold.get(&key_id)?;
            Some(TemporaryKeyMetadata {
                key_id,
                state: slot_state_name(slot.state).to_string(),
                issued_at: cold.issued_at,
                expires_at: slot.expires_at,
                label: cold.label.clone(),
            })
        })
        .collect::<Vec<_>>();
    all.sort_by_key(|item| std::cmp::Reverse(item.issued_at));
    let items = all.iter().skip(start).take(page_size).cloned().collect();
    let next_page = (start.saturating_add(page_size) < all.len()).then_some(page.saturating_add(1));
    Ok(KeyPage {
        schema_version: 1,
        items,
        next_page,
    })
}

fn actor_show(
    inner: &Arc<AuthStateInner>,
    config: &AuthConfig,
    cold: &HashMap<u64, ColdMetadata>,
    key_id: u64,
    reveal: bool,
) -> Result<IssuedTemporaryKey, AuthFailure> {
    let result = metadata_with_credential(inner, cold, key_id, reveal)?;
    append_audit(
        config,
        &inner.admin_key(),
        audit(
            if reveal {
                "temporary_key_reveal"
            } else {
                "temporary_key_show"
            },
            Some(key_id),
            result.metadata.label.clone(),
        ),
    )?;
    Ok(result)
}

fn actor_renew(
    inner: &Arc<AuthStateInner>,
    config: &AuthConfig,
    cold: &HashMap<u64, ColdMetadata>,
    wheel: &mut TimingWheel,
    key_id: u64,
    ttl: Duration,
) -> Result<IssuedTemporaryKey, AuthFailure> {
    ensure_store_available(inner)?;
    let expires_at = validate_ttl(config, ttl)?;
    let index = key_slot(key_id) as usize;
    let mut slots = inner
        .slots
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let slot = slots.get_mut(index).ok_or_else(|| key_not_found(key_id))?;
    validate_slot_identity(slot, key_id)?;
    if slot.state != SlotState::Active || slot.expires_at <= unix_seconds() {
        return Err(AuthFailure::new(
            "temporary_key_not_renewable",
            "only an active, unexpired temporary key can be renewed",
            false,
        ));
    }
    let label = cold
        .get(&key_id)
        .and_then(|metadata| metadata.label.clone());
    append_mutation(
        config,
        &inner.admin_key(),
        StateMutation::Renew { key_id, expires_at },
        audit("temporary_key_renew", Some(key_id), label),
    )?;
    let lease = slot.lease.upgrade().ok_or_else(|| {
        AuthFailure::new(
            "temporary_key_inactive",
            "temporary key lease is no longer active",
            true,
        )
    })?;
    slot.expires_at = expires_at;
    lease.expires_at.store(expires_at, Ordering::Release);
    lease.wheel_version.fetch_add(1, Ordering::AcqRel);
    wheel.insert(lease);
    drop(slots);
    metadata_with_credential(inner, cold, key_id, true)
}

fn actor_revoke(
    inner: &Arc<AuthStateInner>,
    config: &AuthConfig,
    cold: &HashMap<u64, ColdMetadata>,
    tombstones: &mut VecDeque<(u64, u64)>,
    key_id: u64,
) -> Result<TemporaryKeyMetadata, AuthFailure> {
    ensure_store_available(inner)?;
    let now = unix_seconds();
    let index = key_slot(key_id) as usize;
    let mut slots = inner
        .slots
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let slot = slots.get_mut(index).ok_or_else(|| key_not_found(key_id))?;
    validate_slot_identity(slot, key_id)?;
    if slot.state != SlotState::Active {
        return Err(AuthFailure::new(
            "temporary_key_not_active",
            "temporary key is not active",
            false,
        ));
    }
    let cold_metadata = cold.get(&key_id).ok_or_else(|| key_not_found(key_id))?;
    append_mutation(
        config,
        &inner.admin_key(),
        StateMutation::Revoke { key_id, at: now },
        audit(
            "temporary_key_revoke",
            Some(key_id),
            cold_metadata.label.clone(),
        ),
    )?;
    slot.state = SlotState::Revoked;
    if let Some(lease) = slot.lease.upgrade() {
        lease.cancellation.cancel();
    }
    tombstones.push_back((now.saturating_add(TOMBSTONE_RETENTION.as_secs()), key_id));
    Ok(TemporaryKeyMetadata {
        key_id,
        state: slot_state_name(slot.state).to_string(),
        issued_at: cold_metadata.issued_at,
        expires_at: slot.expires_at,
        label: cold_metadata.label.clone(),
    })
}

fn actor_gc(
    inner: &Arc<AuthStateInner>,
    config: &AuthConfig,
    cold: &mut HashMap<u64, ColdMetadata>,
    tombstones: &mut VecDeque<(u64, u64)>,
) -> Result<u64, AuthFailure> {
    ensure_store_available(inner)?;
    let now = unix_seconds();
    let mut removed = 0_u64;
    let mut slots = inner
        .slots
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    for (index, slot) in slots.iter_mut().enumerate() {
        if matches!(slot.state, SlotState::Expired | SlotState::Revoked)
            || (slot.state == SlotState::Active && slot.expires_at <= now)
        {
            let key_id = make_key_id(slot.generation, index as u32);
            if let Some(lease) = slot.lease.upgrade() {
                lease.cancellation.cancel();
            }
            slot.state = SlotState::Free;
            slot.expires_at = 0;
            slot.lease = Weak::new();
            cold.remove(&key_id);
            removed = removed.saturating_add(1);
        }
    }
    tombstones.clear();
    drop(slots);
    let snapshot = build_snapshot(inner, cold);
    let admin_key = inner.admin_key();
    if let Err(error) =
        write_snapshot_and_truncate_wal(config, &admin_key, &snapshot).and_then(|()| {
            append_audit(
                config,
                &admin_key,
                audit("temporary_key_gc", None, Some(format!("removed={removed}"))),
            )
        })
    {
        inner.safe_mode.store(true, Ordering::Release);
        cancel_all_temporary_leases(inner);
        return Err(error);
    }
    Ok(removed)
}

fn actor_reset(
    inner: &Arc<AuthStateInner>,
    config: &AuthConfig,
    cold: &mut HashMap<u64, ColdMetadata>,
    wheel: &mut TimingWheel,
    action: &str,
) -> Result<(), AuthFailure> {
    let new_instance_id = random_instance_id();
    let snapshot = empty_snapshot(inner, new_instance_id);
    let admin_key = inner.admin_key();
    if let Err(error) = write_snapshot_and_truncate_wal(config, &admin_key, &snapshot)
        .and_then(|()| append_audit(config, &admin_key, audit(action, None, None)))
    {
        inner.safe_mode.store(true, Ordering::Release);
        cancel_all_temporary_leases(inner);
        return Err(error);
    }
    if let Err(error) = atomic_write(
        &config.state_dir.join("server-instance-id"),
        &new_instance_id,
        0o600,
    ) {
        inner.safe_mode.store(true, Ordering::Release);
        cancel_all_temporary_leases(inner);
        return Err(error);
    }

    cancel_all_temporary_leases(inner);
    {
        let mut slots = inner
            .slots
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for slot in slots.iter_mut() {
            slot.state = SlotState::Free;
            slot.expires_at = 0;
            slot.lease = Weak::new();
        }
    }
    *inner
        .instance_id
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = new_instance_id;
    cold.clear();
    wheel.clear(unix_seconds());
    inner.safe_mode.store(false, Ordering::Release);
    Ok(())
}

fn actor_rotate_root(
    inner: &Arc<AuthStateInner>,
    config: &AuthConfig,
    cold: &mut HashMap<u64, ColdMetadata>,
    wheel: &mut TimingWheel,
    admin_lease: &mut Arc<AuthLease>,
    new_key: AesKeyType,
) -> Result<(), AuthFailure> {
    if new_key == inner.admin_key() {
        return Err(AuthFailure::new(
            "administrator_key_unchanged",
            "new administrator key must differ from the current key",
            false,
        ));
    }
    let new_key_string = String::from_utf8(new_key.to_vec()).map_err(|_| {
        AuthFailure::new(
            "administrator_key_invalid",
            "administrator key must be 32 UTF-8 bytes for MSG_HEADER_KEY compatibility",
            false,
        )
    })?;
    if new_key_string.chars().any(char::is_whitespace) {
        return Err(AuthFailure::new(
            "administrator_key_invalid",
            "administrator key must not contain whitespace",
            false,
        ));
    }

    let snapshot = empty_snapshot(inner, inner.instance_id());
    if let Err(error) = write_snapshot_and_truncate_wal(config, &new_key, &snapshot)
        .and_then(|()| {
            append_audit(
                config,
                &new_key,
                audit("administrator_key_rotate", None, None),
            )
        })
        .and_then(|()| write_admin_key(&config.state_dir, &new_key_string))
    {
        inner.safe_mode.store(true, Ordering::Release);
        cancel_all_temporary_leases(inner);
        return Err(error);
    }

    cancel_all_temporary_leases(inner);
    let old_admin_lease = admin_lease.clone();
    {
        let mut slots = inner
            .slots
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for slot in slots.iter_mut() {
            slot.state = SlotState::Free;
            slot.expires_at = 0;
            slot.lease = Weak::new();
        }
    }
    cold.clear();
    wheel.clear(unix_seconds());
    let new_admin_lease = Arc::new(AuthLease::new(0, u64::MAX));
    *inner
        .admin_key
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = new_key;
    *inner
        .admin_lease
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Arc::downgrade(&new_admin_lease);
    set_process_msg_header_key(Some(&new_key_string)).map_err(AuthFailure::internal)?;
    inner.safe_mode.store(false, Ordering::Release);
    old_admin_lease.cancellation.cancel();
    *admin_lease = new_admin_lease;
    Ok(())
}

fn actor_set_legacy_protocol(
    inner: &Arc<AuthStateInner>,
    config: &AuthConfig,
    policy: LegacyProtocolPolicy,
) -> Result<(), AuthFailure> {
    ensure_store_available(inner)?;
    append_mutation(
        config,
        &inner.admin_key(),
        StateMutation::LegacyProtocol(policy),
        audit("legacy_protocol_update", None, Some(format!("{policy:?}"))),
    )?;
    inner
        .legacy_protocol_allowed
        .store(policy.is_allowed(), Ordering::Release);
    Ok(())
}

fn actor_status(inner: &Arc<AuthStateInner>) -> AuthStatus {
    let slots = inner
        .slots
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let active_keys = slots
        .iter()
        .filter(|slot| slot.state == SlotState::Active)
        .count();
    let expired_keys = slots
        .iter()
        .filter(|slot| slot.state == SlotState::Expired)
        .count();
    let revoked_keys = slots
        .iter()
        .filter(|slot| slot.state == SlotState::Revoked)
        .count();
    let last_legacy_connection_at = inner.last_legacy_connection_at.load(Ordering::Acquire);
    AuthStatus {
        schema_version: 1,
        safe_mode: inner.safe_mode.load(Ordering::Acquire),
        capacity: slots.len(),
        active_keys,
        expired_keys,
        revoked_keys,
        legacy_protocol: if inner.legacy_protocol_allowed.load(Ordering::Acquire) {
            LegacyProtocolPolicy::Allow
        } else {
            LegacyProtocolPolicy::Deny
        },
        active_legacy_connections: inner.active_legacy_connections.load(Ordering::Acquire),
        last_legacy_connection_at: (last_legacy_connection_at != 0)
            .then_some(last_legacy_connection_at),
        auth_successes: inner.auth_successes.load(Ordering::Relaxed),
        auth_failures: inner.auth_failures.load(Ordering::Relaxed),
        server_instance_id: hex(&inner.instance_id()),
    }
}

fn ensure_store_available(inner: &AuthStateInner) -> Result<(), AuthFailure> {
    if inner.safe_mode.load(Ordering::Acquire) {
        Err(AuthFailure::new(
            "temporary_key_store_unavailable",
            "temporary key store is in administrator safe mode",
            false,
        ))
    } else {
        Ok(())
    }
}

fn validate_slot_identity(slot: &SlotHot, key_id: u64) -> Result<(), AuthFailure> {
    if slot.generation != key_generation(key_id) || slot.state == SlotState::Free {
        Err(key_not_found(key_id))
    } else {
        Ok(())
    }
}

fn key_not_found(key_id: u64) -> AuthFailure {
    AuthFailure::new(
        "temporary_key_not_found",
        format!("temporary key {key_id} does not exist"),
        false,
    )
}

fn metadata_with_credential(
    inner: &Arc<AuthStateInner>,
    cold: &HashMap<u64, ColdMetadata>,
    key_id: u64,
    reveal: bool,
) -> Result<IssuedTemporaryKey, AuthFailure> {
    let slots = inner
        .slots
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let slot = slots
        .get(key_slot(key_id) as usize)
        .ok_or_else(|| key_not_found(key_id))?;
    validate_slot_identity(slot, key_id)?;
    let cold = cold.get(&key_id).ok_or_else(|| key_not_found(key_id))?;
    let credential = if reveal {
        let key = derive_temporary_key(&inner.admin_key(), &inner.instance_id(), key_id)?;
        encode_temporary_credential(key_id, &key)
    } else {
        String::new()
    };
    Ok(IssuedTemporaryKey {
        metadata: TemporaryKeyMetadata {
            key_id,
            state: slot_state_name(slot.state).to_string(),
            issued_at: cold.issued_at,
            expires_at: slot.expires_at,
            label: cold.label.clone(),
        },
        credential,
    })
}

fn slot_state_name(state: SlotState) -> &'static str {
    match state {
        SlotState::Free => "free",
        SlotState::Active => "active",
        SlotState::Expired => "expired",
        SlotState::Revoked => "revoked",
    }
}

fn audit(action: &str, key_id: Option<u64>, label: Option<String>) -> AuditRecord {
    AuditRecord {
        at: unix_seconds(),
        action: action.to_string(),
        key_id,
        label,
    }
}

fn cancel_all_temporary_leases(inner: &AuthStateInner) {
    let slots = inner
        .slots
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    for lease in slots.iter().filter_map(|slot| slot.lease.upgrade()) {
        lease.cancellation.cancel();
    }
}

fn build_snapshot(inner: &AuthStateInner, cold: &HashMap<u64, ColdMetadata>) -> PersistedSnapshot {
    let slots = inner
        .slots
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let generations = slots.iter().map(|slot| slot.generation).collect();
    let entries = slots
        .iter()
        .enumerate()
        .filter_map(|(index, slot)| {
            if slot.state == SlotState::Free {
                return None;
            }
            let key_id = make_key_id(slot.generation, index as u32);
            let cold = cold.get(&key_id)?;
            Some(PersistedEntry {
                key_id,
                state: slot.state,
                issued_at: cold.issued_at,
                expires_at: slot.expires_at,
                label: cold.label.clone(),
            })
        })
        .collect();
    PersistedSnapshot {
        schema_version: SNAPSHOT_SCHEMA_VERSION,
        instance_id: inner.instance_id(),
        generations,
        entries,
        legacy_protocol: if inner.legacy_protocol_allowed.load(Ordering::Acquire) {
            LegacyProtocolPolicy::Allow
        } else {
            LegacyProtocolPolicy::Deny
        },
    }
}

fn empty_snapshot(inner: &AuthStateInner, instance_id: [u8; INSTANCE_ID_LEN]) -> PersistedSnapshot {
    let slots = inner
        .slots
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    PersistedSnapshot {
        schema_version: SNAPSHOT_SCHEMA_VERSION,
        instance_id,
        generations: slots.iter().map(|slot| slot.generation).collect(),
        entries: Vec::new(),
        legacy_protocol: if inner.legacy_protocol_allowed.load(Ordering::Acquire) {
            LegacyProtocolPolicy::Allow
        } else {
            LegacyProtocolPolicy::Deny
        },
    }
}

fn load_persisted_state(
    config: &AuthConfig,
    admin_key: &AesKeyType,
    instance_id: [u8; INSTANCE_ID_LEN],
) -> (Option<PersistedSnapshot>, bool) {
    match try_load_persisted_state(config, admin_key, instance_id) {
        Ok(state) => (Some(state), false),
        Err(error) => {
            tracing::error!(
                event = "auth_state_safe_mode",
                auth_stage = "state_load",
                reason = %error.code,
                error = %error,
                "temporary key store failed closed in administrator safe mode"
            );
            (None, true)
        }
    }
}

fn try_load_persisted_state(
    config: &AuthConfig,
    admin_key: &AesKeyType,
    instance_id: [u8; INSTANCE_ID_LEN],
) -> Result<PersistedSnapshot, AuthFailure> {
    let snapshot_path = config.state_dir.join("auth.snapshot");
    let mut snapshot = if snapshot_path.exists() {
        let bytes = std::fs::read(&snapshot_path).map_err(|error| {
            AuthFailure::new(
                "temporary_key_store_unavailable",
                format!("failed to read `{}`: {error}", snapshot_path.display()),
                false,
            )
        })?;
        let plain = open_blob(admin_key, &bytes)?;
        serde_json::from_slice::<PersistedSnapshot>(&plain).map_err(|error| {
            AuthFailure::new(
                "temporary_key_store_unavailable",
                format!("failed to decode auth snapshot: {error}"),
                false,
            )
        })?
    } else {
        PersistedSnapshot {
            schema_version: SNAPSHOT_SCHEMA_VERSION,
            instance_id,
            generations: vec![0; config.max_temporary_keys],
            entries: Vec::new(),
            legacy_protocol: config.legacy_protocol,
        }
    };
    if snapshot.schema_version != SNAPSHOT_SCHEMA_VERSION || snapshot.instance_id != instance_id {
        return Err(AuthFailure::new(
            "temporary_key_store_unavailable",
            "auth snapshot schema or server instance id does not match",
            false,
        ));
    }
    snapshot.generations.resize(config.max_temporary_keys, 0);
    snapshot.generations.truncate(config.max_temporary_keys);

    let wal_path = config.state_dir.join("auth.wal");
    if wal_path.exists() {
        for record in read_wal(&wal_path, admin_key)? {
            if let WalRecord::Mutation { mutation, .. } = record {
                apply_persisted_mutation(&mut snapshot, mutation, config.max_temporary_keys)?;
            }
        }
    }
    Ok(snapshot)
}

fn apply_persisted_mutation(
    snapshot: &mut PersistedSnapshot,
    mutation: StateMutation,
    capacity: usize,
) -> Result<(), AuthFailure> {
    match mutation {
        StateMutation::Issue(entry) => {
            let index = key_slot(entry.key_id) as usize;
            if index >= capacity {
                return Err(AuthFailure::new(
                    "temporary_key_store_unavailable",
                    "WAL issue record references a slot outside the configured capacity",
                    false,
                ));
            }
            snapshot.generations[index] = key_generation(entry.key_id);
            snapshot
                .entries
                .retain(|current| key_slot(current.key_id) as usize != index);
            snapshot.entries.push(entry);
        }
        StateMutation::Renew { key_id, expires_at } => {
            let entry = snapshot
                .entries
                .iter_mut()
                .find(|entry| entry.key_id == key_id)
                .ok_or_else(|| {
                    AuthFailure::new(
                        "temporary_key_store_unavailable",
                        "WAL renew record references an unknown key",
                        false,
                    )
                })?;
            entry.expires_at = expires_at;
            entry.state = SlotState::Active;
        }
        StateMutation::Revoke { key_id, .. } => {
            let entry = snapshot
                .entries
                .iter_mut()
                .find(|entry| entry.key_id == key_id)
                .ok_or_else(|| {
                    AuthFailure::new(
                        "temporary_key_store_unavailable",
                        "WAL revoke record references an unknown key",
                        false,
                    )
                })?;
            entry.state = SlotState::Revoked;
        }
        StateMutation::LegacyProtocol(policy) => snapshot.legacy_protocol = policy,
    }
    Ok(())
}

fn append_mutation(
    config: &AuthConfig,
    admin_key: &AesKeyType,
    mutation: StateMutation,
    audit: AuditRecord,
) -> Result<(), AuthFailure> {
    append_wal(config, admin_key, &WalRecord::Mutation { mutation, audit })
}

fn append_audit(
    config: &AuthConfig,
    admin_key: &AesKeyType,
    audit: AuditRecord,
) -> Result<(), AuthFailure> {
    append_wal(config, admin_key, &WalRecord::Audit(audit))
}

fn append_wal(
    config: &AuthConfig,
    admin_key: &AesKeyType,
    record: &WalRecord,
) -> Result<(), AuthFailure> {
    let plain = serde_json::to_vec(record).map_err(|error| {
        AuthFailure::internal(format!("failed to encode auth WAL record: {error}"))
    })?;
    let sealed = seal_blob(admin_key, &plain)?;
    let length = u32::try_from(sealed.len())
        .map_err(|_| AuthFailure::internal("auth WAL record is too large"))?;
    let path = config.state_dir.join("auth.wal");
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|error| {
            AuthFailure::new(
                "temporary_key_store_unavailable",
                format!("failed to open `{}`: {error}", path.display()),
                true,
            )
        })?;
    #[cfg(unix)]
    file.set_permissions(std::fs::Permissions::from_mode(0o600))
        .map_err(|error| {
            AuthFailure::new(
                "temporary_key_store_unavailable",
                format!("failed to secure `{}`: {error}", path.display()),
                false,
            )
        })?;
    file.write_all(&length.to_be_bytes())
        .and_then(|()| file.write_all(&sealed))
        .and_then(|()| file.sync_data())
        .map_err(|error| {
            AuthFailure::new(
                "temporary_key_store_unavailable",
                format!("failed to durably append `{}`: {error}", path.display()),
                true,
            )
        })
}

fn read_wal(path: &Path, admin_key: &AesKeyType) -> Result<Vec<WalRecord>, AuthFailure> {
    let mut file = File::open(path).map_err(|error| {
        AuthFailure::new(
            "temporary_key_store_unavailable",
            format!("failed to open `{}`: {error}", path.display()),
            false,
        )
    })?;
    let mut records = Vec::new();
    loop {
        let mut length = [0_u8; 4];
        match file.read(&mut length[..1]) {
            Ok(0) => break,
            Ok(1) => {}
            Ok(_) => unreachable!("single-byte WAL prefix read"),
            Err(error) => {
                return Err(AuthFailure::new(
                    "temporary_key_store_unavailable",
                    format!("failed to read auth WAL length: {error}"),
                    false,
                ));
            }
        }
        file.read_exact(&mut length[1..]).map_err(|error| {
            AuthFailure::new(
                "temporary_key_store_unavailable",
                format!("truncated auth WAL length: {error}"),
                false,
            )
        })?;
        let length = u32::from_be_bytes(length) as usize;
        if length > 1024 * 1024 {
            return Err(AuthFailure::new(
                "temporary_key_store_unavailable",
                "auth WAL record exceeds 1 MiB",
                false,
            ));
        }
        let mut sealed = vec![0_u8; length];
        file.read_exact(&mut sealed).map_err(|error| {
            AuthFailure::new(
                "temporary_key_store_unavailable",
                format!("truncated auth WAL record: {error}"),
                false,
            )
        })?;
        let plain = open_blob(admin_key, &sealed)?;
        records.push(serde_json::from_slice(&plain).map_err(|error| {
            AuthFailure::new(
                "temporary_key_store_unavailable",
                format!("failed to decode auth WAL record: {error}"),
                false,
            )
        })?);
    }
    Ok(records)
}

fn write_snapshot_and_truncate_wal(
    config: &AuthConfig,
    admin_key: &AesKeyType,
    snapshot: &PersistedSnapshot,
) -> Result<(), AuthFailure> {
    let plain = serde_json::to_vec(snapshot).map_err(|error| {
        AuthFailure::internal(format!("failed to encode auth snapshot: {error}"))
    })?;
    let sealed = seal_blob(admin_key, &plain)?;
    let snapshot_path = config.state_dir.join("auth.snapshot");
    atomic_write(&snapshot_path, &sealed, 0o600)?;
    let wal_path = config.state_dir.join("auth.wal");
    let wal = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&wal_path)
        .map_err(|error| {
            AuthFailure::new(
                "temporary_key_store_unavailable",
                format!("failed to truncate `{}`: {error}", wal_path.display()),
                true,
            )
        })?;
    wal.sync_all().map_err(|error| {
        AuthFailure::new(
            "temporary_key_store_unavailable",
            format!("failed to sync `{}`: {error}", wal_path.display()),
            true,
        )
    })
}

fn seal_blob(admin_key: &AesKeyType, plain: &[u8]) -> Result<Vec<u8>, AuthFailure> {
    let key = LessSafeKey::new(
        UnboundKey::new(&AES_256_GCM, admin_key)
            .map_err(|_| AuthFailure::internal("failed to initialize state encryption key"))?,
    );
    let mut nonce_bytes = [0_u8; 12];
    let mut rng = rand::rng();
    for byte in &mut nonce_bytes {
        *byte = rng.random();
    }
    let mut output = plain.to_vec();
    key.seal_in_place_append_tag(
        Nonce::assume_unique_for_key(nonce_bytes),
        Aad::from(STATE_AAD),
        &mut output,
    )
    .map_err(|_| AuthFailure::internal("failed to encrypt authentication state"))?;
    let mut sealed = Vec::with_capacity(STATE_BLOB_MAGIC.len() + nonce_bytes.len() + output.len());
    sealed.extend_from_slice(STATE_BLOB_MAGIC);
    sealed.extend_from_slice(&nonce_bytes);
    sealed.extend_from_slice(&output);
    Ok(sealed)
}

fn open_blob(admin_key: &AesKeyType, sealed: &[u8]) -> Result<Vec<u8>, AuthFailure> {
    if sealed.len() < STATE_BLOB_MAGIC.len() + 12 + AES_256_GCM.tag_len()
        || &sealed[..STATE_BLOB_MAGIC.len()] != STATE_BLOB_MAGIC
    {
        return Err(AuthFailure::new(
            "temporary_key_store_unavailable",
            "authentication state blob has an invalid header",
            false,
        ));
    }
    let nonce_start = STATE_BLOB_MAGIC.len();
    let nonce_end = nonce_start + 12;
    let nonce_bytes: [u8; 12] = sealed[nonce_start..nonce_end]
        .try_into()
        .expect("validated nonce width");
    let mut plain = sealed[nonce_end..].to_vec();
    let key = LessSafeKey::new(UnboundKey::new(&AES_256_GCM, admin_key).map_err(|_| {
        AuthFailure::new(
            "temporary_key_store_unavailable",
            "failed to initialize state decryption key",
            false,
        )
    })?);
    let opened = key
        .open_in_place(
            Nonce::assume_unique_for_key(nonce_bytes),
            Aad::from(STATE_AAD),
            &mut plain,
        )
        .map_err(|_| {
            AuthFailure::new(
                "temporary_key_store_unavailable",
                "authentication state integrity check failed",
                false,
            )
        })?;
    let len = opened.len();
    plain.truncate(len);
    Ok(plain)
}

fn prepare_state_dir(path: &Path) -> Result<(), AuthFailure> {
    std::fs::create_dir_all(path).map_err(|error| {
        AuthFailure::new(
            "auth_state_unavailable",
            format!(
                "failed to create auth state directory `{}`: {error}",
                path.display()
            ),
            false,
        )
    })?;
    #[cfg(unix)]
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).map_err(|error| {
        AuthFailure::new(
            "auth_state_unavailable",
            format!(
                "failed to secure auth state directory `{}`: {error}",
                path.display()
            ),
            false,
        )
    })?;
    Ok(())
}

fn load_or_create_instance_id(path: &Path) -> Result<[u8; INSTANCE_ID_LEN], AuthFailure> {
    let instance_path = path.join("server-instance-id");
    if instance_path.exists() {
        let bytes = std::fs::read(&instance_path).map_err(|error| {
            AuthFailure::new(
                "auth_state_unavailable",
                format!("failed to read `{}`: {error}", instance_path.display()),
                false,
            )
        })?;
        return bytes.try_into().map_err(|_| {
            AuthFailure::new(
                "auth_state_unavailable",
                "server instance id must be exactly 16 bytes",
                false,
            )
        });
    }
    let instance_id = random_instance_id();
    atomic_write(&instance_path, &instance_id, 0o600)?;
    Ok(instance_id)
}

fn random_instance_id() -> [u8; INSTANCE_ID_LEN] {
    let mut instance_id = [0_u8; INSTANCE_ID_LEN];
    let mut rng = rand::rng();
    for byte in &mut instance_id {
        *byte = rng.random();
    }
    instance_id
}

fn write_admin_key(state_dir: &Path, key: &str) -> Result<(), AuthFailure> {
    atomic_write(
        &state_dir.join("admin.key"),
        format!("{key}\n").as_bytes(),
        0o600,
    )
}

pub fn generate_admin_key() -> String {
    const CHARSET: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";
    let mut rng = rand::rng();
    (0..32)
        .map(|_| CHARSET[rng.random_range(0..CHARSET.len())] as char)
        .collect()
}

pub fn initialize_admin_key(path: &Path, force: bool) -> Result<String, AuthFailure> {
    if path.exists() && !force {
        return Err(AuthFailure::new(
            "administrator_key_exists",
            format!("administrator key file `{}` already exists", path.display()),
            false,
        ));
    }
    let key = generate_admin_key();
    atomic_write(path, format!("{key}\n").as_bytes(), 0o600)?;
    Ok(key)
}

pub fn write_admin_key_file(path: &Path, key: &str, force: bool) -> Result<(), AuthFailure> {
    let Credential::Admin(_) = parse_credential(key)
        .map_err(|error| AuthFailure::new("administrator_key_invalid", error, false))?
    else {
        return Err(AuthFailure::new(
            "administrator_key_invalid",
            "administrator key file requires a 32-byte administrator key",
            false,
        ));
    };
    if path.exists() && !force {
        return Err(AuthFailure::new(
            "administrator_key_exists",
            format!(
                "administrator key file `{}` already exists; pass --force to replace it",
                path.display()
            ),
            false,
        ));
    }
    atomic_write(path, format!("{key}\n").as_bytes(), 0o600)
}

fn atomic_write(path: &Path, data: &[u8], mode: u32) -> Result<(), AuthFailure> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            AuthFailure::new(
                "auth_state_unavailable",
                format!("failed to create `{}`: {error}", parent.display()),
                false,
            )
        })?;
    }
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("auth-state");
    let mut random_suffix = [0_u8; 8];
    let mut rng = rand::rng();
    for byte in &mut random_suffix {
        *byte = rng.random();
    }
    let temporary = path.with_file_name(format!(
        ".{file_name}.tmp-{}-{}",
        std::process::id(),
        hex(&random_suffix)
    ));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)
        .map_err(|error| {
            AuthFailure::new(
                "auth_state_unavailable",
                format!("failed to open `{}`: {error}", temporary.display()),
                false,
            )
        })?;
    let result = (|| {
        #[cfg(unix)]
        file.set_permissions(std::fs::Permissions::from_mode(mode))
            .map_err(|error| {
                AuthFailure::internal(format!("failed to set key permissions: {error}"))
            })?;
        #[cfg(not(unix))]
        let _ = mode;
        file.write_all(data)
            .and_then(|()| file.sync_all())
            .map_err(|error| {
                AuthFailure::new(
                    "auth_state_unavailable",
                    format!("failed to write `{}`: {error}", temporary.display()),
                    false,
                )
            })?;
        drop(file);
        std::fs::rename(&temporary, path).map_err(|error| {
            AuthFailure::new(
                "auth_state_unavailable",
                format!("failed to replace `{}`: {error}", path.display()),
                false,
            )
        })?;
        #[cfg(unix)]
        if let Some(parent) = path.parent() {
            File::open(parent)
                .and_then(|directory| directory.sync_all())
                .map_err(|error| {
                    AuthFailure::new(
                        "auth_state_unavailable",
                        format!("failed to sync `{}`: {error}", parent.display()),
                        false,
                    )
                })?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(DIGITS[(byte >> 4) as usize] as char);
        output.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    output
}

struct WheelEntry {
    lease: Arc<AuthLease>,
    version: u64,
}

struct TimingWheel {
    now: u64,
    level0: Vec<Vec<WheelEntry>>,
    level1: Vec<Vec<WheelEntry>>,
    level2: Vec<Vec<WheelEntry>>,
    level3: Vec<Vec<WheelEntry>>,
}

impl TimingWheel {
    fn new(now: u64) -> Self {
        Self {
            now,
            level0: empty_buckets(256),
            level1: empty_buckets(64),
            level2: empty_buckets(64),
            level3: empty_buckets(64),
        }
    }

    fn insert(&mut self, lease: Arc<AuthLease>) {
        let version = lease.wheel_version.load(Ordering::Acquire);
        self.insert_with_version(lease, version);
    }

    fn insert_with_version(&mut self, lease: Arc<AuthLease>, version: u64) {
        let expires_at = lease.expires_at();
        let delta = expires_at.saturating_sub(self.now);
        let entry = WheelEntry { lease, version };
        if delta < 1 << 8 {
            self.level0[(expires_at & 0xff) as usize].push(entry);
        } else if delta < 1 << 14 {
            self.level1[((expires_at >> 8) & 0x3f) as usize].push(entry);
        } else if delta < 1 << 20 {
            self.level2[((expires_at >> 14) & 0x3f) as usize].push(entry);
        } else {
            self.level3[((expires_at >> 20) & 0x3f) as usize].push(entry);
        }
    }

    fn advance(&mut self, target: u64) -> Vec<Arc<AuthLease>> {
        let mut due = Vec::new();
        while self.now < target {
            self.now = self.now.saturating_add(1);
            if self.now & 0xff == 0 {
                self.cascade(1);
                if (self.now >> 8) & 0x3f == 0 {
                    self.cascade(2);
                    if (self.now >> 14) & 0x3f == 0 {
                        self.cascade(3);
                    }
                }
            }
            let index = (self.now & 0xff) as usize;
            for entry in std::mem::take(&mut self.level0[index]) {
                if entry.version == entry.lease.wheel_version.load(Ordering::Acquire) {
                    if entry.lease.expires_at() <= self.now {
                        due.push(entry.lease);
                    } else {
                        self.insert(entry.lease);
                    }
                }
            }
        }
        due
    }

    fn cascade(&mut self, level: u8) {
        let entries = match level {
            1 => {
                let index = ((self.now >> 8) & 0x3f) as usize;
                std::mem::take(&mut self.level1[index])
            }
            2 => {
                let index = ((self.now >> 14) & 0x3f) as usize;
                std::mem::take(&mut self.level2[index])
            }
            3 => {
                let index = ((self.now >> 20) & 0x3f) as usize;
                std::mem::take(&mut self.level3[index])
            }
            _ => Vec::new(),
        };
        for entry in entries {
            if entry.version == entry.lease.wheel_version.load(Ordering::Acquire) {
                self.insert_with_version(entry.lease, entry.version);
            }
        }
    }

    fn clear(&mut self, now: u64) {
        *self = Self::new(now);
    }
}

fn empty_buckets(count: usize) -> Vec<Vec<WheelEntry>> {
    std::iter::repeat_with(Vec::new).take(count).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_state_dir(name: &str) -> PathBuf {
        let mut suffix = [0_u8; 8];
        let mut rng = rand::rng();
        for byte in &mut suffix {
            *byte = rng.random();
        }
        std::env::temp_dir().join(format!("pb-mapper-{name}-{}", hex(&suffix)))
    }

    #[test]
    fn key_id_round_trip() {
        let key_id = make_key_id(42, 65_535);
        assert_eq!(key_generation(key_id), 42);
        assert_eq!(key_slot(key_id), 65_535);
    }

    #[test]
    fn derived_key_is_bound_to_instance_and_key_id() {
        let admin = *b"0123456789abcdefghijklmnopqrstuv";
        let instance_a = [1_u8; INSTANCE_ID_LEN];
        let instance_b = [2_u8; INSTANCE_ID_LEN];
        let key = derive_temporary_key(&admin, &instance_a, make_key_id(1, 7)).unwrap();
        assert_eq!(
            key,
            derive_temporary_key(&admin, &instance_a, make_key_id(1, 7)).unwrap()
        );
        assert_ne!(
            key,
            derive_temporary_key(&admin, &instance_b, make_key_id(1, 7)).unwrap()
        );
        assert_ne!(
            key,
            derive_temporary_key(&admin, &instance_a, make_key_id(2, 7)).unwrap()
        );
    }

    #[tokio::test]
    async fn issue_renew_revoke_and_persist() {
        let state_dir = temp_state_dir("auth-lifecycle");
        let admin = *b"0123456789abcdefghijklmnopqrstuv";
        let config = AuthConfig {
            state_dir: state_dir.clone(),
            max_temporary_keys: 8,
            max_temporary_key_ttl: Duration::from_secs(3600),
            legacy_protocol: LegacyProtocolPolicy::Allow,
        };
        let runtime = AuthRuntime::start(admin, config.clone()).await.unwrap();
        let issued = runtime
            .issue(Duration::from_secs(60), Some("demo".to_string()))
            .await
            .unwrap();
        assert!(issued.credential.starts_with("pbmt1_"));
        let context = runtime.authenticate(issued.metadata.key_id).unwrap();
        assert!(!context.is_admin);
        let cancellation = context.cancellation_token().unwrap();
        let renewed = runtime
            .renew(issued.metadata.key_id, Duration::from_secs(120))
            .await
            .unwrap();
        assert_eq!(renewed.metadata.key_id, issued.metadata.key_id);
        assert_eq!(renewed.credential, issued.credential);
        assert!(renewed.metadata.expires_at > issued.metadata.expires_at);
        runtime.revoke(issued.metadata.key_id).await.unwrap();
        assert!(cancellation.is_cancelled());
        assert_eq!(
            context.ensure_active().unwrap_err().code,
            "temporary_key_revoked"
        );
        assert_eq!(
            runtime
                .authenticate(issued.metadata.key_id)
                .unwrap_err()
                .code,
            "temporary_key_revoked"
        );
        drop(runtime);

        tokio::time::sleep(Duration::from_millis(20)).await;
        let restored = AuthRuntime::start(admin, config).await.unwrap();
        assert_eq!(
            restored
                .authenticate(issued.metadata.key_id)
                .unwrap_err()
                .code,
            "temporary_key_revoked"
        );
        let _ = std::fs::remove_dir_all(state_dir);
    }

    #[tokio::test]
    async fn reset_rotates_instance_and_prevents_old_key_id_reuse() {
        let state_dir = temp_state_dir("auth-reset");
        let admin = *b"0123456789abcdefghijklmnopqrstuv";
        let config = AuthConfig {
            state_dir: state_dir.clone(),
            max_temporary_keys: 1,
            max_temporary_key_ttl: Duration::from_secs(3600),
            legacy_protocol: LegacyProtocolPolicy::Allow,
        };
        let runtime = AuthRuntime::start(admin, config).await.unwrap();
        let before = runtime.status().await.unwrap().server_instance_id;
        let old = runtime
            .issue(Duration::from_secs(60), Some("before-reset".to_string()))
            .await
            .unwrap();
        let old_context = runtime.authenticate(old.metadata.key_id).unwrap();
        let old_cancellation = old_context.cancellation_token().unwrap();

        runtime.reset().await.unwrap();

        let after = runtime.status().await.unwrap().server_instance_id;
        assert_ne!(after, before);
        assert!(old_cancellation.is_cancelled());
        assert!(runtime.authenticate(old.metadata.key_id).is_err());
        let replacement = runtime
            .issue(Duration::from_secs(60), Some("after-reset".to_string()))
            .await
            .unwrap();
        assert_ne!(replacement.metadata.key_id, old.metadata.key_id);
        assert_ne!(replacement.credential, old.credential);

        drop(runtime);
        tokio::time::sleep(Duration::from_millis(20)).await;
        let _ = std::fs::remove_dir_all(state_dir);
    }

    #[tokio::test]
    async fn corrupt_wal_fails_temporary_keys_closed_until_admin_reset() {
        let state_dir = temp_state_dir("auth-safe-mode");
        let admin = *b"0123456789abcdefghijklmnopqrstuv";
        let config = AuthConfig {
            state_dir: state_dir.clone(),
            max_temporary_keys: 4,
            max_temporary_key_ttl: Duration::from_secs(3600),
            legacy_protocol: LegacyProtocolPolicy::Allow,
        };
        let runtime = AuthRuntime::start(admin, config.clone()).await.unwrap();
        let issued = runtime
            .issue(Duration::from_secs(60), Some("corrupt-me".to_string()))
            .await
            .unwrap();
        drop(runtime);
        tokio::time::sleep(Duration::from_millis(20)).await;
        std::fs::write(state_dir.join("auth.wal"), b"broken-wal").unwrap();

        let recovered = AuthRuntime::start(admin, config).await.unwrap();
        assert!(recovered.status().await.unwrap().safe_mode);
        assert_eq!(
            recovered
                .authenticate(issued.metadata.key_id)
                .unwrap_err()
                .code,
            "temporary_key_store_unavailable"
        );
        recovered.reset().await.unwrap();
        assert!(!recovered.status().await.unwrap().safe_mode);

        drop(recovered);
        tokio::time::sleep(Duration::from_millis(20)).await;
        let _ = std::fs::remove_dir_all(state_dir);
    }

    #[test]
    fn timing_wheel_ignores_stale_renewal_entry() {
        let now = 1_000;
        let lease = Arc::new(AuthLease::new(make_key_id(1, 0), now + 5));
        let mut wheel = TimingWheel::new(now);
        wheel.insert(lease.clone());
        lease.expires_at.store(now + 20, Ordering::Release);
        lease.wheel_version.fetch_add(1, Ordering::AcqRel);
        wheel.insert(lease.clone());
        assert!(wheel.advance(now + 6).is_empty());
        assert_eq!(wheel.advance(now + 20).len(), 1);
    }
}
