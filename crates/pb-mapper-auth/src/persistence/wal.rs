//! Encrypted WAL append, replay, and snapshot compaction.
use super::super::*;
use super::{
    atomic_write, auth_snapshot_path, auth_wal_path, cancel_all_temporary_leases, open_blob,
    push_audit_record, seal_blob, sync_parent_directory,
};

/// What a failed append leaves the store fit for.
#[derive(Debug)]
pub(crate) enum WalState {
    /// Nothing reached the WAL, or what did was rolled back, so a later append
    /// starts at a known-good offset.
    Intact,
    /// The WAL, or the means to keep it, is in doubt: the store fails closed.
    Uncertain,
}

/// A failed append: the failure for the caller, and what it means for the store.
#[derive(Debug)]
pub(crate) struct WalAppendError {
    pub(crate) failure: AuthFailure,
    pub(crate) wal: WalState,
}

impl WalAppendError {
    fn intact(failure: AuthFailure) -> Self {
        Self {
            failure,
            wal: WalState::Intact,
        }
    }

    fn uncertain(failure: AuthFailure) -> Self {
        Self {
            failure,
            wal: WalState::Uncertain,
        }
    }
}

/// Appends `record` to the WAL durably, as the actor's own change. Marks the
/// snapshot stale, and enters safe mode when a failed append leaves the WAL
/// uncertain, handing the caller its failure either way.
pub(crate) fn append_record(
    config: &AuthConfig,
    inner: &AuthStateInner,
    record: &WalRecord,
) -> Result<(), AuthFailure> {
    inner.mark_snapshot_stale();
    append_wal(config, &inner.admin_key(), record).map_err(|WalAppendError { failure, wal }| {
        match wal {
            WalState::Intact => {}
            WalState::Uncertain => {
                inner.safe_mode.store(true, Ordering::Release);
                cancel_all_temporary_leases(inner);
            }
        }
        failure
    })
}

pub(crate) fn append_mutation(
    config: &AuthConfig,
    inner: &AuthStateInner,
    mutation: StateMutation,
    audit: AuditRecord,
) -> Result<(), AuthFailure> {
    append_record(
        config,
        inner,
        &WalRecord::Mutation {
            mutation,
            audit: audit.clone(),
        },
    )?;
    push_audit_record(inner, audit);
    Ok(())
}

pub(crate) fn append_audit(
    config: &AuthConfig,
    inner: &AuthStateInner,
    audit: AuditRecord,
) -> Result<(), AuthFailure> {
    append_record(config, inner, &WalRecord::Audit(audit.clone()))?;
    push_audit_record(inner, audit);
    Ok(())
}

/// Appends `record` to the WAL and makes it durable. A failure the WAL is known
/// to have survived is retryable for the client; any other fails the store closed.
pub(crate) fn append_wal(
    config: &AuthConfig,
    admin_key: &AesKeyType,
    record: &WalRecord,
) -> Result<(), WalAppendError> {
    let plain = serde_json::to_vec(record)
        .map_err(|error| {
            AuthFailure::internal(format!("failed to encode auth WAL record: {error}"))
        })
        .map_err(WalAppendError::uncertain)?;
    let sealed = seal_blob(admin_key, &plain).map_err(WalAppendError::uncertain)?;
    let length = u32::try_from(sealed.len())
        .map_err(|_| AuthFailure::internal("auth WAL record is too large"))
        .map_err(WalAppendError::uncertain)?;
    let path = auth_wal_path(&config.state_dir);
    let created = !path.exists();
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|error| {
            WalAppendError::intact(AuthFailure::new(
                codes::TEMPORARY_KEY_STORE_UNAVAILABLE,
                format!("failed to open `{}`: {error}", path.display()),
                true,
            ))
        })?;
    // WHY intact: nothing has been written, so the WAL still matches the
    // state. Refusing this append keeps every record out of a file whose mode
    // could not be secured without failing the store closed, which would end
    // the sessions of every live key and need a reset that discards them all.
    #[cfg(unix)]
    file.set_permissions(std::fs::Permissions::from_mode(0o600))
        .map_err(|error| {
            WalAppendError::intact(AuthFailure::new(
                codes::TEMPORARY_KEY_STORE_UNAVAILABLE,
                format!("failed to secure `{}`: {error}", path.display()),
                false,
            ))
        })?;
    let start_len = file
        .metadata()
        .map(|metadata| metadata.len())
        .map_err(|error| {
            WalAppendError::intact(AuthFailure::new(
                codes::TEMPORARY_KEY_STORE_UNAVAILABLE,
                format!("failed to inspect `{}`: {error}", path.display()),
                true,
            ))
        })?;
    if let Err(error) = file
        .write_all(&length.to_be_bytes())
        .and_then(|()| file.write_all(&sealed))
        .and_then(|()| file.sync_data())
    {
        // Rolled back, a later append can start at a known-good offset. If
        // truncation fails, the next record would be unreadable.
        let rolled_back = file
            .set_len(start_len)
            .and_then(|()| file.sync_data())
            .is_ok();
        return Err(if rolled_back {
            WalAppendError::intact(AuthFailure::new(
                codes::TEMPORARY_KEY_STORE_UNAVAILABLE,
                format!("failed to durably append `{}`: {error}", path.display()),
                true,
            ))
        } else {
            WalAppendError::uncertain(AuthFailure::new(
                codes::TEMPORARY_KEY_STORE_UNAVAILABLE,
                format!(
                    "failed to durably append `{}` and could not restore the previous WAL length: {error}",
                    path.display()
                ),
                false,
            ))
        });
    }
    if created {
        sync_parent_directory(&path).map_err(WalAppendError::uncertain)?;
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

/// Reads the first byte of the next frame's length prefix into `first`, or
/// returns `false` at a clean end of file. The byte is read alone so that end
/// of file between frames is told apart from one inside a length prefix, and
/// an interrupted read is retried, as `read_exact` does for the rest.
fn frame_begins(file: &mut impl Read, first: &mut u8) -> std::io::Result<bool> {
    loop {
        match file.read(std::slice::from_mut(first)) {
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            result => return result.map(|read| read != 0),
        }
    }
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
        let mut length = [0_u8; 4];
        let [first, ..] = &mut length;
        match frame_begins(&mut file, first) {
            Ok(false) => break,
            Ok(true) => {}
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

#[cfg(test)]
mod tests {
    use std::io::{Error, ErrorKind, Read};

    use super::frame_begins;

    /// Answers each read from a script: an error kind, or the bytes to hand out.
    struct Script(Vec<Result<&'static [u8], ErrorKind>>);

    impl Read for Script {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            match self.0.remove(0) {
                Ok(bytes) => {
                    buf[..bytes.len()].copy_from_slice(bytes);
                    Ok(bytes.len())
                }
                Err(kind) => Err(Error::from(kind)),
            }
        }
    }

    /// A signal landing on the read between two frames is not damage: the
    /// read is retried, as `read_exact` retries the rest of the frame.
    #[test]
    fn an_interrupted_read_between_frames_is_retried() {
        let mut first = 0;
        let mut file = Script(vec![Err(ErrorKind::Interrupted), Ok(&[7])]);
        assert!(frame_begins(&mut file, &mut first).unwrap());
        assert_eq!(first, 7);
        let mut file = Script(vec![Err(ErrorKind::Interrupted), Ok(&[])]);
        assert!(!frame_begins(&mut file, &mut first).unwrap());
        let mut file = Script(vec![Err(ErrorKind::PermissionDenied)]);
        assert!(frame_begins(&mut file, &mut first).is_err());
    }
}
