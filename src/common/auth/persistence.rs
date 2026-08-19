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

pub(super) const AUTH_SNAPSHOT_FILE: &str = "auth.snapshot";
pub(super) const AUTH_WAL_FILE: &str = "auth.wal";

pub(super) fn auth_snapshot_path(state_dir: &Path) -> PathBuf {
    state_dir.join(AUTH_SNAPSHOT_FILE)
}

pub(super) fn auth_wal_path(state_dir: &Path) -> PathBuf {
    state_dir.join(AUTH_WAL_FILE)
}

pub fn encrypted_auth_state_exists(state_dir: &Path) -> bool {
    auth_snapshot_path(state_dir).exists() || auth_wal_path(state_dir).exists()
}

/// Create the state directory and take `auth.lock` before any credential or
/// snapshot file is read or written.
pub(super) fn prepare_state_dir_and_lock(state_dir: &Path) -> Result<Arc<File>, AuthFailure> {
    prepare_state_dir(state_dir)?;
    Ok(Arc::new(acquire_state_dir_lock(state_dir)?))
}

pub fn acquire_state_dir_lock(state_dir: &Path) -> Result<File, AuthFailure> {
    let path = state_dir.join("auth.lock");
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .map_err(|error| {
            AuthFailure::new(
                "auth_state_unavailable",
                format!("failed to open `{}`: {error}", path.display()),
                false,
            )
        })?;
    lock_exclusive_nonblock(&file).map_err(|error| {
        AuthFailure::new(
            "auth_state_locked",
            format!(
                "authentication state directory `{}` is already in use: {error}",
                state_dir.display()
            ),
            false,
        )
    })?;
    Ok(file)
}

fn lock_exclusive_nonblock(file: &File) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        extern "C" {
            fn flock(fd: i32, operation: i32) -> i32;
        }
        const LOCK_EX: i32 = 2;
        const LOCK_NB: i32 = 4;
        use std::os::unix::io::AsRawFd;
        if unsafe { flock(file.as_raw_fd(), LOCK_EX | LOCK_NB) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        const LOCKFILE_FAIL_IMMEDIATELY: u32 = 0x1;
        const LOCKFILE_EXCLUSIVE_LOCK: u32 = 0x2;
        #[repr(C)]
        struct Overlapped {
            internal: usize,
            internal_high: usize,
            offset: u32,
            offset_high: u32,
            event: *mut core::ffi::c_void,
        }
        extern "system" {
            fn LockFileEx(
                file: *mut core::ffi::c_void,
                flags: u32,
                reserved: u32,
                bytes_low: u32,
                bytes_high: u32,
                overlapped: *mut Overlapped,
            ) -> i32;
        }
        let mut overlapped = Overlapped {
            internal: 0,
            internal_high: 0,
            offset: 0,
            offset_high: 0,
            event: core::ptr::null_mut(),
        };
        let ok = unsafe {
            LockFileEx(
                file.as_raw_handle(),
                LOCKFILE_FAIL_IMMEDIATELY | LOCKFILE_EXCLUSIVE_LOCK,
                0,
                1,
                0,
                &mut overlapped,
            )
        };
        if ok == 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = file;
        Ok(())
    }
}

pub(super) fn compaction_is_allowed(safe_mode: bool) -> bool {
    !safe_mode
}

pub(super) fn clear_retained_high_slot_entries(inner: &AuthStateInner) {
    inner
        .high_slot_entries
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clear();
}

pub(crate) fn replace_file(from: &Path, to: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;

        const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
        const MOVEFILE_WRITE_THROUGH: u32 = 0x8;
        extern "system" {
            fn MoveFileExW(
                lp_existing_file_name: *const u16,
                lp_new_file_name: *const u16,
                dw_flags: u32,
            ) -> i32;
        }
        fn wide(path: &Path) -> Vec<u16> {
            path.as_os_str()
                .encode_wide()
                .chain(std::iter::once(0))
                .collect()
        }
        let from_w = wide(from);
        let to_w = wide(to);
        let ok = unsafe {
            MoveFileExW(
                from_w.as_ptr(),
                to_w.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        };
        if ok == 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
    #[cfg(not(windows))]
    std::fs::rename(from, to)
}

pub(crate) fn sync_parent_directory(path: &Path) -> Result<(), AuthFailure> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    open_directory_for_sync(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| {
            AuthFailure::new(
                "temporary_key_store_unavailable",
                format!("failed to sync `{}`: {error}", parent.display()),
                false,
            )
        })
}

fn open_directory_for_sync(path: &Path) -> std::io::Result<File> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const GENERIC_READ: u32 = 0x8000_0000;
        const GENERIC_WRITE: u32 = 0x4000_0000;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        OpenOptions::new()
            .access_mode(GENERIC_READ | GENERIC_WRITE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)
    }
    #[cfg(not(windows))]
    File::open(path)
}

pub(super) fn push_audit_record(inner: &AuthStateInner, record: AuditRecord) {
    let mut records = inner
        .audit_records
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    while records.len() >= AUDIT_RECORD_CAPACITY {
        records.pop_front();
    }
    records.push_back(record);
}

pub(super) fn cancel_all_temporary_leases(inner: &AuthStateInner) {
    let slots = inner
        .slots
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    for lease in slots.iter().filter_map(|slot| slot.lease.upgrade()) {
        lease.cancel_rotated();
    }
}

fn snapshot_generations(inner: &AuthStateInner) -> Vec<u32> {
    let slots = inner
        .slots
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let extra = inner
        .high_slot_generations
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut generations = slots.iter().map(|slot| slot.generation).collect::<Vec<_>>();
    generations.extend_from_slice(&extra);
    generations
}

pub(super) fn split_high_slot_state(
    snapshot: &PersistedSnapshot,
    capacity: usize,
) -> (Vec<u32>, Vec<PersistedEntry>) {
    let high_generations = snapshot.generations.get(capacity..).unwrap_or(&[]).to_vec();
    let high_entries = snapshot
        .entries
        .iter()
        .filter(|entry| key_slot(entry.key_id) as usize >= capacity)
        .cloned()
        .collect();
    (high_generations, high_entries)
}

pub(super) fn build_snapshot(
    inner: &AuthStateInner,
    cold: &HashMap<u64, ColdMetadata>,
    admin_replays: &VecDeque<AdminReplayRecord>,
) -> PersistedSnapshot {
    let slots = inner
        .slots
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let generations = snapshot_generations(inner);
    let mut entries = slots
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
                tombstoned_at: (cold.tombstoned_at != 0).then_some(cold.tombstoned_at),
            })
        })
        .collect::<Vec<_>>();
    entries.extend(
        inner
            .high_slot_entries
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .cloned(),
    );
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
        admin_replays: admin_replays.iter().cloned().collect(),
        audit_records: inner
            .audit_records
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone(),
        root_epoch: inner.root_epoch.load(Ordering::Acquire),
    }
}

pub(super) fn normalize_tombstone_times(snapshot: &mut PersistedSnapshot, now: u64) -> bool {
    let mut changed = false;
    for entry in &mut snapshot.entries {
        if entry.tombstoned_at.is_some() {
            continue;
        }
        let tombstoned_at = match entry.state {
            SlotState::Expired => Some(entry.expires_at),
            SlotState::Revoked => snapshot
                .audit_records
                .iter()
                .rev()
                .find(|record| {
                    record.action == "temporary_key_revoke" && record.key_id == Some(entry.key_id)
                })
                .map(|record| record.at)
                .or(Some(now)),
            SlotState::Free | SlotState::Active => None,
        };
        if tombstoned_at.is_some() {
            entry.tombstoned_at = tombstoned_at;
            changed = true;
        }
    }
    changed
}

pub(super) fn empty_snapshot(
    inner: &AuthStateInner,
    instance_id: [u8; INSTANCE_ID_LEN],
    admin_replays: &VecDeque<AdminReplayRecord>,
) -> PersistedSnapshot {
    PersistedSnapshot {
        schema_version: SNAPSHOT_SCHEMA_VERSION,
        instance_id,
        generations: snapshot_generations(inner),
        entries: Vec::new(),
        legacy_protocol: if inner.legacy_protocol_allowed.load(Ordering::Acquire) {
            LegacyProtocolPolicy::Allow
        } else {
            LegacyProtocolPolicy::Deny
        },
        admin_replays: admin_replays.iter().cloned().collect(),
        audit_records: inner
            .audit_records
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone(),
        root_epoch: inner.root_epoch.load(Ordering::Acquire),
    }
}

pub(super) fn load_persisted_state(
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

pub(super) fn try_load_persisted_state(
    config: &AuthConfig,
    admin_key: &AesKeyType,
    instance_id: [u8; INSTANCE_ID_LEN],
) -> Result<PersistedSnapshot, AuthFailure> {
    let snapshot_path = auth_snapshot_path(&config.state_dir);
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
            admin_replays: Vec::new(),
            audit_records: VecDeque::new(),
            root_epoch: 0,
        }
    };
    if snapshot.schema_version != SNAPSHOT_SCHEMA_VERSION || snapshot.instance_id != instance_id {
        return Err(AuthFailure::new(
            "temporary_key_store_unavailable",
            "auth snapshot schema or server instance id does not match",
            false,
        ));
    }
    if snapshot.generations.len() < config.max_temporary_keys {
        snapshot.generations.resize(config.max_temporary_keys, 0);
    }

    let wal_path = auth_wal_path(&config.state_dir);
    if wal_path.exists() {
        for record in read_wal(&wal_path, admin_key)? {
            match record {
                WalRecord::Mutation { mutation, audit } => {
                    apply_persisted_mutation(&mut snapshot, mutation, config.max_temporary_keys)?;
                    push_persisted_audit(&mut snapshot.audit_records, audit);
                }
                WalRecord::AdminReplay(record) => snapshot.admin_replays.push(record),
                WalRecord::Audit(audit) => push_persisted_audit(&mut snapshot.audit_records, audit),
            }
        }
    }
    Ok(snapshot)
}

pub(super) fn apply_persisted_mutation(
    snapshot: &mut PersistedSnapshot,
    mutation: StateMutation,
    capacity: usize,
) -> Result<(), AuthFailure> {
    match mutation {
        StateMutation::Issue(entry) => {
            let index = key_slot(entry.key_id) as usize;
            if snapshot.generations.len() <= index {
                snapshot.generations.resize(index + 1, 0);
            }
            snapshot.generations[index] = key_generation(entry.key_id);
            if index >= capacity {
                snapshot
                    .entries
                    .retain(|current| key_slot(current.key_id) as usize != index);
                snapshot.entries.push(entry);
                return Ok(());
            }
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
            entry.tombstoned_at = None;
        }
        StateMutation::Revoke { key_id, at } => {
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
            entry.tombstoned_at = Some(at);
        }
        StateMutation::LegacyProtocol(policy) => snapshot.legacy_protocol = policy,
    }
    Ok(())
}

// WHY: `append_wal` sets `retryable` only after it restores the file to the
// pre-append length. Callers persist first and publish hot state only after
// that returns, so a retryable error is a clean no-op. Fail closed only when
// that rollback itself failed and the WAL length is unknown.
pub(super) fn fail_closed_on_uncertain_wal(
    inner: &AuthStateInner,
    result: Result<(), AuthFailure>,
) -> Result<(), AuthFailure> {
    if let Err(error) = &result {
        if !error.retryable {
            inner.safe_mode.store(true, Ordering::Release);
            cancel_all_temporary_leases(inner);
        }
    }
    result
}

pub(super) fn append_mutation(
    config: &AuthConfig,
    inner: &AuthStateInner,
    mutation: StateMutation,
    audit: AuditRecord,
) -> Result<(), AuthFailure> {
    fail_closed_on_uncertain_wal(
        inner,
        append_wal(
            config,
            &inner.admin_key(),
            &WalRecord::Mutation {
                mutation,
                audit: audit.clone(),
            },
        ),
    )?;
    push_audit_record(inner, audit);
    Ok(())
}

pub(super) fn append_audit(
    config: &AuthConfig,
    inner: &AuthStateInner,
    audit: AuditRecord,
) -> Result<(), AuthFailure> {
    fail_closed_on_uncertain_wal(
        inner,
        append_wal(config, &inner.admin_key(), &WalRecord::Audit(audit.clone())),
    )?;
    push_audit_record(inner, audit);
    Ok(())
}

pub(super) fn push_persisted_audit(records: &mut VecDeque<AuditRecord>, record: AuditRecord) {
    while records.len() >= AUDIT_RECORD_CAPACITY {
        records.pop_front();
    }
    records.push_back(record);
}

pub(super) fn append_wal(
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
    let path = auth_wal_path(&config.state_dir);
    let created = !path.exists();
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
    let start_len = file
        .metadata()
        .map(|metadata| metadata.len())
        .map_err(|error| {
            AuthFailure::new(
                "temporary_key_store_unavailable",
                format!("failed to inspect `{}`: {error}", path.display()),
                true,
            )
        })?;
    if let Err(error) = file
        .write_all(&length.to_be_bytes())
        .and_then(|()| file.write_all(&sealed))
        .and_then(|()| file.sync_data())
    {
        // retryable == rolled_back. A later append can then start at a known
        // good offset. If truncation fails, the next record would be unreadable.
        let rolled_back = file
            .set_len(start_len)
            .and_then(|()| file.sync_data())
            .is_ok();
        return Err(AuthFailure::new(
            "temporary_key_store_unavailable",
            if rolled_back {
                format!("failed to durably append `{}`: {error}", path.display())
            } else {
                format!(
                    "failed to durably append `{}` and could not restore the previous WAL length: {error}",
                    path.display()
                )
            },
            rolled_back,
        ));
    }
    if created {
        sync_parent_directory(&path)?;
    }
    Ok(())
}

pub(super) fn read_wal(path: &Path, admin_key: &AesKeyType) -> Result<Vec<WalRecord>, AuthFailure> {
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

pub(super) fn write_snapshot_and_truncate_wal(
    config: &AuthConfig,
    admin_key: &AesKeyType,
    snapshot: &PersistedSnapshot,
) -> Result<(), AuthFailure> {
    let plain = serde_json::to_vec(snapshot).map_err(|error| {
        AuthFailure::internal(format!("failed to encode auth snapshot: {error}"))
    })?;
    let sealed = seal_blob(admin_key, &plain)?;
    let snapshot_path = auth_snapshot_path(&config.state_dir);
    atomic_write(&snapshot_path, &sealed, 0o600)?;
    truncate_auth_wal(&config.state_dir)
}

pub(super) fn truncate_auth_wal(state_dir: &Path) -> Result<(), AuthFailure> {
    let wal_path = auth_wal_path(state_dir);
    let created = !wal_path.exists();
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
    })?;
    if created {
        sync_parent_directory(&wal_path)?;
    }
    Ok(())
}

pub(super) fn seal_blob(admin_key: &AesKeyType, plain: &[u8]) -> Result<Vec<u8>, AuthFailure> {
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

pub(super) fn open_blob(admin_key: &AesKeyType, sealed: &[u8]) -> Result<Vec<u8>, AuthFailure> {
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

pub(super) fn prepare_state_dir(path: &Path) -> Result<(), AuthFailure> {
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

pub(super) fn load_or_create_instance_id(
    path: &Path,
) -> Result<[u8; INSTANCE_ID_LEN], AuthFailure> {
    let instance_path = path.join("server-instance-id");
    if let Some(instance_id) = read_instance_id_file(&instance_path)? {
        return Ok(instance_id);
    }
    let instance_id = random_instance_id();
    atomic_write(&instance_path, &instance_id, 0o600)?;
    Ok(instance_id)
}

pub(super) fn read_instance_id_file(
    path: &Path,
) -> Result<Option<[u8; INSTANCE_ID_LEN]>, AuthFailure> {
    if !path.exists() {
        return Ok(None);
    }
    let bytes = std::fs::read(path).map_err(|error| {
        AuthFailure::new(
            "auth_state_unavailable",
            format!("failed to read `{}`: {error}", path.display()),
            false,
        )
    })?;
    bytes.try_into().map(Some).map_err(|_| {
        AuthFailure::new(
            "auth_state_unavailable",
            "server instance id must be exactly 16 bytes",
            false,
        )
    })
}

/// Promote `server-instance-id.next` when the snapshot already belongs to it.
///
/// Reset writes that staged file, then the empty snapshot, then the live
/// instance-id file. A crash after the snapshot lands would otherwise fail
/// closed on the next start because the live file still has the old id.
pub(super) fn recover_instance_id_after_reset(
    state_dir: &Path,
    admin_key: &AesKeyType,
    current: [u8; INSTANCE_ID_LEN],
) -> Result<[u8; INSTANCE_ID_LEN], AuthFailure> {
    let next_path = state_dir.join("server-instance-id.next");
    let Some(next) = read_instance_id_file(&next_path)? else {
        return Ok(current);
    };
    let snapshot_path = auth_snapshot_path(state_dir);
    if !snapshot_path.exists() {
        let _ = std::fs::remove_file(&next_path);
        return Ok(current);
    }
    let bytes = std::fs::read(&snapshot_path).map_err(|error| {
        AuthFailure::new(
            "temporary_key_store_unavailable",
            format!("failed to read `{}`: {error}", snapshot_path.display()),
            false,
        )
    })?;
    let Ok(plain) = open_blob(admin_key, &bytes) else {
        return Ok(current);
    };
    let Ok(snapshot) = serde_json::from_slice::<PersistedSnapshot>(&plain) else {
        return Ok(current);
    };
    if snapshot.instance_id == current {
        let _ = std::fs::remove_file(&next_path);
        return Ok(current);
    }
    if snapshot.instance_id != next {
        return Ok(current);
    }
    // The reset snapshot is complete. Any leftover WAL still belongs to the
    // previous instance and must not be replayed onto the new derivation id.
    truncate_auth_wal(state_dir)?;
    atomic_write(&state_dir.join("server-instance-id"), &next, 0o600)?;
    let _ = std::fs::remove_file(&next_path);
    Ok(next)
}

pub(super) fn random_instance_id() -> [u8; INSTANCE_ID_LEN] {
    let mut instance_id = [0_u8; INSTANCE_ID_LEN];
    let mut rng = rand::rng();
    for byte in &mut instance_id {
        *byte = rng.random();
    }
    instance_id
}

pub(super) fn write_admin_key(state_dir: &Path, key: &str) -> Result<(), AuthFailure> {
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
    refuse_write_if_encrypted_state(path, force)?;
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
    if path.file_name() == Some(std::ffi::OsStr::new("admin.key"))
        && !key_matches_existing_snapshot(path.parent(), key)
    {
        refuse_write_if_encrypted_state(path, force)?;
    }
    atomic_write(path, format!("{key}\n").as_bytes(), 0o600)
}

pub(super) fn reset_already_installed(
    state_dir: &Path,
    admin_key: &AesKeyType,
    new_instance_id: &[u8; INSTANCE_ID_LEN],
) -> bool {
    let Ok(Some(live)) = read_instance_id_file(&state_dir.join("server-instance-id")) else {
        return false;
    };
    if live != *new_instance_id {
        return false;
    }
    let Ok(bytes) = std::fs::read(auth_snapshot_path(state_dir)) else {
        return false;
    };
    let Ok(plain) = open_blob(admin_key, &bytes) else {
        return false;
    };
    let Ok(snapshot) = serde_json::from_slice::<PersistedSnapshot>(&plain) else {
        return false;
    };
    snapshot.instance_id == *new_instance_id
}

pub(super) fn key_matches_existing_snapshot(state_dir: Option<&Path>, key: &str) -> bool {
    let Some(state_dir) = state_dir else {
        return false;
    };
    let snapshot_path = auth_snapshot_path(state_dir);
    if !snapshot_path.exists() {
        return false;
    }
    let Ok(Credential::Admin(admin_key)) = parse_credential(key) else {
        return false;
    };
    let Ok(bytes) = std::fs::read(&snapshot_path) else {
        return false;
    };
    open_blob(&admin_key, &bytes).is_ok()
}

fn refuse_write_if_encrypted_state(path: &Path, force: bool) -> Result<(), AuthFailure> {
    // Creating or replacing the live root while snapshot/WAL remain leaves
    // those files encrypted under the previous key. Staging `admin.key.next`
    // is the rotate path and must stay allowed.
    let Some(state_dir) = path.parent() else {
        return Ok(());
    };
    if !encrypted_auth_state_exists(state_dir) {
        return Ok(());
    }
    Err(AuthFailure::new(
        "administrator_key_state_exists",
        format!(
            "refusing to {} `{}` while encrypted auth state exists; use `pb-mapper admin root-key rotate` or `pb-mapper admin auth-state reset --confirm`",
            if force { "replace" } else { "create" },
            path.display()
        ),
        false,
    ))
}

pub(super) fn atomic_write(path: &Path, data: &[u8], mode: u32) -> Result<(), AuthFailure> {
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
        replace_file(&temporary, path).map_err(|error| {
            AuthFailure::new(
                "auth_state_unavailable",
                format!("failed to replace `{}`: {error}", path.display()),
                false,
            )
        })?;
        sync_parent_directory(path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

pub(super) fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub(super) fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(DIGITS[(byte >> 4) as usize] as char);
        output.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    output
}
