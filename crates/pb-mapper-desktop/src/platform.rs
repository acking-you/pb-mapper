use crate::{Backend, Capabilities, Display, Result};

pub fn capabilities() -> Capabilities {
    Capabilities {
        host: cfg!(windows),
        viewer: true,
        secure_desktop: false,
        mode: if cfg!(windows) {
            "windows_desktop"
        } else {
            "native_viewer"
        }
        .into(),
    }
}
pub fn displays() -> Result<Vec<Display>> {
    #[cfg(windows)]
    {
        windows::displays()
    }
    #[cfg(not(windows))]
    {
        Ok(Vec::new())
    }
}
/// Blocking native backend, for a dedicated capture/input worker. The current
/// platform must support hosting; this does not install or elevate a helper.
pub fn native_backend(id: u32) -> Result<Box<dyn Backend>> {
    #[cfg(windows)]
    {
        windows::create(id)
    }
    #[cfg(not(windows))]
    {
        let _ = id;
        Err(crate::error("Desktop hosting currently requires Windows"))
    }
}

#[cfg(windows)]
#[path = "windows.rs"]
pub(crate) mod windows;
