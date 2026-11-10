//! Which administrator credential a relay starts with, and temporary-key
//! derivation.
use super::*;

fn validate_admin_credential(raw: &str) -> Result<Credential, AuthFailure> {
    let credential = parse_credential(raw.trim())
        .map_err(|error| AuthFailure::new(codes::ADMINISTRATOR_KEY_INVALID, error, false))?;
    if !credential.is_admin() {
        return Err(AuthFailure::new(
            codes::ADMINISTRATOR_KEY_REQUIRED,
            "the server key file contains a temporary credential",
            false,
        ));
    }
    Ok(credential)
}

pub(super) fn load_server_admin_credential(state_dir: &Path) -> Result<Credential, AuthFailure> {
    let path = admin_key_path(state_dir);
    let raw = if let Some(raw) = read_admin_key(&path)? {
        raw
    } else if process_credential_configured() {
        let credential = get_process_credential()
            .map_err(|error| AuthFailure::new(codes::ADMINISTRATOR_KEY_INVALID, error, false))?;
        let Credential::Admin(key) = credential else {
            return Err(AuthFailure::new(
                codes::ADMINISTRATOR_KEY_REQUIRED,
                "the relay server cannot start with a temporary credential",
                false,
            ));
        };
        let key = String::from_utf8(key.to_vec()).map_err(|_| {
            AuthFailure::new(
                codes::ADMINISTRATOR_KEY_INVALID,
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
                codes::ADMINISTRATOR_KEY_REQUIRED,
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
    let path = admin_key_path(state_dir);
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
    expand_temporary_key(&temporary_root(admin_key, instance_id), key_id)
}

/// What every temporary key of one root key and instance id is expanded
/// from: the HKDF extraction, which depends on nothing else.
pub(crate) fn temporary_root(admin_key: &AesKeyType, instance_id: &[u8; INSTANCE_ID_LEN]) -> Prk {
    Salt::new(HKDF_SHA256, instance_id).extract(admin_key)
}

/// The temporary key `key_id` under `root`, a [`temporary_root`].
pub(crate) fn expand_temporary_key(root: &Prk, key_id: KeyId) -> Result<AesKeyType, AuthFailure> {
    let key_id_bytes = key_id.to_be_bytes();
    let info = [b"pb-mapper-temp-key-v1".as_slice(), key_id_bytes.as_slice()];
    let output = root
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
