//! The background grant at rest: DPAPI-protected for this machine, private
//! to SYSTEM and administrators, and replaced atomically.
use super::{FILE_NAME, Options, REVOKED_MARKER, os};
use crate::{HostIdentity, Result, error};
use pb_mapper_client::sdk::{Client, ClientConfig};
use serde::{Deserialize, Serialize};
use std::path::Path;

const STORAGE_LIMIT: u64 = 32 * 1024;
/// Every stored grant was protected with these, for this machine; they
/// never change.
const ENTROPY: &[u8] = b"pb-mapper/background-host/v1";
const DESCRIPTION: &str = "pb-mapper background host";

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Saved {
    schema: u32,
    pub(super) owner_sid: String,
    pub(super) options: Options,
    pub(super) identity: HostIdentity,
}
impl Saved {
    pub(super) fn prepare(
        options: Options,
        owner_sid: &str,
        previous: Option<&Self>,
        imported: Option<HostIdentity>,
    ) -> Result<Self> {
        let key_id = validate(&options)?;
        let same_identity = previous.is_some_and(|old| {
            old.owner_sid == owner_sid
                && old.options.server == options.server
                && old.options.settings.service == options.settings.service
                && namespace(&old.options).is_ok_and(|old| old == key_id)
        });
        let identity = if same_identity {
            previous
                .ok_or_else(|| error("Missing previous background identity"))?
                .identity
                .clone()
        } else if let Some(identity) = imported {
            identity.acceptor()?;
            identity
        } else {
            HostIdentity::generate()?
        };
        Ok(Self {
            schema: 1,
            owner_sid: owner_sid.into(),
            options,
            identity,
        })
    }
}
fn validate(options: &Options) -> Result<u64> {
    if options.server.is_empty()
        || options.server.len() > 1024
        || options.credential.len() > 512
        || !crate::valid_service(&options.settings.service)
    {
        return Err(error("Invalid background host configuration"));
    }
    let namespace = namespace(options)?;
    client(options)?;
    Ok(namespace)
}
pub(super) fn namespace(options: &Options) -> Result<u64> {
    pb_mapper_core::checksum::parse_credential(options.credential.trim())
        .map(|credential| credential.key_id())
        .map_err(|_| error("Invalid relay credential"))
}
pub(super) fn client(options: &Options) -> Result<Client> {
    Client::new(ClientConfig {
        server: options.server.clone(),
        credential: options.credential.clone(),
        keep_alive: true,
        namespace: None,
    })
    .and_then(|client| client.with_relay_transport(options.relay_transport))
    .map_err(|_| error("Invalid relay configuration"))
}

pub(super) fn load(root: &Path, owner_sid: &str) -> Result<Option<Saved>> {
    if root.join(REVOKED_MARKER).exists() {
        return Err(error("Background authorization has a pending revocation"));
    }
    let path = root.join(FILE_NAME);
    if !path.exists() {
        return Ok(None);
    }
    private_file(&path)?;
    if std::fs::metadata(&path)?.len() > STORAGE_LIMIT {
        return Err(error("Background authorization exceeds its size limit"));
    }
    let plain = protect(&std::fs::read(&path)?, false)?;
    let saved: Saved =
        serde_json::from_slice(&plain).map_err(|_| error("Invalid background authorization"))?;
    if saved.schema != 1 || saved.owner_sid != owner_sid {
        return Err(error(
            "Background authorization belongs to another installation owner",
        ));
    }
    validate(&saved.options)?;
    saved.identity.acceptor()?;
    Ok(Some(saved))
}
pub(super) fn save(root: &Path, saved: &Saved) -> Result<()> {
    use std::{io::Write, os::windows::io::FromRawHandle};
    use windows::{
        Win32::{Foundation::GENERIC_WRITE, Storage::FileSystem::*},
        core::PCWSTR,
    };
    os::protected_path(root)?;
    let target = root.join(FILE_NAME);
    if target.exists() {
        private_file(&target)?;
    }
    let cipher = protect(&serde_json::to_vec(saved).map_err(error)?, true)?;
    let temporary = root.join(format!("background-{}.tmp", crate::security::secret()?));
    let descriptor = os::Descriptor::new("O:SYG:SYD:P(A;;FA;;;SY)(A;;FA;;;BA)")?;
    let result = (|| {
        let handle = unsafe {
            CreateFileW(
                PCWSTR(os::wide(&temporary).as_ptr()),
                GENERIC_WRITE.0,
                FILE_SHARE_READ,
                Some(descriptor.attributes()),
                CREATE_NEW,
                FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT,
                None,
            )
        }
        .map_err(error)?;
        let mut file = unsafe { std::fs::File::from_raw_handle(handle.0) };
        file.write_all(&cipher)?;
        file.sync_all()?;
        drop(file);
        unsafe {
            MoveFileExW(
                PCWSTR(os::wide(&temporary).as_ptr()),
                PCWSTR(os::wide(&target).as_ptr()),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        }
        .map_err(error)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temporary);
    }
    result
}
pub(super) fn remove(root: &Path) -> Result<()> {
    let target = root.join(FILE_NAME);
    if target.exists() {
        private_file(&target)?;
        std::fs::remove_file(target)?;
    }
    Ok(())
}

pub(super) fn mark_revoked(root: &Path) -> Result<()> {
    use std::{io::Write, os::windows::io::FromRawHandle};
    use windows::{
        Win32::{Foundation::GENERIC_WRITE, Storage::FileSystem::*},
        core::PCWSTR,
    };
    os::protected_path(root)?;
    let path = root.join(REVOKED_MARKER);
    if path.exists() {
        private_file(&path)?;
        return Ok(());
    }
    let descriptor = os::Descriptor::new("O:SYG:SYD:P(A;;FA;;;SY)(A;;FA;;;BA)")?;
    let handle = unsafe {
        CreateFileW(
            PCWSTR(os::wide(&path).as_ptr()),
            GENERIC_WRITE.0,
            FILE_SHARE_READ,
            Some(descriptor.attributes()),
            CREATE_NEW,
            FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_WRITE_THROUGH,
            None,
        )
    }
    .map_err(error)?;
    let mut file = unsafe { std::fs::File::from_raw_handle(handle.0) };
    file.write_all(b"revoked")?;
    file.sync_all()?;
    Ok(())
}
pub(super) fn clear_marker(root: &Path) -> Result<()> {
    let path = root.join(REVOKED_MARKER);
    if path.exists() {
        private_file(&path)?;
        std::fs::remove_file(path)?;
    }
    Ok(())
}
pub(crate) fn revoke_installation(root: &Path) -> Result<()> {
    remove(root)?;
    clear_marker(root)
}

pub(super) fn protect(bytes: &[u8], encrypt: bool) -> Result<Vec<u8>> {
    crate::platform::windows::dpapi::protect(
        bytes,
        encrypt,
        crate::platform::windows::dpapi::Scope::Machine,
        ENTROPY,
        DESCRIPTION,
        STORAGE_LIMIT,
    )
}

/// Not only unwritable, as every installation file is: nobody but SYSTEM
/// and administrators may even read a grant.
fn private_file(path: &Path) -> Result<()> {
    os::protected_file(path)?;
    os::check_acl(path, u32::MAX).map_err(|failure| {
        error(format!(
            "Background authorization must be private to SYSTEM and administrators: {failure}"
        ))
    })
}
