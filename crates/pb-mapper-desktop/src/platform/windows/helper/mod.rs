//! Opt-in Windows service. The unprivileged UI manages explicitly authorized
//! capture and optional persistent hosting through protected local IPC. Each
//! capture worker is bound to the approved owner's current console login,
//! including its locked desktop; another login never inherits that grant.
//! There is no arbitrary command execution, file or clipboard API. The
//! backend it serves crosses the pipe through the platform-neutral `oop`.
pub mod background;
#[cfg(feature = "windows-helper")]
mod install;
mod os;
mod pipe;
#[cfg(feature = "windows-helper")]
mod process_acl;
mod restart;
#[cfg(feature = "windows-helper")]
mod service;
pub(crate) mod worker;

use crate::{
    BackendCaps, DisplaySelection, HostOs, Result, error,
    launch::{Launch, Launcher},
    oop::{client::RemoteBackend, ipc},
};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::net::windows::named_pipe::NamedPipeClient;

pub use os::{Action, request_installation};
pub use restart::prepare as prepare_application_restart;

#[cfg(feature = "windows-helper")]
pub(crate) fn record_input_failure(detail: &str) {
    os::record_service_event("input_failed", detail);
}

/// What a session through the helper can do when its worker does not
/// describe itself in time: it locks the workstation, as the helper always
/// can.
fn fallback_caps() -> BackendCaps {
    BackendCaps {
        lock: true,
        os: Some(HostOs::Windows),
        ..BackendCaps::default()
    }
}

/// Sessions captured by the helper, on the display `selection` names.
/// Refused before the host registers unless the helper is ready.
pub(crate) fn launch(selection: &DisplaySelection) -> Result<Arc<dyn Launch>> {
    status().ensure_ready()?;
    let display_id = super::desktop::resolve(selection)?.id;
    Ok(Arc::new(Launcher::new(
        move || backend(display_id),
        fallback_caps(),
    )))
}

/// Connect from the dedicated SDK capture worker. No automatic privilege
/// escalation or fallback: failure is observable at the viewer and local UI.
pub(crate) fn backend(display_id: u32) -> Result<RemoteBackend<NamedPipeClient>> {
    status().ensure_ready()?;
    let config = configuration()?;
    connect_backend(display_id, &config)
}

/// The background supervisor uses the same bounded IPC as the UI. The server
/// independently accepts only its own kernel-reported PID, never arbitrary
/// SYSTEM callers, and binds the worker to the approved owner's current login.
#[cfg(feature = "windows-helper")]
fn service_backend(display_id: u32) -> Result<RemoteBackend<NamedPipeClient>> {
    let config = configuration()?;
    if os::current_sid()? != "S-1-5-18"
        || os::session(std::process::id())? != 0
        || !os::same_path(&std::env::current_exe()?, &config.helper_executable)
    {
        return Err(error("background capture requires the installed service"));
    }
    connect_backend(display_id, &config)
}

fn connect_backend(
    display_id: u32,
    config: &Configuration,
) -> Result<RemoteBackend<NamedPipeClient>> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let pipe = {
        let _entered = runtime.enter();
        let pipe = pipe::connect(PIPE)?;
        os::verify_server(&pipe, &config.helper_executable, None)?;
        pipe
    };
    RemoteBackend::start(runtime, pipe, display_id, fallback_caps())
}

/// The helper IPC this build speaks. A helper installed by an older build
/// listens on other pipes, so it is offered an update instead of being used.
/// Any change to a request or a response bumps it, with both pipe names.
/// 3: `VideoFormat` carries the encoder's GOP. 4: `Request::Input` carries
/// the whole expected desktop. 5: the background host's `Options` carry
/// `lan_first`. 6: video may be H.265, `Request::H265` asks the worker
/// whether it encodes it, and an applied input answers with the pointer.
pub const HELPER_PROTOCOL: u32 = 6;

const SERVICE: &str = "PbMapperDesktop";
const PIPE: &str = r"\\.\pipe\pb-mapper.desktop-helper.v6";
const EXECUTABLE: &str = "pb-mapper-desktop-helper.exe";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Configuration {
    /// 1: written by protocol 1 helpers, which record no protocol. 2: records
    /// the protocol.
    schema: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    protocol: Option<u32>,
    owner_sid: String,
    ui_executable: PathBuf,
    helper_executable: PathBuf,
}
impl Configuration {
    #[cfg(feature = "windows-helper")]
    fn current(owner_sid: String, ui_executable: PathBuf, helper_executable: PathBuf) -> Self {
        Self {
            schema: 2,
            protocol: Some(HELPER_PROTOCOL),
            owner_sid,
            ui_executable,
            helper_executable,
        }
    }
    fn protocol(&self) -> u32 {
        self.protocol.unwrap_or(1)
    }
    fn parse(data: &[u8]) -> Result<Self> {
        if data.len() > 8192 {
            return Err(error("invalid helper installation"));
        }
        let config: Self = serde_json::from_slice(data).map_err(error)?;
        match (config.schema, config.protocol) {
            (1, None) | (2, Some(_)) => Ok(config),
            _ => Err(error("unsupported helper installation")),
        }
    }
}
fn configuration() -> Result<Configuration> {
    let path = os::install_root()?.join("installation.json");
    os::protected_file(&path)?;
    let config = Configuration::parse(&std::fs::read(path)?)?;
    os::protected_file(&config.helper_executable)?;
    os::protected_file(&config.ui_executable)?;
    Ok(config)
}

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
    static DIGESTS: std::sync::Mutex<Vec<Entry>> = std::sync::Mutex::new(Vec::new());
    let metadata = std::fs::metadata(path).ok()?;
    let (length, modified) = (metadata.len(), metadata.modified().ok()?);
    let cached = DIGESTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .find(|(cached, l, m, _)| cached == path && *l == length && *m == modified)
        .map(|entry| entry.3);
    if cached.is_some() {
        return cached;
    }
    let digest = sha256(std::fs::File::open(path).ok()?).ok()?;
    let mut digests = DIGESTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
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
impl Status {
    /// Validate the existing local authorization; this never installs, starts
    /// or elevates the helper on the caller's behalf.
    pub fn ensure_ready(&self) -> Result<()> {
        if self.update_required {
            return Err(error(
                "Update the Windows desktop service to use remote desktop",
            ));
        }
        if !self.installed {
            return Err(error(
                "Install the Windows desktop service on the host before sharing UAC or the lock screen",
            ));
        }
        if !self.running {
            return Err(error(
                "Enable the Windows desktop service on the host before sharing UAC or the lock screen",
            ));
        }
        if !self.authorized_application {
            return Err(error(
                "Open the protected installed application on the host to use full desktop access",
            ));
        }
        if !self.identity_verified {
            return Err(error(format!(
                "Windows desktop service identity could not be verified; install/update the helper: {}",
                self.message
            )));
        }
        Ok(())
    }
}

pub fn status() -> Status {
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
            Status {
                installed: true,
                running,
                startup_disabled,
                authorized_application: os::current_sid().is_ok_and(|sid| sid == config.owner_sid)
                    && std::env::current_exe()
                        .is_ok_and(|path| os::same_path(&path, &config.ui_executable)),
                identity_verified: identity.is_ok(),
                installed_application: Some(config.ui_executable),
                protocol,
                update_required: update_required(protocol, running, || pipe::exists(PIPE)),
                update_available: update_available(&config.helper_executable),
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

/// Entry point of the dedicated helper executable, never the Flutter process.
#[cfg(feature = "windows-helper")]
pub fn run() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [mode] if mode == "service" => service::run(),
        [mode, pipe, parent] if mode == "worker" => {
            worker::run(pipe, parent.parse().map_err(error)?)
        }
        [mode, parent] if mode == "restart-ui" => restart::run(parent.parse().map_err(error)?),
        [mode, source, sid] if mode == "install" => {
            install::install(&PathBuf::from(source), sid, false)
        }
        [mode, source, sid, then] if mode == "install" && then == "enable" => {
            install::install(&PathBuf::from(source), sid, true)
        }
        [mode] if mode == "enable" => install::enable(),
        [mode] if mode == "disable" => install::disable(),
        [mode] if mode == "uninstall" => install::uninstall(),
        _ => Err(error(
            "expected install <UI bundle> <authorized SID> [enable], service, enable, disable or uninstall",
        )),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod readiness_tests {
    use super::{Configuration, HELPER_PROTOCOL, Status, update_required};

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
        // refuses the background host's `lan_first`, and protocol 5 H.265:
        // all are outdated too.
        for outdated in [3, 4, 5] {
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
        assert!(status.ensure_ready().is_err());
        status.installed = true;
        assert!(status.ensure_ready().is_err());
        status.running = true;
        assert!(status.ensure_ready().is_err());
        status.authorized_application = true;
        assert!(status.ensure_ready().is_err());
        status.identity_verified = true;
        assert!(status.ensure_ready().is_ok());
        // An installation retained after Disable does not retain capture
        // readiness, even though its approved caller remains the same.
        status.running = false;
        status.startup_disabled = true;
        assert!(status.ensure_ready().is_err());
    }
}
