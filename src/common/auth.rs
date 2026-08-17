//! Authentication state for protocol-v2 connections and administrator operations.
//!
//! The root administrator key is never copied into a temporary credential. Temporary
//! keys are derived from `(root key, server instance id, key id)` and the hot slot table
//! stores only lifecycle metadata plus a weak lease reference. The background actor owns
//! the strong leases through a hierarchical timing wheel.

use std::collections::{HashMap, HashSet, VecDeque};
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
use subtle::ConstantTimeEq;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use super::checksum::{
    encode_temporary_credential, get_process_credential, parse_credential,
    set_process_msg_header_key, AesKeyType, Credential, ENV_MSG_HEADER_KEY,
    MACHINE_MSG_HEADER_KEY_PATH,
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
const ADMIN_REPLAY_RETENTION: Duration = Duration::from_secs(10 * 60);
const ADMIN_REPLAY_CAPACITY: usize = 65_536;
const AUDIT_RECORD_CAPACITY: usize = 4096;

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

    fn admin_authority(&self) -> Result<Weak<AuthLease>, AuthFailure> {
        if !self.is_admin {
            return Err(AuthFailure::new(
                "admin_permission_required",
                "administrator credential is required for this operation",
                false,
            ));
        }
        self.ensure_active()?;
        Ok(self.lease.clone())
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
struct AdminState {
    key: AesKeyType,
    lease: Weak<AuthLease>,
}

#[derive(Debug)]
struct AuthStateInner {
    admin: RwLock<AdminState>,
    instance_id: RwLock<[u8; INSTANCE_ID_LEN]>,
    slots: RwLock<Box<[SlotHot]>>,
    safe_mode: AtomicBool,
    legacy_protocol_allowed: AtomicBool,
    active_legacy_connections: AtomicU64,
    last_legacy_connection_at: AtomicU64,
    auth_successes: AtomicU64,
    auth_failures: AtomicU64,
    audit_records: RwLock<VecDeque<AuditRecord>>,
}

impl AuthStateInner {
    fn admin_key(&self) -> AesKeyType {
        self.admin
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .key
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
    ClaimAdminMutation {
        authority: Weak<AuthLease>,
        fingerprint: [u8; 32],
        client_timestamp: u64,
        response: oneshot::Sender<Result<(), AuthFailure>>,
    },
    Issue {
        authority: Weak<AuthLease>,
        ttl: Duration,
        label: Option<String>,
        response: oneshot::Sender<Result<IssuedTemporaryKey, AuthFailure>>,
    },
    List {
        authority: Weak<AuthLease>,
        page: u32,
        page_size: u16,
        response: oneshot::Sender<Result<KeyPage, AuthFailure>>,
    },
    Show {
        authority: Weak<AuthLease>,
        key_id: u64,
        reveal: bool,
        response: oneshot::Sender<Result<IssuedTemporaryKey, AuthFailure>>,
    },
    Renew {
        authority: Weak<AuthLease>,
        key_id: u64,
        ttl: Duration,
        response: oneshot::Sender<Result<IssuedTemporaryKey, AuthFailure>>,
    },
    Revoke {
        authority: Weak<AuthLease>,
        key_id: u64,
        response: oneshot::Sender<Result<TemporaryKeyMetadata, AuthFailure>>,
    },
    Gc {
        authority: Weak<AuthLease>,
        response: oneshot::Sender<Result<u64, AuthFailure>>,
    },
    Reset {
        authority: Weak<AuthLease>,
        response: oneshot::Sender<Result<(), AuthFailure>>,
    },
    RotateRoot {
        authority: Weak<AuthLease>,
        new_key: AesKeyType,
        response: oneshot::Sender<Result<(), AuthFailure>>,
    },
    SetLegacyProtocol {
        authority: Weak<AuthLease>,
        policy: LegacyProtocolPolicy,
        response: oneshot::Sender<Result<(), AuthFailure>>,
    },
    Status {
        authority: Weak<AuthLease>,
        response: oneshot::Sender<Result<AuthStatus, AuthFailure>>,
    },
    Audit {
        authority: Weak<AuthLease>,
        action: String,
        key_id: Option<u64>,
        detail: Option<String>,
        response: oneshot::Sender<Result<(), AuthFailure>>,
    },
}

mod runtime;

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
    } else if std::env::var_os(ENV_MSG_HEADER_KEY).is_some() {
        let credential = get_process_credential()
            .map_err(|error| AuthFailure::new("administrator_key_invalid", error, false))?;
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
    #[serde(default)]
    admin_replays: Vec<AdminReplayRecord>,
    #[serde(default)]
    audit_records: VecDeque<AuditRecord>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct AdminReplayRecord {
    fingerprint: [u8; 32],
    client_timestamp: u64,
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
    AdminReplay(AdminReplayRecord),
}

mod actor;
use actor::{run_auth_actor, AuthActorState};
mod persistence;
pub use persistence::*;
mod timing_wheel;
use timing_wheel::TimingWheel;
#[cfg(test)]
mod tests;
