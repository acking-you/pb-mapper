//! Authentication state for protocol-v2 connections and administrator operations.
//!
//! # How a temporary credential works
//!
//! Nothing secret is stored per key. A temporary credential is *derived* from the
//! root key, the server instance id, and the key id, so the server can verify a
//! credential it holds no copy of, and a key id is all the state a key needs:
//!
//! ```text
//! issue:  root key + instance id + key id  --HKDF-->  credential handed to the client
//! verify: root key + instance id + key id  --HKDF-->  compare against what was presented
//! ```
//!
//! Because the material is derived, invalidating every key at once is a matter of
//! changing an input: a root rotation replaces the root key, a state reset replaces
//! the instance id. Neither has to touch individual keys.
//!
//! # Where the state lives
//!
//! ```text
//!                    key_id = generation:slot
//!                             |
//!   request ──> derive & compare ──> slots[slot]  ── lifecycle: Free/Active/
//!                                        │            Expired/Revoked, expires_at
//!                                        │
//!                                   Weak lease ──> Arc lease, owned by the actor's
//!                                        ^          timing wheel — the single place
//!                                        │          a lease's lifetime ends
//!   AuthContext (also Weak) ─────────────┘
//! ```
//!
//! The slot table is a preallocated array indexed straight off the key id, so
//! verification costs an array index and churn does not grow memory. The
//! `SlotState` docs below cover the table's layout, why generations exist, and
//! why dead rows linger. `Leases` (`leases.rs`) owns the three structures a
//! key's lifetime spans; `timing_wheel.rs` schedules the expiries.
//!
//! # Where mutations happen
//!
//! ```text
//! AuthRuntime (facade) ──channel──> one actor ──> encrypted snapshot + WAL
//! ```
//!
//! Every mutation is serialized through a single actor, so a request authorized
//! before a root rotation cannot execute against the state that replaced it.
//!
//! The facade and model types stay in this root module; runtime checks, actor
//! mutations, persistence, expiry scheduling, and tests live in the children.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use parking_lot::{Mutex, RwLock};
use rand::RngExt;
use ring::aead::{AES_256_GCM, Aad, LessSafeKey, Nonce, UnboundKey};
use ring::hkdf::{HKDF_SHA256, Salt};
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use super::checksum::{
    AesKeyType, Credential, ENV_MSG_HEADER_KEY, MACHINE_MSG_HEADER_KEY_PATH,
    encode_temporary_credential, env_safe_admin_key_error, get_process_credential,
    is_env_safe_admin_key, parse_credential, set_process_msg_header_key,
};

/// The namespace administrator connections operate in. Tenant namespaces are the
/// key id that owns them, so this mirrors [`ADMIN_KEY_ID`].
pub const ADMIN_NAMESPACE: u64 = ADMIN_KEY_ID.as_u64();
pub const DEFAULT_AUTH_STATE_DIR: &str = "/var/lib/pb-mapper/auth";
pub const DEFAULT_TEMP_KEY_CAPACITY: usize = 65_536;
pub const MAX_TEMP_KEY_CAPACITY: usize = 1_048_576;
pub const DEFAULT_MAX_TEMP_KEY_TTL: Duration = Duration::from_secs(30 * 24 * 60 * 60);
pub const MIN_TEMP_KEY_TTL: Duration = Duration::from_secs(10);
pub const MAX_TEMP_KEY_TTL: Duration = Duration::from_secs(365 * 24 * 60 * 60);
const TOMBSTONE_RETENTION: Duration = Duration::from_secs(60);
/// Longest delay any scheduled cleanup can ask for, so the timing wheel can tell
/// a plausible wait from a clock correction.
const MAX_SCHEDULABLE_DELAY: Duration =
    Duration::from_secs(MAX_TEMP_KEY_TTL.as_secs() + TOMBSTONE_RETENTION.as_secs());
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
    key_id: KeyId,
    expires_at: AtomicU64,
    cancellation: CancellationToken,
    cancel_reason: AtomicU8,
}

impl AuthLease {
    fn new(key_id: KeyId, expires_at: u64) -> Self {
        Self {
            key_id,
            expires_at: AtomicU64::new(expires_at),
            cancellation: CancellationToken::new(),
            cancel_reason: AtomicU8::new(LEASE_CANCEL_NONE),
        }
    }

    pub fn key_id(&self) -> KeyId {
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
    pub key_id: KeyId,
    pub namespace: u64,
    pub is_admin: bool,
    lease: Weak<AuthLease>,
}

impl AuthContext {
    fn from_lease(key_id: KeyId, is_admin: bool, lease: &Arc<AuthLease>) -> Self {
        Self {
            key_id,
            namespace: if is_admin {
                ADMIN_NAMESPACE
            } else {
                key_id.as_u64()
            },
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
        LEASE_CANCEL_EXPIRED => {
            AuthFailure::new("temporary_key_expired", "temporary key has expired", false)
        }
        LEASE_CANCEL_ROTATED => AuthFailure::new(
            "temporary_key_rotated",
            "temporary credential was invalidated by administrator root rotation or auth-state reset",
            false,
        ),
        LEASE_CANCEL_REVOKED => {
            AuthFailure::new("temporary_key_revoked", "temporary key was revoked", false)
        }
        _ => AuthFailure::new(
            "temporary_key_inactive",
            "credential lease has been cancelled",
            false,
        ),
    }
}

/// # The slot table
///
/// A temporary key is never stored. It is *derived* on demand from
/// `(root key, instance id, key id)`, so the server can verify a credential it
/// has no copy of. That makes the key id the whole identity of a key, and a
/// key id is a slot index plus a generation counter:
///
/// ```text
///  key_id: u64
/// ┌───────────────────────────┬───────────────────────────┐
/// │  generation (high 32)     │  slot index (low 32)      │
/// └───────────────────────────┴───────────────────────────┘
///        ^ bumped on reuse            ^ where the row lives
/// ```
///
/// The slot index is a direct offset into `AuthStateInner::slots`, a
/// preallocated `Box<[SlotHot]>`. So verifying a credential is an array index,
/// not a map lookup or a scan, and the table's memory does not grow with churn:
///
/// ```text
/// slots: [ SlotHot; max_temporary_keys ]
///   idx 0  gen 7  Active   expires_at=…  lease─┐
///   idx 1  gen 0  Free                         │  Weak, so the actor's
///   idx 2  gen 3  Expired  (tombstoned)        │  timing wheel is the
///   idx 3  gen 9  Active   expires_at=…  lease─┴─ only strong owner
/// ```
///
/// ## Why the generation counter
///
/// A freed slot is reused, so the index alone would let a *retired* credential
/// authenticate against the *new* tenant of that row. The generation bump makes
/// the old key id refer to a row that no longer exists:
///
/// ```text
/// issue   -> idx 2, gen 3  =>  key_id 0x0000_0003_0000_0002
/// expire  -> idx 2 retired, generation kept at 3
/// reissue -> idx 2, gen 4  =>  key_id 0x0000_0004_0000_0002
///            the old key id still names gen 3, which nothing matches
/// ```
///
/// This is why [`SlotHot::retire`] clears the row but preserves `generation`,
/// and why a generation is never reset — not by expiry, GC, root rotation, or a
/// full state reset.
///
/// ## The lifecycle
///
/// ```text
///          issue                  deadline passes / revoke
///   Free ─────────> Active ──────────────────────────────> Expired
///     ^               │                                    Revoked
///     │               └── renew: same row, later expires_at    │
///     │                                                        │
///     └──────────── retire, after TOMBSTONE_RETENTION ─────────┘
/// ```
///
/// `Expired`/`Revoked` are tombstones, not garbage. A row lingers in that state
/// for `TOMBSTONE_RETENTION` so a client that presents a dead credential is told
/// *why* ("expired", "revoked") instead of receiving the indistinguishable
/// "unknown key" it would get from an already-recycled row. `Leases` owns that
/// delay; see `leases.rs`.
///
/// ## Slots above capacity
///
/// `max_temporary_keys` is configurable, so a restart can shrink the table below
/// what the persisted state used. Those rows cannot be indexed any more, but
/// their generations still have to be honoured — otherwise growing the table
/// again would reissue a key id that was already handed out. They are retained
/// out-of-line in `high_slot_generations` / `high_slot_entries`, which is why so
/// many operations check the array first and fall back to a scan of that vector.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SlotState {
    /// Never used, or retired and past its tombstone.
    Free,
    /// A live credential. `expires_at` is authoritative.
    Active,
    /// Dead. Retained for `TOMBSTONE_RETENTION` so the reason survives.
    Expired,
    Revoked,
}

/// One row of the slot table. `generation` outlives every other field.
#[derive(Debug)]
struct SlotHot {
    generation: Generation,
    state: SlotState,
    expires_at: u64,
    /// `Weak`, because the actor's timing wheel holds the strong reference and is
    /// the single place a lease's lifetime ends. See `timing_wheel.rs`.
    lease: Weak<AuthLease>,
}

impl SlotHot {
    /// Whether this row still belongs to `key_id`'s generation. A row that has
    /// been reissued belongs to a newer tenant and must not be touched on the
    /// old one's behalf.
    fn holds(&self, key_id: KeyId) -> bool {
        self.generation == key_id.generation() && self.state != SlotState::Free
    }

    /// Whether a garbage collection should free this row: it is already dead, or
    /// it is active but past its deadline.
    fn is_collectable(&self, now: u64) -> bool {
        match self.state {
            SlotState::Expired | SlotState::Revoked => true,
            SlotState::Active => self.expires_at <= now,
            SlotState::Free => false,
        }
    }

    /// Frees the slot for reuse while keeping its generation, so a key id that
    /// has been handed out is never issued a second time.
    fn retire(&mut self) {
        *self = Self {
            generation: self.generation,
            ..Self::default()
        };
    }
}

impl Default for SlotHot {
    fn default() -> Self {
        Self {
            generation: Generation::FIRST,
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
    /// Preallocated, indexed directly by `key_slot(key_id)`. Documented on
    /// [`SlotState`].
    slots: RwLock<Box<[SlotHot]>>,
    /// Rows the configured capacity no longer covers, because a restart shrank
    /// the table below what the persisted state used:
    ///
    /// ```text
    ///  slots: [ 0 1 2 3 ]  <- indexable
    ///  high:          [ 4 5 ]  <- generations still honoured, out-of-line
    /// ```
    ///
    /// Their generations must be kept so growing the table again cannot reissue
    /// a key id that was already handed out, and their entries so a still-live
    /// credential in that range keeps working. This is the fallback path that
    /// operations take after missing in `slots`.
    high_slot_generations: RwLock<Vec<Generation>>,
    high_slot_entries: RwLock<Vec<PersistedEntry>>,
    /// Per-key description that no authentication check needs, kept out of the
    /// hot slot row. Lives here rather than inside the actor so a key's handle
    /// can drop it without the actor being involved; see `leases.rs`.
    cold: RwLock<HashMap<KeyId, ColdMetadata>>,
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

impl AuthStateInner {
    /// Rows the configured capacity no longer covers. See the field's docs; the
    /// fallback is a scan because the range is small and rarely touched.
    fn high(&self) -> parking_lot::RwLockReadGuard<'_, Vec<PersistedEntry>> {
        self.high_slot_entries.read()
    }

    fn high_mut(&self) -> parking_lot::RwLockWriteGuard<'_, Vec<PersistedEntry>> {
        self.high_slot_entries.write()
    }

    fn slots(&self) -> parking_lot::RwLockReadGuard<'_, Box<[SlotHot]>> {
        self.slots.read()
    }

    fn slots_mut(&self) -> parking_lot::RwLockWriteGuard<'_, Box<[SlotHot]>> {
        self.slots.write()
    }

    fn cold(&self) -> parking_lot::RwLockReadGuard<'_, HashMap<KeyId, ColdMetadata>> {
        self.cold.read()
    }

    fn cold_mut(&self) -> parking_lot::RwLockWriteGuard<'_, HashMap<KeyId, ColdMetadata>> {
        self.cold.write()
    }

    fn admin_key(&self) -> AesKeyType {
        self.admin.read().key
    }

    fn instance_id(&self) -> [u8; INSTANCE_ID_LEN] {
        *self.instance_id.read()
    }
}

#[derive(Clone)]
pub struct AuthRuntime {
    inner: Weak<AuthStateInner>,
    command_tx: mpsc::Sender<AuthCommand>,
    config: AuthConfig,
    _state_lock: Arc<File>,
    actor: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
    actor_abort: tokio::task::AbortHandle,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TemporaryKeyMetadata {
    pub key_id: KeyId,
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
        key_id: KeyId,
        reveal: bool,
        response: oneshot::Sender<Result<IssuedTemporaryKey, AuthFailure>>,
    },
    Renew {
        authority: Weak<AuthLease>,
        key_id: KeyId,
        ttl: Duration,
        response: oneshot::Sender<Result<IssuedTemporaryKey, AuthFailure>>,
    },
    Revoke {
        authority: Weak<AuthLease>,
        key_id: KeyId,
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
        key_id: Option<KeyId>,
        detail: Option<String>,
        response: oneshot::Sender<Result<(), AuthFailure>>,
    },
    Shutdown {
        response: oneshot::Sender<()>,
    },
}

mod config;
pub use config::default_auth_state_dir;
#[cfg(all(test, not(any(windows, target_os = "macos"))))]
pub(crate) use config::linux_default_auth_state_dir;
#[cfg(test)]
pub(in crate::common::auth) use config::parse_legacy_protocol_policy;
#[cfg(test)]
pub(crate) use config::platform_default_auth_state_dir;
#[cfg(all(test, not(any(windows, target_os = "macos"))))]
pub(in crate::common::auth) use config::{linux_system_auth_dir_usable, unix_effective_uid};
mod keys;
pub use keys::derive_temporary_key;
#[cfg(test)]
pub(in crate::common::auth) use keys::recover_admin_key_after_rotation;
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
    key_id: KeyId,
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
    generations: Vec<Generation>,
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
    Renew { key_id: KeyId, expires_at: u64 },
    Revoke { key_id: KeyId, at: u64 },
    LegacyProtocol(LegacyProtocolPolicy),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct AuditRecord {
    at: u64,
    action: String,
    key_id: Option<KeyId>,
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
use actor::{AuthActorState, run_auth_actor};
mod persistence;
pub use persistence::*;
pub(in crate::common::auth) use persistence::{
    append_audit, append_mutation, append_wal, atomic_write, auth_snapshot_path, build_snapshot,
    cancel_all_temporary_leases, compaction_is_allowed, empty_snapshot,
    fail_closed_on_uncertain_wal, hex, key_matches_existing_state, load_or_create_instance_id,
    load_persisted_state, normalize_tombstone_times, open_blob, prepare_state_dir_and_lock,
    push_audit_record, push_persisted_audit, random_instance_id, recover_instance_id_after_reset,
    reset_already_installed, rotation_already_installed, split_high_slot_state, truncate_auth_wal,
    unix_seconds, write_admin_key, write_snapshot_and_truncate_wal,
};
#[cfg(test)]
pub(in crate::common::auth) use persistence::{
    prepare_state_dir, read_instance_id_file, try_load_persisted_state,
};
mod ids;
pub use ids::{ADMIN_KEY_ID, Generation, KeyId, SlotIndex};
mod leases;
use leases::Leases;
mod timing_wheel;
use timing_wheel::{Timer, TimingWheel};
#[cfg(test)]
mod tests;
