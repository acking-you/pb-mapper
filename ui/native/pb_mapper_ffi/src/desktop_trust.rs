//! One explicitly remembered host grant per application profile. Both the TLS
//! private key and bearer capability use current-user Windows DPAPI; relay
//! credentials are never copied into this store.
use super::err;
use crate::error::CtlError;
use pb_mapper::desktop::HostIdentity;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

const LIMIT: u64 = 64 * 1024;
pub(super) fn supported() -> bool {
    cfg!(windows)
}
#[derive(Clone)]
pub(super) struct Scope {
    pub directory: PathBuf,
    pub relay: String,
    pub namespace: u64,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Settings {
    pub service: String,
    pub display_id: u32,
    pub allow_control: bool,
    pub relay_only: bool,
    pub privileged: bool,
    pub auto_start: bool,
    /// Service ownership, including a pending one-click setup across app restart.
    #[serde(default)]
    pub background: bool,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Saved {
    schema: u32,
    relay: String,
    namespace: u64,
    pub settings: Settings,
    identity: Vec<u8>,
}
impl Saved {
    #[cfg(windows)]
    pub fn for_background(
        scope: &Scope,
        service: String,
        display_id: u32,
        previous: Option<&Self>,
        live_identity: Option<&HostIdentity>,
    ) -> Result<Self, CtlError> {
        if !service.starts_with("desktop:")
            || service.len() <= 8
            || service.len() > 128
            || !service
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-:".contains(&b))
        {
            return Err(CtlError::invalid_argument("invalid desktop service"));
        }
        let identity = if let Some(identity) = live_identity {
            identity.clone()
        } else if let Some(saved) =
            previous.filter(|saved| saved.matches(scope) && saved.settings.service == service)
        {
            saved.identity()?
        } else {
            HostIdentity::generate().map_err(err)?
        };
        Self::new(
            scope,
            Settings {
                service,
                display_id,
                allow_control: true,
                relay_only: false,
                privileged: true,
                auto_start: true,
                background: true,
            },
            &identity,
        )
    }

    pub fn new(
        scope: &Scope,
        settings: Settings,
        identity: &HostIdentity,
    ) -> Result<Self, CtlError> {
        Ok(Self {
            schema: 1,
            relay: scope.relay.clone(),
            namespace: scope.namespace,
            settings,
            identity: identity.encode_secret().map_err(err)?,
        })
    }
    pub fn matches(&self, scope: &Scope) -> bool {
        self.relay == scope.relay && self.namespace == scope.namespace
    }
    pub fn identity(&self) -> Result<HostIdentity, CtlError> {
        HostIdentity::decode_secret(&self.identity).map_err(err)
    }
    pub fn metadata(&self) -> Value {
        json!({"relay": self.relay, "service": self.settings.service, "display_id": self.settings.display_id,
            "allow_control": self.settings.allow_control, "relay_only": self.settings.relay_only,
            "privileged": self.settings.privileged, "auto_start": self.settings.auto_start,
            "background": self.settings.background})
    }
}
fn path(directory: &Path) -> PathBuf {
    directory.join("desktop-trust.dpapi")
}
pub(super) fn load(directory: &Path) -> Result<Option<Saved>, CtlError> {
    let path = path(directory);
    let metadata = match std::fs::metadata(&path) {
        Ok(metadata) => metadata,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(err(e)),
    };
    if metadata.len() > LIMIT {
        return Err(err("stored desktop trust exceeds its size limit"));
    }
    let cipher = std::fs::read(path).map_err(err)?;
    let plain = protect(&cipher, false)?;
    if plain.len() as u64 > LIMIT {
        return Err(err("stored desktop trust exceeds its size limit"));
    }
    let saved: Saved =
        serde_json::from_slice(&plain).map_err(|_| err("invalid saved desktop trust"))?;
    if saved.schema != 1
        || saved.relay.len() > 1024
        || saved.settings.service.len() > 128
        || !saved.settings.service.starts_with("desktop:")
    {
        return Err(err("invalid saved desktop trust"));
    }
    saved.identity()?;
    Ok(Some(saved))
}
pub(super) fn save(directory: &Path, saved: &Saved) -> Result<(), CtlError> {
    let plain = serde_json::to_vec(saved).map_err(err)?;
    if plain.len() as u64 > LIMIT {
        return Err(err("desktop trust exceeds its size limit"));
    }
    let cipher = protect(&plain, true)?;
    std::fs::create_dir_all(directory).map_err(err)?;
    let mut nonce = [0u8; 16];
    use ring::rand::SecureRandom;
    ring::rand::SystemRandom::new()
        .fill(&mut nonce)
        .map_err(err)?;
    let nonce: String = nonce.iter().map(|b| format!("{b:02x}")).collect();
    let temporary = directory.join(format!("desktop-trust-{nonce}.tmp"));
    let result = (|| {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(err)?;
        file.write_all(&cipher).map_err(err)?;
        file.sync_all().map_err(err)?;
        drop(file);
        replace(&temporary, &path(directory))
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temporary);
    }
    result
}
pub(super) fn revoke(directory: &Path) -> Result<(), CtlError> {
    match std::fs::remove_file(path(directory)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(err(e)),
    }
}
#[cfg(windows)]
fn protect(bytes: &[u8], encrypt: bool) -> Result<Vec<u8>, CtlError> {
    use windows::{
        Win32::{
            Foundation::{HLOCAL, LocalFree},
            Security::Cryptography::*,
        },
        core::w,
    };
    let input = CRYPT_INTEGER_BLOB {
        cbData: bytes.len() as u32,
        pbData: bytes.as_ptr().cast_mut(),
    };
    let entropy = b"pb-mapper/desktop-trust/v1";
    let entropy = CRYPT_INTEGER_BLOB {
        cbData: entropy.len() as u32,
        pbData: entropy.as_ptr().cast_mut(),
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    unsafe {
        if encrypt {
            CryptProtectData(&input, w!("pb-mapper desktop trust"), Some(&entropy), None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut output)
        } else {
            CryptUnprotectData(&input, None, Some(&entropy), None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut output)
        }
    }.map_err(|_| err("Windows could not unlock desktop trust for this user; revoke it and authorize a new invitation"))?;
    let result = if output.cbData as u64 > LIMIT || output.pbData.is_null() {
        Err(err("Windows returned invalid desktop trust data"))
    } else {
        Ok(unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize) }.to_vec())
    };
    unsafe {
        let _ = LocalFree(Some(HLOCAL(output.pbData.cast())));
    }
    result
}
#[cfg(not(windows))]
fn protect(_: &[u8], _: bool) -> Result<Vec<u8>, CtlError> {
    Err(err(
        "remembered desktop hosting currently requires Windows DPAPI",
    ))
}
#[cfg(windows)]
fn replace(source: &Path, destination: &Path) -> Result<(), CtlError> {
    use std::os::windows::ffi::OsStrExt;
    use windows::{Win32::Storage::FileSystem::*, core::PCWSTR};
    let wide = |path: &Path| {
        path.as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect::<Vec<_>>()
    };
    unsafe {
        MoveFileExW(
            PCWSTR(wide(source).as_ptr()),
            PCWSTR(wide(destination).as_ptr()),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    }
    .map_err(err)
}
#[cfg(not(windows))]
fn replace(_: &Path, _: &Path) -> Result<(), CtlError> {
    Err(err("remembered desktop hosting currently requires Windows"))
}

#[cfg(all(test, windows))]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    #[test]
    fn dpapi_round_trip_tampering_and_revocation() {
        let root = std::env::temp_dir().join(format!("pb-desktop-trust-{}", std::process::id()));
        let identity = HostIdentity::generate().unwrap();
        let scope = Scope {
            directory: root.clone(),
            relay: "test-relay:7666".into(),
            namespace: 1,
        };
        let saved = Saved::new(
            &scope,
            Settings {
                service: "desktop:trust-test".into(),
                display_id: 1,
                allow_control: false,
                relay_only: true,
                privileged: false,
                auto_start: false,
                background: false,
            },
            &identity,
        )
        .unwrap();
        save(&root, &saved).unwrap();
        let loaded = load(&root).unwrap().unwrap();
        assert!(loaded.matches(&scope));
        assert!(!loaded.matches(&Scope {
            namespace: 2,
            ..scope.clone()
        }));
        assert_eq!(
            identity.encode_secret().unwrap(),
            loaded.identity().unwrap().encode_secret().unwrap()
        );
        let mut encrypted = std::fs::read(path(&root)).unwrap();
        assert!(
            !encrypted
                .windows(b"desktop:trust-test".len())
                .any(|bytes| bytes == b"desktop:trust-test")
        );
        let last = encrypted.len() - 1;
        encrypted[last] ^= 1;
        std::fs::write(path(&root), encrypted).unwrap();
        assert!(load(&root).is_err());
        revoke(&root).unwrap();
        assert!(load(&root).unwrap().is_none());
        std::fs::remove_dir(root).unwrap();
    }

    #[test]
    fn background_handoff_keeps_the_live_link_through_restart() {
        let scope = Scope {
            directory: Default::default(),
            relay: "relay:7666".into(),
            namespace: 1,
        };
        let identity = HostIdentity::generate().unwrap();
        let pending =
            Saved::for_background(&scope, "desktop:handoff".into(), 1, None, Some(&identity))
                .unwrap();
        // The same DPAPI representation used between portable and installed
        // processes carries the identity, owner scope and the one pending action.
        let encrypted = protect(&serde_json::to_vec(&pending).unwrap(), true).unwrap();
        let restored: Saved = serde_json::from_slice(&protect(&encrypted, false).unwrap()).unwrap();
        assert!(restored.settings.background && restored.settings.auto_start);
        assert!(restored.settings.allow_control && restored.settings.privileged);
        assert!(!restored.settings.relay_only);
        let resumed =
            Saved::for_background(&scope, "desktop:handoff".into(), 2, Some(&restored), None)
                .unwrap();
        assert_eq!(
            resumed.identity().unwrap().encode_secret().unwrap(),
            identity.encode_secret().unwrap()
        );
        let different = Saved::for_background(
            &Scope {
                namespace: 2,
                ..scope
            },
            "desktop:handoff".into(),
            1,
            Some(&restored),
            None,
        )
        .unwrap();
        assert_ne!(
            different.identity().unwrap().encode_secret().unwrap(),
            identity.encode_secret().unwrap()
        );
    }
}
