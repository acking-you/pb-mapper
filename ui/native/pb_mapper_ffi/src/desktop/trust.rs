//! One explicitly remembered host grant per application profile: the SDK's
//! `trust::TrustFile`, sealed with current-user Windows DPAPI, or on macOS
//! with a key in the login Keychain, holding the app's `Settings`. Relay
//! credentials are never copied into it. The remembered computers
//! (`desktop_computers.rs`) share its DPAPI call and its private file writes.
use super::err;
use crate::error::CtlError;
use pb_mapper::desktop::{
    self, HostIdentity,
    trust::{RelayScope, Remembered, Sealer, TrustFile},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub(super) fn supported() -> bool {
    cfg!(any(windows, target_os = "macos"))
}
#[derive(Clone)]
pub(super) struct Scope {
    pub directory: PathBuf,
    pub relay: String,
    pub namespace: u64,
}
impl Scope {
    fn relay(&self) -> RelayScope {
        RelayScope {
            relay: self.relay.clone(),
            namespace: self.namespace,
        }
    }
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
#[derive(Clone)]
pub(super) struct Saved {
    scope: RelayScope,
    pub settings: Settings,
    identity: HostIdentity,
    /// The device of `settings.display_id` when it was saved, which finds the
    /// display again when its id changed.
    display_device: Option<String>,
}
impl Saved {
    #[cfg(windows)]
    pub fn for_background(
        scope: &Scope,
        service: String,
        display_id: u32,
        allow_control: bool,
        previous: Option<&Self>,
        live_identity: Option<&HostIdentity>,
    ) -> Result<Self, CtlError> {
        if !desktop::valid_service(&service) {
            return Err(CtlError::invalid_argument("invalid desktop service"));
        }
        let identity = if let Some(identity) = live_identity {
            identity.clone()
        } else if let Some(saved) =
            previous.filter(|saved| saved.matches(scope) && saved.settings.service == service)
        {
            saved.identity().clone()
        } else {
            HostIdentity::generate().map_err(err)?
        };
        Ok(Self::new(
            scope,
            Settings {
                service,
                display_id,
                allow_control,
                relay_only: false,
                privileged: true,
                auto_start: true,
                background: true,
            },
            &identity,
        ))
    }

    pub fn new(scope: &Scope, settings: Settings, identity: &HostIdentity) -> Self {
        let display_device = display_device(settings.display_id);
        Self {
            scope: scope.relay(),
            settings,
            identity: identity.clone(),
            display_device,
        }
    }
    pub fn matches(&self, scope: &Scope) -> bool {
        self.matches_relay(&scope.relay, scope.namespace)
    }
    /// Whether this grant is for `relay` and `namespace`.
    pub fn matches_relay(&self, relay: &str, namespace: u64) -> bool {
        self.scope.relay == relay && self.scope.namespace == namespace
    }
    pub fn identity(&self) -> &HostIdentity {
        &self.identity
    }
    /// `invitation` is the saved identity's own, offered only for the
    /// current relay and namespace.
    pub fn metadata(&self, invitation: Option<String>) -> Value {
        json!({"relay": self.scope.relay, "service": self.settings.service, "display_id": self.settings.display_id,
            "allow_control": self.settings.allow_control, "relay_only": self.settings.relay_only,
            "privileged": self.settings.privileged, "auto_start": self.settings.auto_start,
            "background": self.settings.background, "invitation": invitation})
    }
}
impl From<Remembered<Settings>> for Saved {
    fn from(remembered: Remembered<Settings>) -> Self {
        Self {
            scope: remembered.scope,
            settings: remembered.settings,
            identity: remembered.identity,
            display_device: remembered.display_device,
        }
    }
}

/// Current-user DPAPI with the trust file's own entropy, which has never
/// changed, so every file the app wrote stays readable.
#[cfg(windows)]
type Dpapi = desktop::trust::CurrentUser;
#[cfg(windows)]
type TrustSealer = Dpapi;
#[cfg(windows)]
const TRUST: TrustSealer = Dpapi::TRUST;
/// macOS: the login Keychain's key.
#[cfg(target_os = "macos")]
type TrustSealer = desktop::trust::Keychain;
#[cfg(target_os = "macos")]
const TRUST: TrustSealer = desktop::trust::Keychain::TRUST;
/// Where there is no DPAPI, nothing is sealed with it: a file found is
/// unreadable.
#[cfg(not(windows))]
struct Dpapi;
/// Where there is no sealer, nothing is remembered.
#[cfg(not(any(windows, target_os = "macos")))]
type TrustSealer = Dpapi;
#[cfg(not(any(windows, target_os = "macos")))]
const TRUST: TrustSealer = Dpapi;
#[cfg(not(windows))]
impl Sealer for Dpapi {
    fn seal(&self, _: &[u8]) -> desktop::Result<Vec<u8>> {
        Err(std::io::Error::other(
            "Windows DPAPI is not available on this platform",
        ))
    }
    fn open(&self, sealed: &[u8]) -> desktop::Result<Vec<u8>> {
        self.seal(sealed)
    }
}

fn file(directory: &Path) -> TrustFile<TrustSealer, Settings> {
    TrustFile::new(directory.to_path_buf(), TRUST)
}
/// An unreadable grant is `PROTOCOL`: reading it again does not mend it,
/// only revoking does. Any other failure is `IO`, the file system refusing
/// for now.
fn failure(failure: std::io::Error) -> CtlError {
    if failure.kind() == std::io::ErrorKind::InvalidData {
        CtlError::protocol(failure.to_string())
    } else {
        err(failure)
    }
}

/// The device name of the display `display_id` names now, which the service
/// and a remembered grant keep beside the id: a reboot or a mode change may
/// give the display another id, never another name.
pub(super) fn display_device(display_id: u32) -> Option<String> {
    desktop::displays()
        .ok()?
        .into_iter()
        .find(|display| display.id == display_id)
        .map(|display| display.name)
}

/// `None` when nothing is remembered. The display is the remembered
/// device's, under the id it has now.
pub(super) fn load(directory: &Path) -> Result<Option<Saved>, CtlError> {
    let Some(remembered) = file(directory).load().map_err(failure)? else {
        return Ok(None);
    };
    let service = &remembered.settings.service;
    if service.len() > 128 || !service.starts_with("desktop:") {
        return Err(CtlError::protocol("invalid saved desktop trust"));
    }
    let display_id = desktop::displays()
        .ok()
        .and_then(|displays| remembered.display_id(&displays));
    let mut saved = Saved::from(remembered);
    if let Some(display_id) = display_id {
        saved.settings.display_id = display_id;
    }
    Ok(Some(saved))
}
/// A display id that does not resolve now (a monitor undocked, or replugged
/// under a new id while sharing) keeps the device the stored grant names for
/// the same id, so `load` finds the display again once it is back.
pub(super) fn save(directory: &Path, saved: &Saved) -> Result<(), CtlError> {
    let file = file(directory);
    let display_device = saved.display_device.clone().or_else(|| {
        file.load()
            .ok()
            .flatten()
            .filter(|stored| stored.settings.display_id == saved.settings.display_id)
            .and_then(|stored| stored.display_device)
    });
    file.save(&Remembered {
        scope: saved.scope.clone(),
        settings: saved.settings.clone(),
        identity: saved.identity.clone(),
        display_device,
    })
    .map_err(err)
}
/// Replaces `path` with `bytes` all at once, so a crash leaves the old file
/// or the new one, never a torn one.
pub(crate) fn write_private(path: &Path, bytes: &[u8]) -> Result<(), CtlError> {
    desktop::trust::write_durably(path, bytes).map_err(err)
}
pub(super) fn revoke(directory: &Path) -> Result<(), CtlError> {
    file(directory).revoke().map_err(err)
}
/// Current-user DPAPI sealing `bytes`. `entropy` and `description` belong
/// to the data: the same entropy must be given to open it again.
pub(crate) fn dpapi_seal(
    bytes: &[u8],
    entropy: &'static [u8],
    description: &'static str,
) -> Result<Vec<u8>, CtlError> {
    dpapi(entropy, description).seal(bytes).map_err(err)
}
/// Current-user DPAPI opening what `dpapi_seal` sealed with `entropy`.
pub(crate) fn dpapi_open(
    sealed: &[u8],
    entropy: &'static [u8],
    description: &'static str,
) -> Result<Vec<u8>, CtlError> {
    dpapi(entropy, description).open(sealed).map_err(err)
}
#[cfg(windows)]
fn dpapi(entropy: &'static [u8], description: &'static str) -> Dpapi {
    Dpapi {
        entropy,
        description,
    }
}
#[cfg(not(windows))]
fn dpapi(_: &'static [u8], _: &'static str) -> Dpapi {
    Dpapi
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use crate::error::ErrorCode;
    use crate::test_support::TempRoot;

    fn path(directory: &Path) -> PathBuf {
        directory.join("desktop-trust.dpapi")
    }

    #[test]
    fn dpapi_round_trip_tampering_and_revocation() {
        let temp = TempRoot::new("desktop-trust");
        let root = temp.path().join("profile");
        let identity = HostIdentity::generate().unwrap();
        let scope = Scope {
            directory: root.clone(),
            relay: "test-relay:7666".into(),
            namespace: 1,
        };
        let settings = Settings {
            service: "desktop:trust-test".into(),
            display_id: 1,
            allow_control: false,
            relay_only: true,
            privileged: false,
            auto_start: false,
            background: false,
        };
        // A file as the app has always written it: this JSON, sealed with
        // these constants.
        let written = serde_json::to_vec(&json!({
            "schema": 1, "relay": scope.relay, "namespace": scope.namespace,
            "settings": settings, "identity": identity.encode_secret().unwrap(),
        }))
        .unwrap();
        let sealed = dpapi_seal(
            &written,
            b"pb-mapper/desktop-trust/v1",
            "pb-mapper desktop trust",
        )
        .unwrap();
        write_private(&path(&root), &sealed).unwrap();
        let loaded = load(&root).unwrap().unwrap();
        assert!(loaded.matches(&scope));
        assert_eq!(loaded.settings.service, "desktop:trust-test");

        let saved = Saved::new(&scope, settings, &identity);
        save(&root, &saved).unwrap();
        let loaded = load(&root).unwrap().unwrap();
        assert!(loaded.matches(&scope));
        assert!(!loaded.matches(&Scope {
            namespace: 2,
            ..scope.clone()
        }));
        assert_eq!(
            identity.encode_secret().unwrap(),
            loaded.identity().encode_secret().unwrap()
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
        assert_eq!(
            load(&root).err().map(|failure| failure.code()),
            Some(ErrorCode::Protocol)
        );
        revoke(&root).unwrap();
        assert!(load(&root).unwrap().is_none());
        std::fs::remove_dir(root).unwrap();
    }

    /// A grant the file system will not hand over yet is not an unreadable
    /// one: the app retries it rather than offering to revoke it.
    #[test]
    fn a_file_that_cannot_be_opened_is_io_not_unreadable() {
        let temp = TempRoot::new("desktop-trust-io");
        let root = temp.path().join("profile");
        std::fs::create_dir_all(path(&root)).unwrap();
        assert_eq!(
            load(&root).err().map(|failure| failure.code()),
            Some(ErrorCode::Io)
        );
    }

    /// A display that is not attached when a grant is saved again keeps the
    /// device name it had, so it is found again when it comes back.
    #[test]
    fn a_display_away_at_a_save_keeps_its_device() {
        let temp = TempRoot::new("desktop-trust-device");
        let root = temp.path().join("profile");
        let scope = Scope {
            directory: root.clone(),
            relay: "relay:7666".into(),
            namespace: 1,
        };
        let settings = Settings {
            service: "desktop:docked".into(),
            // No display has this id here.
            display_id: u32::MAX,
            allow_control: false,
            relay_only: false,
            privileged: false,
            auto_start: true,
            background: false,
        };
        let identity = HostIdentity::generate().unwrap();
        let docked = Saved {
            display_device: Some(r"\\.\DISPLAY99".into()),
            ..Saved::new(&scope, settings.clone(), &identity)
        };
        save(&root, &docked).unwrap();
        let undocked = Saved::new(&scope, settings, &identity);
        assert_eq!(undocked.display_device, None);
        save(&root, &undocked).unwrap();
        let stored = load(&root).unwrap().unwrap();
        assert_eq!(stored.display_device.as_deref(), Some(r"\\.\DISPLAY99"));
        // Another display chosen meanwhile names its own device, or none.
        let moved = Saved {
            settings: Settings {
                display_id: u32::MAX - 1,
                ..stored.settings.clone()
            },
            ..stored
        };
        save(
            &root,
            &Saved {
                display_device: None,
                ..moved
            },
        )
        .unwrap();
        assert_eq!(load(&root).unwrap().unwrap().display_device, None);
        revoke(&root).unwrap();
        std::fs::remove_dir(root).unwrap();
    }

    #[test]
    fn background_handoff_keeps_the_live_link_through_restart() {
        let temp = TempRoot::new("desktop-trust-handoff");
        let root = temp.path().join("profile");
        let scope = Scope {
            directory: root.clone(),
            relay: "relay:7666".into(),
            namespace: 1,
        };
        let identity = HostIdentity::generate().unwrap();
        let pending = Saved::for_background(
            &scope,
            "desktop:handoff".into(),
            1,
            true,
            None,
            Some(&identity),
        )
        .unwrap();
        // The same DPAPI file, between portable and installed processes,
        // carries the identity, owner scope and the one pending action.
        save(&root, &pending).unwrap();
        // Data protected for another purpose never decrypts as trust.
        assert!(
            dpapi_open(
                &std::fs::read(path(&root)).unwrap(),
                b"pb-mapper remembered computers",
                "pb-mapper desktop trust"
            )
            .is_err()
        );
        let restored = load(&root).unwrap().unwrap();
        assert!(restored.settings.background && restored.settings.auto_start);
        assert!(restored.settings.allow_control && restored.settings.privileged);
        assert!(!restored.settings.relay_only);
        // Enabling again view-only keeps the identity and records the choice.
        let resumed = Saved::for_background(
            &scope,
            "desktop:handoff".into(),
            2,
            false,
            Some(&restored),
            None,
        )
        .unwrap();
        assert!(!resumed.settings.allow_control);
        assert_eq!(
            resumed.identity().encode_secret().unwrap(),
            identity.encode_secret().unwrap()
        );
        let different = Saved::for_background(
            &Scope {
                namespace: 2,
                ..scope
            },
            "desktop:handoff".into(),
            1,
            true,
            Some(&restored),
            None,
        )
        .unwrap();
        assert_ne!(
            different.identity().encode_secret().unwrap(),
            identity.encode_secret().unwrap()
        );
        revoke(&root).unwrap();
        std::fs::remove_dir(root).unwrap();
    }
}
