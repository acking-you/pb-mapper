//! The probe's own Win32 view of the displays, independent of the product's,
//! so that what it reports checks the host rather than repeating it.
use pb_mapper_desktop::{Mode, displays};
use windows::{
    Win32::Graphics::Gdi::{
        CDS_TEST, CDS_TYPE, ChangeDisplaySettingsExW, DEVMODEW, DISP_CHANGE_SUCCESSFUL,
        DM_DISPLAYFREQUENCY, DM_PELSHEIGHT, DM_PELSWIDTH, ENUM_CURRENT_SETTINGS,
        ENUM_DISPLAY_SETTINGS_FLAGS, ENUM_DISPLAY_SETTINGS_MODE, EnumDisplaySettingsExW,
    },
    core::PCWSTR,
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// One display's mode, as Windows reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Shown {
    pub(crate) device: String,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) hz: u32,
    orientation: u32,
    pub(crate) primary: bool,
}
impl Shown {
    pub(crate) fn size(&self) -> Mode {
        Mode {
            width: self.width,
            height: self.height,
        }
    }
}
impl std::fmt::Display for Shown {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} {}x{} at {} Hz, orientation {}{}",
            self.device,
            self.width,
            self.height,
            self.hz,
            self.orientation * 90,
            if self.primary { ", primary" } else { "" }
        )
    }
}

pub(crate) fn print(label: &str, displays: &[Shown]) -> Result<()> {
    for display in displays {
        println!("{label}: {display}");
    }
    Ok(())
}

/// Every display's mode now.
pub(crate) fn every_display() -> Result<Vec<Shown>> {
    displays()?
        .into_iter()
        .map(|display| shown(&display.name))
        .collect()
}

/// The mode `device` shows now.
pub(crate) fn shown(device: &str) -> Result<Shown> {
    let display = displays()?
        .into_iter()
        .find(|display| display.name == device)
        .ok_or("the display is gone")?;
    let current = read(device, ENUM_CURRENT_SETTINGS).ok_or("the mode cannot be read")?;
    Ok(Shown {
        primary: (display.x, display.y) == (0, 0),
        ..current
    })
}

/// `device`'s mode at `index`: the current one, or the registry's.
pub(crate) fn read(device: &str, index: ENUM_DISPLAY_SETTINGS_MODE) -> Option<Shown> {
    let name: Vec<u16> = device.encode_utf16().chain(Some(0)).collect();
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
    // A display device's DEVMODE carries the display half of the union.
    let orientation = unsafe { mode.Anonymous1.Anonymous2.dmDisplayOrientation.0 };
    found.as_bool().then(|| Shown {
        device: device.to_owned(),
        width: mode.dmPelsWidth,
        height: mode.dmPelsHeight,
        hz: mode.dmDisplayFrequency,
        orientation,
        primary: false,
    })
}

/// `mode` (at `hz`, 0 for any) on `device` with `flags`, tested first as
/// the host's own path does; or for `None` the registry mode.
pub(crate) fn change(device: &str, mode: Option<(Mode, u32)>, flags: CDS_TYPE) -> Result<()> {
    let name: Vec<u16> = device.encode_utf16().chain(Some(0)).collect();
    let devmode = mode.map(|(mode, hz)| DEVMODEW {
        dmSize: size_of::<DEVMODEW>() as u16,
        dmFields: if hz == 0 {
            DM_PELSWIDTH | DM_PELSHEIGHT
        } else {
            DM_PELSWIDTH | DM_PELSHEIGHT | DM_DISPLAYFREQUENCY
        },
        dmPelsWidth: mode.width,
        dmPelsHeight: mode.height,
        dmDisplayFrequency: hz,
        ..Default::default()
    });
    let call = |flags| unsafe {
        ChangeDisplaySettingsExW(
            PCWSTR(name.as_ptr()),
            devmode.as_ref().map(std::ptr::from_ref),
            None,
            flags,
            None,
        )
    };
    if devmode.is_some() && call(CDS_TEST) != DISP_CHANGE_SUCCESSFUL {
        return Err("the display does not take that mode".into());
    }
    let changed = call(flags);
    if changed != DISP_CHANGE_SUCCESSFUL {
        return Err(format!("the display refused the change ({})", changed.0).into());
    }
    Ok(())
}
