//! Windows display modes, by device name (`\\.\DISPLAY1`): listed with
//! `EnumDisplaySettingsExW`, changed with `ChangeDisplaySettingsExW` only
//! dynamically (`dwFlags = 0`, after `CDS_TEST`), never with
//! `CDS_UPDATEREGISTRY` or `CDS_GLOBAL`: a mode a host set never outlives a
//! logoff or a reboot. A reset is a NULL DEVMODE, the mode in the registry,
//! which is the user's own and which no host here ever writes, then the
//! original dynamically where that differs. Every call runs per-monitor DPI
//! aware, so sizes are physical pixels. A change's scope is the login's
//! logon time and the monitor's device interface name: a dynamic mode ends
//! with the login, and a monitor plugged in later under the same
//! `\\.\DISPLAYn` starts at its own registry mode.

use super::{
    desktop::{DpiScope, display_lost},
    win32::{from_wide_nul, session_info, wide},
};
use crate::{
    Result, error,
    modes::{Mode, Setting},
    platform::DisplayModes,
};
use windows::{
    Win32::{
        Graphics::Gdi::{
            CDS_TEST, CDS_TYPE, ChangeDisplaySettingsExW, DEVMODE_FIELD_FLAGS, DEVMODEW,
            DISP_CHANGE_SUCCESSFUL, DISPLAY_DEVICEW, DM_BITSPERPEL, DM_DISPLAYFREQUENCY,
            DM_PELSHEIGHT, DM_PELSWIDTH, ENUM_CURRENT_SETTINGS, ENUM_DISPLAY_SETTINGS_FLAGS,
            ENUM_DISPLAY_SETTINGS_MODE, EnumDisplayDevicesW, EnumDisplaySettingsExW,
        },
        System::RemoteDesktop::ProcessIdToSessionId,
    },
    core::PCWSTR,
};

/// `EnumDisplayDevicesW`'s flag for a monitor's device interface name
/// (`WindowsAndMessaging`, a feature this crate does not otherwise need).
const EDD_GET_DEVICE_INTERFACE_NAME: u32 = 1;

/// No mode is offered below this.
const MIN_SIZE: (u32, u32) = (800, 600);
/// The most modes offered.
const MAX_MODES: usize = 64;
/// Modes enumerated at most: a driver lists a few hundred.
const MAX_ENUMERATED: u32 = 4096;
/// The change flags: dynamic, kept nowhere.
const DYNAMIC: CDS_TYPE = CDS_TYPE(0);

/// One `DEVMODEW` of a display, as [`filter_modes`] reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Raw {
    pub width: u32,
    pub height: u32,
    pub hz: u32,
    pub bpp: u32,
    pub orientation: u32,
}
impl Raw {
    fn setting(self) -> Setting {
        Setting {
            width: self.width,
            height: self.height,
            hz: self.hz,
            bpp: self.bpp,
        }
    }
}

/// What a viewer is offered of a display's modes `all`, which shows
/// `current`: the modes at its depth and orientation, one per size (at its
/// refresh rate where offered, else the fastest), none below 800x600,
/// largest first, at most 64.
pub(crate) fn filter_modes(all: &[Raw], current: &Raw) -> Vec<Raw> {
    let mut kept: Vec<Raw> = Vec::new();
    let offered = all.iter().filter(|mode| {
        mode.bpp == current.bpp
            && mode.orientation == current.orientation
            && mode.width >= MIN_SIZE.0
            && mode.height >= MIN_SIZE.1
    });
    for mode in offered {
        match kept
            .iter_mut()
            .find(|kept| (kept.width, kept.height) == (mode.width, mode.height))
        {
            Some(kept) if kept.hz != current.hz && (mode.hz == current.hz || mode.hz > kept.hz) => {
                *kept = *mode;
            }
            Some(_) => {}
            None => kept.push(*mode),
        }
    }
    let area = |mode: &Raw| u64::from(mode.width) * u64::from(mode.height);
    kept.sort_by(|a, b| area(b).cmp(&area(a)).then(b.width.cmp(&a.width)));
    kept.truncate(MAX_MODES);
    kept
}

/// The modes of the display a device name names.
pub(crate) struct WinModes;

impl WinModes {
    fn mode(device: &str, index: ENUM_DISPLAY_SETTINGS_MODE) -> Option<Raw> {
        let name = wide(device);
        let mut mode = DEVMODEW {
            dmSize: size_of::<DEVMODEW>() as u16,
            ..Default::default()
        };
        let found = unsafe {
            EnumDisplaySettingsExW(
                PCWSTR(name.as_ptr()),
                index,
                &mut mode,
                ENUM_DISPLAY_SETTINGS_FLAGS(0),
            )
        };
        // A display device's DEVMODE carries the display half of the union;
        // zeroed when nothing was found.
        let orientation = unsafe { mode.Anonymous1.Anonymous2.dmDisplayOrientation.0 };
        found.as_bool().then_some(Raw {
            width: mode.dmPelsWidth,
            height: mode.dmPelsHeight,
            hz: mode.dmDisplayFrequency,
            bpp: mode.dmBitsPerPel,
            orientation,
        })
    }

    fn current_raw(device: &str) -> Result<Raw> {
        Self::mode(device, ENUM_CURRENT_SETTINGS)
            .ok_or_else(|| display_lost("the display's mode cannot be read"))
    }

    fn offered(device: &str) -> Result<Vec<Raw>> {
        let current = Self::current_raw(device)?;
        let all: Vec<Raw> = (0..MAX_ENUMERATED)
            .map_while(|index| Self::mode(device, ENUM_DISPLAY_SETTINGS_MODE(index)))
            .collect();
        Ok(filter_modes(&all, &current))
    }

    /// `setting`, or the registry mode for `None`, with `flags`.
    fn change(device: &str, setting: Option<&Setting>, flags: CDS_TYPE) -> Result<()> {
        let name = wide(device);
        let mode = setting.map(|setting| {
            let mut fields = DM_PELSWIDTH | DM_PELSHEIGHT;
            if setting.bpp != 0 {
                fields |= DM_BITSPERPEL;
            }
            if setting.hz != 0 {
                fields |= DM_DISPLAYFREQUENCY;
            }
            DEVMODEW {
                dmSize: size_of::<DEVMODEW>() as u16,
                dmFields: DEVMODE_FIELD_FLAGS(fields.0),
                dmPelsWidth: setting.width,
                dmPelsHeight: setting.height,
                dmBitsPerPel: setting.bpp,
                dmDisplayFrequency: setting.hz,
                ..Default::default()
            }
        });
        let changed = unsafe {
            ChangeDisplaySettingsExW(
                PCWSTR(name.as_ptr()),
                mode.as_ref().map(std::ptr::from_ref),
                None,
                flags,
                None,
            )
        };
        if changed != DISP_CHANGE_SUCCESSFUL {
            return Err(error(format!(
                "the display refused the mode change ({})",
                changed.0
            )));
        }
        Ok(())
    }
}

impl DisplayModes for WinModes {
    const SUPPORTED: bool = true;
    fn available(&mut self, device: &str) -> bool {
        let _dpi = DpiScope::enter();
        Self::mode(device, ENUM_CURRENT_SETTINGS).is_some()
    }
    fn list(&mut self, device: &str) -> Result<Vec<Mode>> {
        let _dpi = DpiScope::enter();
        Ok(Self::offered(device)?
            .into_iter()
            .map(|mode| mode.setting().size())
            .collect())
    }
    fn current(&mut self, device: &str) -> Result<Setting> {
        let _dpi = DpiScope::enter();
        Ok(Self::current_raw(device)?.setting())
    }
    fn target(&mut self, device: &str, mode: Mode) -> Result<Setting> {
        let _dpi = DpiScope::enter();
        Self::offered(device)?
            .into_iter()
            .find(|offered| (offered.width, offered.height) == (mode.width, mode.height))
            .map(Raw::setting)
            .ok_or_else(|| error("the display does not offer that resolution"))
    }
    fn apply(&mut self, device: &str, setting: &Setting) -> Result<()> {
        let _dpi = DpiScope::enter();
        Self::change(device, Some(setting), CDS_TEST)?;
        Self::change(device, Some(setting), DYNAMIC)
    }
    fn reset(&mut self, device: &str, original: &Setting) -> Result<()> {
        let _dpi = DpiScope::enter();
        Self::change(device, None, DYNAMIC)?;
        let now = Self::current_raw(device)?.setting();
        if (now.width, now.height, now.hz) != (original.width, original.height, original.hz) {
            // The registry held another mode than the one this host found.
            Self::change(device, Some(original), DYNAMIC)?;
        }
        Ok(())
    }
    fn scope(&mut self, device: &str) -> Result<String> {
        Ok(format!(
            "logon {}; monitor {}",
            logon_time()?,
            monitor(device)?
        ))
    }
}

/// The device interface name of the monitor on `device`, which names that
/// monitor instance; `DisplayLost` when none is attached.
fn monitor(device: &str) -> Result<String> {
    let name = wide(device);
    let mut found = DISPLAY_DEVICEW {
        cb: size_of::<DISPLAY_DEVICEW>() as u32,
        ..Default::default()
    };
    let attached = unsafe {
        EnumDisplayDevicesW(
            PCWSTR(name.as_ptr()),
            0,
            &mut found,
            EDD_GET_DEVICE_INTERFACE_NAME,
        )
    };
    let id = from_wide_nul(&found.DeviceID);
    if !attached.as_bool() || id.is_empty() {
        return Err(display_lost("no monitor is attached to the display"));
    }
    Ok(id)
}

/// When this process's login began, which names that login: a session id
/// is reused after a logoff.
fn logon_time() -> Result<i64> {
    let mut session = 0;
    unsafe { ProcessIdToSessionId(std::process::id(), &mut session) }.map_err(error)?;
    session_info(session)?
        .map(|info| info.LogonTime)
        .ok_or_else(|| error("the login's start cannot be read"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(width: u32, height: u32, hz: u32, bpp: u32, orientation: u32) -> Raw {
        Raw {
            width,
            height,
            hz,
            bpp,
            orientation,
        }
    }

    /// A fixed driver list: duplicates by refresh, another depth, a rotated
    /// mode and sizes below 800x600 are left out, the current refresh rate
    /// is kept where a size offers it and the fastest otherwise, largest
    /// first, and never more than 64.
    #[test]
    fn the_offered_modes_are_one_per_size_at_the_current_depth_largest_first() {
        let current = raw(2560, 1440, 165, 32, 0);
        let all = [
            raw(1920, 1080, 60, 32, 0),
            raw(1920, 1080, 165, 32, 0),
            raw(1920, 1080, 144, 32, 0),
            raw(2560, 1440, 60, 32, 0),
            raw(2560, 1440, 165, 32, 0),
            raw(1280, 1024, 60, 32, 0),
            raw(1280, 1024, 75, 32, 0),
            raw(1600, 900, 60, 16, 0),
            raw(1440, 2560, 165, 32, 1),
            raw(800, 600, 60, 32, 0),
            raw(640, 480, 60, 32, 0),
            raw(1024, 576, 60, 32, 0),
        ];
        let offered = filter_modes(&all, &current);
        assert_eq!(
            offered,
            [
                raw(2560, 1440, 165, 32, 0),
                raw(1920, 1080, 165, 32, 0),
                raw(1280, 1024, 75, 32, 0),
                raw(800, 600, 60, 32, 0),
            ]
        );
        let many: Vec<Raw> = (0..100).map(|i| raw(800 + i, 600, 60, 32, 0)).collect();
        let capped = filter_modes(&many, &current);
        assert_eq!(capped.len(), MAX_MODES);
        assert_eq!(capped[0].width, 899);
    }
}
