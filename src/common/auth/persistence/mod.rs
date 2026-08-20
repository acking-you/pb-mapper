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
mod fs;
mod snapshot;
mod wal;

#[cfg(test)]
pub(in crate::common::auth) use admin_key::read_instance_id_file;
pub use admin_key::{generate_admin_key, initialize_admin_key, write_admin_key_file};
pub(in crate::common::auth) use admin_key::{
    key_matches_existing_state, load_or_create_instance_id, random_instance_id,
    recover_instance_id_after_reset, reset_already_installed, rotation_already_installed,
    write_admin_key,
};
pub(in crate::common::auth) use blob::{open_blob, seal_blob};
pub use fs::acquire_state_dir_lock;
#[cfg(test)]
pub(in crate::common::auth) use fs::prepare_state_dir;
pub(in crate::common::auth) use fs::{atomic_write, prepare_state_dir_and_lock};
pub(crate) use fs::{replace_file, sync_parent_directory};
#[cfg(test)]
pub(in crate::common::auth) use snapshot::try_load_persisted_state;
pub(in crate::common::auth) use snapshot::{
    build_snapshot, cancel_all_temporary_leases, compaction_is_allowed, empty_snapshot,
    load_persisted_state, normalize_tombstone_times, push_audit_record, push_persisted_audit,
    split_high_slot_state,
};
pub(in crate::common::auth) use wal::{
    append_audit, append_mutation, append_wal, fail_closed_on_uncertain_wal, read_wal,
    truncate_auth_wal, write_snapshot_and_truncate_wal,
};

pub(in crate::common::auth) const AUTH_SNAPSHOT_FILE: &str = "auth.snapshot";
pub(in crate::common::auth) const AUTH_WAL_FILE: &str = "auth.wal";

pub(in crate::common::auth) fn auth_snapshot_path(state_dir: &Path) -> PathBuf {
    state_dir.join(AUTH_SNAPSHOT_FILE)
}

pub(in crate::common::auth) fn auth_wal_path(state_dir: &Path) -> PathBuf {
    state_dir.join(AUTH_WAL_FILE)
}

pub fn encrypted_auth_state_exists(state_dir: &Path) -> bool {
    auth_snapshot_path(state_dir).exists() || auth_wal_path(state_dir).exists()
}

pub(in crate::common::auth) fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub(in crate::common::auth) fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(DIGITS[(byte >> 4) as usize] as char);
        output.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    output
}
