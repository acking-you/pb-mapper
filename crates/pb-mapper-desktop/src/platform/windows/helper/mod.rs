//! Opt-in Windows service. The unprivileged UI manages explicitly authorized
//! capture and optional persistent hosting through protected local IPC. Each
//! capture worker is bound to the approved owner's current console login,
//! including its locked desktop; another login never inherits that grant.
//! There is no arbitrary command execution, file or clipboard API. The
//! backend it serves crosses the pipe through the platform-neutral `oop`.
pub mod background;
mod config;
#[cfg(feature = "windows-helper")]
mod install;
mod os;
mod pipe;
#[cfg(feature = "windows-helper")]
mod process_acl;
mod readiness;
mod restart;
#[cfg(feature = "windows-helper")]
mod service;
pub(crate) mod worker;

use crate::{
    BackendCaps, DisplaySelection, HostOs, Result, error,
    launch::{Launch, Launcher},
    oop::{client::RemoteBackend, ipc},
};
use config::{Configuration, configuration};
#[cfg(feature = "windows-helper")]
use std::path::PathBuf;
use std::sync::Arc;
use tokio::net::windows::named_pipe::NamedPipeClient;

pub use os::{Action, request_installation};
pub use readiness::{NotReady, Status, status};
pub use restart::prepare as prepare_application_restart;

#[cfg(feature = "windows-helper")]
pub(crate) fn record_input_failure(detail: &str) {
    os::record_service_event("input_failed", detail);
}

/// What a session through the helper can do until its worker's `Start`
/// answer says: it locks the workstation, as the helper always can.
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
    let display = super::desktop::resolve(selection)?;
    let device = (!display.name.is_empty()).then_some(display.name);
    Ok(Arc::new(Launcher::new(
        move || backend(display.id, device.clone()),
        fallback_caps(),
    )))
}

/// Connect from the dedicated SDK capture worker. No automatic privilege
/// escalation or fallback: failure is observable at the viewer and local UI.
pub(crate) fn backend(
    display_id: u32,
    display_device: Option<String>,
) -> Result<RemoteBackend<NamedPipeClient>> {
    status().ensure_ready()?;
    let config = configuration()?;
    connect_backend(display_id, display_device, &config)
}

/// The background supervisor uses the same bounded IPC as the UI. The server
/// independently accepts only its own kernel-reported PID, never arbitrary
/// SYSTEM callers, and binds the worker to the approved owner's current login.
#[cfg(feature = "windows-helper")]
fn service_backend(
    display_id: u32,
    display_device: Option<String>,
) -> Result<RemoteBackend<NamedPipeClient>> {
    let config = configuration()?;
    if os::current_sid()? != "S-1-5-18"
        || os::session(std::process::id())? != 0
        || !os::same_path(&std::env::current_exe()?, &config.helper_executable)
    {
        return Err(error("background capture requires the installed service"));
    }
    connect_backend(display_id, display_device, &config)
}

fn connect_backend(
    display_id: u32,
    display_device: Option<String>,
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
    RemoteBackend::start(runtime, pipe, display_id, display_device, fallback_caps())
}

/// The helper IPC this build speaks. A helper installed by an older build
/// listens on other pipes, so it is offered an update instead of being used.
/// Any change to a request or a response bumps it, with both pipe names.
/// 3: `VideoFormat` carries the encoder's GOP. 4: `Request::Input` carries
/// the whole expected desktop. 5: the background host's `Options` carry
/// `lan_first`. 6: video may be H.265, `Request::H265` asks the worker
/// whether it encodes it, and an applied input answers with the pointer.
/// 7: display modes (`DisplayModes`, `SetDisplayMode`,
/// `RestoreDisplayMode`); `Start` and `Attach` name the display's device;
/// a response carries its failure's kind, and `Start` the worker's caps;
/// the background host's `Settings` keep the device and it restores on
/// request.
pub const HELPER_PROTOCOL: u32 = 7;

/// What a worker exits with when a display mode it changed is still
/// journalled: the service then starts a `restore` worker.
#[cfg(feature = "windows-helper")]
pub(crate) const RESTORE_PENDING: u32 = 3;

const SERVICE: &str = "PbMapperDesktop";
const PIPE: &str = r"\\.\pipe\pb-mapper.desktop-helper.v7";
const EXECUTABLE: &str = "pb-mapper-desktop-helper.exe";

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
        [mode, session] if mode == "restore" => worker::restore(session.parse().map_err(error)?),
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
