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
//! # Namespaces
//!
//! A namespace decides which services a credential can see. By default a key owns
//! one named by its own id, so revoking it takes the namespace with it. The
//! administrator can instead allocate a namespace that no key owns and issue
//! several keys into it, which lets one tenant hold a separately revocable key
//! per device without moving the others:
//!
//! ```text
//! allocate_namespace() --> 7
//! issue_in_namespace(7) --> key A ─┐  same services; revoking A leaves B working,
//! issue_in_namespace(7) --> key B ─┘  and namespace 7 is never handed out again
//! ```
//!
//! A key's namespace is fixed at issue and read off its lease, so authentication
//! does no extra work for it. `ids.rs` shows why the two kinds cannot collide, and
//! `persistence/namespaces.rs` why the allocator lives outside the encrypted state.
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
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
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

use pb_mapper_core::checksum::{
    AesKeyType, Credential, MACHINE_MSG_HEADER_KEY_PATH, encode_temporary_credential,
    env_safe_admin_key_error, get_process_credential, is_env_safe_admin_key, parse_credential,
    process_credential_configured, set_process_msg_header_key,
};

/// The namespace administrator connections operate in. A key that was not issued
/// into a namespace owns the one its id names, so this mirrors [`ADMIN_KEY_ID`].
pub const ADMIN_NAMESPACE: u64 = ADMIN_KEY_ID.as_u64();
/// Reported in [`AuthStatus::capabilities`] by a relay that can allocate
/// namespaces, issue keys into them, and persist each key's namespace.
pub const CAPABILITY_KEY_NAMESPACES: &str = "key_namespaces";
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

mod actor;
mod clock;
mod config;
mod ids;
mod keys;
mod lease;
mod leases;
mod persistence;
mod runtime;
mod state;
#[cfg(test)]
mod tests;
mod timing_wheel;
mod types;

// The published API. Each name is listed, so nothing becomes public by accident.
pub use config::default_auth_state_dir;
pub use ids::{ADMIN_KEY_ID, Generation, KeyId, MAX_ALLOCATED_NAMESPACE, SlotIndex};
pub use keys::derive_temporary_key;
pub use lease::{AuthContext, AuthLease};
pub use persistence::{
    acquire_state_dir_lock, discard_staged_admin_key, encrypted_auth_state_exists,
    generate_admin_key, initialize_admin_key, stage_admin_key_candidate, staged_admin_key_path,
    write_admin_key_file,
};
pub use runtime::{AuthRuntime, LegacyConnectionGuard};
pub use types::{
    AuthConfig, AuthFailure, AuthStatus, IssuedTemporaryKey, KeyPage, LegacyProtocolPolicy,
    TemporaryKeyMetadata,
};

// What the child modules share through their `use super::*`.
use actor::{Actor, AdminReplays, AuthCommand};
use clock::unix_seconds;
use keys::{load_isolated_server_admin_credential, load_server_admin_credential};
use leases::Leases;
use persistence::{
    AdminReplayRecord, AuditRecord, PersistedEntry, PersistedSnapshot, StateMutation, WalRecord,
    append_audit, append_mutation, append_wal, atomic_write, auth_snapshot_path, build_snapshot,
    cancel_all_temporary_leases, empty_snapshot, fail_closed_on_uncertain_wal, hex,
    load_or_create_instance_id, load_persisted_state, normalize_tombstone_times, open_blob,
    persist_recovery_admin_key, prepare_state_dir_and_lock, push_audit_record,
    push_persisted_audit, random_instance_id, read_admin_key, read_namespace_high_water,
    recover_admin_key_after_rotation, recover_instance_id_after_reset, reset_already_installed,
    restore_namespace_high_water, rotation_already_installed, split_high_slot_state,
    truncate_auth_wal, write_admin_key, write_namespace_high_water,
    write_snapshot_and_truncate_wal,
};
use state::{
    AdminState, AuthStateInner, ColdMetadata, LiveLeases, PreviousRoot, RestoredState, SlotHot,
    SlotState,
};
use timing_wheel::{Timer, TimingWheel};
