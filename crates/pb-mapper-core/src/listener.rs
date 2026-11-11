//! The TCP listeners pb-mapper binds for its peers: the relay's, a connect
//! tunnel's local one, the direct path's LAN acceptor and the desktop host's.
//! Each binds here, so its accept queue is as deep as this crate says rather
//! than as deep as the locked Tokio and mio choose.

use std::io;
use std::net::SocketAddr;

use tokio::net::{TcpListener, TcpSocket};

/// How many connections a listener's accept queue holds: those the kernel
/// has completed that are not yet accepted. A connect tunnel's local
/// connections wait there while its setups are full, and clients reconnecting
/// to a relay while its accept loop backs off.
///
/// Spelled out because Tokio's own `TcpListener::bind` leaves the depth to
/// mio, and mio changed it under a lock-file update: up to 1.1 it asked for
/// the system's maximum where `listen` lowers a larger request to it (Linux,
/// whose `net.core.somaxconn` is 4096 from 5.4 on, the BSDs and Apple), and
/// 128 elsewhere; from 1.2 it asks for 128 everywhere. These are mio 1.1's
/// values, the depths pb-mapper 0.11.0 listened with: the system's maximum
/// there, and 128 on Windows and Android.
pub const ACCEPT_BACKLOG: u32 = if cfg!(any(
    target_os = "linux",
    target_os = "freebsd",
    target_os = "openbsd",
    target_vendor = "apple",
)) {
    // The largest request `listen` takes; the kernel lowers it to its maximum.
    i32::MAX as u32
} else {
    128
};

/// A socket for a listener on `addr`'s family, set up as Tokio's own
/// `TcpListener::bind` sets one up: with address reuse, so a listener bound
/// again need not wait out its old connections' TIME_WAIT, except on
/// Windows, where reuse would let another socket take over a port in use.
pub fn socket(addr: SocketAddr) -> io::Result<TcpSocket> {
    let socket = match addr {
        SocketAddr::V4(_) => TcpSocket::new_v4()?,
        SocketAddr::V6(_) => TcpSocket::new_v6()?,
    };
    #[cfg(not(windows))]
    socket.set_reuseaddr(true)?;
    Ok(socket)
}

/// Listen on `addr` with an accept queue of [`ACCEPT_BACKLOG`].
///
/// It panics outside a Tokio runtime, as `TcpListener::from_std` does.
pub fn bind(addr: SocketAddr) -> io::Result<TcpListener> {
    let socket = socket(addr)?;
    socket.bind(addr)?;
    socket.listen(ACCEPT_BACKLOG)
}

/// Listen as [`bind`] does on the first address `addrs` resolves to that
/// binds, as Tokio's `TcpListener::bind` tries them.
pub async fn bind_first(addrs: impl tokio::net::ToSocketAddrs) -> io::Result<TcpListener> {
    let mut last = None;
    for addr in tokio::net::lookup_host(addrs).await? {
        match bind(addr) {
            Ok(listener) => return Ok(listener),
            Err(error) => last = Some(error),
        }
    }
    Err(last.unwrap_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "could not resolve to any address",
        )
    }))
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;
    use std::time::Duration;

    /// A listener's queue is Linux's maximum: it holds more connections than
    /// the 128 Tokio's own bind asks for since mio 1.2, none of them accepted.
    /// A connection that finds the queue full has its SYN dropped and does
    /// not complete for a second or more.
    #[tokio::test]
    async fn a_listener_queues_as_many_connections_as_the_system_allows() {
        // Past mio 1.2's 128, within the 4096 of Linux 5.4 and later. Linux
        // queues one more than the depth `listen` was given.
        const CONNECTIONS: usize = 300;
        let maximum: usize = std::fs::read_to_string("/proc/sys/net/core/somaxconn")
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let listener = bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))).unwrap();
        let addr = listener.local_addr().unwrap();
        let wanted = CONNECTIONS.min(maximum);
        let mut queued = Vec::with_capacity(wanted);
        while queued.len() < wanted {
            match tokio::time::timeout(
                Duration::from_millis(500),
                tokio::net::TcpStream::connect(addr),
            )
            .await
            {
                Ok(Ok(stream)) => queued.push(stream),
                _ => break,
            }
        }
        assert_eq!(
            queued.len(),
            wanted,
            "the accept queue held {} connections",
            queued.len()
        );
    }
}
