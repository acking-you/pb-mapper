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

/// What a client may do with a helper pipe: read and write it, without
/// `FILE_CREATE_PIPE_INSTANCE`. A generic-write ACE would let a user create
/// another server instance and impersonate the helper.
const CLIENT_ACCESS: u32 = 0x0012_019b;
const _: () = assert!(
    CLIENT_ACCESS == (FILE_GENERIC_READ.0 | FILE_GENERIC_WRITE.0) & !FILE_CREATE_PIPE_INSTANCE.0,
    "generic read and write, less creating a pipe instance"
);

#[cfg(feature = "windows-helper")]
pub fn bind(name: &str, sid: Option<&str>, first: bool) -> Result<NamedPipeServer> {
    let mut sddl = "D:P(A;;FA;;;SY)(A;;FA;;;BA)".to_string();
    if let Some(sid) = sid {
        sddl.push_str(&format!("(A;;{CLIENT_ACCESS:#010x};;;{sid})"));
    }
    let descriptor = os::Descriptor::new(&sddl)?;
    let mut options = ServerOptions::new();
    options
        .first_pipe_instance(first)
        .reject_remote_clients(true)
        .max_instances(8)
        .in_buffer_size(super::transfer::PIPE_QUOTA)
        .out_buffer_size(super::transfer::PIPE_QUOTA);
    unsafe { options.create_with_security_attributes_raw(name, descriptor.attributes().cast()) }
}
pub fn connect(name: &str) -> Result<NamedPipeClient> {
    let name = os::wide(name);
    // Identification only: a spoofed pipe cannot impersonate this client.
    let handle = unsafe {
        CreateFileW(
            PCWSTR(name.as_ptr()),
            CLIENT_ACCESS,
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

#[cfg(all(test, feature = "windows-helper"))]
mod tests {
    use super::*;
    use crate::oop::ipc;

    /// A unit within the quota waits in the pipe, in either direction, while
    /// nobody reads it: its write completes at once, so the writer's next
    /// write, which starts only once the last one completed, never waits on
    /// the reader. Every unit still arrives whole.
    #[tokio::test]
    async fn a_unit_waits_in_the_pipe_until_its_reader_reads() {
        let name = format!(
            r"\\.\pipe\pb-mapper.desktop-test.{}",
            crate::security::secret().unwrap()
        );
        let owner = os::current_sid().unwrap();
        let mut server = bind(&name, Some(owner.as_str()), true).unwrap();
        let mut client = connect(&name).unwrap();
        server.connect().await.unwrap();
        let unit: Vec<u8> = (0..=255u8)
            .cycle()
            .take(super::super::transfer::PIPE_QUOTA as usize * 3 / 4)
            .collect();
        let bound = std::time::Duration::from_secs(5);
        // The worker's direction, toward the service: the pipe's in quota.
        tokio::time::timeout(bound, async {
            ipc::write_bytes(&mut client, &unit, ipc::MAX_FRAME).await?;
            ipc::write_bytes(&mut client, &unit, ipc::MAX_FRAME).await
        })
        .await
        .expect("the first unit waits in the pipe")
        .unwrap();
        for _ in 0..2 {
            let read = ipc::read_bytes(&mut server, ipc::MAX_FRAME).await.unwrap();
            assert!(read == unit);
        }
        // The service's direction, toward the app: the out quota.
        tokio::time::timeout(bound, async {
            ipc::write_bytes(&mut server, &unit, ipc::MAX_FRAME).await?;
            ipc::write_bytes(&mut server, &unit, ipc::MAX_FRAME).await
        })
        .await
        .expect("the first unit waits in the pipe")
        .unwrap();
        for _ in 0..2 {
            let read = ipc::read_bytes(&mut client, ipc::MAX_FRAME).await.unwrap();
            assert!(read == unit);
        }
    }
}
