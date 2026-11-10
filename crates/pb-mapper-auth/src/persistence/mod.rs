//! Durable, encrypted authentication state and audit/replay retention.
//!
//! ```text
//! startup:   lock -> admin.key -> recover instance id -> decrypt snapshot -> replay WAL
//! mutation:  command   -> fsync encrypted WAL -> publish hot-state change
//! compact:   hot state + audit + replay set -> snapshot -> truncate WAL
//! ```
//!
//! Snapshot replacement and administrator-key files use atomic rename. Bounded audit
//! and replay collections are carried through compaction so security history does not
//! disappear when the WAL is truncated.

use super::*;

mod admin_key;
mod blob;
mod format;
mod fs;
mod instance_id;
mod namespaces;
mod snapshot;
mod wal;

pub use admin_key::{
    discard_staged_admin_key, generate_admin_key, initialize_admin_key, stage_admin_key_candidate,
    staged_admin_key_path, write_admin_key_file,
};
pub(crate) use admin_key::{
    persist_recovery_admin_key, read_admin_key, recover_admin_key_after_rotation,
    rotation_already_installed, write_admin_key,
};
pub(crate) use blob::{open_blob, seal_blob};
pub(crate) use format::{
    AdminReplayRecord, AuditRecord, PersistedEntry, PersistedSnapshot, StateMutation, WalRecord,
};
pub use fs::acquire_state_dir_lock;
#[cfg(test)]
pub(crate) use fs::prepare_state_dir;
pub(crate) use fs::sync_parent_directory;
pub(crate) use fs::{atomic_write, create_new_write, prepare_state_dir_and_lock};
#[cfg(test)]
pub(crate) use instance_id::read_instance_id_file;
pub(crate) use instance_id::{
    load_or_create_instance_id, random_instance_id, recover_instance_id_after_reset,
    reset_already_installed,
};
pub(crate) use namespaces::{
    read_namespace_high_water, restore_namespace_high_water, write_namespace_high_water,
};
#[cfg(test)]
pub(crate) use snapshot::try_load_persisted_state;
pub(crate) use snapshot::{
    build_snapshot, cancel_all_temporary_leases, compaction_is_allowed, empty_snapshot,
    load_persisted_state, normalize_tombstone_times, push_audit_record, push_persisted_audit,
    split_high_slot_state,
};
pub(crate) use wal::{
    append_audit, append_mutation, append_wal, fail_closed_on_uncertain_wal, read_wal,
    truncate_auth_wal, write_snapshot_and_truncate_wal,
};

pub(crate) const AUTH_SNAPSHOT_FILE: &str = "auth.snapshot";
pub(crate) const AUTH_WAL_FILE: &str = "auth.wal";

pub(crate) fn auth_snapshot_path(state_dir: &Path) -> PathBuf {
    state_dir.join(AUTH_SNAPSHOT_FILE)
}

pub(crate) fn auth_wal_path(state_dir: &Path) -> PathBuf {
    state_dir.join(AUTH_WAL_FILE)
}

pub fn encrypted_auth_state_exists(state_dir: &Path) -> bool {
    auth_snapshot_path(state_dir).exists() || auth_wal_path(state_dir).exists()
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(DIGITS[(byte >> 4) as usize] as char);
        output.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    output
}
