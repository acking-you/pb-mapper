//! Administrator key files, instance id, and recovery-key identity checks.
use super::super::*;
use super::{
    atomic_write, auth_snapshot_path, auth_wal_path, encrypted_auth_state_exists, open_blob,
    truncate_auth_wal,
};

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

pub(crate) fn write_admin_key(state_dir: &Path, key: &str) -> Result<(), AuthFailure> {
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
        && !key_matches_existing_state(path.parent(), key)
    {
        refuse_write_if_encrypted_state(path, force)?;
    }
    atomic_write(path, format!("{key}\n").as_bytes(), 0o600)
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

pub(crate) fn rotation_already_installed(state_dir: &Path, new_key: &str) -> bool {
    key_matches_existing_snapshot(Some(state_dir), new_key)
        && live_admin_key_matches(state_dir, new_key)
}

fn live_admin_key_matches(state_dir: &Path, new_key: &str) -> bool {
    let Ok(raw) = std::fs::read(state_dir.join("admin.key")) else {
        return false;
    };
    let Ok(text) = std::str::from_utf8(&raw) else {
        return false;
    };
    text.trim().as_bytes() == new_key.trim().as_bytes()
}

pub(crate) fn key_matches_existing_snapshot(state_dir: Option<&Path>, key: &str) -> bool {
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

pub(crate) fn key_matches_existing_state(state_dir: Option<&Path>, key: &str) -> bool {
    if key_matches_existing_snapshot(state_dir, key) {
        return true;
    }
    let Some(state_dir) = state_dir else {
        return false;
    };
    if auth_snapshot_path(state_dir).exists() {
        return false;
    }
    let wal_path = auth_wal_path(state_dir);
    if !wal_path.exists() {
        return false;
    }
    let Ok(Credential::Admin(admin_key)) = parse_credential(key) else {
        return false;
    };
    wal_decrypts_with_key(&wal_path, &admin_key)
}

fn wal_decrypts_with_key(path: &Path, admin_key: &AesKeyType) -> bool {
    let Ok(mut file) = File::open(path) else {
        return false;
    };
    let Ok(metadata) = file.metadata() else {
        return false;
    };
    if metadata.len() == 0 {
        return true;
    }
    let mut length = [0_u8; 4];
    if file.read_exact(&mut length).is_err() {
        return false;
    }
    let length = u32::from_be_bytes(length) as usize;
    if length == 0 || length > 1024 * 1024 {
        return false;
    }
    let mut sealed = vec![0_u8; length];
    if file.read_exact(&mut sealed).is_err() {
        return false;
    }
    open_blob(admin_key, &sealed).is_ok()
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
