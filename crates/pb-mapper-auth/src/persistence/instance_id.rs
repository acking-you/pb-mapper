//! The server instance id: the salt every temporary key is derived with.
use super::super::*;

pub(crate) fn load_or_create_instance_id(
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

pub(crate) fn read_instance_id_file(
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
pub(crate) fn recover_instance_id_after_reset(
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

pub(crate) fn random_instance_id() -> [u8; INSTANCE_ID_LEN] {
    let mut instance_id = [0_u8; INSTANCE_ID_LEN];
    let mut rng = rand::rng();
    for byte in &mut instance_id {
        *byte = rng.random();
    }
    instance_id
}

pub(crate) fn reset_already_installed(
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
