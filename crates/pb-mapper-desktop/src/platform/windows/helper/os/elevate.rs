//! The elevated installer, behind one consent prompt per action.
use super::{Handle, current_sid, install_root, protected_file, wide};
use crate::{Result, error, platform::windows::helper::EXECUTABLE};
use windows::{
    Win32::{
        Foundation::WAIT_OBJECT_0,
        System::Threading::{GetExitCodeProcess, WaitForSingleObject},
        UI::{Shell::*, WindowsAndMessaging::SW_HIDE},
    },
    core::PCWSTR,
};

/// What the elevated installer is asked to do, each behind one consent prompt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Install this bundle, or update the installed helper in place. An update
    /// keeps the service's startup type and running state, stopped or Disabled
    /// included, unless `enable` restores automatic startup and starts it, as
    /// `Enable` does.
    Install {
        enable: bool,
    },
    Enable,
    Disable,
    Uninstall,
}

/// Launch a separate installer with the OS consent UI. No password or UAC
/// policy is handled here. The caller's process never gains administrator rights.
pub fn request_installation(action: Action) -> Result<()> {
    let executable = std::env::current_exe()?;
    let source = executable
        .parent()
        .ok_or_else(|| error("missing application directory"))?;
    let helper = source.join(EXECUTABLE);
    if !helper.is_file() {
        return Err(error("This build does not include the Windows helper"));
    }
    let params = match action {
        Action::Install { enable } => format!(
            "install \"{}\" {}{}",
            source.display(),
            current_sid()?,
            if enable { " enable" } else { "" }
        ),
        Action::Enable => "enable".into(),
        Action::Disable => "disable".into(),
        Action::Uninstall => "uninstall".into(),
    };
    let file = wide(&helper);
    let parameters = wide(params);
    let verb = wide("runas");
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC,
        lpVerb: PCWSTR(verb.as_ptr()),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(parameters.as_ptr()),
        nShow: SW_HIDE.0,
        ..Default::default()
    };
    let started = std::time::SystemTime::now();
    unsafe { ShellExecuteExW(&mut info) }.map_err(error)?;
    let process = unsafe { Handle::own(info.hProcess) };
    if unsafe { WaitForSingleObject(process.raw(), 300_000) } != WAIT_OBJECT_0 {
        return Err(error("helper installation did not finish"));
    }
    let mut code = 1;
    unsafe { GetExitCodeProcess(process.raw(), &mut code) }.map_err(error)?;
    if code != 0 {
        // The log lives beside the protected installation, never at a caller-
        // supplied location that an elevated process could be tricked into writing.
        let log = install_root()?.join("installation.log");
        let detail = protected_file(&log).ok().and_then(|()| {
            let metadata = std::fs::metadata(&log).ok()?;
            if metadata.len() > 4096 || metadata.modified().ok()? < started {
                return None;
            }
            std::fs::read_to_string(log).ok()
        });
        return Err(error(detail.unwrap_or_else(|| {
            "helper installer failed before it could write its protected installation log".into()
        })));
    }
    Ok(())
}
