//! Process-owned power requests. A service may wait for its next connection
//! with the display off. Dropping the guard releases the request.
use std::io;

#[cfg(any(target_os = "macos", target_os = "linux"))]
const REASON: &str = "Keep pb-mapper available for incoming connections";

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
    _inhibitor: zbus::zvariant::OwnedFd,
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
        {
            // A sleep inhibitor works with desktop power managers that request
            // suspend themselves. Leave screensaver, locking and DPMS alone.
            let request = async {
                let bus = zbus::Connection::system().await?;
                let reply = bus
                    .call_method(
                        Some("org.freedesktop.login1"),
                        "/org/freedesktop/login1",
                        Some("org.freedesktop.login1.Manager"),
                        "Inhibit",
                        &("sleep", "pb-mapper", REASON, "block"),
                    )
                    .await?;
                reply.body().deserialize::<zbus::zvariant::OwnedFd>()
            };
            let fd = tokio::time::timeout(std::time::Duration::from_secs(5), request)
                .await
                .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "logind inhibitor timed out"))?
                .map_err(io::Error::other)?;
            Ok(Self { _inhibitor: fd })
        }
        #[cfg(not(any(target_os = "macos", windows, target_os = "linux")))]
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Keeping pb-mapper awake requires macOS, Windows or Linux with logind",
        ));
    }
}
