//! The helper service as the service control manager reports it.
use super::wide;
use crate::{Result, error, platform::windows::helper::SERVICE};
use windows::{
    Win32::{Foundation::ERROR_SERVICE_DOES_NOT_EXIST, System::Services::*},
    core::PCWSTR,
};

struct ServiceHandle(SC_HANDLE);
impl Drop for ServiceHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseServiceHandle(self.0);
        }
    }
}
fn open_service(access: u32) -> windows::core::Result<ServiceHandle> {
    let manager = ServiceHandle(unsafe { OpenSCManagerW(None, None, SC_MANAGER_CONNECT) }?);
    Ok(ServiceHandle(unsafe {
        OpenServiceW(manager.0, PCWSTR(wide(SERVICE).as_ptr()), access)
    }?))
}
fn service_status() -> Result<SERVICE_STATUS_PROCESS> {
    query_service_status(&open_service(SERVICE_QUERY_STATUS).map_err(error)?)
}
fn query_service_status(service: &ServiceHandle) -> Result<SERVICE_STATUS_PROCESS> {
    let mut status = SERVICE_STATUS_PROCESS::default();
    let mut needed = 0;
    unsafe {
        QueryServiceStatusEx(
            service.0,
            SC_STATUS_PROCESS_INFO,
            Some(std::slice::from_raw_parts_mut(
                (&mut status as *mut SERVICE_STATUS_PROCESS).cast(),
                size_of::<SERVICE_STATUS_PROCESS>(),
            )),
            &mut needed,
        )
    }
    .map_err(error)?;
    Ok(status)
}
pub fn service_state() -> Result<Option<(bool, bool)>> {
    let service = match open_service(SERVICE_QUERY_STATUS | SERVICE_QUERY_CONFIG) {
        Ok(service) => service,
        Err(e)
            if e.code() == windows::core::HRESULT::from_win32(ERROR_SERVICE_DOES_NOT_EXIST.0) =>
        {
            return Ok(None);
        }
        Err(e) => return Err(error(e)),
    };
    let disabled = service_startup_disabled(&service)?;
    let state = query_service_status(&service)?.dwCurrentState;
    // A disabled startup policy alone says nothing about an existing process.
    // Pending transitions and failed queries must never allow a second host.
    Ok(Some((
        state == SERVICE_RUNNING,
        state == SERVICE_STOPPED && disabled,
    )))
}
fn service_startup_disabled(service: &ServiceHandle) -> Result<bool> {
    // QueryServiceConfig has a documented 8 KiB maximum. Word storage keeps
    // QUERY_SERVICE_CONFIGW aligned and avoids a second, racy size query.
    let mut buffer = [0usize; 8192 / size_of::<usize>()];
    let config = buffer.as_mut_ptr().cast::<QUERY_SERVICE_CONFIGW>();
    let mut needed = 0;
    unsafe { QueryServiceConfigW(service.0, Some(config), 8192, &mut needed) }.map_err(error)?;
    Ok(unsafe { (*config).dwStartType } == SERVICE_DISABLED)
}

pub fn running_service_pid() -> Result<u32> {
    let status = service_status()?;
    if status.dwCurrentState != SERVICE_RUNNING || status.dwProcessId == 0 {
        return Err(error("helper service is not running"));
    }
    Ok(status.dwProcessId)
}
