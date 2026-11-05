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

const SERVICE: &str = "PbMapperDesktop";
const PIPE: &str = r"\\.\pipe\pb-mapper.desktop-helper.v1";
const EXECUTABLE: &str = "pb-mapper-desktop-helper.exe";
const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Configuration {
    schema: u32,
    owner_sid: String,
    ui_executable: PathBuf,
    helper_executable: PathBuf,
}
fn configuration() -> Result<Configuration> {
    let path = os::install_root()?.join("installation.json");
    os::protected_file(&path)?;
    let data = std::fs::read(path)?;
    if data.len() > 8192 {
        return Err(error("invalid helper installation"));
    }
    let config: Configuration = serde_json::from_slice(&data).map_err(error)?;
    if config.schema != 1 {
        return Err(error("unsupported helper installation"));
    }
    os::protected_file(&config.helper_executable)?;
    os::protected_file(&config.ui_executable)?;
    Ok(config)
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
    pub message: String,
}
impl Status {
    /// Validate the existing local authorization; this never installs, starts
    /// or elevates the helper on the caller's behalf.
    pub fn ensure_ready(&self) -> Result<()> {
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
            Status {
                installed: true,
                running,
                startup_disabled,
                authorized_application: os::current_sid().is_ok_and(|sid| sid == config.owner_sid)
                    && std::env::current_exe()
                        .is_ok_and(|path| os::same_path(&path, &config.ui_executable)),
                identity_verified: identity.is_ok(),
                installed_application: Some(config.ui_executable),
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
    Context,
    Capture,
    ConfigureVideo {
        preferences: crate::VideoPreferences,
    },
    CaptureVideo {
        expected: DesktopContext,
        codec: crate::VideoCodec,
        keyframe: bool,
        bitrate: u32,
    },
    Presented {
        generation: u64,
    },
    Input {
        generation: u64,
        sequence: u64,
        input: Input,
    },
    Release,
    Close,
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
mod readiness_tests {
    use super::Status;

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
