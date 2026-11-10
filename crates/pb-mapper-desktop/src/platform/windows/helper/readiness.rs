//! Whether the installed helper can be used now, and whether installing
//! from this application would update it.
use super::{EXECUTABLE, HELPER_PROTOCOL, PIPE, configuration, os, pipe};
use crate::{Result, error};
use serde::Serialize;
use std::path::{Path, PathBuf};

/// An installed helper must be updated before use when it speaks another
/// protocol, or when it runs without the pipe of this one.
fn update_required(protocol: u32, running: bool, pipe_present: impl FnOnce() -> bool) -> bool {
    protocol != HELPER_PROTOCOL || (running && !pipe_present())
}

/// Whether the `installed` helper is another build than the one bundled
/// beside this application, which installing would copy. A helper of this
/// protocol still works, but the background host runs in it, so a new build
/// reaches background sessions only once installed. False with no bundled
/// helper (a development run), when either cannot be read, and for the
/// installed application, whose bundled helper is the installed one.
fn update_available(installed: &Path) -> bool {
    let Some(bundled) = std::env::current_exe()
        .ok()
        .and_then(|executable| Some(executable.parent()?.join(EXECUTABLE)))
    else {
        return false;
    };
    !os::same_path(&bundled, installed) && other_build(digest(installed), digest(&bundled))
}

type Sha256 = [u8; 32];

/// Two helper executables are other builds when both were read and their
/// bytes differ.
fn other_build(installed: Option<Sha256>, bundled: Option<Sha256>) -> bool {
    matches!((installed, bundled), (Some(installed), Some(bundled)) if installed != bundled)
}

fn sha256(mut reader: impl std::io::Read) -> std::io::Result<Sha256> {
    let mut context = ring::digest::Context::new(&ring::digest::SHA256);
    let mut buffer = vec![0; 64 * 1024];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => context.update(&buffer[..read]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    let mut digest = [0; 32];
    digest.copy_from_slice(context.finish().as_ref());
    Ok(digest)
}

/// `path`'s SHA-256, read again only when its length or modification time
/// changes: a status poll costs a metadata read.
fn digest(path: &Path) -> Option<Sha256> {
    type Entry = (PathBuf, u64, std::time::SystemTime, Sha256);
    static DIGESTS: parking_lot::Mutex<Vec<Entry>> = parking_lot::Mutex::new(Vec::new());
    let metadata = std::fs::metadata(path).ok()?;
    let (length, modified) = (metadata.len(), metadata.modified().ok()?);
    let cached = DIGESTS
        .lock()
        .iter()
        .find(|(cached, l, m, _)| cached == path && *l == length && *m == modified)
        .map(|entry| entry.3);
    if cached.is_some() {
        return cached;
    }
    let digest = sha256(std::fs::File::open(path).ok()?).ok()?;
    let mut digests = DIGESTS.lock();
    digests.retain(|(cached, ..)| cached != path);
    digests.push((path.to_path_buf(), length, modified, digest));
    Some(digest)
}

#[derive(Default, Serialize)]
pub struct Status {
    pub installed: bool,
    pub running: bool,
    /// Only true after SCM positively reports both Disabled and Stopped; an
    /// unavailable or restarting service must not permit a second host.
    pub startup_disabled: bool,
    /// The installation names this Windows user as its owner: false when it
    /// names another user, or cannot be read. Only its owner can install
    /// over it.
    pub owned_by_user: bool,
    /// Owned by this user, and installed for this application's executable.
    pub authorized_application: bool,
    /// The live SCM process can be queried and matches the protected helper.
    pub identity_verified: bool,
    pub installed_application: Option<PathBuf>,
    /// The installed helper's IPC protocol; 0 when none can be read.
    pub protocol: u32,
    /// The installed helper is older than this build: installing again
    /// updates it in place, keeping its background hosting grant.
    pub update_required: bool,
    /// The installed helper is another build of this protocol: it still
    /// works, and installing again puts this build's host in the service.
    pub update_available: bool,
    pub message: String,
}
/// Why the helper cannot be used now, in the order [`Status::not_ready`]
/// checks it. Its `Display` is the sentence a person reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NotReady {
    /// The installed helper is older than this build.
    Outdated,
    NotInstalled,
    /// Installed but not running: stopped, Disabled, or restarting.
    Stopped,
    /// The helper serves another installed copy of the app for this user,
    /// which installing from this one takes over.
    ForeignApplication,
    /// The helper belongs to another Windows user, or its installation cannot
    /// be read: installing from this application is refused.
    OtherOwner,
    /// The running service cannot be shown to be the protected helper; the
    /// text says why.
    Unverified(String),
}

impl std::fmt::Display for NotReady {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Outdated => "Update the Windows desktop service to use remote desktop",
            Self::NotInstalled => {
                "Install the Windows desktop service on the host before sharing UAC or the lock screen"
            }
            Self::Stopped => {
                "Enable the Windows desktop service on the host before sharing UAC or the lock screen"
            }
            Self::ForeignApplication => {
                "Open the protected installed application on the host to use full desktop access"
            }
            Self::OtherOwner => {
                "The Windows desktop service belongs to another user, or its installation cannot be read"
            }
            Self::Unverified(message) => {
                return write!(
                    f,
                    "Windows desktop service identity could not be verified; install/update the helper: {message}"
                );
            }
        })
    }
}

impl Status {
    /// Why the existing local authorization cannot be used, if it cannot.
    pub fn not_ready(&self) -> Option<NotReady> {
        if self.update_required {
            Some(NotReady::Outdated)
        } else if !self.installed {
            Some(NotReady::NotInstalled)
        } else if !self.running {
            Some(NotReady::Stopped)
        } else if !self.authorized_application {
            Some(if self.owned_by_user {
                NotReady::ForeignApplication
            } else {
                NotReady::OtherOwner
            })
        } else if !self.identity_verified {
            Some(NotReady::Unverified(self.message.clone()))
        } else {
            None
        }
    }

    /// Validate the existing local authorization; this never installs, starts
    /// or elevates the helper on the caller's behalf.
    pub fn ensure_ready(&self) -> Result<()> {
        self.not_ready().map_or(Ok(()), |reason| Err(error(reason)))
    }
}

pub fn status() -> Status {
    status_with(true)
}

/// [`status`]'s [`Status::ensure_ready`], without the update check, which
/// readiness never reads.
pub(super) fn ensure_ready() -> Result<()> {
    status_with(false).ensure_ready()
}

/// [`status`], with `update_available` false unless `update_check` asks for
/// it: the check reads both helper executables, and hashes them after a
/// change.
pub(super) fn status_with(update_check: bool) -> Status {
    // A loaded service may outlive a damaged/unreadable installation.json.
    // Only an affirmative SCM "does not exist" permits treating it as absent.
    let (exists, running, startup_disabled) = match os::service_state() {
        Ok(Some((running, startup_disabled))) => (true, running, startup_disabled),
        Ok(None) => (false, false, false),
        Err(_) => (true, false, false),
    };
    match configuration() {
        Ok(config) => {
            let identity = if running {
                os::verify_running_service(&config.helper_executable)
            } else {
                Err(error("Windows desktop service is not running"))
            };
            let protocol = config.protocol();
            let owned_by_user = os::current_sid().is_ok_and(|sid| sid == config.owner_sid);
            Status {
                installed: true,
                running,
                startup_disabled,
                owned_by_user,
                authorized_application: owned_by_user
                    && std::env::current_exe()
                        .is_ok_and(|path| os::same_path(&path, &config.ui_executable)),
                identity_verified: identity.is_ok(),
                installed_application: Some(config.ui_executable),
                protocol,
                update_required: update_required(protocol, running, || pipe::exists(PIPE)),
                update_available: update_check && update_available(&config.helper_executable),
                message: identity.err().map_or_else(
                    || {
                        "Helper requires the installed application; the UI stays unprivileged"
                            .into()
                    },
                    |e| e.to_string(),
                ),
            }
        }
        Err(e) => Status {
            installed: exists,
            running,
            startup_disabled,
            message: e.to_string(),
            ..Default::default()
        },
    }
}

#[cfg(test)]
mod readiness_tests {
    use super::{HELPER_PROTOCOL, NotReady, Status, update_required};
    use crate::platform::windows::helper::Configuration;

    #[test]
    fn readiness_requires_the_current_protocol() {
        let installation = |extra: &str| {
            Configuration::parse(
                format!(
                    r#"{{{extra}"owner_sid":"S-1-5-21-1","ui_executable":"ui.exe","helper_executable":"helper.exe"}}"#
                )
                .as_bytes(),
            )
        };
        // A PR #51 installation is protocol 1, and survives a rollback intact.
        let old = installation(r#""schema":1,"#).unwrap();
        assert_eq!(old.protocol(), 1);
        assert!(!serde_json::to_string(&old).unwrap().contains("protocol"));
        assert!(update_required(old.protocol(), true, || true));
        assert!(update_required(old.protocol(), false, || false));
        // Protocol 3 sends input with its generation alone, protocol 4
        // refuses the background host's `lan_first`, protocol 5 H.265, and
        // protocol 6 display modes: all are outdated too.
        for outdated in [3, 4, 5, 6] {
            let previous = installation(&format!(r#""schema":2,"protocol":{outdated},"#)).unwrap();
            assert!(update_required(previous.protocol(), true, || true));
        }
        let current =
            installation(&format!(r#""schema":2,"protocol":{HELPER_PROTOCOL},"#)).unwrap();
        assert_eq!(current.protocol(), HELPER_PROTOCOL);
        assert!(!update_required(current.protocol(), true, || true));
        assert!(!update_required(current.protocol(), false, || false));
        // Running, but serving another protocol's pipe.
        assert!(update_required(current.protocol(), true, || false));
        assert!(update_required(HELPER_PROTOCOL + 1, true, || true));
        for invalid in [
            r#""schema":1,"protocol":1,"#,
            r#""schema":2,"#,
            r#""schema":3,"protocol":3,"#,
        ] {
            assert!(installation(invalid).is_err());
        }

        // An outdated helper is never used, however ready it is otherwise.
        let mut status = Status {
            installed: true,
            running: true,
            authorized_application: true,
            identity_verified: true,
            protocol: 1,
            update_required: true,
            ..Status::default()
        };
        assert!(status.ensure_ready().is_err());
        status.protocol = HELPER_PROTOCOL;
        status.update_required = false;
        assert!(status.ensure_ready().is_ok());
        // Another build of this protocol still works until it is updated.
        status.update_available = true;
        assert!(status.ensure_ready().is_ok());
    }

    #[test]
    fn another_build_of_the_helper_is_offered_an_update() {
        use super::{other_build, sha256};
        let digest = |bytes: &[u8]| sha256(bytes).ok();
        // Longer than one read, and differing only in its last byte.
        let installed = vec![7; 200_000];
        let mut bundled = installed.clone();
        *bundled.last_mut().unwrap() = 8;
        assert!(!other_build(digest(&installed), digest(&installed.clone())));
        assert!(other_build(digest(&installed), digest(&bundled)));
        // No bundled helper, or one that cannot be read, offers nothing.
        assert!(!other_build(digest(&installed), None));
        assert!(!other_build(None, digest(&bundled)));
    }

    #[test]
    fn capture_requires_installed_running_service_and_approved_caller() {
        let mut status = Status::default();
        assert_eq!(status.not_ready(), Some(NotReady::NotInstalled));
        assert!(status.ensure_ready().is_err());
        status.installed = true;
        assert_eq!(status.not_ready(), Some(NotReady::Stopped));
        status.running = true;
        assert_eq!(status.not_ready(), Some(NotReady::OtherOwner));
        status.owned_by_user = true;
        assert_eq!(status.not_ready(), Some(NotReady::ForeignApplication));
        status.authorized_application = true;
        assert!(matches!(status.not_ready(), Some(NotReady::Unverified(_))));
        status.identity_verified = true;
        assert_eq!(status.not_ready(), None);
        assert!(status.ensure_ready().is_ok());
        // An outdated helper comes first, whatever else holds.
        status.update_required = true;
        status.running = false;
        assert_eq!(status.not_ready(), Some(NotReady::Outdated));
        // An installation retained after Disable does not retain capture
        // readiness, even though its approved caller remains the same.
        status.update_required = false;
        status.startup_disabled = true;
        assert_eq!(status.not_ready(), Some(NotReady::Stopped));
    }
}
