//! Windows requests are tied to a kernel handle, so any thread can release them.
use std::{
    io,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
};
use windows::{
    Win32::{
        Foundation::HANDLE,
        System::{
            Power::{
                POWER_REQUEST_TYPE, PowerClearRequest, PowerCreateRequest,
                PowerRequestDisplayRequired, PowerRequestSystemRequired, PowerSetRequest,
            },
            Threading::{POWER_REQUEST_CONTEXT_SIMPLE_STRING, REASON_CONTEXT, REASON_CONTEXT_0},
        },
    },
    core::{PCWSTR, PWSTR, w},
};

pub struct PowerRequest {
    handle: OwnedHandle,
    kinds: &'static [POWER_REQUEST_TYPE],
}
impl PowerRequest {
    pub(super) fn for_services() -> io::Result<Self> {
        Self::new(
            w!("Keep pb-mapper available for incoming connections"),
            &[PowerRequestSystemRequired],
        )
    }

    /// A viewer is watching the screen, which must also stay lit.
    pub fn for_capture() -> io::Result<Self> {
        Self::new(
            w!("pb-mapper: a remote desktop viewer is watching this screen"),
            &[PowerRequestDisplayRequired, PowerRequestSystemRequired],
        )
    }

    fn new(reason: PCWSTR, kinds: &'static [POWER_REQUEST_TYPE]) -> io::Result<Self> {
        let context = REASON_CONTEXT {
            Version: 0,
            Flags: POWER_REQUEST_CONTEXT_SIMPLE_STRING,
            Reason: REASON_CONTEXT_0 {
                SimpleReasonString: PWSTR(reason.0.cast_mut()),
            },
        };
        // SAFETY: both the context and its read-only reason outlive this call.
        let handle = unsafe { PowerCreateRequest(&context) }?;
        let request = Self {
            // SAFETY: PowerCreateRequest returned a new owned kernel handle.
            handle: unsafe { OwnedHandle::from_raw_handle(handle.0) },
            kinds,
        };
        for &kind in kinds {
            // SAFETY: the request owns this open handle throughout the call.
            unsafe { PowerSetRequest(request.handle(), kind) }?;
        }
        Ok(request)
    }

    fn handle(&self) -> HANDLE {
        HANDLE(self.handle.as_raw_handle())
    }
}
impl Drop for PowerRequest {
    fn drop(&mut self) {
        for &kind in self.kinds {
            // SAFETY: the handle closes after drop; an unset kind (partial acquisition) simply fails.
            let _ = unsafe { PowerClearRequest(self.handle(), kind) };
        }
    }
}
