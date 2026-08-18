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

pub fn default_auth_state_dir() -> PathBuf {
    std::env::var_os("PB_MAPPER_AUTH_STATE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(platform_default_auth_state_dir)
}

/// Linux systemd/Docker keep `/var/lib/pb-mapper/auth` when that path is usable
/// (root, or an already-writable service directory). Unprivileged Linux,
/// macOS, and Windows binaries need an application data directory instead.
pub(crate) fn platform_default_auth_state_dir() -> PathBuf {
    #[cfg(windows)]
    {
        let base = std::env::var_os("LOCALAPPDATA")
            .or_else(|| std::env::var_os("APPDATA"))
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"));
        base.join("pb-mapper").join("auth")
    }
    #[cfg(target_os = "macos")]
    {
        match std::env::var_os("HOME") {
            Some(home) => PathBuf::from(home)
                .join("Library")
                .join("Application Support")
                .join("pb-mapper")
                .join("auth"),
            None => PathBuf::from("/Library/Application Support/pb-mapper/auth"),
        }
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        linux_default_auth_state_dir(
            unix_effective_uid(),
            linux_system_auth_dir_usable(),
            std::env::var_os("XDG_DATA_HOME").as_deref(),
            std::env::var_os("HOME").as_deref(),
        )
    }
}

pub(crate) fn linux_default_auth_state_dir(
    euid: u32,
    system_dir_usable: bool,
    xdg_data_home: Option<&std::ffi::OsStr>,
    home: Option<&std::ffi::OsStr>,
) -> PathBuf {
    if euid == 0 || system_dir_usable {
        return PathBuf::from(DEFAULT_AUTH_STATE_DIR);
    }
    if let Some(xdg) = xdg_data_home {
        if !xdg.is_empty() {
            return PathBuf::from(xdg).join("pb-mapper").join("auth");
        }
    }
    if let Some(home) = home {
        if !home.is_empty() {
            return PathBuf::from(home)
                .join(".local")
                .join("share")
                .join("pb-mapper")
                .join("auth");
        }
    }
    PathBuf::from(DEFAULT_AUTH_STATE_DIR)
}

#[cfg(not(any(windows, target_os = "macos")))]
fn unix_effective_uid() -> u32 {
    extern "C" {
        fn geteuid() -> u32;
    }
    unsafe { geteuid() }
}

#[cfg(not(any(windows, target_os = "macos")))]
fn linux_system_auth_dir_usable() -> bool {
    let path = Path::new(DEFAULT_AUTH_STATE_DIR);
    path.is_dir() && unix_path_is_writable(path)
}

#[cfg(not(any(windows, target_os = "macos")))]
fn unix_path_is_writable(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c_path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    extern "C" {
        fn access(pathname: *const std::os::raw::c_char, mode: i32) -> i32;
    }
    const W_OK: i32 = 2;
    unsafe { access(c_path.as_ptr(), W_OK) == 0 }
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            state_dir: default_auth_state_dir(),
            max_temporary_keys: env_usize(
                "PB_MAPPER_AUTH_MAX_TEMP_KEYS",
                DEFAULT_TEMP_KEY_CAPACITY,
                1,
                MAX_TEMP_KEY_CAPACITY,
            ),
            max_temporary_key_ttl: Duration::from_secs(env_u64(
                "PB_MAPPER_AUTH_MAX_TEMP_TTL_SECS",
                DEFAULT_MAX_TEMP_KEY_TTL.as_secs(),
                MIN_TEMP_KEY_TTL.as_secs(),
                MAX_TEMP_KEY_TTL.as_secs(),
            )),
            legacy_protocol: legacy_protocol_from_env(),
        }
    }
}

fn legacy_protocol_from_env() -> LegacyProtocolPolicy {
    match std::env::var("PB_MAPPER_LEGACY_PROTOCOL") {
        Err(std::env::VarError::NotPresent) => LegacyProtocolPolicy::Allow,
        Err(std::env::VarError::NotUnicode(_)) => {
            tracing::error!(
                event = "legacy_protocol_config_invalid",
                "PB_MAPPER_LEGACY_PROTOCOL is not UTF-8; denying legacy framing"
            );
            LegacyProtocolPolicy::Deny
        }
        Ok(value) => parse_legacy_protocol_policy(&value).unwrap_or_else(|| {
            tracing::error!(
                event = "legacy_protocol_config_invalid",
                value,
                "PB_MAPPER_LEGACY_PROTOCOL must be `allow` or `deny`; denying legacy framing"
            );
            LegacyProtocolPolicy::Deny
        }),
    }
}

fn parse_legacy_protocol_policy(value: &str) -> Option<LegacyProtocolPolicy> {
    match value.trim().to_ascii_lowercase().as_str() {
        "allow" => Some(LegacyProtocolPolicy::Allow),
        "deny" => Some(LegacyProtocolPolicy::Deny),
        _ => None,
    }
}

fn env_usize(name: &str, default: usize, min: usize, max: usize) -> usize {
    env_bounded(name, default, min, max)
}

fn env_u64(name: &str, default: u64, min: u64, max: u64) -> u64 {
    env_bounded(name, default, min, max)
}

fn env_bounded<T>(name: &str, default: T, min: T, max: T) -> T
where
    T: std::str::FromStr + PartialOrd + Copy + fmt::Display,
{
    match std::env::var(name) {
        Err(std::env::VarError::NotPresent) => default,
        Ok(raw) => match raw.parse::<T>() {
            Ok(value) if value >= min && value <= max => value,
            _ => {
                tracing::warn!(
                    event = "auth_config_value_invalid",
                    variable = name,
                    value = raw,
                    min = %min,
                    max = %max,
                    fallback = %default,
                    "invalid authentication configuration value; using the default"
                );
                default
            }
        },
        Err(std::env::VarError::NotUnicode(_)) => {
            tracing::warn!(
                event = "auth_config_value_invalid",
                variable = name,
                min = %min,
                max = %max,
                fallback = %default,
                "authentication configuration value is not UTF-8; using the default"
            );
            default
        }
    }
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

    pub(crate) fn admin_cancellation_token(&self) -> Result<CancellationToken, AuthFailure> {
        if !self.is_admin {
            return Err(AuthFailure::new(
                "admin_permission_required",
                "administrator credential is required for this operation",
                false,
            ));
        }
        self.cancellation_token()
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
    _state_lock: Arc<File>,
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

fn read_admin_key(path: &Path) -> Result<Option<String>, AuthFailure> {
    if !path.exists() {
        return Ok(None);
    }
    #[cfg(unix)]
    {
        let metadata = std::fs::metadata(path).map_err(|error| {
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
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(
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
    std::fs::read_to_string(path).map(Some).map_err(|error| {
        AuthFailure::new(
            "administrator_key_required",
            format!(
                "administrator key file `{}` could not be read: {error}",
                path.display()
            ),
            false,
        )
    })
}

fn validate_admin_credential(raw: &str) -> Result<Credential, AuthFailure> {
    let credential = parse_credential(raw.trim())
        .map_err(|error| AuthFailure::new("administrator_key_invalid", error, false))?;
    if !credential.is_admin() {
        return Err(AuthFailure::new(
            "administrator_key_required",
            "the server key file contains a temporary credential",
            false,
        ));
    }
    Ok(credential)
}

fn recover_admin_key_after_rotation(
    state_dir: &Path,
    current: &str,
) -> Result<String, AuthFailure> {
    let snapshot_path = auth_snapshot_path(state_dir);
    if !snapshot_path.exists() {
        return Ok(current.to_string());
    }
    let bytes = std::fs::read(&snapshot_path).map_err(|error| {
        AuthFailure::new(
            "temporary_key_store_unavailable",
            format!("failed to read `{}`: {error}", snapshot_path.display()),
            false,
        )
    })?;
    if let Ok(Credential::Admin(current_key)) = parse_credential(current.trim()) {
        if open_blob(&current_key, &bytes).is_ok() {
            return Ok(current.to_string());
        }
    }
    let Some(next) = read_admin_key(&state_dir.join("admin.key.next"))? else {
        return Ok(current.to_string());
    };
    let Ok(Credential::Admin(next_key)) = parse_credential(next.trim()) else {
        return Ok(current.to_string());
    };
    if open_blob(&next_key, &bytes).is_err() {
        return Ok(current.to_string());
    }
    // The rotation snapshot is complete under the staged key. Leftover WAL
    // records are still encrypted with the previous key.
    truncate_auth_wal(state_dir)?;
    write_admin_key(state_dir, next.trim())?;
    let _ = std::fs::remove_file(state_dir.join("admin.key.next"));
    Ok(next)
}

fn load_server_admin_credential(state_dir: &Path) -> Result<Credential, AuthFailure> {
    let path = state_dir.join("admin.key");
    let raw = if let Some(raw) = read_admin_key(&path)? {
        raw
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
        validate_admin_credential(&key)?;
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
    let raw = recover_admin_key_after_rotation(state_dir, &raw)?;
    let credential = validate_admin_credential(&raw)?;
    set_process_msg_header_key(Some(raw.trim())).map_err(AuthFailure::internal)?;
    Ok(credential)
}

/// Load or create an app-local relay root without reading or mutating the process credential.
///
/// The Flutter process uses its configured process credential for the remote relay, while its
/// optional embedded relay owns an independent administrator key under the app data directory.
fn load_isolated_server_admin_credential(state_dir: &Path) -> Result<Credential, AuthFailure> {
    let path = state_dir.join("admin.key");
    let raw = match read_admin_key(&path)? {
        Some(raw) => raw,
        None => {
            let key = initialize_admin_key(&path, false)?;
            tracing::warn!(
                event = "isolated_administrator_key_initialized",
                path = %path.display(),
                "generated an administrator key for an embedded relay"
            );
            key
        }
    };
    let raw = recover_admin_key_after_rotation(state_dir, &raw)?;
    validate_admin_credential(&raw)
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
