//! Process-owned power requests. A service may wait for its next connection
//! with the display off. Dropping the guard releases the request.
use std::io;

#[cfg(target_os = "macos")]
const REASON: &str = "Keep pb-mapper available for incoming connections";

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
pub mod windows;

/// The operating system releases the request when the owning process exits.
#[must_use]
pub struct SystemAwake {
    #[cfg(target_os = "macos")]
    _activity: macos::Activity,
    #[cfg(windows)]
    _request: windows::PowerRequest,
    #[cfg(target_os = "linux")]
    _inhibitor: linux::Inhibitor,
}

impl SystemAwake {
    pub async fn acquire() -> io::Result<Self> {
        #[cfg(target_os = "macos")]
        return Ok(Self {
            _activity: macos::Activity::new(REASON),
        });
        #[cfg(windows)]
        return Ok(Self {
            _request: windows::PowerRequest::for_services()?,
        });
        #[cfg(target_os = "linux")]
        return Ok(Self {
            _inhibitor: linux::Inhibitor::acquire().await?,
        });
        #[cfg(not(any(target_os = "macos", windows, target_os = "linux")))]
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Keeping pb-mapper awake requires macOS, Windows or Linux with logind",
        ));
    }
}
