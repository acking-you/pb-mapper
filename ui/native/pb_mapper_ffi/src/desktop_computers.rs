//! Remembered computers, the viewer's side: who this computer reconnects to
//! without pasting an invitation again. One encrypted file per profile keeps
//! each computer's invitation beside what the UI shows of it. Windows seals
//! it with current-user DPAPI. Android seals it with AES-256-GCM under a data
//! key that an Android Keystore key wraps (`ComputerKeystore.kt`); Dart
//! unwraps it and unlocks the vault once per process. Other platforms cannot
//! remember computers.
//!
//! Invitations never return to Dart: a record crosses without one, a computer
//! is remembered from the live session, which already passed its certificate
//! pin, and `connect` resolves an id here. Nothing logs an invitation.
use crate::{
    desktop::{
        trust::{protect, write_private},
        valid_max_fps,
    },
    error::CtlError,
    handle::PbMapperHandle,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use pb_mapper::desktop::{Invitation, QualityPreset, Resolution};
use ring::{
    aead::{AES_256_GCM, Aad, LessSafeKey, NONCE_LEN, Nonce, UnboundKey},
    rand::{SecureRandom, SystemRandom},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::LazyLock,
};

const MAX_COMPUTERS: usize = 50;
/// Fifty records with their invitations fit many times over.
const LIMIT: u64 = 256 * 1024;
const KEY_LIMIT: u64 = 1024;
/// 2: a resolution is written only when chosen, and a remembered "fit
/// local" display mode may be. Version 1 is read, never written.
const VERSION: u32 = 2;
const DPAPI_FILE: &str = "desktop-computers.dpapi";
const SEALED_FILE: &str = "desktop-computers.bin";
const KEY_FILE: &str = "desktop-computers.key";
const ENTROPY: &[u8] = b"pb-mapper remembered computers";
const DESCRIPTION: &str = "pb-mapper remembered computers";
const AAD: &[u8] = b"pb-mapper/computers/1";
const UNSUPPORTED: &str = "remembering computers is not supported here";

/// One process, one vault, decrypted for the process's lifetime once opened.
static VAULT: LazyLock<parking_lot::Mutex<Option<Vault>>> = LazyLock::new(Default::default);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Platform {
    Dpapi,
    Keystore,
    Unsupported,
}
const PLATFORM: Platform = if cfg!(windows) {
    Platform::Dpapi
} else if cfg!(target_os = "android") {
    Platform::Keystore
} else {
    Platform::Unsupported
};

#[derive(Deserialize)]
#[serde(tag = "op", deny_unknown_fields)]
enum Request {
    #[serde(rename = "computers_support")]
    Support {},
    #[serde(rename = "computers_key")]
    Key {},
    #[serde(rename = "computers_unlock")]
    Unlock {
        key: String,
        #[serde(default)]
        wrapped: Option<String>,
    },
    /// Deletes every remembered computer: the way out of a vault that can
    /// no longer be read, on every platform that keeps one.
    #[serde(rename = "computers_reset")]
    Reset {},
    #[serde(rename = "computers_list")]
    List {},
    #[serde(rename = "computers_remember")]
    Remember {
        id: String,
        name: String,
        service: String,
        /// The short form, `AB12-CD34-EF56-7890`.
        fingerprint: String,
        relay: String,
        namespace: u64,
        relay_only: bool,
        quality: QualityPreset,
        /// Absent: the default, whatever it is when the computer connects.
        #[serde(default)]
        resolution: Option<Resolution>,
        #[serde(default)]
        max_fps: u32,
        #[serde(default)]
        display_mode: Option<RememberedMode>,
        #[serde(default)]
        replace_id: Option<String>,
    },
    /// A field left out stays as it is; `resolution` or `display_mode`
    /// given as `null` goes back to the default.
    #[serde(rename = "computers_update")]
    Update {
        id: String,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        quality: Option<QualityPreset>,
        #[serde(default, deserialize_with = "given")]
        resolution: Option<Option<Resolution>>,
        #[serde(default, deserialize_with = "given")]
        display_mode: Option<Option<RememberedMode>>,
        #[serde(default)]
        max_fps: Option<u32>,
        #[serde(default)]
        last_connected_ms: Option<u64>,
        #[serde(default)]
        last_path: Option<String>,
        #[serde(default)]
        last_control_allowed: Option<bool>,
    },
    #[serde(rename = "computers_forget")]
    Forget { id: String },
}

/// A field that is present, `null` included, as `Some`: left out, it is
/// `None` by `#[serde(default)]`.
fn given<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

/// The display mode re-applied when this computer is controlled again. Only
/// "fit local" is remembered: an exact size belongs to the display it was
/// chosen for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RememberedMode {
    FitLocal,
}

/// A remembered computer as Dart sees it: never its invitation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    id: String,
    name: String,
    service: String,
    fingerprint: String,
    relay: String,
    namespace: u64,
    relay_only: bool,
    quality: QualityPreset,
    /// The largest picture asked of it, when one was chosen; `None` follows
    /// the default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    resolution: Option<Resolution>,
    /// Its maximum frame rate: 0 is Auto, see `valid_max_fps`.
    #[serde(default)]
    max_fps: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    display_mode: Option<RememberedMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_connected_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_control_allowed: Option<bool>,
}
impl Record {
    fn check(&self) -> Result<(), CtlError> {
        let invalid = |what: &str| Err(CtlError::invalid_argument(format!("invalid {what}")));
        if !valid_id(&self.id) {
            return invalid("remembered computer id");
        }
        if self.name.trim().is_empty() || self.name.chars().count() > 128 {
            return invalid("remembered computer name");
        }
        if !self.service.starts_with("desktop:") || self.service.len() > 128 {
            return invalid("desktop service");
        }
        if self.fingerprint.len() > 64 || self.relay.len() > 1024 {
            return invalid("remembered computer");
        }
        if self.last_path.as_ref().is_some_and(|path| path.len() > 32) {
            return invalid("connection path");
        }
        if !valid_max_fps(self.max_fps) {
            return invalid("maximum frame rate");
        }
        Ok(())
    }
}

/// Ids are generated by Dart: 128 random bits as lowercase hex.
fn valid_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

#[derive(Clone)]
struct Computer {
    record: Record,
    invitation: Invitation,
}

enum Protection {
    /// Windows: current-user DPAPI.
    Dpapi,
    /// AES-256-GCM under the data key the Keystore wraps.
    Sealed(Box<LessSafeKey>),
}

struct Vault {
    directory: PathBuf,
    protection: Protection,
    /// Read and decrypted on first use.
    computers: Option<Vec<Computer>>,
}
impl Vault {
    fn new(directory: &Path, protection: Protection) -> Self {
        Self {
            directory: directory.to_owned(),
            protection,
            computers: None,
        }
    }

    fn path(&self) -> PathBuf {
        self.directory.join(match self.protection {
            Protection::Dpapi => DPAPI_FILE,
            Protection::Sealed(_) => SEALED_FILE,
        })
    }

    fn computers(&mut self) -> Result<&mut Vec<Computer>, CtlError> {
        let computers = match self.computers.take() {
            Some(computers) => computers,
            None => self.read()?,
        };
        Ok(self.computers.insert(computers))
    }

    fn read(&self) -> Result<Vec<Computer>, CtlError> {
        let Some(sealed) = read_limited(&self.path(), LIMIT)? else {
            return Ok(Vec::new());
        };
        decode(&self.open(sealed)?)
    }

    /// Writes `computers`, and only then keeps them: a failed write changes
    /// nothing.
    fn commit(&mut self, computers: Vec<Computer>) -> Result<(), CtlError> {
        let sealed = self.seal(encode(&computers)?)?;
        write_private(&self.path(), &sealed)?;
        self.computers = Some(computers);
        Ok(())
    }

    fn seal(&self, mut plain: Vec<u8>) -> Result<Vec<u8>, CtlError> {
        match &self.protection {
            Protection::Dpapi => protect(&plain, true, ENTROPY, DESCRIPTION)
                .map_err(|failure| CtlError::io(failure.to_string())),
            Protection::Sealed(key) => {
                let failed = || CtlError::io("the remembered computers could not be encrypted");
                let mut nonce = [0; NONCE_LEN];
                SystemRandom::new().fill(&mut nonce).map_err(|_| failed())?;
                key.seal_in_place_append_tag(
                    Nonce::assume_unique_for_key(nonce),
                    Aad::from(AAD),
                    &mut plain,
                )
                .map_err(|_| failed())?;
                Ok([&nonce[..], &plain[..]].concat())
            }
        }
    }

    fn open(&self, mut sealed: Vec<u8>) -> Result<Vec<u8>, CtlError> {
        match &self.protection {
            Protection::Dpapi => {
                protect(&sealed, false, ENTROPY, DESCRIPTION).map_err(|_| unreadable())
            }
            Protection::Sealed(key) => {
                if sealed.len() < NONCE_LEN + AES_256_GCM.tag_len() {
                    return Err(unreadable());
                }
                let (nonce, ciphertext) = sealed.split_at_mut(NONCE_LEN);
                let nonce = Nonce::try_assume_unique_for_key(nonce).map_err(|_| unreadable())?;
                let plain = key
                    .open_in_place(nonce, Aad::from(AAD), ciphertext)
                    .map_err(|_| unreadable())?;
                Ok(plain.to_vec())
            }
        }
    }

    fn list(&mut self) -> Result<Value, CtlError> {
        let records: Vec<_> = self.computers()?.iter().map(|c| &c.record).collect();
        Ok(json!(records))
    }

    /// Stores the live session's invitation under `record.id`, but only when
    /// that session is the computer the caller names: a late answer from the
    /// previous computer during a fast switch must not land under this id.
    /// `replace_id`'s entry, or one with the same id, is replaced in place.
    fn remember(
        &mut self,
        record: Record,
        replace_id: Option<String>,
        current: Option<&Invitation>,
    ) -> Result<Value, CtlError> {
        record.check()?;
        let current = current.ok_or_else(|| CtlError::invalid_argument("no connected session"))?;
        if current.service != record.service || current.short_fingerprint() != record.fingerprint {
            return Err(CtlError::invalid_argument(
                "the connected session is a different computer",
            ));
        }
        let replaced = |computer: &Computer| {
            computer.record.id == record.id || replace_id.as_ref() == Some(&computer.record.id)
        };
        let mut computers = self.computers()?.clone();
        let position = computers.iter().position(replaced);
        computers.retain(|computer| !replaced(computer));
        if computers.len() >= MAX_COMPUTERS {
            return Err(CtlError::invalid_argument(format!(
                "at most {MAX_COMPUTERS} remembered computers"
            )));
        }
        let at = position.unwrap_or(computers.len()).min(computers.len());
        computers.insert(
            at,
            Computer {
                record: record.clone(),
                invitation: current.clone(),
            },
        );
        self.commit(computers)?;
        Ok(json!(record))
    }

    fn update(&mut self, id: &str, change: impl FnOnce(&mut Record)) -> Result<Value, CtlError> {
        let mut computers = self.computers()?.clone();
        let computer = computers
            .iter_mut()
            .find(|computer| computer.record.id == id)
            .ok_or_else(|| CtlError::not_found("remembered computer not found"))?;
        change(&mut computer.record);
        computer.record.check()?;
        let record = computer.record.clone();
        self.commit(computers)?;
        Ok(json!(record))
    }

    fn forget(&mut self, id: &str) -> Result<(), CtlError> {
        let mut computers = self.computers()?.clone();
        let before = computers.len();
        computers.retain(|computer| computer.record.id != id);
        if computers.len() == before {
            return Ok(());
        }
        self.commit(computers)
    }

    fn invitation(&mut self, id: &str) -> Result<Option<Invitation>, CtlError> {
        Ok(self
            .computers()?
            .iter()
            .find(|computer| computer.record.id == id)
            .map(|computer| computer.invitation.clone()))
    }
}

fn unreadable() -> CtlError {
    CtlError::protocol("the remembered computers cannot be read")
}

/// `{"version":2,"computers":[{…record, "invitation":"pbd1.…"}]}`.
fn encode(computers: &[Computer]) -> Result<Vec<u8>, CtlError> {
    let entries = computers
        .iter()
        .map(|computer| {
            let mut entry = serde_json::to_value(&computer.record)?;
            if let Some(entry) = entry.as_object_mut() {
                entry.insert("invitation".into(), computer.invitation.encode().into());
            }
            Ok(entry)
        })
        .collect::<Result<Vec<_>, serde_json::Error>>()
        .map_err(|failure| CtlError::internal(failure.to_string()))?;
    serde_json::to_vec(&json!({"version": VERSION, "computers": entries}))
        .map_err(|failure| CtlError::internal(failure.to_string()))
}

fn decode(plain: &[u8]) -> Result<Vec<Computer>, CtlError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Stored {
        version: u32,
        computers: Vec<Value>,
    }
    let stored: Stored = serde_json::from_slice(plain).map_err(|_| unreadable())?;
    if !(1..=VERSION).contains(&stored.version) || stored.computers.len() > MAX_COMPUTERS {
        return Err(unreadable());
    }
    stored
        .computers
        .into_iter()
        .map(|mut entry| {
            let invitation = entry
                .as_object_mut()
                .and_then(|entry| entry.remove("invitation"))
                .and_then(|text| Invitation::decode(text.as_str()?).ok())
                .ok_or_else(unreadable)?;
            let mut record: Record = serde_json::from_value(entry).map_err(|_| unreadable())?;
            // Version 1 wrote a resolution whether chosen or not, and its
            // default was 1080p: that, or none, follows today's default.
            // The others were chosen.
            if stored.version == 1 && record.resolution == Some(Resolution::P1080) {
                record.resolution = None;
            }
            record.check().map_err(|_| unreadable())?;
            if invitation.service != record.service
                || invitation.short_fingerprint() != record.fingerprint
            {
                return Err(unreadable());
            }
            Ok(Computer { record, invitation })
        })
        .collect()
}

/// `None` when the file does not exist.
fn read_limited(path: &Path, limit: u64) -> Result<Option<Vec<u8>>, CtlError> {
    let io = |failure: std::io::Error| CtlError::io(failure.to_string());
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.len() > limit => Err(unreadable()),
        Ok(_) => std::fs::read(path).map(Some).map_err(io),
        Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(failure) => Err(io(failure)),
    }
}

fn support(platform: Platform) -> Value {
    match platform {
        Platform::Dpapi => json!({"available": true, "keystore": false}),
        Platform::Keystore => json!({"available": true, "keystore": true}),
        Platform::Unsupported => {
            json!({"available": false, "reason": UNSUPPORTED, "keystore": false})
        }
    }
}

/// Deletes the vault file, and with it every remembered computer. What was
/// read of it is dropped first, so the next op reads the file again whether
/// or not it is gone; on Android the data key stays installed.
fn reset(slot: &mut Option<Vault>, directory: &Path, platform: Platform) -> Result<(), CtlError> {
    let file = match platform {
        Platform::Dpapi => DPAPI_FILE,
        Platform::Keystore => SEALED_FILE,
        Platform::Unsupported => return Err(CtlError::invalid_argument(UNSUPPORTED)),
    };
    if let Some(vault) = slot.as_mut() {
        vault.computers = None;
    }
    match std::fs::remove_file(directory.join(file)) {
        Ok(()) => tracing::warn!("remembered computers were reset"),
        Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => {}
        Err(failure) => return Err(CtlError::io(failure.to_string())),
    }
    Ok(())
}

/// The vault for `directory`, opening it if this platform needs no unlock.
fn open<'a>(
    slot: &'a mut Option<Vault>,
    directory: &Path,
    platform: Platform,
) -> Result<&'a mut Vault, CtlError> {
    match platform {
        Platform::Unsupported => return Err(CtlError::invalid_argument(UNSUPPORTED)),
        Platform::Dpapi
            if slot
                .as_ref()
                .is_none_or(|vault| vault.directory != directory) =>
        {
            *slot = Some(Vault::new(directory, Protection::Dpapi));
        }
        _ => {}
    }
    slot.as_mut()
        .filter(|vault| vault.directory == directory)
        .ok_or_else(|| CtlError::vault_unavailable("the remembered computers are locked"))
}

/// Installs the data key Dart unwrapped. A new key comes with its wrapped
/// form, which is kept beside the vault.
fn unlock(directory: &Path, key: &str, wrapped: Option<&str>) -> Result<Vault, CtlError> {
    let key = STANDARD
        .decode(key)
        .ok()
        .filter(|key| key.len() == 32)
        .and_then(|key| UnboundKey::new(&AES_256_GCM, &key).ok())
        .ok_or_else(|| CtlError::invalid_argument("the vault key must be 32 bytes"))?;
    let wrapped = wrapped
        .map(|wrapped| {
            STANDARD
                .decode(wrapped)
                .ok()
                .filter(|bytes| !bytes.is_empty() && bytes.len() as u64 <= KEY_LIMIT)
                .ok_or_else(|| CtlError::invalid_argument("invalid wrapped vault key"))
        })
        .transpose()?;
    if let Some(wrapped) = wrapped {
        write_private(&directory.join(KEY_FILE), &wrapped)?;
    }
    Ok(Vault::new(
        directory,
        Protection::Sealed(Box::new(LessSafeKey::new(key))),
    ))
}

fn apply(
    slot: &mut Option<Vault>,
    directory: &Path,
    platform: Platform,
    current: Option<Invitation>,
    request: Request,
) -> Result<Value, CtlError> {
    let keystore = || {
        (platform == Platform::Keystore)
            .then_some(())
            .ok_or_else(|| CtlError::invalid_argument("this platform keeps no vault key"))
    };
    match request {
        Request::Support {} => Ok(support(platform)),
        Request::Key {} => {
            keystore()?;
            let wrapped = read_limited(&directory.join(KEY_FILE), KEY_LIMIT)?;
            Ok(json!({"wrapped": wrapped.map(|bytes| STANDARD.encode(bytes))}))
        }
        Request::Unlock { key, wrapped } => {
            keystore()?;
            *slot = Some(unlock(directory, &key, wrapped.as_deref())?);
            Ok(json!({}))
        }
        Request::Reset {} => {
            reset(slot, directory, platform)?;
            Ok(json!({}))
        }
        Request::List {} => open(slot, directory, platform)?.list(),
        Request::Remember {
            id,
            name,
            service,
            fingerprint,
            relay,
            namespace,
            relay_only,
            quality,
            resolution,
            max_fps,
            display_mode,
            replace_id,
        } => {
            let record = Record {
                id,
                name,
                service,
                fingerprint,
                relay,
                namespace,
                relay_only,
                quality,
                resolution,
                max_fps,
                display_mode,
                last_connected_ms: None,
                last_path: None,
                last_control_allowed: None,
            };
            open(slot, directory, platform)?.remember(record, replace_id, current.as_ref())
        }
        Request::Update {
            id,
            name,
            quality,
            resolution,
            display_mode,
            max_fps,
            last_connected_ms,
            last_path,
            last_control_allowed,
        } => open(slot, directory, platform)?.update(&id, |record| {
            if let Some(name) = name {
                record.name = name;
            }
            if let Some(quality) = quality {
                record.quality = quality;
            }
            if let Some(resolution) = resolution {
                record.resolution = resolution;
            }
            if let Some(display_mode) = display_mode {
                record.display_mode = display_mode;
            }
            if let Some(max_fps) = max_fps {
                record.max_fps = max_fps;
            }
            if last_connected_ms.is_some() {
                record.last_connected_ms = last_connected_ms;
            }
            if last_path.is_some() {
                record.last_path = last_path;
            }
            if last_control_allowed.is_some() {
                record.last_control_allowed = last_control_allowed;
            }
        }),
        Request::Forget { id } => {
            open(slot, directory, platform)?.forget(&id)?;
            Ok(json!({}))
        }
    }
}

/// The `computers_*` ops of `pb_mapper_desktop_call_json`.
pub(crate) async fn call(handle: &PbMapperHandle, request: Value) -> Result<Value, CtlError> {
    let request: Request = serde_json::from_value(request)
        .map_err(|_| CtlError::invalid_argument("invalid remembered computers request"))?;
    let directory = handle.state.lock().await.desktop_config_dir();
    let current = handle.desktop.current_invitation();
    tokio::task::spawn_blocking(move || {
        apply(&mut VAULT.lock(), &directory, PLATFORM, current, request)
    })
    .await
    .map_err(|failure| CtlError::internal(failure.to_string()))?
}

/// The invitation remembered under `id`, for `connect`. A locked or
/// unreadable vault is `VAULT_UNAVAILABLE`, never a retryable failure.
pub(crate) async fn invitation_for(
    handle: &PbMapperHandle,
    id: &str,
) -> Result<Option<Invitation>, CtlError> {
    let directory = handle.state.lock().await.desktop_config_dir();
    let id = id.to_owned();
    tokio::task::spawn_blocking(move || lookup(&mut VAULT.lock(), &directory, PLATFORM, &id))
        .await
        .map_err(|failure| CtlError::internal(failure.to_string()))?
}

fn lookup(
    slot: &mut Option<Vault>,
    directory: &Path,
    platform: Platform,
    id: &str,
) -> Result<Option<Invitation>, CtlError> {
    open(slot, directory, platform)
        .and_then(|vault| vault.invitation(id))
        .map_err(|failure| CtlError::vault_unavailable(failure.to_string()))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;
    use pb_mapper::desktop::HostIdentity;

    const KEY: [u8; 32] = [7; 32];

    fn request(value: Value) -> Request {
        serde_json::from_value(value).unwrap()
    }

    fn remember(id: &str, service: &str, fingerprint: &str) -> Request {
        request(
            json!({"op": "computers_remember", "id": id, "name": "Office",
            "service": service, "fingerprint": fingerprint, "relay": "relay:7666",
            "namespace": 3, "relay_only": false, "quality": "balanced"}),
        )
    }

    #[test]
    fn vault_round_trip_hides_secrets_and_rejects_tampering() {
        let directory =
            std::env::temp_dir().join(format!("pb-desktop-computers-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        let unlock = || {
            request(
                json!({"op": "computers_unlock", "key": STANDARD.encode(KEY),
                "wrapped": STANDARD.encode(b"wrapped by the keystore")}),
            )
        };
        let platform = Platform::Keystore;
        let mut slot = None;
        let current = HostIdentity::generate()
            .unwrap()
            .invitation("desktop:office".into())
            .unwrap();
        let fingerprint = current.short_fingerprint();
        let id = "0123456789abcdef0123456789abcdef";
        let run = |slot: &mut Option<Vault>, current: Option<&Invitation>, request: Request| {
            apply(slot, &directory, platform, current.cloned(), request)
        };

        // Locked until Dart hands over the unwrapped key.
        let locked = run(&mut slot, None, request(json!({"op": "computers_list"})));
        assert_eq!(locked.unwrap_err().code(), ErrorCode::VaultUnavailable);
        assert_eq!(
            lookup(&mut slot, &directory, platform, id)
                .unwrap_err()
                .code(),
            ErrorCode::VaultUnavailable
        );
        run(&mut slot, None, unlock()).unwrap();
        let wrapped = run(&mut slot, None, request(json!({"op": "computers_key"}))).unwrap();
        assert_eq!(
            wrapped["wrapped"],
            STANDARD.encode(b"wrapped by the keystore")
        );

        // Only the live session's own computer is remembered.
        for (session, service, print) in [
            (None, "desktop:office", fingerprint.as_str()),
            (Some(&current), "desktop:other", fingerprint.as_str()),
            (Some(&current), "desktop:office", "0000-0000-0000-0000"),
        ] {
            let refused = run(&mut slot, session, remember(id, service, print));
            assert_eq!(refused.unwrap_err().code(), ErrorCode::InvalidArgument);
        }
        assert!(!directory.join(SEALED_FILE).exists());

        let record = run(
            &mut slot,
            Some(&current),
            remember(id, "desktop:office", &fingerprint),
        )
        .unwrap();
        assert!(record.get("invitation").is_none());
        // Nothing chosen yet: no resolution, and Auto.
        assert_eq!(
            (record.get("resolution"), &record["max_fps"]),
            (None, &json!(0))
        );
        let renamed = request(json!({"op": "computers_update", "id": id, "name": "Desk",
            "last_connected_ms": 5, "resolution": "1440p", "max_fps": 120,
            "display_mode": "fit_local"}));
        run(&mut slot, None, renamed).unwrap();
        let unbounded = request(json!({"op": "computers_update", "id": id, "max_fps": 240}));
        let refused = run(&mut slot, None, unbounded);
        assert_eq!(refused.unwrap_err().code(), ErrorCode::InvalidArgument);
        let listed = run(&mut slot, None, request(json!({"op": "computers_list"}))).unwrap();
        assert_eq!(listed[0]["name"], "Desk");
        assert_eq!(
            (
                &listed[0]["resolution"],
                &listed[0]["max_fps"],
                &listed[0]["display_mode"]
            ),
            (&json!("1440p"), &json!(120), &json!("fit_local"))
        );
        assert!(listed[0].get("invitation").is_none());
        // Back to the default is a null; a field left out stays.
        let reset = request(json!({"op": "computers_update", "id": id, "resolution": null}));
        let record = run(&mut slot, None, reset).unwrap();
        assert_eq!(
            (record.get("resolution"), &record["display_mode"]),
            (None, &json!("fit_local"))
        );

        // Version 1 wrote 1080p, its default, whether chosen or not: that,
        // or nothing, follows today's default; another size was chosen.
        // The vault is written back as version 2.
        let earlier = |resolution: Value| {
            let mut entry = json!({"id": id, "name": "Office", "service": "desktop:office",
                "fingerprint": fingerprint, "relay": "relay:7666", "namespace": 3,
                "relay_only": false, "quality": "balanced", "invitation": current.encode()});
            if !resolution.is_null() {
                entry["resolution"] = resolution;
            }
            entry
        };
        let v1 = json!({"version": 1, "computers":
            [earlier(json!("1080p")), earlier(Value::Null), earlier(json!("1440p"))]});
        let decoded = decode(&serde_json::to_vec(&v1).unwrap()).unwrap();
        let resolutions: Vec<_> = decoded.iter().map(|c| c.record.resolution).collect();
        assert_eq!(resolutions, [None, None, Some(Resolution::P1440)]);
        let rewritten: Value = serde_json::from_slice(&encode(&decoded).unwrap()).unwrap();
        assert_eq!(rewritten["version"], 2);
        assert!(rewritten["computers"][0].get("resolution").is_none());

        // The file holds neither the record nor the invitation in the clear.
        let sealed = std::fs::read(directory.join(SEALED_FILE)).unwrap();
        for secret in [b"desktop:office".as_slice(), current.encode().as_bytes()] {
            assert!(!sealed.windows(secret.len()).any(|bytes| bytes == secret));
        }

        // A new process unlocks with the same key and finds the invitation.
        let mut reopened = None;
        run(&mut reopened, None, unlock()).unwrap();
        let found = lookup(&mut reopened, &directory, platform, id)
            .unwrap()
            .unwrap();
        assert_eq!(found.encode(), current.encode());
        assert!(
            lookup(&mut reopened, &directory, platform, &"f".repeat(32))
                .unwrap()
                .is_none()
        );

        // One flipped byte and nothing decrypts.
        let mut tampered = sealed;
        let last = tampered.len() - 1;
        tampered[last] ^= 1;
        std::fs::write(directory.join(SEALED_FILE), tampered).unwrap();
        let mut reopened = None;
        run(&mut reopened, None, unlock()).unwrap();
        let failed = run(
            &mut reopened,
            None,
            request(json!({"op": "computers_list"})),
        );
        assert_eq!(failed.unwrap_err().code(), ErrorCode::Protocol);
        assert_eq!(
            lookup(&mut reopened, &directory, platform, id)
                .unwrap_err()
                .code(),
            ErrorCode::VaultUnavailable
        );

        // A reset starts it empty instead of locking it for good.
        run(
            &mut reopened,
            None,
            request(json!({"op": "computers_reset"})),
        )
        .unwrap();
        let listed = run(
            &mut reopened,
            None,
            request(json!({"op": "computers_list"})),
        )
        .unwrap();
        assert_eq!(listed, json!([]));
        std::fs::remove_dir_all(directory).unwrap();
    }

    /// DPAPI that can no longer open the file, as after a restored profile
    /// or a password reset, is a vault no op can read; only a reset leaves.
    #[test]
    fn an_unreadable_dpapi_vault_can_be_reset() {
        let directory =
            std::env::temp_dir().join(format!("pb-desktop-computers-reset-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join(DPAPI_FILE), b"sealed for someone else").unwrap();
        let mut slot = None;
        let mut run = |op: &str| {
            apply(
                &mut slot,
                &directory,
                Platform::Dpapi,
                None,
                request(json!({"op": op})),
            )
        };

        assert_eq!(
            run("computers_list").unwrap_err().code(),
            ErrorCode::Protocol
        );
        run("computers_reset").unwrap();
        assert_eq!(run("computers_list").unwrap(), json!([]));
        assert!(!directory.join(DPAPI_FILE).exists());
        std::fs::remove_dir_all(directory).unwrap();
    }
}
