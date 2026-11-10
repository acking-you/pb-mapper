//! Encrypted WAL append, replay, and snapshot compaction.
use super::super::*;
use super::{
    atomic_write, auth_snapshot_path, auth_wal_path, cancel_all_temporary_leases, open_blob,
    push_audit_record, seal_blob, sync_parent_directory,
};

pub(crate) fn fail_closed_on_uncertain_wal(
    inner: &AuthStateInner,
    result: Result<(), AuthFailure>,
) -> Result<(), AuthFailure> {
    if let Err(error) = &result
        && !error.retryable
    {
        inner.safe_mode.store(true, Ordering::Release);
        cancel_all_temporary_leases(inner);
    }
    result
}

pub(crate) fn append_mutation(
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

pub(crate) fn append_audit(
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

pub(crate) fn append_wal(
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
                codes::TEMPORARY_KEY_STORE_UNAVAILABLE,
                format!("failed to open `{}`: {error}", path.display()),
                true,
            )
        })?;
    #[cfg(unix)]
    file.set_permissions(std::fs::Permissions::from_mode(0o600))
        .map_err(|error| {
            AuthFailure::new(
                codes::TEMPORARY_KEY_STORE_UNAVAILABLE,
                format!("failed to secure `{}`: {error}", path.display()),
                false,
            )
        })?;
    let start_len = file
        .metadata()
        .map(|metadata| metadata.len())
        .map_err(|error| {
            AuthFailure::new(
                codes::TEMPORARY_KEY_STORE_UNAVAILABLE,
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
            codes::TEMPORARY_KEY_STORE_UNAVAILABLE,
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

/// The largest sealed record a WAL frame may announce.
const MAX_WAL_RECORD_LEN: usize = 1024 * 1024;

/// Why the record a WAL frame announces could not be read.
pub(super) enum WalRecordError {
    /// The length prefix announces more than `MAX_WAL_RECORD_LEN`.
    TooLarge,
    /// The file failed or ended before the record did.
    Truncated(std::io::Error),
}

/// Reads the sealed record a frame's big-endian length `prefix` announces into
/// `sealed`, replacing what it held.
pub(super) fn read_wal_record(
    file: &mut File,
    prefix: [u8; 4],
    sealed: &mut Vec<u8>,
) -> Result<(), WalRecordError> {
    let length = u32::from_be_bytes(prefix) as usize;
    if length > MAX_WAL_RECORD_LEN {
        return Err(WalRecordError::TooLarge);
    }
    sealed.resize(length, 0);
    file.read_exact(sealed).map_err(WalRecordError::Truncated)
}

pub(crate) fn read_wal(path: &Path, admin_key: &AesKeyType) -> Result<Vec<WalRecord>, AuthFailure> {
    let mut file = File::open(path).map_err(|error| {
        AuthFailure::new(
            codes::TEMPORARY_KEY_STORE_UNAVAILABLE,
            format!("failed to open `{}`: {error}", path.display()),
            false,
        )
    })?;
    let mut records = Vec::new();
    let mut sealed = Vec::new();
    loop {
        // The first byte alone, so a clean end of file between frames is told
        // apart from one inside a length prefix.
        let mut length = [0_u8; 4];
        match file.read(&mut length[..1]) {
            Ok(0) => break,
            Ok(_) => {}
            Err(error) => {
                return Err(AuthFailure::new(
                    codes::TEMPORARY_KEY_STORE_UNAVAILABLE,
                    format!("failed to read auth WAL length: {error}"),
                    false,
                ));
            }
        }
        file.read_exact(&mut length[1..]).map_err(|error| {
            AuthFailure::new(
                codes::TEMPORARY_KEY_STORE_UNAVAILABLE,
                format!("truncated auth WAL length: {error}"),
                false,
            )
        })?;
        read_wal_record(&mut file, length, &mut sealed).map_err(|error| match error {
            WalRecordError::TooLarge => AuthFailure::new(
                codes::TEMPORARY_KEY_STORE_UNAVAILABLE,
                "auth WAL record exceeds 1 MiB",
                false,
            ),
            WalRecordError::Truncated(error) => AuthFailure::new(
                codes::TEMPORARY_KEY_STORE_UNAVAILABLE,
                format!("truncated auth WAL record: {error}"),
                false,
            ),
        })?;
        let plain = open_blob(admin_key, &sealed)?;
        records.push(serde_json::from_slice(&plain).map_err(|error| {
            AuthFailure::new(
                codes::TEMPORARY_KEY_STORE_UNAVAILABLE,
                format!("failed to decode auth WAL record: {error}"),
                false,
            )
        })?);
    }
    Ok(records)
}

pub(crate) fn write_snapshot_and_truncate_wal(
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

pub(crate) fn truncate_auth_wal(state_dir: &Path) -> Result<(), AuthFailure> {
    let wal_path = auth_wal_path(state_dir);
    let created = !wal_path.exists();
    let wal = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&wal_path)
        .map_err(|error| {
            AuthFailure::new(
                codes::TEMPORARY_KEY_STORE_UNAVAILABLE,
                format!("failed to truncate `{}`: {error}", wal_path.display()),
                true,
            )
        })?;
    wal.sync_all().map_err(|error| {
        AuthFailure::new(
            codes::TEMPORARY_KEY_STORE_UNAVAILABLE,
            format!("failed to sync `{}`: {error}", wal_path.display()),
            true,
        )
    })?;
    if created {
        sync_parent_directory(&wal_path)?;
    }
    Ok(())
}
