//! Authentication state for protocol-v2 connections and administrator operations.
//!
//! The root administrator key is never copied into a temporary credential. Temporary
//! keys are derived from `(root key, server instance id, key id)` and the hot slot table
//! stores only lifecycle metadata plus a weak lease reference. The background actor owns
//! the strong leases through a hierarchical timing wheel.
//!
//! ```text
//! administrator key + instance id + key id -> derived temporary credential
//!                                      |
//! request -> AuthContext -> Weak lease -+-> actor-owned Arc lease -> timing wheel
//!                                      +-> cancel on expiry/revoke/reset/rotation
//!
//! AuthRuntime facade -> serialized actor -> encrypted snapshot + WAL
//! ```
//!
//! The facade/model types remain in this root module; runtime checks, actor mutations,
//! persistence, expiry scheduling, and focused tests live in their respective children.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
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
    encode_temporary_credential, env_safe_admin_key_error, get_process_credential,
    is_env_safe_admin_key, parse_credential, set_process_msg_header_key, AesKeyType, Credential,
    ENV_MSG_HEADER_KEY, MACHINE_MSG_HEADER_KEY_PATH,
};

pub const ADMIN_NAMESPACE: u64 = 0;
pub const DEFAULT_AUTH_STATE_DIR: &str = "/var/lib/pb-mapper/auth";
pub const DEFAULT_TEMP_KEY_CAPACITY: usize = 65_536;
pub const MAX_TEMP_KEY_CAPACITY: usize = 1_048_576;
pub const DEFAULT_MAX_TEMP_KEY_TTL: Duration = Duration::from_secs(30 * 24 * 60 * 60);
pub const MIN_TEMP_KEY_TTL: Duration = Duration::from_secs(10);
pub const MAX_TEMP_KEY_TTL: Duration = Duration::from_secs(365 * 24 * 60 * 60);
const TOMBSTONE_RETENTION: Duration = Duration::from_secs(60);
const SNAPSHOT_COMPACTION_INTERVAL: Duration = Duration::from_secs(5 * 60);
const SNAPSHOT_SCHEMA_VERSION: u16 = 1;
const STATE_BLOB_MAGIC: &[u8; 5] = b"PBAS1";
const STATE_AAD: &[u8] = b"pb-mapper-auth-state-v1";
const INSTANCE_ID_LEN: usize = 16;
const ADMIN_REPLAY_RETENTION: Duration = Duration::from_secs(10 * 60);
const ADMIN_REPLAY_CAPACITY: usize = 65_536;
const AUDIT_RECORD_CAPACITY: usize = 4096;

#[cfg(test)]
pub(crate) static PROCESS_CREDENTIAL_TEST_LOCK: std::sync::LazyLock<tokio::sync::Mutex<()>> =
    std::sync::LazyLock::new(|| tokio::sync::Mutex::new(()));

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

const LEASE_CANCEL_NONE: u8 = 0;
const LEASE_CANCEL_EXPIRED: u8 = 1;
const LEASE_CANCEL_REVOKED: u8 = 2;
const LEASE_CANCEL_ROTATED: u8 = 3;

#[derive(Debug)]
pub struct AuthLease {
    key_id: u64,
    expires_at: AtomicU64,
    wheel_version: AtomicU64,
    cancellation: CancellationToken,
    cancel_reason: AtomicU8,
}

impl AuthLease {
    fn new(key_id: u64, expires_at: u64) -> Self {
        Self {
            key_id,
            expires_at: AtomicU64::new(expires_at),
            wheel_version: AtomicU64::new(1),
            cancellation: CancellationToken::new(),
            cancel_reason: AtomicU8::new(LEASE_CANCEL_NONE),
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

    fn record_cancel(&self, reason: u8) {
        let _ = self.cancel_reason.compare_exchange(
            LEASE_CANCEL_NONE,
            reason,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
        self.cancellation.cancel();
    }

    pub(crate) fn cancel_expired(&self) {
        self.record_cancel(LEASE_CANCEL_EXPIRED);
    }

    pub(crate) fn cancel_revoked(&self) {
        self.record_cancel(LEASE_CANCEL_REVOKED);
    }

    pub(crate) fn cancel_rotated(&self) {
        self.record_cancel(LEASE_CANCEL_ROTATED);
    }

    #[cfg(test)]
    pub(crate) fn expire_now(&self) {
        self.expires_at.store(0, Ordering::Release);
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
            return Err(cancelled_lease_failure(self.is_admin, &lease));
        }
        if !self.is_admin && lease.expires_at() <= unix_seconds() {
            lease.cancel_expired();
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

    pub(crate) fn admin_cancellation_token(&self) -> Result<CancellationToken, AuthFailure> {
        self.require_admin()?;
        self.cancellation_token()
    }

    fn admin_authority(&self) -> Result<Weak<AuthLease>, AuthFailure> {
        self.require_admin()?;
        self.ensure_active()?;
        Ok(self.lease.clone())
    }

    fn require_admin(&self) -> Result<(), AuthFailure> {
        if self.is_admin {
            Ok(())
        } else {
            Err(AuthFailure::new(
                "admin_permission_required",
                "administrator credential is required for this operation",
                false,
            ))
        }
    }
}

fn cancelled_lease_failure(is_admin: bool, lease: &AuthLease) -> AuthFailure {
    if is_admin {
        return AuthFailure::new(
            "administrator_key_rotated",
            "credential lease has been cancelled",
            false,
        );
    }
    match lease.cancel_reason.load(Ordering::Acquire) {
        LEASE_CANCEL_EXPIRED => AuthFailure::new(
            "temporary_key_expired",
            "temporary key has expired",
            false,
        ),
        LEASE_CANCEL_ROTATED => AuthFailure::new(
            "temporary_key_rotated",
            "temporary credential was invalidated by administrator root rotation or auth-state reset",
            false,
        ),
        LEASE_CANCEL_REVOKED => AuthFailure::new(
            "temporary_key_revoked",
            "temporary key was revoked",
            false,
        ),
        _ => AuthFailure::new(
            "temporary_key_inactive",
            "credential lease has been cancelled",
            false,
        ),
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
    issued_epoch: u64,
    lease: Weak<AuthLease>,
}

impl Default for SlotHot {
    fn default() -> Self {
        Self {
            generation: 0,
            state: SlotState::Free,
            expires_at: 0,
            issued_epoch: 0,
            lease: Weak::new(),
        }
    }
}

#[derive(Debug)]
struct AdminState {
    key: AesKeyType,
    lease: Weak<AuthLease>,
}

#[derive(Clone, Debug)]
struct PreviousRoot {
    admin_key: AesKeyType,
    instance_id: [u8; INSTANCE_ID_LEN],
}

#[derive(Debug)]
struct AuthStateInner {
    admin: RwLock<AdminState>,
    sync_process_credential: bool,
    instance_id: RwLock<[u8; INSTANCE_ID_LEN]>,
    slots: RwLock<Box<[SlotHot]>>,
    /// Generations and entries for slots above the current capacity. Kept so a
    /// later capacity increase cannot reuse a discarded slot's key id.
    high_slot_generations: RwLock<Vec<u32>>,
    high_slot_entries: RwLock<Vec<PersistedEntry>>,
    safe_mode: AtomicBool,
    legacy_protocol_allowed: AtomicBool,
    active_legacy_connections: AtomicU64,
    last_legacy_connection_at: AtomicU64,
    auth_successes: AtomicU64,
    auth_failures: AtomicU64,
    root_epoch: AtomicU64,
    previous_root: RwLock<Option<PreviousRoot>>,
    audit_records: RwLock<VecDeque<AuditRecord>>,
}

fn recover_lock<T>(result: std::sync::LockResult<T>) -> T {
    result.unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl AuthStateInner {
    fn admin_key(&self) -> AesKeyType {
        recover_lock(self.admin.read()).key
    }

    fn instance_id(&self) -> [u8; INSTANCE_ID_LEN] {
        *recover_lock(self.instance_id.read())
    }
}

#[derive(Clone)]
pub struct AuthRuntime {
    inner: Weak<AuthStateInner>,
    command_tx: mpsc::Sender<AuthCommand>,
    config: AuthConfig,
    _state_lock: Arc<File>,
    actor: Arc<std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>>,
    actor_abort: tokio::task::AbortHandle,
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
    tombstoned_at: u64,
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
    Shutdown {
        response: oneshot::Sender<()>,
    },
}

mod config;
pub use config::default_auth_state_dir;
#[cfg(test)]
pub(in crate::common::auth) use config::parse_legacy_protocol_policy;
#[cfg(test)]
pub(crate) use config::{linux_default_auth_state_dir, platform_default_auth_state_dir};
#[cfg(all(test, not(any(windows, target_os = "macos"))))]
pub(in crate::common::auth) use config::{linux_system_auth_dir_usable, unix_effective_uid};
mod keys;
#[cfg(test)]
pub(in crate::common::auth) use keys::recover_admin_key_after_rotation;
pub use keys::{derive_temporary_key, key_generation, key_slot, make_key_id};
pub(in crate::common::auth) use keys::{
    load_isolated_server_admin_credential, load_server_admin_credential,
};
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

#[derive(Clone, Debug, Serialize, Deserialize)]
struct PersistedEntry {
    key_id: u64,
    state: SlotState,
    issued_at: u64,
    expires_at: u64,
    label: Option<String>,
    #[serde(default)]
    tombstoned_at: Option<u64>,
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
    #[serde(default)]
    root_epoch: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct AdminReplayRecord {
    fingerprint: [u8; 32],
    client_timestamp: u64,
    /// Server receipt time used for retention. Older snapshots omit this field
    /// (`0` after serde default) and fall back to `client_timestamp`.
    #[serde(default)]
    accepted_at: u64,
}

impl AdminReplayRecord {
    fn within_retention(&self, now: u64) -> bool {
        let anchor = if self.accepted_at == 0 {
            self.client_timestamp
        } else {
            self.accepted_at
        };
        now.saturating_sub(anchor) <= ADMIN_REPLAY_RETENTION.as_secs()
    }
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
pub(in crate::common::auth) use persistence::{
    append_audit, append_mutation, append_wal, atomic_write, auth_snapshot_path, build_snapshot,
    cancel_all_temporary_leases, clear_retained_high_slot_entries, compaction_is_allowed,
    empty_snapshot, fail_closed_on_uncertain_wal, hex, key_matches_existing_state,
    load_or_create_instance_id, load_persisted_state, normalize_tombstone_times, open_blob,
    prepare_state_dir_and_lock, push_audit_record, push_persisted_audit, random_instance_id,
    recover_instance_id_after_reset, reset_already_installed, rotation_already_installed,
    split_high_slot_state, truncate_auth_wal, unix_seconds, write_admin_key,
    write_snapshot_and_truncate_wal,
};
#[cfg(test)]
pub(in crate::common::auth) use persistence::{
    prepare_state_dir, read_instance_id_file, try_load_persisted_state,
};
mod timing_wheel;
use timing_wheel::TimingWheel;
#[cfg(test)]
mod tests;
