//! A higher GPU scheduling priority for the capture device while an encoder
//! built to ask for it streams (`EncoderOptions::gpu_priority`, off by
//! default: `video_bench --gpu-priority` measures it under a GPU-bound
//! load before a session asks for it). Behind a game or a 3D tool that
//! keeps the GPU busy, the capture copy, the video processor's conversion
//! and the encoder's input queue behind that app's work.
//!
//! The device's own GPU thread priority (`IDXGIDevice::SetGPUThreadPriority`)
//! goes to its highest, 7, and the process's GPU scheduling class to `HIGH`
//! (`D3DKMTSetProcessSchedulingPriorityClass`). A positive device priority
//! needs the privilege to raise a base priority
//! (`SeIncreaseBasePriorityPrivilege`) enabled: the guard enables it for its
//! calls where this process's token holds it, as an elevated process's does,
//! and puts it back after them. An unelevated app's token does not hold it,
//! nor does the helper's capture worker's, which is restricted to no
//! privileges (`helper/child.rs`): there both calls are made without it.
//! What each call did is logged, and a refusal changes nothing.
//!
//! The class is counted across the process's encoders as
//! `timer::TimerResolution` counts timers: the first guard reads the class
//! and raises it, the last one puts it back unless something else changed
//! it meanwhile. Never `REALTIME`, which can starve the desktop compositor,
//! and never lower than it was. The outcome is logged once per process.

use std::{
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    sync::atomic::{AtomicBool, Ordering},
};
use windows::{
    Wdk::Graphics::Direct3D::{
        D3DKMT_SCHEDULINGPRIORITYCLASS, D3DKMT_SCHEDULINGPRIORITYCLASS_HIGH,
        D3DKMTGetProcessSchedulingPriorityClass, D3DKMTSetProcessSchedulingPriorityClass,
    },
    Win32::{
        Foundation::{GetLastError, HANDLE, LUID},
        Graphics::{Direct3D11::ID3D11Device, Dxgi::IDXGIDevice},
        Security::{
            AdjustTokenPrivileges, LUID_AND_ATTRIBUTES, LookupPrivilegeValueW,
            SE_INC_BASE_PRIORITY_NAME, SE_PRIVILEGE_ENABLED, TOKEN_ADJUST_PRIVILEGES,
            TOKEN_PRIVILEGES, TOKEN_QUERY,
        },
        System::Threading::{GetCurrentProcess, OpenProcessToken},
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
/// Also serializes the guards' privilege scopes, so that one never puts
/// the privilege back while another's call needs it.
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

/// `SeIncreaseBasePriorityPrivilege` enabled in this process's token until
/// it drops, which puts it back as it was.
struct BasePriority {
    token: OwnedHandle,
    /// The privilege's state before: no entry where it was enabled already.
    previous: TOKEN_PRIVILEGES,
}
impl BasePriority {
    /// Enable the privilege. An error where the token cannot be adjusted,
    /// or does not hold it (`ERROR_NOT_ALL_ASSIGNED`).
    fn enable() -> windows::core::Result<Self> {
        let mut token = HANDLE::default();
        // SAFETY: the current process's pseudo-handle, and an out parameter
        // to a local.
        unsafe {
            OpenProcessToken(
                GetCurrentProcess(),
                TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
                &mut token,
            )
        }?;
        // SAFETY: a handle the call above opened, which nothing else owns.
        let token = unsafe { OwnedHandle::from_raw_handle(token.0) };
        let mut luid = LUID::default();
        // SAFETY: a static name, and an out parameter to a local.
        unsafe { LookupPrivilegeValueW(None, SE_INC_BASE_PRIORITY_NAME, &mut luid) }?;
        let enabled = TOKEN_PRIVILEGES {
            PrivilegeCount: 1,
            Privileges: [LUID_AND_ATTRIBUTES {
                Luid: luid,
                Attributes: SE_PRIVILEGE_ENABLED,
            }],
        };
        let mut previous = TOKEN_PRIVILEGES::default();
        // Windows requires it whenever the previous state is asked for.
        let mut previous_length = 0;
        // SAFETY: the token, opened to adjust its privileges, and locals that
        // outlive the call.
        unsafe {
            AdjustTokenPrivileges(
                HANDLE(token.as_raw_handle()),
                false,
                Some(&enabled),
                size_of::<TOKEN_PRIVILEGES>() as u32,
                Some(&mut previous),
                Some(&mut previous_length),
            )
        }?;
        // It succeeds without the privilege, and says so here:
        // `ERROR_NOT_ALL_ASSIGNED`, having changed nothing.
        // SAFETY: reads this thread's last error.
        unsafe { GetLastError() }.ok()?;
        Ok(Self { token, previous })
    }
}
impl Drop for BasePriority {
    fn drop(&mut self) {
        // SAFETY: the token this scope opened, and the state the call that
        // enabled the privilege returned.
        let _ = unsafe {
            AdjustTokenPrivileges(
                HANDLE(self.token.as_raw_handle()),
                false,
                Some(&self.previous),
                0,
                None,
                None,
            )
        };
    }
}

/// A raised GPU priority for this process while it lives.
pub(crate) struct GpuPriority(());
impl GpuPriority {
    /// Raise `device`'s GPU thread priority and hold the process's GPU
    /// scheduling class at `HIGH` at least. Every call may fail harmlessly.
    pub(crate) fn hold(device: &ID3D11Device) -> Self {
        let mut held = HELD.lock();
        let privilege = BasePriority::enable();
        let device_refused = device
            .cast::<IDXGIDevice>()
            // SAFETY: a COM call on an interface this caller holds.
            .and_then(|dxgi| unsafe { dxgi.SetGPUThreadPriority(DEVICE_PRIORITY) })
            .err()
            .map(|failure| failure.code().0);
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
                    privilege = privilege.is_ok(),
                    privilege_refused = ?privilege.as_ref().err().map(|failure| failure.code().0),
                    device_priority = device_refused.is_none(),
                    device_refused = ?device_refused,
                    class_before = ?before.map(|class| class.0),
                    class_raised = raised,
                );
            }
        }
        drop(privilege);
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
            let _privilege = BasePriority::enable();
            set_class(before);
        }
    }
}
