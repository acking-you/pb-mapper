//! Administrator credential load, recovery, and temporary-key derivation.
use super::*;

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

fn persist_recovery_admin_key(
    state_dir: &Path,
    key: &str,
    mismatch_message: &'static str,
) -> Result<(), AuthFailure> {
    if encrypted_auth_state_exists(state_dir) && !key_matches_existing_state(Some(state_dir), key) {
        return Err(AuthFailure::new(
            "administrator_key_invalid",
            mismatch_message,
            false,
        ));
    }
    write_admin_key(state_dir, key)
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

pub(super) fn recover_admin_key_after_rotation(
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
    if let Ok(Credential::Admin(current_key)) = parse_credential(current.trim())
        && open_blob(&current_key, &bytes).is_ok()
    {
        return Ok(current.to_string());
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

pub(super) fn load_server_admin_credential(state_dir: &Path) -> Result<Credential, AuthFailure> {
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
        persist_recovery_admin_key(
            state_dir,
            &key,
            "MSG_HEADER_KEY does not decrypt the existing authentication state; refusing to write admin.key",
        )?;
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
        persist_recovery_admin_key(
            state_dir,
            key.trim(),
            "legacy administrator key does not decrypt the existing authentication state; refusing to write admin.key",
        )?;
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
pub(super) fn load_isolated_server_admin_credential(
    state_dir: &Path,
) -> Result<Credential, AuthFailure> {
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

pub fn derive_temporary_key(
    admin_key: &AesKeyType,
    instance_id: &[u8; INSTANCE_ID_LEN],
    key_id: KeyId,
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
