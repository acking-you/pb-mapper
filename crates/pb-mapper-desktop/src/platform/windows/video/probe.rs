//! Whether a display's GPU has a hardware H.265 encoder this pipeline can
//! use, probed once per adapter.
use super::{
    Runtime,
    codec::{Codec, EncoderSettings, luid_value},
    device_manager,
};
use crate::{Result, VideoCodec, error};
use std::time::Instant;
use windows::Win32::{
    Foundation::HMODULE,
    Graphics::{
        Direct3D::D3D_DRIVER_TYPE_UNKNOWN,
        Direct3D11::*,
        Dxgi::{CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory1},
    },
};

/// The adapter driving display `id` (its `HMONITOR`, as `Display::id` holds it).
fn display_adapter(id: u32) -> Result<IDXGIAdapter1> {
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1().map_err(error)? };
    for index in 0.. {
        let Ok(adapter) = (unsafe { factory.EnumAdapters1(index) }) else {
            break;
        };
        for output in 0.. {
            let Ok(output) = (unsafe { adapter.EnumOutputs(output) }) else {
                break;
            };
            let desc = unsafe { output.GetDesc().map_err(error)? };
            if desc.Monitor.0 as usize as u32 == id {
                return Ok(adapter);
            }
        }
    }
    Err(error("selected display is no longer available"))
}

/// What [`h265_available`] probed in this process, by adapter LUID.
static PROBED: parking_lot::Mutex<Vec<(u64, bool)>> = parking_lot::const_mutex(Vec::new());

/// The GPU that drives `display`, with its driver, as the helper's worker
/// names it to the host, which keeps the worker's H.265 answer under it:
/// the adapter's vendor, device, subsystem and revision, and the version of
/// its user-mode driver; with what this process's probe of that adapter
/// answered, once it probed it. `None` when no adapter drives the display,
/// or DXGI reports no driver version for it, so that the host asks again.
#[cfg(feature = "windows-helper")]
pub(crate) fn named_adapter(display: &crate::Display) -> Option<crate::oop::server::NamedAdapter> {
    use windows::{Win32::Graphics::Dxgi::IDXGIDevice, core::Interface};
    let adapter = display_adapter(display.id).ok()?;
    let desc = unsafe { adapter.GetDesc1() }.ok()?;
    // Asked of `IDXGIDevice`, as DXGI documents: the version of the
    // Direct3D 9 user-mode driver, which every component of a WDDM 2.3 or
    // later driver package shares. An adapter without one, such as a
    // driver that answers `DXGI_ERROR_UNSUPPORTED`, has no key.
    let driver = unsafe { adapter.CheckInterfaceSupport(&IDXGIDevice::IID) }.ok()?;
    let key = crate::oop::proto::AdapterKey::new(
        desc.VendorId,
        desc.DeviceId,
        desc.SubSysId,
        desc.Revision,
        u64::try_from(driver).ok()?,
    )?;
    let luid = luid_value(desc.AdapterLuid);
    let h265 = PROBED
        .lock()
        .iter()
        .find(|(probed, _)| *probed == luid)
        .map(|&(_, usable)| usable);
    Some(crate::oop::server::NamedAdapter { key, h265 })
}

/// Whether the GPU that drives display `id` has a hardware H.265 encoder this
/// pipeline can use: an MFT for that adapter that activates, takes the Main
/// 4:2:0 output and NV12 input types at 1920x1080@60 with B-frames off, and
/// starts streaming, without encoding a frame. Probed once per adapter LUID
/// and kept for the process; a display no adapter drives is not kept.
pub fn h265_available(id: u32) -> bool {
    let probe = || -> Result<bool> {
        let adapter = display_adapter(id)?;
        let luid = luid_value(unsafe { adapter.GetDesc1().map_err(error)? }.AdapterLuid);
        // Held while probing: one probe per adapter, however many ask at once.
        let mut probed = PROBED.lock();
        if let Some(&(_, usable)) = probed.iter().find(|(probed, _)| *probed == luid) {
            return Ok(usable);
        }
        let started = Instant::now();
        let probe = probe_h265(&adapter);
        tracing::info!(
            target: "pb_mapper_desktop::session",
            event = "desktop_h265_probe",
            usable = probe.is_ok(),
            ms = started.elapsed().as_millis() as u64,
            detail = ?probe.as_ref().err().map(ToString::to_string),
        );
        probed.push((luid, probe.is_ok()));
        Ok(probe.is_ok())
    };
    probe().unwrap_or_else(|failure| {
        tracing::info!(
            target: "pb_mapper_desktop::session",
            event = "desktop_h265_probe",
            usable = false,
            detail = ?failure.to_string(),
        );
        false
    })
}

fn probe_h265(adapter: &IDXGIAdapter1) -> Result<()> {
    let _runtime = Runtime::new()?;
    let mut device = None;
    let mut context = None;
    unsafe {
        D3D11CreateDevice(
            adapter,
            D3D_DRIVER_TYPE_UNKNOWN,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_VIDEO_SUPPORT | D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )
        .map_err(error)?;
    }
    let device = device.ok_or_else(|| error("missing D3D11 device"))?;
    let context = context.ok_or_else(|| error("missing D3D11 device context"))?;
    let manager = device_manager(&device, &context)?;
    Codec::new(
        &device,
        &manager,
        EncoderSettings {
            kind: VideoCodec::H265,
            width: 1920,
            height: 1080,
            fps: 60,
            bitrate: 8_000_000,
            options: super::EncoderOptions::default(),
        },
        // Nothing waits for the probe's events.
        None,
    )
    .map(drop)
}
