//! A remembered host grant: one sealed file per directory holding the relay
//! scope, the app's settings and the host identity, readable by the files
//! the app has always written.
//!
//! A [`HostIdentity`] is a private key and a bearer grant: whoever holds it
//! can be the host its invitation names. [`TrustFile`] keeps one sealed by a
//! [`Sealer`] (on Windows, `CurrentUser` DPAPI, as the app does), bound to
//! the relay and namespace it was made for, beside the embedder's own
//! settings. Restarting with it keeps every saved invitation valid; revoking
//! it is deleting the file.
//!
//! ```no_run
//! # #[cfg(windows)]
//! # async fn example(client: &pb_mapper_client::sdk::Client) -> std::io::Result<()> {
//! use pb_mapper_desktop::{
//!     Host, HostOptions,
//!     trust::{CurrentUser, RelayScope, Remembered, TrustFile},
//! };
//!
//! let file = TrustFile::<_, String>::new("state".into(), CurrentUser::TRUST);
//! let scope = RelayScope {
//!     relay: client.server().into(),
//!     namespace: client.credential_namespace(),
//! };
//! let service = "desktop:office".to_string();
//! let mut builder = Host::builder(HostOptions::new(service.clone()));
//! if let Some(saved) = file.load()?.filter(|saved| saved.matches(&scope)) {
//!     builder = builder.identity(saved.identity);
//! }
//! let host = builder.start(client).await?;
//! file.save(&Remembered {
//!     scope,
//!     settings: service,
//!     identity: host.identity().clone(),
//!     display_device: None,
//! })?;
//! # Ok(())
//! # }
//! ```
use crate::{Display, HostIdentity, Result, error};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    io,
    marker::PhantomData,
    path::{Path, PathBuf},
};

/// The file a [`TrustFile`] keeps in its directory: named for the store of
/// the app's that it reads, whatever seals it.
const FILE: &str = "desktop-trust.dpapi";
/// The most a trust file holds, sealed or not.
const LIMIT: u64 = 64 * 1024;
/// The only record layout so far.
const SCHEMA: u32 = 1;

/// Seals what a [`TrustFile`] writes, and opens what it reads.
pub trait Sealer {
    fn seal(&self, plain: &[u8]) -> Result<Vec<u8>>;
    /// Fails for data another sealer, user or purpose sealed, and for data
    /// changed since.
    fn open(&self, sealed: &[u8]) -> Result<Vec<u8>>;
}

/// Current-user Windows DPAPI under fixed `entropy` and `description`: only
/// this Windows user, and only with the same entropy, opens what it seals.
#[cfg(windows)]
#[cfg_attr(docsrs, doc(cfg(windows)))]
#[derive(Clone, Copy, Debug)]
pub struct CurrentUser {
    pub entropy: &'static [u8],
    pub description: &'static str,
}
#[cfg(windows)]
impl CurrentUser {
    /// The app's own trust file's, so that file stays readable.
    pub const TRUST: Self = Self {
        entropy: b"pb-mapper/desktop-trust/v1",
        description: "pb-mapper desktop trust",
    };
    /// The most DPAPI takes or hands back; each store bounds its own data
    /// further.
    const LIMIT: u64 = 1024 * 1024;
    fn protect(&self, bytes: &[u8], encrypt: bool) -> Result<Vec<u8>> {
        use crate::windows::dpapi;
        dpapi::protect(
            bytes,
            encrypt,
            dpapi::Scope::CurrentUser,
            self.entropy,
            self.description,
            Self::LIMIT,
        )
    }
}
#[cfg(windows)]
impl Sealer for CurrentUser {
    fn seal(&self, plain: &[u8]) -> Result<Vec<u8>> {
        self.protect(plain, true)
    }
    fn open(&self, sealed: &[u8]) -> Result<Vec<u8>> {
        self.protect(sealed, false)
    }
}

/// Where a grant was made: a relay address and the credential's namespace
/// (`Client::credential_namespace`).
/// A grant is resumed only where it was made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelayScope {
    pub relay: String,
    pub namespace: u64,
}

/// A remembered grant, with the embedder's `settings` for it.
#[derive(Clone, Debug)]
pub struct Remembered<T> {
    pub scope: RelayScope,
    pub settings: T,
    pub identity: HostIdentity,
    /// The shared display's [`Display::name`], which survives what changes
    /// its id: see [`Self::display_id`].
    pub display_device: Option<String>,
}
impl<T> Remembered<T> {
    /// Made for this relay and namespace.
    pub fn matches(&self, scope: &RelayScope) -> bool {
        self.scope == *scope
    }
    /// The id the remembered display device has among `displays` now
    /// (`crate::displays()`), which a reboot or a mode change may have
    /// changed; `None` when none was remembered or it is not attached.
    pub fn display_id(&self, displays: &[Display]) -> Option<u32> {
        let device = self.display_device.as_deref()?;
        displays
            .iter()
            .find(|display| display.name == device)
            .map(|display| display.id)
    }
}

/// The JSON the app has always written, `display_device` added: an
/// existing file reads as one without it.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record<T> {
    schema: u32,
    relay: String,
    namespace: u64,
    settings: T,
    identity: Vec<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    display_device: Option<String>,
}

/// One remembered grant in a directory, sealed by `S`, with settings `T`.
pub struct TrustFile<S: Sealer, T> {
    directory: PathBuf,
    sealer: S,
    settings: PhantomData<fn() -> T>,
}
impl<S: Sealer, T: Serialize + DeserializeOwned> TrustFile<S, T> {
    pub fn new(directory: PathBuf, sealer: S) -> Self {
        Self {
            directory,
            sealer,
            settings: PhantomData,
        }
    }
    fn path(&self) -> PathBuf {
        self.directory.join(FILE)
    }
    /// `None` when nothing is remembered. A file that cannot be read as a
    /// grant (too large, not opened by this sealer, or invalid) fails with
    /// `ErrorKind::InvalidData`: reading it again does not mend it, only
    /// revoking does. Any other failure is the file system's, for now.
    pub fn load(&self) -> Result<Option<Remembered<T>>> {
        let invalid = |message: &str| io::Error::new(io::ErrorKind::InvalidData, message);
        let too_large = || invalid("stored desktop trust exceeds its size limit");
        let path = self.path();
        let metadata = match std::fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(failure) if failure.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(failure) => return Err(failure),
        };
        if metadata.len() > LIMIT {
            return Err(too_large());
        }
        let sealed = std::fs::read(&path)?;
        let plain = self.sealer.open(&sealed).map_err(|_| {
            invalid(
                "the remembered desktop grant cannot be unsealed here; revoke it and authorize a new invitation",
            )
        })?;
        if plain.len() as u64 > LIMIT {
            return Err(too_large());
        }
        let record: Record<T> =
            serde_json::from_slice(&plain).map_err(|_| invalid("invalid saved desktop trust"))?;
        if record.schema != SCHEMA
            || record.relay.len() > 1024
            || record
                .display_device
                .as_ref()
                .is_some_and(|device| device.len() > 1024)
        {
            return Err(invalid("invalid saved desktop trust"));
        }
        let identity = HostIdentity::decode_secret(&record.identity)
            .map_err(|_| invalid("invalid saved desktop trust"))?;
        Ok(Some(Remembered {
            scope: RelayScope {
                relay: record.relay,
                namespace: record.namespace,
            },
            settings: record.settings,
            identity,
            display_device: record.display_device,
        }))
    }
    /// Replaces what is remembered, all at once.
    pub fn save(&self, remembered: &Remembered<T>) -> Result<()> {
        let plain = serde_json::to_vec(&Record {
            schema: SCHEMA,
            relay: remembered.scope.relay.clone(),
            namespace: remembered.scope.namespace,
            settings: &remembered.settings,
            identity: remembered.identity.encode_secret()?,
            display_device: remembered.display_device.clone(),
        })
        .map_err(io::Error::other)?;
        if plain.len() as u64 > LIMIT {
            return Err(error("desktop trust exceeds its size limit"));
        }
        write_durably(&self.path(), &self.sealer.seal(&plain)?)
    }
    /// Forget the grant; nothing remembered is no failure.
    pub fn revoke(&self) -> Result<()> {
        match std::fs::remove_file(self.path()) {
            Err(failure) if failure.kind() != io::ErrorKind::NotFound => Err(failure),
            _ => Ok(()),
        }
    }
}

/// Replaces `path` with `bytes` all at once: a new file beside it, readable
/// by its owner alone where the platform has modes, synced, renamed over it
/// and its directory synced, so a crash leaves the old file or the new one,
/// never a torn one. The directory is created first.
///
/// The sequence is core's `durable_file::replace_contents`; its failures
/// arrive as the I/O error of the step that failed.
pub fn write_durably(path: &Path, bytes: &[u8]) -> Result<()> {
    use pb_mapper_core::durable_file::{NewFileMode, replace_contents};
    let (Some(directory), Some(_)) = (path.parent(), path.file_stem()) else {
        return Err(error("invalid private file path"));
    };
    std::fs::create_dir_all(directory)?;
    replace_contents(path, bytes, NewFileMode::Exactly(0o600)).map_err(io::Error::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pb_mapper_core::test_support::TempPath;

    /// XOR with a key byte and a tag: another key fails to open, as another
    /// user's DPAPI does.
    struct Xor(u8);
    impl Sealer for Xor {
        fn seal(&self, plain: &[u8]) -> Result<Vec<u8>> {
            let mut sealed: Vec<u8> = plain.iter().map(|byte| byte ^ self.0).collect();
            sealed.push(self.0);
            Ok(sealed)
        }
        fn open(&self, sealed: &[u8]) -> Result<Vec<u8>> {
            match sealed.split_last() {
                Some((&key, body)) if key == self.0 => Ok(body.iter().map(|b| b ^ key).collect()),
                _ => Err(error("sealed by another key")),
            }
        }
    }

    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Settings {
        service: String,
        display_id: u32,
    }

    fn display(id: u32, name: &str) -> Display {
        Display {
            id,
            name: name.into(),
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
        }
    }

    #[test]
    fn a_grant_round_trips_sealed_in_its_scope() {
        let directory = TempPath::new("trust");
        let file = TrustFile::<_, Settings>::new(directory.to_path_buf(), Xor(0x5a));
        assert!(file.load().unwrap().is_none());
        let scope = RelayScope {
            relay: "relay:7666".into(),
            namespace: 7,
        };
        let identity = HostIdentity::generate().unwrap();
        file.save(&Remembered {
            scope: scope.clone(),
            settings: Settings {
                service: "desktop:trust".into(),
                display_id: 2,
            },
            identity: identity.clone(),
            display_device: Some(r"\\.\DISPLAY2".into()),
        })
        .unwrap();
        let sealed = std::fs::read(directory.join(FILE)).unwrap();
        assert!(!sealed.windows(13).any(|bytes| bytes == b"desktop:trust"));

        let loaded = file.load().unwrap().unwrap();
        assert!(loaded.matches(&scope));
        assert!(!loaded.matches(&RelayScope {
            namespace: 8,
            ..scope.clone()
        }));
        assert!(!loaded.matches(&RelayScope {
            relay: "other:7666".into(),
            ..scope
        }));
        assert_eq!(loaded.settings.display_id, 2);
        assert_eq!(
            loaded.identity.encode_secret().unwrap(),
            identity.encode_secret().unwrap()
        );
        // The ids of a display may change; its device name does not.
        let now = [display(1, r"\\.\DISPLAY1"), display(3, r"\\.\DISPLAY2")];
        assert_eq!(loaded.display_id(&now), Some(3));
        assert_eq!(loaded.display_id(&now[..1]), None);

        // Another key cannot open it: unreadable, not a passing failure.
        let stranger = TrustFile::<_, Settings>::new(directory.to_path_buf(), Xor(0x33));
        assert_eq!(
            stranger.load().err().map(|failure| failure.kind()),
            Some(io::ErrorKind::InvalidData)
        );
        file.revoke().unwrap();
        file.revoke().unwrap();
        assert!(file.load().unwrap().is_none());
        // Revoking leaves nothing behind.
        assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 0);
    }

    /// The app has written trust files since before `display_device`.
    #[test]
    fn a_file_the_app_wrote_still_loads() {
        let directory = TempPath::new("trust-app");
        // The app's record as it serialises it, field for field.
        #[derive(Serialize)]
        struct Saved {
            schema: u32,
            relay: String,
            namespace: u64,
            settings: Settings,
            identity: Vec<u8>,
        }
        let written = serde_json::to_vec(&Saved {
            schema: 1,
            relay: "relay:7666".into(),
            namespace: 3,
            settings: Settings {
                service: "desktop:app".into(),
                display_id: 1,
            },
            identity: HostIdentity::generate().unwrap().encode_secret().unwrap(),
        })
        .unwrap();
        write_durably(&directory.join(FILE), &Xor(1).seal(&written).unwrap()).unwrap();
        let file = TrustFile::<_, Settings>::new(directory.to_path_buf(), Xor(1));
        let loaded = file.load().unwrap().unwrap();
        assert_eq!(loaded.settings.service, "desktop:app");
        assert_eq!(loaded.display_device, None);
        // Saved again unchanged, it is byte for byte what the app wrote.
        file.save(&loaded).unwrap();
        let sealed = std::fs::read(directory.join(FILE)).unwrap();
        assert_eq!(Xor(1).open(&sealed).unwrap(), written);
        file.revoke().unwrap();
        assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 0);
    }

    /// A write that cannot replace its file fails and leaves nothing beside it.
    #[test]
    fn a_failed_durable_write_leaves_no_temporary() {
        let directory = TempPath::new("trust-fails");
        let path = directory.join(FILE);
        std::fs::create_dir_all(path.join("kept")).unwrap();
        assert!(write_durably(&path, b"new").is_err());
        assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 1);
    }
}
