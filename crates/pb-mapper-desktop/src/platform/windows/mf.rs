//! Whether Windows Media Foundation is installed.

/// Whether Windows Media Foundation is installed: Windows N and KN without the
/// Media Feature Pack, and Windows Server without the feature, have no
/// mfplat.dll. The app delay-loads it so that it still starts there, and every
/// Media Foundation call must check this first: without the DLL, the first
/// call raises instead of failing.
pub fn media_foundation() -> bool {
    use ::windows::Win32::System::LibraryLoader::{LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW};
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
