//! The TCP half of a direct path within a local network: the provider's
//! acceptor, the consumer's dialer, and the stream both ends forward over.
//! QUIC stays in `endpoint.rs`. What the two share is everything that makes a
//! path trustworthy: the per-session identity, the binding to the grant, and
//! the commit bytes.
//!
//! TCP is offered only on a network both ends share, so a path across the
//! internet never takes it. Unlike QUIC, whose probes open the provider's
//! own firewall for the consumer's packets, TCP needs the provider's firewall
//! to let it in: a consumer that asked for it falls back to QUIC within the
//! head start when it does not get through.

use super::{
    endpoint::{Arrival, Exporter},
    identity::Identity,
    lan::Interfaces,
};
use crate::diagnostics::LanTcpMiss;
use pb_mapper_protocol::direct::MAX_LAN_TCP;
use quinn::rustls::{self, pki_types::ServerName};
use std::{
    io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll, ready},
    time::Duration,
};
use tokio::{
    io::{AsyncWrite, ReadHalf, WriteHalf},
    net::{TcpListener, TcpStream},
    sync::mpsc,
    task::JoinSet,
    time::Instant,
};
use tokio_rustls::{TlsAcceptor, TlsConnector, TlsStream};
use tokio_util::task::AbortOnDropHandle;

/// One direct session's connection over TCP: TLS 1.3, mutually
/// authenticated, the same type at both ends.
pub(super) type TlsTcp = TlsStream<TcpStream>;

/// How long a TCP path is tried alone, with LAN-first on, before QUIC paths
/// are dialed too. A LAN connect and a TLS 1.3 handshake take a few
/// milliseconds; a SYN a firewall drops is not retried for about a second,
/// so waiting longer would only delay the QUIC fallback.
pub(super) const TCP_HEAD_START: Duration = Duration::from_millis(300);

/// Handshakes an acceptor runs at once, and how long each may take: as the
/// QUIC endpoint's.
const MAX_HANDSHAKES: usize = 64;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(3);

/// Keep-alive on both ends, as QUIC's 15 s: a peer gone without a word is
/// noticed rather than waited for.
const KEEP_ALIVE: Duration = Duration::from_secs(15);

/// The name both ends' certificates carry.
const SERVER_NAME: &str = "pb-mapper.invalid";

impl Exporter for TlsTcp {
    const BINDING: &'static [u8] = b"pb-mapper/direct-tcp/1";

    fn export(&self, out: &mut [u8], label: &[u8], context: &[u8]) -> io::Result<()> {
        let exported = match self {
            TlsStream::Client(stream) => {
                stream
                    .get_ref()
                    .1
                    .export_keying_material(out, label, Some(context))
            }
            TlsStream::Server(stream) => {
                stream
                    .get_ref()
                    .1
                    .export_keying_material(out, label, Some(context))
            }
        };
        exported
            .map(|_| ())
            .map_err(|_| io::Error::other("TLS exporter unavailable"))
    }
}

/// A session's stream in halves for forwarding: the write half completes a
/// write only once its records are on the socket.
pub(super) fn split(stream: TlsTcp) -> (ReadHalf<TlsTcp>, WriteThrough<WriteHalf<TlsTcp>>) {
    let (read, write) = tokio::io::split(stream);
    (read, WriteThrough::new(write))
}

/// A TLS writer whose writes complete once their records reached the
/// socket. tokio-rustls returns from a write with records still buffered
/// when the socket is full, and sends them with the next write or flush. The
/// forwarding loop never flushes, so the last bytes of a burst would wait
/// for a write that may never come.
///
/// A write that has handed bytes to TLS and is still flushing them answers
/// `Pending`, and is finished by the next call, which must repeat the same
/// bytes; `write_all`, the only caller, does.
pub(super) struct WriteThrough<W> {
    inner: W,
    /// Bytes handed to TLS whose flush has not finished.
    written: Option<usize>,
}

impl<W> WriteThrough<W> {
    pub fn new(inner: W) -> Self {
        Self {
            inner,
            written: None,
        }
    }
}

impl<W: AsyncWrite + Unpin> AsyncWrite for WriteThrough<W> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = &mut *self;
        let written = match this.written {
            Some(written) => written,
            None => {
                let written = ready!(Pin::new(&mut this.inner).poll_write(cx, buf))?;
                this.written = Some(written);
                written
            }
        };
        let flushed = ready!(Pin::new(&mut this.inner).poll_flush(cx));
        this.written = None;
        Poll::Ready(flushed.map(|()| written))
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

/// The certificate the other end presented, when it presented exactly one.
pub(super) fn peer_certificate(stream: &TlsTcp) -> Option<&[u8]> {
    match stream.get_ref().1.peer_certificates()? {
        [certificate] => Some(certificate.as_ref()),
        _ => None,
    }
}

/// The addresses a connection runs between: the peer's, and this end's.
pub(super) fn addresses(stream: &TlsTcp) -> (io::Result<SocketAddr>, Option<IpAddr>) {
    let tcp = stream.get_ref().0;
    (tcp.peer_addr(), tcp.local_addr().ok().map(|addr| addr.ip()))
}

/// What the acceptor lets reach TLS.
#[derive(Default)]
struct Admission {
    /// Sessions that offered TCP and have not settled.
    pending: usize,
    /// This end's interfaces, as the latest of them discovered them.
    interfaces: Interfaces,
}

impl Admission {
    /// A connection gets to TLS only while a session waits for one, and only
    /// from a network of this end's: the listener's wildcard address does not
    /// make it reachable from anywhere else.
    fn admits(&self, loopback: bool, source: IpAddr) -> bool {
        let source = source.to_canonical();
        self.pending > 0
            && if loopback {
                source.is_loopback()
            } else {
                self.interfaces.network_of(source).is_some()
            }
    }
}

/// A provider's TCP listeners for direct sessions, one per address family,
/// opened the first time a consumer on a shared network asks for TCP and
/// closed with the endpoint.
pub(super) struct Acceptor {
    /// Each listener's address.
    local: Vec<SocketAddr>,
    /// Whether they listen on loopback alone, for a consumer on this
    /// computer under a relay on loopback: tests, and a local check, then
    /// never raise a firewall prompt.
    loopback: bool,
    admission: Arc<Mutex<Admission>>,
    _listeners: Vec<AbortOnDropHandle<()>>,
}

impl Acceptor {
    /// Listen for each family the UDP direct sockets at `udp` use, on the same
    /// port number where it is free, so one number is direct's on both
    /// protocols. Accepted connections go to `arrivals` as the QUIC endpoint's
    /// do.
    pub fn open(
        udp: &[SocketAddr],
        loopback: bool,
        identity: &Identity,
        arrivals: mpsc::Sender<Arrival>,
    ) -> io::Result<Self> {
        let acceptor = TlsAcceptor::from(identity.tcp_server()?);
        let admission = Arc::new(Mutex::new(Admission::default()));
        let mut local = Vec::new();
        let mut listeners = Vec::new();
        for udp in udp {
            let ip = match (udp.is_ipv4(), loopback) {
                (true, true) => IpAddr::V4(Ipv4Addr::LOCALHOST),
                (true, false) => IpAddr::V4(Ipv4Addr::UNSPECIFIED),
                (false, true) => IpAddr::V6(Ipv6Addr::LOCALHOST),
                (false, false) => IpAddr::V6(Ipv6Addr::UNSPECIFIED),
            };
            let listener = match listen(SocketAddr::new(ip, udp.port()))
                .or_else(|_| listen(SocketAddr::new(ip, 0)))
            {
                Ok(listener) => listener,
                Err(error) if udp.is_ipv6() => {
                    tracing::debug!(%error, "IPv6 direct TCP listener unavailable");
                    continue;
                }
                Err(error) => return Err(error),
            };
            local.push(listener.local_addr()?);
            listeners.push(AbortOnDropHandle::new(tokio::spawn(accept(
                listener,
                loopback,
                acceptor.clone(),
                admission.clone(),
                arrivals.clone(),
            ))));
        }
        if local.is_empty() {
            return Err(io::Error::other("no direct TCP listener"));
        }
        tracing::debug!(?local, "direct TCP listening");
        Ok(Self {
            local,
            loopback,
            admission,
            _listeners: listeners,
        })
    }

    /// Offer TCP to one session: the addresses to reach this end at on the
    /// networks it shares, as `interfaces` lists them, and the guard that
    /// keeps the listeners admitting connections until it drops.
    pub fn offer(&self, interfaces: &Interfaces) -> (Offered, Vec<SocketAddr>) {
        let ips = if self.loopback {
            vec![
                IpAddr::V4(Ipv4Addr::LOCALHOST),
                IpAddr::V6(Ipv6Addr::LOCALHOST),
            ]
        } else {
            interfaces.lan_addrs()
        };
        let mut addrs: Vec<_> = ips
            .into_iter()
            .filter_map(|ip| {
                let port = self.local.iter().find(|l| l.is_ipv4() == ip.is_ipv4())?;
                Some(SocketAddr::new(ip, port.port()))
            })
            .collect();
        addrs.truncate(MAX_LAN_TCP);
        let mut admission = self.admission.lock().unwrap_or_else(|e| e.into_inner());
        admission.pending += 1;
        admission.interfaces = interfaces.clone();
        drop(admission);
        (
            Offered {
                admission: self.admission.clone(),
            },
            addrs,
        )
    }
}

/// A session's claim on the acceptor: while any is held, connections from
/// this end's networks reach TLS.
pub(super) struct Offered {
    admission: Arc<Mutex<Admission>>,
}

impl Drop for Offered {
    fn drop(&mut self) {
        let mut admission = self.admission.lock().unwrap_or_else(|e| e.into_inner());
        admission.pending = admission.pending.saturating_sub(1);
    }
}

fn listen(addr: SocketAddr) -> io::Result<TcpListener> {
    use socket2::{Domain, Protocol, Socket, Type};
    let socket = Socket::new(Domain::for_address(addr), Type::STREAM, Some(Protocol::TCP))?;
    if addr.is_ipv6() {
        // The IPv4 listener has its own socket on the same port.
        socket.set_only_v6(true)?;
    }
    // As Tokio's own listeners: a restart need not wait out TIME_WAIT. Not on
    // Windows, where it would let another socket take the port over.
    #[cfg(unix)]
    socket.set_reuse_address(true)?;
    socket.set_nonblocking(true)?;
    socket.bind(&addr.into())?;
    socket.listen(MAX_HANDSHAKES as i32)?;
    TcpListener::from_std(socket.into())
}

/// Accept on one listener for as long as it lives.
async fn accept(
    listener: TcpListener,
    loopback: bool,
    acceptor: TlsAcceptor,
    admission: Arc<Mutex<Admission>>,
    arrivals: mpsc::Sender<Arrival>,
) {
    let mut handshakes = JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, source) = match accepted {
                    Ok(accepted) => accepted,
                    Err(error) => {
                        // Out of descriptors, say: let some close before the next.
                        tracing::debug!(%error, "direct TCP accept failed");
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        continue;
                    }
                };
                let admitted = admission
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .admits(loopback, source.ip());
                if !admitted || handshakes.len() >= MAX_HANDSHAKES {
                    tracing::debug!(%source, admitted, "direct TCP connection dropped before TLS");
                    continue;
                }
                let acceptor = acceptor.clone();
                let arrivals = arrivals.clone();
                handshakes.spawn(async move {
                    configure(&stream);
                    let started = Instant::now();
                    match tokio::time::timeout(HANDSHAKE_TIMEOUT, acceptor.accept(stream)).await {
                        Ok(Ok(stream)) => {
                            // The server's handshake ends with the client's
                            // Finished: one round trip after its own flight.
                            let rtt = started.elapsed();
                            let stream = Box::new(TlsStream::Server(stream));
                            let _ = arrivals.try_send(Arrival::Tcp { stream, rtt });
                        }
                        result => tracing::debug!(?result, %source, "direct TCP TLS accept failed"),
                    }
                });
            }
            Some(_) = handshakes.join_next(), if !handshakes.is_empty() => {}
        }
    }
}

/// Small frames, a desktop's input among them, go at once; and a peer that
/// vanished is found out.
fn configure(stream: &TcpStream) {
    let _ = stream.set_nodelay(true);
    let keep_alive = socket2::TcpKeepalive::new()
        .with_time(KEEP_ALIVE)
        .with_interval(KEEP_ALIVE);
    let _ = socket2::SockRef::from(stream).set_tcp_keepalive(&keep_alive);
}

/// A TCP path that connected: the stream, the round trip its connect took,
/// and its whole handshake, TLS included.
pub(super) struct Won {
    pub stream: Box<TlsTcp>,
    pub rtt: Duration,
    pub handshake: Duration,
}

/// Dial a provider's TCP address and complete TLS with this end's identity,
/// trusting only `config`'s one certificate.
async fn dial(addr: SocketAddr, config: Arc<rustls::ClientConfig>) -> Result<Won, LanTcpMiss> {
    let started = Instant::now();
    let stream = TcpStream::connect(addr).await.map_err(|error| {
        tracing::debug!(%error, %addr, "direct TCP connect failed");
        match error.kind() {
            io::ErrorKind::TimedOut => LanTcpMiss::TimedOut,
            _ => LanTcpMiss::Refused,
        }
    })?;
    let rtt = started.elapsed();
    configure(&stream);
    let name = ServerName::try_from(SERVER_NAME).map_err(|_| LanTcpMiss::Handshake)?;
    let stream = TlsConnector::from(config)
        .connect(name, stream)
        .await
        .map_err(|error| {
            tracing::debug!(%error, %addr, "direct TCP TLS handshake failed");
            LanTcpMiss::Handshake
        })?;
    Ok(Won {
        stream: Box::new(TlsStream::Client(stream)),
        rtt,
        handshake: started.elapsed(),
    })
}

/// A setup's TCP attempts, one per address of the provider's on a network
/// this end shares, and how they went.
pub(super) struct Race {
    attempts: JoinSet<Result<Won, LanTcpMiss>>,
    /// Whether this end asked for TCP at all.
    asked: bool,
    /// Why TCP is not the path, as far as is known.
    miss: Option<LanTcpMiss>,
    /// The end of the head start.
    until: Instant,
    /// When the last attempt finished, once every one has.
    settled: Option<Instant>,
    /// Attempts started, and whether one connected.
    dialed: u32,
    won: bool,
}

impl Race {
    /// No TCP: this end did not ask.
    pub fn idle() -> Self {
        Self {
            attempts: JoinSet::new(),
            asked: false,
            miss: None,
            until: Instant::now(),
            settled: None,
            dialed: 0,
            won: false,
        }
    }

    /// Asked, and there is nothing to dial, for `reason`.
    pub fn missed(reason: LanTcpMiss) -> Self {
        Self {
            asked: true,
            miss: Some(reason),
            ..Self::idle()
        }
    }

    /// Dial each of `addrs` at once, with `config`.
    pub fn start(addrs: &[SocketAddr], config: Arc<rustls::ClientConfig>) -> Self {
        let mut attempts = JoinSet::new();
        for &addr in addrs {
            attempts.spawn(dial(addr, config.clone()));
        }
        Self {
            attempts,
            asked: true,
            miss: None,
            until: Instant::now() + TCP_HEAD_START,
            settled: None,
            dialed: addrs.len() as u32,
            won: false,
        }
    }

    /// Attempts started, which count among a setup's handshakes.
    pub fn dialed(&self) -> u32 {
        self.dialed
    }

    /// Whether an attempt is still in flight.
    pub fn pending(&self) -> bool {
        !self.attempts.is_empty()
    }

    /// Until when every QUIC path waits, with LAN-first on: the head start,
    /// while an attempt is in flight. Off, TCP and QUIC race as equals.
    pub fn holds(&self, lan_first: bool, now: Instant) -> Option<Instant> {
        (lan_first && self.pending() && now < self.until).then_some(self.until)
    }

    /// When the head start ended, with LAN-first on and attempts made: at
    /// its end, or when the last attempt failed before then. QUIC paths off
    /// the LAN wait for QUIC on it from then, not from their own answer,
    /// which may have come while TCP held every QUIC path back.
    pub fn released(&self, lan_first: bool) -> Option<Instant> {
        (lan_first && self.dialed > 0).then(|| {
            self.settled
                .map_or(self.until, |settled| settled.min(self.until))
        })
    }

    /// The next attempt to finish, or never when none is in flight.
    pub async fn next(&mut self) -> Result<Won, LanTcpMiss> {
        let finished = match self.attempts.join_next().await {
            Some(joined) => joined.unwrap_or(Err(LanTcpMiss::Handshake)),
            None => std::future::pending().await,
        };
        if self.attempts.is_empty() {
            self.settled = Some(Instant::now());
        }
        self.won |= finished.is_ok();
        finished
    }

    /// Note an attempt that failed. A handshake failure says the most, that
    /// the provider was reached; otherwise the first reason stands.
    pub fn record(&mut self, miss: LanTcpMiss) {
        self.miss = match self.miss {
            Some(LanTcpMiss::Handshake) => Some(LanTcpMiss::Handshake),
            Some(_) if miss == LanTcpMiss::Handshake => Some(miss),
            Some(earlier) => Some(earlier),
            None => Some(miss),
        };
    }

    /// Why a session that asked for TCP did not get it, as of `now`: none
    /// when it did not ask, or did. An attempt still in flight when another
    /// path won missed only once its head start was over: before then, with
    /// LAN-first off, QUIC just won a fair race. After, it timed out.
    pub fn outcome(&self, now: Instant) -> Option<LanTcpMiss> {
        if !self.asked || self.won || (self.pending() && now < self.until) {
            return None;
        }
        self.miss.or(self.pending().then_some(LanTcpMiss::TimedOut))
    }

    /// Whether the outcome is one that attempts paid for, so later sessions
    /// should not try again at once: a provider that refuses TCP, or whose
    /// firewall drops it, does so for every session.
    pub fn costly(miss: LanTcpMiss) -> bool {
        matches!(
            miss,
            LanTcpMiss::Refused | LanTcpMiss::TimedOut | LanTcpMiss::Handshake
        )
    }
}

/// The addresses of `offered` this end may dial: those on a network it
/// shares, as `is_lan` judges, or loopback when the relay is on it.
pub(super) fn targets(
    offered: &[SocketAddr],
    is_lan: impl Fn(SocketAddr) -> bool,
    loopback: bool,
) -> Vec<SocketAddr> {
    offered
        .iter()
        .copied()
        .filter(|addr| is_lan(*addr) || (loopback && addr.ip().is_loopback()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// With LAN-first off, QUIC can win a fair race while TCP is still
    /// connecting: TCP missed nothing, and is not paused, until its head
    /// start is over, even if another of its attempts was refused.
    #[tokio::test]
    async fn an_attempt_in_flight_misses_only_once_its_head_start_is_over() {
        let mut attempts = JoinSet::new();
        attempts.spawn(std::future::pending::<Result<Won, LanTcpMiss>>());
        let mut race = Race {
            attempts,
            asked: true,
            until: Instant::now() + TCP_HEAD_START,
            dialed: 1,
            ..Race::idle()
        };
        let early = race.until - Duration::from_millis(1);
        assert_eq!(race.outcome(early), None);
        assert_eq!(race.outcome(race.until), Some(LanTcpMiss::TimedOut));
        race.record(LanTcpMiss::Refused);
        assert_eq!(race.outcome(early), None);
        assert_eq!(race.outcome(race.until), Some(LanTcpMiss::Refused));
    }
}
