//! A higher GPU scheduling priority for the capture device while an encoder
//! built to ask for it streams (`EncoderOptions::gpu_priority`, off by
//! default: `video_bench --gpu-priority` measures it under a GPU-bound
//! load before a session asks for it). Behind a game or a 3D tool that
//! keeps the GPU busy, the capture copy, the video processor's conversion
//! and the encoder's input queue behind that app's work.
//!
//! The device's own GPU thread priority (`IDXGIDevice::SetGPUThreadPriority`)
//! goes to its highest, 7, which needs the privilege to raise a base
//! priority: the helper's SYSTEM worker has it, the app usually not, and a
//! refusal changes nothing. The process's GPU scheduling class goes to
//! `HIGH` (`D3DKMTSetProcessSchedulingPriorityClass`), counted across the
//! process's encoders as `timer::TimerResolution` counts timers: the first
//! guard reads the class and raises it, the last one puts it back unless
//! something else changed it meanwhile. Never `REALTIME`, which can starve
//! the desktop compositor, and never lower than it was. The outcome is
//! logged once per process.

use std::sync::atomic::{AtomicBool, Ordering};
use windows::{
    Wdk::Graphics::Direct3D::{
        D3DKMT_SCHEDULINGPRIORITYCLASS, D3DKMT_SCHEDULINGPRIORITYCLASS_HIGH,
        D3DKMTGetProcessSchedulingPriorityClass, D3DKMTSetProcessSchedulingPriorityClass,
    },
    Win32::{
        Graphics::{Direct3D11::ID3D11Device, Dxgi::IDXGIDevice},
        System::Threading::GetCurrentProcess,
    },
    core::Interface,
};

/// The highest `SetGPUThreadPriority` takes.
const DEVICE_PRIORITY: i32 = 7;

struct Held {
    guards: usize,
    /// The class to put back, once this process raised it.
    before: Option<D3DKMT_SCHEDULINGPRIORITYCLASS>,
}
static HELD: parking_lot::Mutex<Held> = parking_lot::const_mutex(Held {
    guards: 0,
    before: None,
});
static LOGGED: AtomicBool = AtomicBool::new(false);

/// The process's GPU scheduling class, where it reads.
fn class() -> Option<D3DKMT_SCHEDULINGPRIORITYCLASS> {
    let mut class = D3DKMT_SCHEDULINGPRIORITYCLASS::default();
    // SAFETY: the current process's pseudo-handle, and an out parameter to a
    // local.
    unsafe { D3DKMTGetProcessSchedulingPriorityClass(GetCurrentProcess(), &mut class) }
        .is_ok()
        .then_some(class)
}

fn set_class(class: D3DKMT_SCHEDULINGPRIORITYCLASS) -> bool {
    // SAFETY: the current process's pseudo-handle and a class by value.
    unsafe { D3DKMTSetProcessSchedulingPriorityClass(GetCurrentProcess(), class) }.is_ok()
}

/// A raised GPU priority for this process while it lives.
pub(crate) struct GpuPriority(());
impl GpuPriority {
    /// Raise `device`'s GPU thread priority and hold the process's GPU
    /// scheduling class at `HIGH` at least. Every call may fail harmlessly.
    pub(crate) fn hold(device: &ID3D11Device) -> Self {
        let device_refused = device
            .cast::<IDXGIDevice>()
            // SAFETY: a COM call on an interface this caller holds.
            .and_then(|dxgi| unsafe { dxgi.SetGPUThreadPriority(DEVICE_PRIORITY) })
            .err()
            .map(|failure| failure.code().0);
        let mut held = HELD.lock();
        held.guards += 1;
        if held.guards == 1 {
            let before = class();
            // Only ever up, and never past HIGH.
            let raised = before.is_some_and(|before| {
                before.0 < D3DKMT_SCHEDULINGPRIORITYCLASS_HIGH.0
                    && set_class(D3DKMT_SCHEDULINGPRIORITYCLASS_HIGH)
            });
            held.before = before.filter(|_| raised);
            if !LOGGED.swap(true, Ordering::Relaxed) {
                tracing::info!(
                    target: "pb_mapper_desktop::session",
                    event = "desktop_gpu_priority",
                    device_priority = device_refused.is_none(),
                    device_refused = ?device_refused,
                    class_before = ?before.map(|class| class.0),
                    class_raised = raised,
                );
            }
        }
        Self(())
    }
}
impl Drop for GpuPriority {
    fn drop(&mut self) {
        let mut held = HELD.lock();
        held.guards = held.guards.saturating_sub(1);
        if held.guards > 0 {
            return;
        }
        // Back to what it was, unless something else set a class since.
        if let Some(before) = held.before.take()
            && class() == Some(D3DKMT_SCHEDULINGPRIORITYCLASS_HIGH)
        {
            set_class(before);
        }
    }
}
