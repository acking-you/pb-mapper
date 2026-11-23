//! Address changes on macOS and iOS, from the kernel's routing socket.
//!
//! The kernel writes a message to every routing socket when an address is
//! added or removed and when an interface goes up or down. Waiting for those
//! leaves the watcher asleep while nothing changes, where the poll reads every
//! address every two seconds. A message only says when to look: the addresses
//! are read and compared as the poll reads them, so what counts as a change,
//! and the one hint it raises, stay the same. Like the poll, and unlike the
//! system's change callbacks, it needs no thread of its own.

use std::convert::Infallible;
use std::io::{self, Read};

use socket2::{Domain, Socket, Type};
use tokio::io::Interest;
use tokio::io::unix::AsyncFd;
use tokio::sync::watch;
use tokio::time::{Duration, Instant};

use super::{Addresses, POLL, raise};

/// How long the socket must stay quiet after a message before the addresses
/// are read. A change comes as a run of messages (each address, the
/// interface), and an address being removed may still be listed when its
/// message arrives.
const QUIET: Duration = Duration::from_millis(250);

/// The longest a reading waits for a socket that does not go quiet: the
/// poll's period, so a storm of messages costs no more than the poll did.
const LONGEST_WAIT: Duration = POLL;

/// How often the addresses are read without a message, for a change the
/// socket missed: the kernel drops messages for a reader that falls behind.
const SAFETY: Duration = Duration::from_secs(60);

const _: () = assert!(
    QUIET.as_millis() < LONGEST_WAIT.as_millis() && LONGEST_WAIT.as_millis() < SAFETY.as_millis(),
    "the waits nest: a quiet socket within the longest wait, that within the safety period"
);

/// Room for one message. Only its header is looked at, and a longer message
/// is cut short, not split.
const MESSAGE: usize = 2048;

/// The most reads in one wake, so that a flood of messages cannot hold the
/// task and the reading it schedules.
const DRAIN: usize = 64;

/// The socket's receive buffer. Darwin drops what does not fit without
/// telling the reader, and a network switch writes a message for every
/// neighbour route it clears beside the few that tell of the change; the
/// default buffer holds about forty.
const BUFFER: usize = 256 * 1024;

/// `<net/route.h>`'s message types. libc names them for macOS only; iOS runs
/// the same kernel, with the same values and header.
const RTM_NEWADDR: u8 = 0xc;
const RTM_DELADDR: u8 = 0xd;
const RTM_IFINFO: u8 = 0xe;

/// The kernel's routing socket, read without blocking.
pub(super) struct Routes(AsyncFd<Socket>);

impl Routes {
    /// A routing socket for the messages of every address family.
    pub(super) fn open() -> io::Result<Self> {
        let socket = Socket::new(
            Domain::from(libc::PF_ROUTE),
            Type::from(libc::SOCK_RAW),
            None,
        )?;
        // Best effort: the safety reading still finds what a drop hides.
        if let Err(error) = socket.set_recv_buffer_size(BUFFER) {
            tracing::debug!(event = "network_route_buffer_unchanged", %error);
        }
        Self::from_socket(socket)
    }

    fn from_socket(socket: Socket) -> io::Result<Self> {
        socket.set_nonblocking(true)?;
        // Nothing is written to it: no wake for room to write.
        Ok(Self(AsyncFd::with_interest(socket, Interest::READABLE)?))
    }

    /// Wait for messages, then read those waiting, up to [`DRAIN`]. Cancel
    /// safe: nothing is read before the socket is readable, and from then on
    /// it completes without waiting. Stopping at [`DRAIN`] leaves the socket
    /// marked readable, so the next call reads on without waiting.
    async fn next(&self, buffer: &mut [u8]) -> io::Result<Messages> {
        let mut guard = self.0.readable().await?;
        let mut messages = Messages::default();
        for _ in 0..DRAIN {
            match guard.try_io(|socket| socket.get_ref().read(buffer)) {
                // Every message has a header: nothing at all means the socket
                // was shut, and reading on would spin.
                Ok(Ok(0)) => return Err(io::ErrorKind::UnexpectedEof.into()),
                Ok(Ok(read)) => {
                    messages.read += 1;
                    messages.change |= buffer.get(..read).is_some_and(tells_of_change);
                }
                Ok(Err(error)) if error.kind() == io::ErrorKind::Interrupted => {}
                // Some kernels report messages they dropped for this socket;
                // one of them may have told of a change.
                Ok(Err(error)) if error.raw_os_error() == Some(libc::ENOBUFS) => {
                    messages.change = true;
                }
                Ok(Err(error)) => return Err(error),
                Err(_would_block) => break,
            }
        }
        // Darwin drops without a word: a wake that found the socket still
        // full may have lost a message that told of a change.
        messages.change |= messages.read == DRAIN;
        Ok(messages)
    }
}

/// What one wake read.
#[derive(Debug, Default)]
struct Messages {
    read: usize,
    /// Whether one of them told of an address or interface change.
    change: bool,
}

/// Whether routing messages tell of an address added or removed, or an
/// interface going up or down; not of routes, which change with every
/// neighbour the system learns. Each message starts with its length (`u16`,
/// in the system's byte order), its version and its type. A read holds one;
/// any more are walked by their lengths.
fn tells_of_change(mut messages: &[u8]) -> bool {
    while let [low, high, _version, kind, ..] = *messages {
        if matches!(kind, RTM_NEWADDR | RTM_DELADDR | RTM_IFINFO) {
            return true;
        }
        let length = usize::from(u16::from_ne_bytes([low, high]));
        match messages.get(length..) {
            Some(rest) if length >= 4 => messages = rest,
            _ => break,
        }
    }
    false
}

/// When to read the addresses: once the socket goes quiet after a message
/// that tells of a change, and every [`SAFETY`] regardless.
#[derive(Debug)]
struct Schedule {
    /// When the addresses were last read.
    read_at: Instant,
    /// The first and the last message since then that told of a change.
    messages: Option<(Instant, Instant)>,
}

impl Schedule {
    fn new(now: Instant) -> Self {
        Self {
            read_at: now,
            messages: None,
        }
    }

    /// A message that tells of a change arrived at `now`.
    fn message(&mut self, now: Instant) {
        let first = self.messages.map_or(now, |(first, _)| first);
        self.messages = Some((first, now));
    }

    /// When the next reading is due.
    fn due(&self) -> Instant {
        match self.messages {
            Some((first, last)) => (last + QUIET).min(first + LONGEST_WAIT),
            None => self.read_at + SAFETY,
        }
    }

    /// The addresses were read at `now`.
    fn read(&mut self, now: Instant) {
        self.read_at = now;
        // A reading the longest wait forced while messages still came may
        // have run before the last of them took effect: read once more when
        // the socket goes quiet.
        self.messages = self
            .messages
            .filter(|&(_, last)| now < last + QUIET)
            .map(|(_, last)| (now, last));
    }
}

/// Raise a hint whenever the addresses change, reading them when a message
/// tells of a change and every [`SAFETY`] otherwise. Returns only when the
/// socket or a reading fails, for the caller to poll instead.
pub(super) async fn watch(
    routes: &Routes,
    changes: &watch::Sender<u64>,
    addresses: &mut Addresses,
) -> io::Result<Infallible> {
    // The socket is open before this first reading, so it holds a message for
    // any change after it.
    addresses.read().await?;
    let mut schedule = Schedule::new(Instant::now());
    let mut buffer = [0; MESSAGE];
    loop {
        tokio::select! {
            messages = routes.next(&mut buffer) => {
                let messages = messages?;
                tracing::trace!(
                    event = "network_route_messages",
                    read = messages.read,
                    change = messages.change
                );
                if messages.change {
                    schedule.message(Instant::now());
                }
                continue;
            }
            () = tokio::time::sleep_until(schedule.due()) => {}
        }
        schedule.read(Instant::now());
        let changed = addresses.read().await?;
        tracing::trace!(event = "network_addresses_read", changed);
        if changed {
            tracing::debug!(event = "network_addresses_changed");
            raise(changes);
        }
    }
}

// The tests build the kernel's headers with libc's macOS-only names.
#[cfg(test)]
#[cfg(target_os = "macos")]
mod tests {
    use super::*;
    use std::io::Write;
    use std::net::{IpAddr, Ipv4Addr};
    use std::os::unix::net::UnixDatagram;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A routing message's header: the `length` it claims, its version and
    /// type, then zeros up to that length.
    fn message(kind: libc::c_int, length: u16) -> Vec<u8> {
        let mut message = vec![0; usize::from(length).max(4)];
        message[..2].copy_from_slice(&length.to_ne_bytes());
        message[2] = libc::RTM_VERSION as u8;
        message[3] = kind as u8;
        message
    }

    /// Addresses and interfaces going up or down are changes; routes, which
    /// come and go with neighbours, are not. A read is walked message by
    /// message, and a length that would loop or overrun ends the walk.
    /// The local message types are libc's on macOS.
    #[test]
    fn the_message_types_match_the_kernels() {
        assert_eq!(libc::c_int::from(RTM_NEWADDR), libc::RTM_NEWADDR);
        assert_eq!(libc::c_int::from(RTM_DELADDR), libc::RTM_DELADDR);
        assert_eq!(libc::c_int::from(RTM_IFINFO), libc::RTM_IFINFO);
    }

    #[test]
    fn only_address_and_interface_messages_tell_of_a_change() {
        for kind in [libc::RTM_NEWADDR, libc::RTM_DELADDR, libc::RTM_IFINFO] {
            assert!(tells_of_change(&message(kind, 120)));
        }
        for kind in [
            libc::RTM_ADD,
            libc::RTM_DELETE,
            libc::RTM_GET,
            libc::RTM_MISS,
        ] {
            assert!(!tells_of_change(&message(kind, 120)));
        }
        let two = [message(libc::RTM_ADD, 92), message(libc::RTM_NEWADDR, 40)].concat();
        assert!(tells_of_change(&two));
        assert!(!tells_of_change(&[]));
        assert!(!tells_of_change(&message(libc::RTM_ADD, 4)[..3]));
        let looping = [message(libc::RTM_ADD, 0), message(libc::RTM_NEWADDR, 40)].concat();
        assert!(!tells_of_change(&looping));
        let overrun = [
            &message(libc::RTM_ADD, 200)[..92],
            &message(libc::RTM_NEWADDR, 40),
        ]
        .concat();
        assert!(!tells_of_change(&overrun));
    }

    /// One message is read once the socket has been quiet for [`QUIET`]; a
    /// run of them once it ends, but never later than [`LONGEST_WAIT`] after
    /// its first, and then once more when it goes quiet. Without messages
    /// the addresses are read every [`SAFETY`].
    #[test]
    fn readings_wait_for_quiet_but_not_forever() {
        let start = Instant::now();
        let mut schedule = Schedule::new(start);
        assert_eq!(schedule.due(), start + SAFETY);

        let at = start + Duration::from_secs(10);
        schedule.message(at);
        assert_eq!(schedule.due(), at + QUIET);
        schedule.message(at + Duration::from_millis(100));
        assert_eq!(schedule.due(), at + Duration::from_millis(100) + QUIET);
        schedule.read(schedule.due());
        assert_eq!(
            schedule.due(),
            at + Duration::from_millis(100) + QUIET + SAFETY
        );

        let storm = start + Duration::from_secs(20);
        let mut now = storm;
        while now < storm + LONGEST_WAIT {
            schedule.message(now);
            now += Duration::from_millis(100);
        }
        assert_eq!(schedule.due(), storm + LONGEST_WAIT);
        let last = now - Duration::from_millis(100);
        schedule.read(storm + LONGEST_WAIT);
        assert_eq!(schedule.due(), last + QUIET);
        schedule.read(last + QUIET);
        assert_eq!(schedule.due(), last + QUIET + SAFETY);
    }

    /// A real routing socket opens and reads what the kernel writes to it.
    /// The kernel sends a request's answer to every routing socket, the
    /// sender's own included, so asking for the loopback route needs no
    /// network change.
    #[tokio::test]
    async fn a_routing_socket_reads_the_kernels_messages() {
        let routes = Routes::open().unwrap();
        let header = std::mem::size_of::<libc::rt_msghdr>();
        let mut request = message(libc::RTM_GET, (header + 16) as u16);
        let addrs = std::mem::offset_of!(libc::rt_msghdr, rtm_addrs);
        request[addrs..addrs + 4].copy_from_slice(&libc::RTA_DST.to_ne_bytes());
        let seq = std::mem::offset_of!(libc::rt_msghdr, rtm_seq);
        request[seq..seq + 4].copy_from_slice(&1i32.to_ne_bytes());
        // sockaddr_in for 127.0.0.1: length, family, port, address.
        let destination = &mut request[header..];
        destination[0] = 16;
        destination[1] = libc::AF_INET as u8;
        destination[4..8].copy_from_slice(&Ipv4Addr::LOCALHOST.octets());
        routes.0.get_ref().write_all(&request).unwrap();

        let mut buffer = [0; MESSAGE];
        let messages = tokio::time::timeout(Duration::from_secs(10), routes.next(&mut buffer))
            .await
            .unwrap()
            .unwrap();
        assert!(messages.read >= 1);
    }

    static SYSTEM: Mutex<Vec<(String, IpAddr)>> = Mutex::new(Vec::new());
    static READINGS: AtomicUsize = AtomicUsize::new(0);

    fn fake_system() -> io::Result<super::super::Snapshot> {
        READINGS.fetch_add(1, Ordering::SeqCst);
        Ok(SYSTEM.lock().unwrap().clone())
    }

    fn set_system(last: u8) {
        *SYSTEM.lock().unwrap() =
            vec![("en0".to_owned(), IpAddr::V4(Ipv4Addr::new(10, 0, 0, last)))];
    }

    /// The watcher sleeps until a message tells of a change, reads the
    /// addresses once the socket goes quiet, and raises one hint when they
    /// changed; a route message alone reads nothing. A change the socket
    /// never told of is found by the safety reading.
    #[tokio::test(start_paused = true)]
    async fn a_message_leads_to_one_reading_and_one_hint() {
        let (kernel, ours) = UnixDatagram::pair().unwrap();
        let routes = Routes::from_socket(Socket::from(std::os::fd::OwnedFd::from(ours))).unwrap();
        let (changes, mut hints) = watch::channel(0);
        set_system(1);
        let watcher = tokio::spawn(async move {
            let mut addresses = Addresses::new(fake_system);
            let Err(error) = watch(&routes, &changes, &mut addresses).await;
            error
        });
        let readings = || READINGS.load(Ordering::SeqCst);

        // The paused clock moves only once the watcher waits on its socket
        // and timer, past its first reading.
        kernel.send(&message(libc::RTM_ADD, 92)).unwrap();
        // A timer of the test's own, so the park that delivers the message
        // advances the paused clock only to it, never to the watcher's
        // safety deadline.
        tokio::time::sleep(Duration::from_millis(1)).await;
        tokio::time::sleep(Duration::from_secs(5)).await;
        assert_eq!(readings(), 1, "a route message reads nothing");

        set_system(2);
        let sent = Instant::now();
        kernel.send(&message(libc::RTM_NEWADDR, 40)).unwrap();
        kernel.send(&message(libc::RTM_IFINFO, 40)).unwrap();
        tokio::time::sleep(Duration::from_millis(1)).await;
        hints.changed().await.unwrap();
        assert_eq!(*hints.borrow_and_update(), 1);
        assert_eq!(readings(), 2, "one reading for the run");
        assert!(sent.elapsed() >= QUIET);

        set_system(3);
        hints.changed().await.unwrap();
        assert_eq!(*hints.borrow_and_update(), 2);
        assert!(sent.elapsed() >= SAFETY);
        assert_eq!(readings(), 3);
        watcher.abort();
    }
}
