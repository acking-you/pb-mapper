//! Unelevated application handoff after an explicitly authorized installation.
//! The replacement does not open a window or claim configuration/IPC until the
//! old UI has exited. Failure to prepare leaves that UI running.
use super::{configuration, os};
use crate::{Result, error};
use std::process::Stdio;
use tokio::io::AsyncReadExt;

pub async fn prepare() -> Result<()> {
    let config = configuration()?;
    if os::current_sid()? != config.owner_sid {
        return Err(error(
            "only the authorized owner may restart this application",
        ));
    }
    // Installation can remove unlocked files from the old release. Launch the
    // new protected helper, never depend on the retired bundle surviving.
    let mut child = tokio::process::Command::new(config.helper_executable)
        .args(["restart-ui", &std::process::id().to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW; no elevation.
        .spawn()?;
    let ready = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        child
            .stdout
            .as_mut()
            .ok_or_else(|| error("missing restart acknowledgement pipe"))?
            .read_u8()
            .await
    })
    .await;
    if !matches!(ready, Ok(Ok(b'R'))) {
        let _ = child.kill().await;
        let _ = child.wait().await;
        return Err(error(
            "could not prepare the application restart; the current window remains open",
        ));
    }
    // The caller now exits through the app's normal shutdown path. The child
    // owns the original process handle, so PID reuse cannot release its wait.
    Ok(())
}

#[cfg(feature = "windows-helper")]
pub(super) fn run(parent: u32) -> Result<()> {
    use std::io::Write;
    let config = configuration()?;
    if os::current_sid()? != config.owner_sid {
        return Err(error(
            "only the authorized owner may restart this application",
        ));
    }
    wait_for_exit(parent, || {
        let mut stdout = std::io::stdout().lock();
        stdout.write_all(b"R")?;
        stdout.flush()
    })?;
    // Revalidate protected files and ownership after the wait; neither a
    // caller-supplied executable nor the elevated installer's token is used.
    let current = configuration()?;
    if current.owner_sid != config.owner_sid {
        return Err(error("application ownership changed during restart"));
    }
    std::process::Command::new(current.ui_executable)
        .arg("--desktop")
        .spawn()?;
    Ok(())
}

#[cfg(feature = "windows-helper")]
fn wait_for_exit(parent: u32, ready: impl FnOnce() -> Result<()>) -> Result<()> {
    use windows::Win32::{
        Foundation::WAIT_OBJECT_0,
        System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject},
    };
    let process = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, parent) }.map_err(error)?;
    let process = unsafe { os::Handle::own(process) };
    ready()?;
    if unsafe { WaitForSingleObject(process.raw(), 60_000) } != WAIT_OBJECT_0 {
        return Err(error("application did not exit; restart cancelled"));
    }
    Ok(())
}

#[cfg(all(test, feature = "windows-helper"))]
mod tests {
    use super::*;

    #[test]
    fn replacement_waits_for_the_original_process_and_requires_acknowledgement() {
        use std::os::windows::process::CommandExt;
        let mut parent = std::process::Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "$null = [Console]::ReadLine()",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(0x0800_0000)
            .spawn()
            .unwrap();
        let id = parent.id();
        // A failed ready signal must not enter the wait or launch anything.
        assert!(wait_for_exit(id, || Err(error("closed acknowledgement pipe"))).is_err());
        let (ready, acknowledged) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || {
            wait_for_exit(id, || {
                ready.send(()).unwrap();
                Ok(())
            })
        });
        acknowledged
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        assert!(parent.try_wait().unwrap().is_none());
        assert!(!waiter.is_finished());
        drop(parent.stdin.take());
        assert!(parent.wait().unwrap().success());
        waiter.join().unwrap().unwrap();
    }
}
