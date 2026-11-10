//! Win32 glue the Windows host and the helper share: wide strings, the names
//! and session records Windows hands back, and memory it allocates for the
//! caller to `LocalFree`.
use crate::{Result, error};
use std::{
    ffi::{OsStr, c_void},
    os::windows::ffi::OsStrExt,
};
use windows::{
    Win32::{
        Foundation::{HANDLE, HLOCAL, LocalFree},
        System::{
            RemoteDesktop::{
                WTSFreeMemory, WTSINFOEX_LEVEL1_W, WTSINFOEXW, WTSQuerySessionInformationW,
                WTSSessionInfoEx,
            },
            StationsAndDesktops::{GetUserObjectInformationW, HDESK, UOI_NAME},
        },
    },
    core::PWSTR,
};

/// `value` as a NUL-terminated wide string.
pub(crate) fn wide(value: impl AsRef<OsStr>) -> Vec<u16> {
    value.as_ref().encode_wide().chain(Some(0)).collect()
}

/// The text in `buffer` before its first NUL, or all of it.
pub(crate) fn from_wide_nul(buffer: &[u16]) -> String {
    let length = buffer.iter().position(|c| *c == 0).unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..length])
}

/// The name of `desktop`: `Default`, `Winlogon`, ...
pub(crate) fn desktop_name(desktop: HDESK) -> Result<String> {
    let mut name = [0u16; 256];
    unsafe {
        GetUserObjectInformationW(
            HANDLE(desktop.0),
            UOI_NAME,
            Some(name.as_mut_ptr().cast()),
            size_of_val(&name) as u32,
            None,
        )
    }
    .map_err(error)?;
    Ok(from_wide_nul(&name))
}

/// What Windows reports of terminal session `session`: `None` when its
/// answer is too short or not the level-1 record.
pub(crate) fn session_info(session: u32) -> Result<Option<WTSINFOEX_LEVEL1_W>> {
    let mut buffer = PWSTR::null();
    let mut len = 0;
    unsafe { WTSQuerySessionInformationW(None, session, WTSSessionInfoEx, &mut buffer, &mut len) }
        .map_err(error)?;
    let info = (len as usize >= size_of::<WTSINFOEXW>()).then(|| {
        // SAFETY: WTS answered at least a WTSINFOEXW; level 1 is the only
        // level it defines.
        let info = unsafe { &*buffer.0.cast::<WTSINFOEXW>() };
        (info.Level == 1).then_some(unsafe { info.Data.WTSInfoExLevel1 })
    });
    unsafe { WTSFreeMemory(buffer.0.cast::<c_void>()) };
    Ok(info.flatten())
}

/// Memory Windows allocated for its caller to free with `LocalFree`, freed
/// when this drops.
pub(crate) struct LocalAllocation(*mut c_void);
impl LocalAllocation {
    /// # Safety
    /// `pointer` is null, or memory the caller must free with `LocalFree`
    /// that nothing else frees.
    pub(crate) unsafe fn own(pointer: *mut c_void) -> Self {
        Self(pointer)
    }
}
impl Drop for LocalAllocation {
    fn drop(&mut self) {
        unsafe {
            let _ = LocalFree(Some(HLOCAL(self.0)));
        }
    }
}
