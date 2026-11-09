//! Named pipes: the helper's transport, with a protected DACL, identification
//! only for clients, and no remote clients.
use super::os;
use crate::{Result, error};
use std::os::windows::io::{FromRawHandle, IntoRawHandle};
use tokio::net::windows::named_pipe::NamedPipeClient;
#[cfg(feature = "windows-helper")]
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use windows::{
    Win32::{Foundation::HANDLE, Storage::FileSystem::*},
    core::PCWSTR,
};

#[cfg(feature = "windows-helper")]
pub fn bind(name: &str, sid: Option<&str>, first: bool) -> Result<NamedPipeServer> {
    // FILE_CREATE_PIPE_INSTANCE (0x4) is deliberately absent for the user.
    // A generic-write ACE would let a user impersonate another server instance.
    let mut sddl = "D:P(A;;FA;;;SY)(A;;FA;;;BA)".to_string();
    if let Some(sid) = sid {
        sddl.push_str(&format!("(A;;0x0012019b;;;{sid})"));
    }
    let descriptor = os::Descriptor::new(&sddl)?;
    let mut options = ServerOptions::new();
    options
        .first_pipe_instance(first)
        .reject_remote_clients(true)
        .max_instances(8)
        .in_buffer_size(8192)
        .out_buffer_size(8192);
    unsafe { options.create_with_security_attributes_raw(name, descriptor.attributes().cast()) }
}
pub fn connect(name: &str) -> Result<NamedPipeClient> {
    let name = os::wide(name);
    // Identification only: a spoofed pipe cannot impersonate this client.
    let handle = unsafe {
        CreateFileW(
            PCWSTR(name.as_ptr()),
            0x0012019b,
            FILE_SHARE_MODE(0),
            None,
            OPEN_EXISTING,
            FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
            None,
        )
    }
    .map_err(error)?;
    let owned = unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(handle.0) };
    unsafe { NamedPipeClient::from_raw_handle(owned.into_raw_handle()) }
}

/// Whether a server has created pipe `name`, without connecting to it. Only a
/// definite "no such pipe" counts as absent; busy instances are present.
pub fn exists(name: &str) -> bool {
    let name = os::wide(name);
    unsafe { windows::Win32::System::Pipes::WaitNamedPipeW(PCWSTR(name.as_ptr()), 1) }.as_bool()
        || std::io::Error::last_os_error().kind() != std::io::ErrorKind::NotFound
}

pub fn raw<T: std::os::windows::io::AsRawHandle>(pipe: &T) -> HANDLE {
    HANDLE(pipe.as_raw_handle())
}
