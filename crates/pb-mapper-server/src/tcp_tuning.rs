//! The relay's own TCP connections on Linux: the congestion control an
//! operator picks for them, and a notice at startup while the kernel restarts
//! the congestion window of a connection that was idle.
//!
//! Both act only on what the relay sends: a relayed remote desktop's picture
//! on its way to the viewer, and the relay's leg toward each end of any other
//! relayed tunnel. What an end sends to the relay runs on that end's own
//! network stack.

use std::io;
use std::path::Path;
use std::str::FromStr;

use snafu::{ResultExt, Snafu};
use tokio::net::TcpListener;

/// Whether Linux restarts an idle connection's congestion window: one value
/// per network namespace from Linux 4.15, so a container reads its own.
const IDLE_RESTART_SYSCTL: &str = "/proc/sys/net/ipv4/tcp_slow_start_after_idle";

/// The name of a TCP congestion control algorithm, such as `bbr` or `cubic`,
/// as Linux's `TCP_CONGESTION` socket option takes it.
///
/// One to [`TcpCongestion::MAX_LEN`] ASCII letters, digits, `_` or `-`, as
/// every algorithm Linux ships is named. The kernel cuts a longer name short
/// and looks up what is left, a name nobody asked for.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TcpCongestion(Box<str>);

impl TcpCongestion {
    /// The longest name Linux takes: `TCP_CA_NAME_MAX` (16) less its NUL.
    pub const MAX_LEN: usize = 15;

    pub fn new(name: &str) -> Result<Self, InvalidTcpCongestion> {
        let valid = (1..=Self::MAX_LEN).contains(&name.len())
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-');
        if !valid {
            return InvalidTcpCongestionSnafu { name }.fail();
        }
        Ok(Self(name.into()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for TcpCongestion {
    type Err = InvalidTcpCongestion;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        Self::new(name)
    }
}

impl std::fmt::Display for TcpCongestion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A name [`TcpCongestion::new`] refused.
#[derive(Debug, Snafu)]
#[snafu(display(
    "`{name}` is not a TCP congestion control name: 1 to {} ASCII letters, digits, `_` or `-`",
    TcpCongestion::MAX_LEN
))]
pub struct InvalidTcpCongestion {
    pub name: String,
}

/// Why [`set_tcp_congestion`] could not choose an algorithm. The system's
/// default stays in place.
#[derive(Debug, Snafu)]
#[snafu(display("cannot send the relay's connections with TCP congestion control `{congestion}`"))]
pub struct TcpCongestionError {
    pub congestion: TcpCongestion,
    source: io::Error,
}

impl TcpCongestionError {
    /// The kernel's answer: [`io::ErrorKind::NotFound`] when it has no
    /// algorithm by that name (`ENOENT`), [`io::ErrorKind::PermissionDenied`]
    /// when this process may not choose it (`EPERM`), and
    /// [`io::ErrorKind::Unsupported`] off Linux.
    pub fn io(&self) -> &io::Error {
        &self.source
    }

    /// What an operator can do about it, for the log.
    pub fn fix(&self) -> String {
        let name = &self.congestion;
        match self.source.kind() {
            // Linux loads the module `tcp_<name>` on demand only for a
            // process with CAP_NET_ADMIN.
            io::ErrorKind::NotFound => format!(
                "load it on the host with `modprobe tcp_{name}` and list `tcp_{name}` in \
                 /etc/modules-load.d/ to keep it after a reboot; \
                 /proc/sys/net/ipv4/tcp_available_congestion_control lists those loaded"
            ),
            // Without CAP_NET_ADMIN a process may choose only an algorithm
            // the host allows; that list is not per container.
            io::ErrorKind::PermissionDenied => format!(
                "add `{name}` to the host's net.ipv4.tcp_allowed_congestion_control, \
                 or give the relay CAP_NET_ADMIN (in Docker, `cap_add: [NET_ADMIN]`)"
            ),
            io::ErrorKind::Unsupported => {
                "only a relay on Linux can choose its TCP congestion control".to_owned()
            }
            _ => "/proc/sys/net/ipv4/tcp_available_congestion_control lists the algorithms \
                  this kernel has"
                .to_owned(),
        }
    }
}

/// Have every connection `listener` accepts send with `congestion`.
///
/// It is set once, on the listener: Linux hands a listening socket's chosen
/// congestion control to each connection it accepts, so the accept loop does
/// no more work per connection. Only what the relay sends changes. On failure
/// the listener, and every connection it accepts, keeps the system's default.
pub fn set_tcp_congestion(
    listener: &TcpListener,
    congestion: &TcpCongestion,
) -> Result<(), TcpCongestionError> {
    choose_congestion(listener, congestion).with_context(|_| TcpCongestionSnafu {
        congestion: congestion.clone(),
    })
}

#[cfg(target_os = "linux")]
fn choose_congestion(listener: &TcpListener, congestion: &TcpCongestion) -> io::Result<()> {
    socket2::SockRef::from(listener).set_tcp_congestion(congestion.as_str().as_bytes())
}

#[cfg(not(target_os = "linux"))]
fn choose_congestion(_listener: &TcpListener, _congestion: &TcpCongestion) -> io::Result<()> {
    Err(io::ErrorKind::Unsupported.into())
}

/// Log `relay_tcp_idle_restart`, with the fix, while Linux restarts the
/// congestion window of a connection that was idle. It only reads the
/// setting, once, as the relay starts; elsewhere it does nothing.
pub(crate) fn log_idle_restart() {
    if cfg!(target_os = "linux") && idle_restart_at(Path::new(IDLE_RESTART_SYSCTL)) == Some(true) {
        tracing::info!(
            event = "relay_tcp_idle_restart",
            sysctl = "net.ipv4.tcp_slow_start_after_idle",
            fix = "`sysctl -w net.ipv4.tcp_slow_start_after_idle=0`, kept in \
                   /etc/sysctl.d/90-pb-mapper.conf; in Docker, the service's `sysctls`",
            "Linux restarts an idle TCP connection's congestion window, so what the relay \
             sends after a pause, such as a remote desktop's next picture, starts slowly again"
        );
    }
}

/// The setting at `path`, or `None` when it cannot be read.
fn idle_restart_at(path: &Path) -> Option<bool> {
    idle_restart(&std::fs::read_to_string(path).ok()?)
}

/// Whether the sysctl's text turns the restart on: any integer but zero, as
/// the kernel reads it.
fn idle_restart(text: &str) -> Option<bool> {
    text.trim().parse::<i64>().ok().map(|value| value != 0)
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, SocketAddr};

    use super::*;

    fn listener() -> TcpListener {
        pb_mapper_core::listener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))).unwrap()
    }

    #[test]
    fn congestion_names_are_what_linux_can_take_whole() {
        for name in ["bbr", "cubic", "reno", "bbr_v3-x", "abcdefghijklmno"] {
            assert_eq!(TcpCongestion::new(name).unwrap().as_str(), name);
        }
        // The kernel would cut the 16-byte name to `abcdefghijklmno`.
        for name in ["", "abcdefghijklmnop", "bbr ", "bbr\0", "b/r", "bbré"] {
            assert_eq!(TcpCongestion::new(name).unwrap_err().name, name);
        }
    }

    #[test]
    fn idle_restart_is_any_nonzero_integer() {
        assert_eq!(idle_restart("1\n"), Some(true));
        assert_eq!(idle_restart("2"), Some(true));
        assert_eq!(idle_restart("0\n"), Some(false));
        assert_eq!(idle_restart(""), None);
        assert_eq!(idle_restart("on"), None);
        assert_eq!(idle_restart_at(Path::new("/nonexistent/pb-mapper")), None);
    }

    #[tokio::test]
    async fn an_algorithm_the_kernel_lacks_is_refused() {
        let congestion = TcpCongestion::new("pbmappernone").unwrap();
        let error = set_tcp_congestion(&listener(), &congestion).unwrap_err();
        let expected = if cfg!(target_os = "linux") {
            io::ErrorKind::NotFound
        } else {
            io::ErrorKind::Unsupported
        };
        assert_eq!(error.io().kind(), expected);
        assert_eq!(error.congestion, congestion);
    }

    /// The relay sets the algorithm once, on its listener, and relies on
    /// Linux to hand it to each connection the listener accepts.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn accepted_connections_take_the_listeners_congestion_control() {
        // Built into every kernel and open to unprivileged processes. A
        // default of reno, which no distribution ships, would make this pass
        // without the listener's choice.
        let reno = TcpCongestion::new("reno").unwrap();
        let listener = listener();
        set_tcp_congestion(&listener, &reno).unwrap();
        let addr = listener.local_addr().unwrap();
        let (dialed, accepted) =
            tokio::join!(tokio::net::TcpStream::connect(addr), listener.accept());
        let _dialed = dialed.unwrap();
        let (accepted, _) = accepted.unwrap();
        let chosen = socket2::SockRef::from(&accepted).tcp_congestion().unwrap();
        // Linux pads the name with NULs to `TCP_CA_NAME_MAX`.
        assert_eq!(chosen.split(|&byte| byte == 0).next(), Some(&b"reno"[..]));
    }
}
