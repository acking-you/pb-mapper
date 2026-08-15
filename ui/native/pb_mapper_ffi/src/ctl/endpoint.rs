//! Where the control channel lives, and how each side reaches it.
//!
//! A named pipe on Windows, a unix domain socket elsewhere. Both come from
//! tokio, which is already a dependency — no IPC crate is needed, and tokio is
//! the only one of the candidates that exposes the Windows security attributes
//! this has to set.
//!
//! Access control is the OS boundary and nothing else: no token file, no TCP
//! port, no shared secret that could end up in a log.

use std::io;

/// Overrides the computed endpoint. For tests, and for running two profiles
/// side by side.
pub const ENDPOINT_ENV: &str = "PB_MAPPER_UI_SOCK";

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;
    use std::io;
    use std::ptr;

    use tokio::net::windows::named_pipe::{
        ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions,
    };

    pub type Stream = NamedPipeClient;
    pub type Server = NamedPipeServer;

    const SDDL_REVISION_1: u32 = 1;

    /// Owner and Local System, nothing else, and `P` so no inherited entry can
    /// widen it later.
    ///
    /// The default descriptor is not safe here: measured, it grants `WD`
    /// (Everyone) and `AN` (Anonymous) `FILE_GENERIC_READ`, which would let any
    /// local process under any account read the control channel.
    const OWNER_ONLY: &str = "D:P(A;;FA;;;OW)(A;;FA;;;SY)";

    #[link(name = "advapi32")]
    extern "system" {
        fn ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl: *const u16,
            revision: u32,
            descriptor: *mut *mut c_void,
            size: *mut u32,
        ) -> i32;
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn LocalFree(mem: *mut c_void) -> *mut c_void;
    }

    #[repr(C)]
    struct SecurityAttributes {
        length: u32,
        descriptor: *mut c_void,
        inherit: i32,
    }

    /// Owns the descriptor for as long as the attributes point at it.
    struct OwnerOnlySecurity {
        attrs: SecurityAttributes,
    }

    impl OwnerOnlySecurity {
        fn new() -> io::Result<Self> {
            let sddl: Vec<u16> = OWNER_ONLY
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();
            let mut descriptor: *mut c_void = ptr::null_mut();
            let ok = unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    sddl.as_ptr(),
                    SDDL_REVISION_1,
                    &mut descriptor,
                    ptr::null_mut(),
                )
            };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Self {
                attrs: SecurityAttributes {
                    length: std::mem::size_of::<SecurityAttributes>() as u32,
                    descriptor,
                    inherit: 0,
                },
            })
        }

        fn as_ptr(&self) -> *mut c_void {
            &self.attrs as *const SecurityAttributes as *mut c_void
        }
    }

    impl Drop for OwnerOnlySecurity {
        fn drop(&mut self) {
            if !self.attrs.descriptor.is_null() {
                unsafe { LocalFree(self.attrs.descriptor) };
            }
        }
    }

    pub fn endpoint() -> String {
        if let Ok(custom) = std::env::var(super::ENDPOINT_ENV) {
            if !custom.is_empty() {
                return custom;
            }
        }
        // Per user, so two accounts on one machine do not collide.
        let user = std::env::var("USERNAME").unwrap_or_else(|_| "default".into());
        let tag: String = user
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
            .collect();
        format!(r"\\.\pipe\pb-mapper-ui.{tag}")
    }

    /// The first instance also claims the name: `first_pipe_instance` makes a
    /// second UI fail here rather than quietly serving half the connections.
    pub fn bind_first(name: &str) -> io::Result<Server> {
        let security = OwnerOnlySecurity::new()?;
        let mut options = ServerOptions::new();
        options.first_pipe_instance(true);
        // SAFETY: the descriptor outlives the call; tokio copies what it needs.
        unsafe { options.create_with_security_attributes_raw(name, security.as_ptr()) }
    }

    /// Every later instance, so more than one client can be served.
    pub fn bind_next(name: &str) -> io::Result<Server> {
        let security = OwnerOnlySecurity::new()?;
        let options = ServerOptions::new();
        // SAFETY: as above.
        unsafe { options.create_with_security_attributes_raw(name, security.as_ptr()) }
    }

    pub async fn connect(name: &str) -> io::Result<Stream> {
        ClientOptions::new().open(name)
    }
}

#[cfg(unix)]
mod imp {
    use std::io;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    use tokio::net::{UnixListener, UnixStream};

    pub type Stream = UnixStream;
    pub type Server = UnixListener;

    pub fn endpoint() -> String {
        if let Ok(custom) = std::env::var(super::ENDPOINT_ENV) {
            if !custom.is_empty() {
                return custom;
            }
        }
        // XDG_RUNTIME_DIR is already per-user and cleaned on logout; TMPDIR on
        // macOS is per-user too. The home fallback is last because it survives
        // reboots and so needs the stale-socket handling below most.
        let base = std::env::var("XDG_RUNTIME_DIR")
            .or_else(|_| std::env::var("TMPDIR"))
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                std::env::var("HOME")
                    .map(|h| PathBuf::from(h).join(".cache"))
                    .unwrap_or_else(|_| PathBuf::from("/tmp"))
            });
        base.join("pb-mapper-ui")
            .join("ctl.sock")
            .to_string_lossy()
            .into_owned()
    }

    pub fn bind_first(name: &str) -> io::Result<Server> {
        let path = PathBuf::from(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
        }
        match UnixListener::bind(&path) {
            Ok(listener) => {
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
                Ok(listener)
            }
            Err(e) if e.kind() == io::ErrorKind::AddrInUse => {
                // Either another UI is live or a crash left the file behind.
                // Connecting is the only way to tell that does not race: a pid
                // file can be stale, and a stale one produces the worst
                // failure — refusing to start because of a process that died.
                match std::os::unix::net::UnixStream::connect(&path) {
                    Ok(_) => Err(io::Error::new(
                        io::ErrorKind::AddrInUse,
                        "another pb-mapper UI is already running",
                    )),
                    Err(_) => {
                        std::fs::remove_file(&path)?;
                        let listener = UnixListener::bind(&path)?;
                        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
                        Ok(listener)
                    }
                }
            }
            Err(e) => Err(e),
        }
    }

    /// Unix listeners serve every connection from one object.
    pub fn bind_next(_name: &str) -> io::Result<Server> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "unix listeners do not need per-connection instances",
        ))
    }

    pub async fn connect(name: &str) -> io::Result<Stream> {
        UnixStream::connect(name).await
    }
}

pub use imp::{bind_first, bind_next, connect, endpoint};

/// Whether a UI is listening right now.
///
/// A connect attempt, not a pid file: it is the only check that cannot report
/// "no UI" while one is running, or block waiting for one that died.
#[allow(dead_code)] // used by the headless fallback in phase 3
pub async fn probe() -> bool {
    connect(&endpoint()).await.is_ok()
}

/// Distinguishes "nothing is listening" from a real transport failure, so the
/// CLI can say `no UI to attach to` only when that is actually what happened.
pub fn is_not_listening(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
    )
}
