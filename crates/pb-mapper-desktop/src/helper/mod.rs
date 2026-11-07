//! Opt-in Windows service. The unprivileged UI manages explicitly authorized
//! capture and optional persistent hosting through protected local IPC. Each
//! capture worker is bound to the approved owner's current console login,
//! including its locked desktop; another login never inherits that grant.
//! There is no arbitrary command execution, file or clipboard API.
pub mod background;
mod client;
#[cfg(feature = "windows-helper")]
mod install;
mod ipc;
mod os;
#[cfg(feature = "windows-helper")]
mod process_acl;
mod restart;
#[cfg(feature = "windows-helper")]
mod service;
pub(crate) mod worker;

use crate::{Backend, DesktopContext, Input, Result, error};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub use client::backend;
#[cfg(feature = "windows-helper")]
pub(crate) use os::SessionIdentity;
pub use os::request_installation;
pub use restart::prepare as prepare_application_restart;

#[cfg(feature = "windows-helper")]
pub(crate) fn record_input_failure(detail: &str) {
    os::record_service_event("input_failed", detail);
}

/// The helper IPC this build speaks. A helper installed by an older build
/// listens on other pipes, so it is offered an update instead of being used.
pub const HELPER_PROTOCOL: u32 = 2;

const SERVICE: &str = "PbMapperDesktop";
const PIPE: &str = r"\\.\pipe\pb-mapper.desktop-helper.v2";
const EXECUTABLE: &str = "pb-mapper-desktop-helper.exe";
const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);
/// The longest a helper drain may wait for encoder output.
const DRAIN_WAIT_MAX_MS: u32 = 4;

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
                update_required: update_required(protocol, running, || ipc::exists(PIPE)),
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

#[derive(Serialize, Deserialize)]
#[serde(tag = "op", deny_unknown_fields, rename_all = "snake_case")]
enum Request {
    Start {
        display_id: u32,
    },
    Attach {
        display_id: u32,
        session: os::SessionIdentity,
    },
    // Struct variants throughout: serde ignores extra fields on an internally
    // tagged unit variant, which would undo `deny_unknown_fields`.
    Context {},
    Capture {},
    ConfigureVideo {
        limits: crate::VideoLimits,
        nominal_fps: u32,
    },
    /// Drain finished output, then submit one capture when `submit` is set.
    /// A drain alone may wait up to `wait_ms` (at most 4) for encoder output
    /// while units are pending. The helper never paces.
    Video {
        expected: DesktopContext,
        codec: crate::VideoCodec,
        submit: bool,
        keyframe: bool,
        rate: crate::VideoRate,
        wait_ms: u32,
    },
    Presented {
        generation: u64,
    },
    /// Includes `Input::Lock`. A `drag_start` press is held by the host's
    /// thread C before it gets here; the helper ignores the flag.
    Input {
        generation: u64,
        sequence: u64,
        input: Input,
    },
    Release {},
    Close {},
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Response {
    context: Option<DesktopContext>,
    error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    video: Option<crate::VideoFormat>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cursor: Option<crate::CursorPosition>,
    /// Units the worker's encoder holds, for the host's submit gate.
    pending: u32,
    /// In the `Start` response: the double-click time and box of the
    /// interactive session, read by the worker, which runs in it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    double_click: Option<crate::DoubleClickGuard>,
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
        [mode, source, sid] if mode == "install" => install::install(&PathBuf::from(source), sid),
        [mode] if mode == "enable" => install::enable(),
        [mode] if mode == "disable" => install::disable(),
        [mode] if mode == "uninstall" => install::uninstall(),
        _ => Err(error(
            "expected install <UI bundle> <authorized SID>, service, enable, disable or uninstall",
        )),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod readiness_tests {
    use super::{Configuration, HELPER_PROTOCOL, Status, update_required};

    #[test]
    fn readiness_requires_protocol_2() {
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
        let current = installation(r#""schema":2,"protocol":2,"#).unwrap();
        assert_eq!(current.protocol(), HELPER_PROTOCOL);
        assert!(!update_required(current.protocol(), true, || true));
        assert!(!update_required(current.protocol(), false, || false));
        // Running, but serving another protocol's pipe.
        assert!(update_required(current.protocol(), true, || false));
        assert!(update_required(3, true, || true));
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
