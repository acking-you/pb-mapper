use crate::{Backend, Capabilities, Display, Result};

pub fn capabilities() -> Capabilities {
    Capabilities {
        // A Windows host encodes H.264 with Media Foundation, and H.265 where
        // its GPU has an encoder for it (`Backend::encodes_h265`).
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
        use ::windows::Win32::System::LibraryLoader::{
            LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
        };
        static INSTALLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        // Loaded from System32 only and kept loaded, so the delay-loaded
        // imports bind to this copy.
        *INSTALLED.get_or_init(|| {
            unsafe {
                LoadLibraryExW(
                    ::windows::core::w!("mfplat.dll"),
                    None,
                    LOAD_LIBRARY_SEARCH_SYSTEM32,
                )
            }
            .is_ok()
        })
    }
    #[cfg(not(windows))]
    {
        false
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

/// The platform's own capture and input for `options`. Refused before the
/// host registers where there are none, so that no service is published only
/// to fail every viewer at its encoder.
pub(crate) fn native_launch(
    options: &crate::HostOptions,
) -> Result<std::sync::Arc<dyn crate::launch::Launch>> {
    if !capabilities().host {
        return Err(crate::error(if cfg!(windows) {
            "Desktop hosting requires Windows Media Foundation"
        } else {
            "Desktop hosting currently requires Windows"
        }));
    }
    #[cfg(windows)]
    {
        use crate::{Access, BackendCaps, DisplaySelection, HostOs, launch::Launcher};
        let id = match &options.display {
            DisplaySelection::Id(id) => *id,
            DisplaySelection::Primary => windows::primary()?.id,
            DisplaySelection::Device(name) => {
                displays()?
                    .into_iter()
                    .find(|display| &display.name == name)
                    .ok_or_else(|| crate::error("selected display is no longer available"))?
                    .id
            }
        };
        // The native desktop and the helper both lock the workstation.
        let fallback = BackendCaps {
            lock: true,
            os: Some(HostOs::Windows),
            ..BackendCaps::default()
        };
        Ok(match options.access {
            Access::Ordinary => {
                std::sync::Arc::new(Launcher::new(move || windows::create(id), fallback))
            }
            Access::Privileged => {
                crate::helper::status().ensure_ready()?;
                std::sync::Arc::new(Launcher::new(move || crate::helper::backend(id), fallback))
            }
        })
    }
    #[cfg(not(windows))]
    {
        let _ = options;
        Err(crate::error("Desktop hosting currently requires Windows"))
    }
}

#[cfg(windows)]
#[path = "windows.rs"]
pub(crate) mod windows;
