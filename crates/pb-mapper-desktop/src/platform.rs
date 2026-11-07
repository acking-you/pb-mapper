use crate::{Backend, Capabilities, Display, Result};

pub fn capabilities() -> Capabilities {
    Capabilities {
        // A Windows host encodes H.264 with Media Foundation.
        host: media_foundation(),
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
/// Whether Windows Media Foundation is installed: Windows N and KN without the
/// Media Feature Pack, and Windows Server without the feature, have no
/// mfplat.dll. The app delay-loads it so that it still starts there, and every
/// Media Foundation call must check this first: without the DLL, the first
/// call raises instead of failing. Always false off Windows.
pub fn media_foundation() -> bool {
    #[cfg(windows)]
    {
        static INSTALLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *INSTALLED.get_or_init(|| system_library(::windows::core::w!("mfplat.dll")))
    }
    #[cfg(not(windows))]
    {
        false
    }
}
/// Loads `name` from System32 only and keeps it loaded, so a delay-loaded
/// import of the same name binds to this copy.
#[cfg(windows)]
pub(crate) fn system_library(name: ::windows::core::PCWSTR) -> bool {
    use ::windows::Win32::System::LibraryLoader::{LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW};
    unsafe { LoadLibraryExW(name, None, LOAD_LIBRARY_SEARCH_SYSTEM32) }.is_ok()
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
