//! The desktop a Windows host captures and injects on: the ordinary desktop
//! of this process, checked before every use, or, in the privileged
//! helper's worker, the input desktop it is bound to; and the selected
//! display's geometry, under per-monitor DPI awareness.
use super::{helper::binding::DesktopBinding, win32};
use crate::{
    BackendError, DesktopKind, Display, DisplaySelection, FailureKind, Result, error,
    platform::Desktop,
};
use windows::Win32::{
    Graphics::Gdi::{GetMonitorInfoW, HMONITOR, MONITORINFOEXW},
    System::StationsAndDesktops::{CloseDesktop, DESKTOP_READOBJECTS, OpenInputDesktop},
    UI::HiDpi::{
        DPI_AWARENESS_CONTEXT, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        SetThreadDpiAwarenessContext,
    },
};

pub(crate) struct DpiScope(DPI_AWARENESS_CONTEXT);
impl DpiScope {
    pub(crate) fn enter() -> Self {
        Self(unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) })
    }
}
impl Drop for DpiScope {
    fn drop(&mut self) {
        unsafe {
            SetThreadDpiAwarenessContext(self.0);
        }
    }
}

pub(crate) fn available() -> Result<()> {
    // An ordinary process must never treat its cached Default desktop as live
    // while input belongs to Winlogon/UAC or another inaccessible desktop.
    let desk = unsafe { OpenInputDesktop(Default::default(), false, DESKTOP_READOBJECTS) }
        .map_err(|_| ordinary_desktop_unavailable())?;
    let name = win32::desktop_name(desk);
    unsafe {
        let _ = CloseDesktop(desk);
    }
    if !name?.eq_ignore_ascii_case("default") {
        return Err(ordinary_desktop_unavailable());
    }
    Ok(())
}
fn ordinary_desktop_unavailable() -> std::io::Error {
    BackendError::new(
        FailureKind::SecureDesktop,
        "secure_desktop_requires_helper: This sharing session uses ordinary desktop access. UAC and the lock screen require the authorized Windows service. Prepare full desktop access on the host and restart sharing; otherwise the picture resumes when the ordinary desktop returns.",
    )
    .into()
}
pub(crate) fn display_lost(message: &str) -> std::io::Error {
    BackendError::new(FailureKind::DisplayLost, message).into()
}
fn describe(monitor: &xcap::Monitor) -> Result<Display> {
    Ok(Display {
        id: monitor.id().map_err(error)?,
        name: monitor.name().map_err(error)?,
        x: monitor.x().map_err(error)?,
        y: monitor.y().map_err(error)?,
        width: monitor.width().map_err(error)?,
        height: monitor.height().map_err(error)?,
    })
}
fn monitor_handle(id: u32) -> Result<HMONITOR> {
    windows_capture::monitor::Monitor::enumerate()
        .map_err(error)?
        .into_iter()
        .find(|m| m.as_raw_hmonitor() as usize as u32 == id)
        .map(|m| HMONITOR(m.as_raw_hmonitor()))
        .ok_or_else(|| display_lost("selected display is no longer available"))
}
fn current_display(id: u32, monitor: HMONITOR) -> Result<Display> {
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
    unsafe { GetMonitorInfoW(monitor, (&mut info as *mut MONITORINFOEXW).cast()).ok() }
        .map_err(error)?;
    let rect = info.monitorInfo.rcMonitor;
    let width = u32::try_from(rect.right - rect.left).map_err(error)?;
    let height = u32::try_from(rect.bottom - rect.top).map_err(error)?;
    if width == 0 || height == 0 {
        return Err(display_lost("display is unavailable"));
    }
    Ok(Display {
        id,
        name: win32::from_wide_nul(&info.szDevice),
        x: rect.left,
        y: rect.top,
        width,
        height,
    })
}
pub(crate) fn displays() -> Result<Vec<Display>> {
    let _dpi = DpiScope::enter();
    available()?;
    xcap::Monitor::all()
        .map_err(error)?
        .iter()
        .map(describe)
        .collect()
}
/// The display `selection` names now. An id is taken as given, even when it
/// is stale, as hosting always took it: its sessions then report the display
/// lost.
pub(crate) fn resolve(selection: &DisplaySelection) -> Result<Display> {
    let _dpi = DpiScope::enter();
    match selection {
        DisplaySelection::Id(id) => Ok(monitor_handle(*id)
            .and_then(|monitor| current_display(*id, monitor))
            .unwrap_or_else(|_| Display {
                id: *id,
                name: String::new(),
                x: 0,
                y: 0,
                width: 0,
                height: 0,
            })),
        DisplaySelection::Primary => {
            let monitor = windows_capture::monitor::Monitor::primary().map_err(error)?;
            let handle = monitor.as_raw_hmonitor();
            current_display(handle as usize as u32, HMONITOR(handle))
        }
        DisplaySelection::Device(name) => {
            let (id, monitor) = by_name(name)
                .ok_or_else(|| display_lost("selected display is no longer available"))?;
            current_display(id, monitor)
        }
    }
}
/// The display whose device name is `name`, as its id and handle.
fn by_name(name: &str) -> Option<(u32, HMONITOR)> {
    windows_capture::monitor::Monitor::enumerate()
        .ok()?
        .into_iter()
        .map(|m| {
            (
                m.as_raw_hmonitor() as usize as u32,
                HMONITOR(m.as_raw_hmonitor()),
            )
        })
        .find(|(id, monitor)| current_display(*id, *monitor).is_ok_and(|d| d.name == name))
}

pub(crate) struct WinDesktop {
    id: u32,
    /// The device name (`\.\DISPLAY1`), which finds the display again when
    /// its handle goes stale.
    name: String,
    monitor: HMONITOR,
    /// The privileged worker's binding to the input desktop.
    binding: Option<DesktopBinding>,
}
impl WinDesktop {
    pub(crate) fn open(display: &Display) -> Result<Self> {
        let (id, monitor) = match monitor_handle(display.id) {
            Ok(monitor) => (display.id, monitor),
            Err(failure) => by_name(&display.name).ok_or(failure)?,
        };
        Ok(Self {
            id,
            name: display.name.clone(),
            monitor,
            binding: None,
        })
    }
    #[cfg(feature = "windows-helper")]
    /// The display of `id`, or, when that went stale, of the `device` name.
    pub(crate) fn bound(id: u32, device: Option<String>, binding: DesktopBinding) -> Result<Self> {
        let _dpi = DpiScope::enter();
        let (id, monitor) = match monitor_handle(id) {
            Ok(monitor) => (id, monitor),
            Err(failure) => device.as_deref().and_then(by_name).ok_or(failure)?,
        };
        let name = current_display(id, monitor)
            .map_or_else(|_| device.unwrap_or_default(), |display| display.name);
        Ok(Self {
            id,
            name,
            monitor,
            binding: Some(binding),
        })
    }
}
impl Desktop for WinDesktop {
    type Scope = DpiScope;
    fn scope(&self) -> DpiScope {
        DpiScope::enter()
    }
    fn prepare(&mut self, on_switch: impl FnOnce()) -> Result<()> {
        let Some(binding) = &mut self.binding else {
            // A secure desktop transition can invalidate DXGI duplication
            // without changing monitor geometry. Rebuild on return to the
            // ordinary desktop rather than retaining old encoder resources.
            return available().inspect_err(|_| on_switch());
        };
        // DXGI/MFT/COM resources belong to the old desktop thread. Destroy
        // them before binding, including a name change without an event.
        binding.enter(on_switch)
    }
    fn display(&mut self) -> Result<Display> {
        current_display(self.id, self.monitor).or_else(|failure| {
            // A mode change may recreate the monitor's handle; its device
            // name stays.
            let (id, monitor) = by_name(&self.name)
                .filter(|(id, _)| *id != self.id)
                .ok_or(failure)?;
            self.id = id;
            self.monitor = monitor;
            current_display(id, monitor)
        })
    }
    fn generation(&self) -> u64 {
        self.binding.as_ref().map_or(0, DesktopBinding::generation)
    }
    fn kind(&self) -> DesktopKind {
        self.binding
            .as_ref()
            .map_or(DesktopKind::Ordinary, DesktopBinding::kind)
    }
    fn changed(&self) -> bool {
        self.binding.as_ref().is_some_and(DesktopBinding::changed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A mode change may give a display another handle: one gone stale is
    /// found again by its device name. Reads monitor geometry only.
    #[test]
    fn a_stale_monitor_handle_is_found_again_by_its_device_name() {
        // No display to read: a service session, or a headless runner.
        let Ok(primary) = resolve(&DisplaySelection::Primary) else {
            return;
        };
        let mut stale = WinDesktop {
            id: 0,
            name: primary.name.clone(),
            monitor: HMONITOR(std::ptr::null_mut()),
            binding: None,
        };
        let _dpi = DpiScope::enter();
        assert_eq!(stale.display().unwrap(), primary);
        assert_eq!(stale.id, primary.id);
    }
}
