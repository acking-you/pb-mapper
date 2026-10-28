//! One UDP socket per address family carries QUIC, STUN and authenticated
//! probes. Quinn's endpoint driver is its only reader: control datagrams are
//! handled and removed in place inside the received batch, so QUIC packets
//! reach Quinn with no extra task, channel, allocation or copy.
use pb_mapper_protocol::direct::{MAX_CANDIDATES, SessionId, SessionSecret, valid_candidate};
#[cfg(test)]
use quinn::udp::Transmit;
use quinn::{AsyncUdpSocket, UdpSender, udp::RecvMeta};
use ring::hmac;
use std::{
    collections::HashMap,
    io::{self, IoSliceMut},
    net::SocketAddr,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll, ready},
    time::Duration,
};
use stun_proto::{
    Instant,
    agent::StunAgent,
    types::{
        TransportType,
        attribute::XorMappedAddress,
        message::{BINDING, Message, MessageClass, MessageWrite, MessageWriteExt, MessageWriteVec},
    },
};
use tokio::sync::mpsc;

const PROBE: &[u8; 8] = b"\x10PBD\0\0\0\x01";
const PROBE_LEN: usize = 8 + 16 + 16 + 1 + 32;
const STUN_COOKIE: [u8; 4] = [0x21, 0x12, 0xa4, 0x42];
/// Probe replies are answered inline by the receive path; bound the work an
/// unauthenticated sender can cause per second.
const MAX_PROBE_REPLIES_PER_SECOND: u32 = 128;

/// Endpoint configuration for every direct endpoint. Both ends are
/// pb-mapper, so not advertising QUIC-bit greasing (RFC 9287) keeps the fixed
/// bit set on every QUIC packet: it alone separates QUIC from STUN and probes,
/// which always clear it.
pub(super) fn endpoint_config() -> quinn::EndpointConfig {
    let mut config = quinn::EndpointConfig::default();
    config.grease_quic_bit(false);
    config
}

struct ProbeState {
    key: SessionSecret,
    nonce: [u8; 16],
    expires: tokio::time::Instant,
    learned: Vec<SocketAddr>,
    /// Authenticated sources still awaiting our own nonce challenge.
    triggered: Vec<SocketAddr>,
}
struct StunPending {
    source: SocketAddr,
    tx: mpsc::Sender<Vec<u8>>,
}
struct ReplyBudget {
    window: tokio::time::Instant,
    used: u32,
}

/// State the receive path consults for non-QUIC datagrams.
struct Control {
    probes: Mutex<HashMap<SessionId, ProbeState>>,
    stun: Mutex<HashMap<[u8; 12], StunPending>>,
    replies: Mutex<ReplyBudget>,
}

#[derive(Clone)]
pub(super) struct Socket {
    inner: Arc<tokio::net::UdpSocket>,
    control: Arc<Control>,
    #[cfg(test)]
    block_quic: Arc<std::sync::atomic::AtomicBool>,
}
impl std::fmt::Debug for Socket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Socket")
            .field("local", &self.inner.local_addr())
            .finish()
    }
}

/// The socket Quinn sees. Sends pass straight through; receives are filtered.
pub(super) struct Demux {
    inner: Box<dyn AsyncUdpSocket>,
    control: Socket,
}
impl std::fmt::Debug for Demux {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DirectUdpSocket")
    }
}
impl AsyncUdpSocket for Demux {
    fn create_sender(&self) -> Pin<Box<dyn UdpSender>> {
        let sender = self.inner.create_sender();
        #[cfg(not(test))]
        return sender;
        #[cfg(test)]
        Box::pin(TestSender {
            inner: sender,
            blocked: self.control.block_quic.clone(),
        })
    }
    fn poll_recv(
        &mut self,
        cx: &mut Context,
        bufs: &mut [IoSliceMut<'_>],
        meta: &mut [RecvMeta],
    ) -> Poll<io::Result<usize>> {
        // Errors, including Windows' ICMP-induced `ConnectionReset` after a
        // probe to a stale candidate, are Quinn's to judge; it ignores that one.
        let count = ready!(self.inner.poll_recv(cx, bufs, meta))?;
        for (buf, meta) in bufs.iter_mut().zip(meta.iter_mut()).take(count) {
            // Quinn skips an entry whose length drops to zero.
            meta.len = self.retain_quic(&mut buf[..meta.len], meta);
        }
        Poll::Ready(Ok(count))
    }
    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.inner.local_addr()
    }
    fn max_receive_segments(&self) -> usize {
        self.inner.max_receive_segments()
    }
    fn may_fragment(&self) -> bool {
        self.inner.may_fragment()
    }
}
impl Demux {
    /// Handle control segments of one (possibly GRO-coalesced) datagram and
    /// compact the QUIC segments to the front. Returns the retained length.
    ///
    /// Every segment but the last is `stride` long, and the retained ones
    /// keep their order, so the result still satisfies that invariant.
    fn retain_quic(&self, data: &mut [u8], meta: &RecvMeta) -> usize {
        let stride = meta.stride.clamp(1, data.len().max(1));
        if data.chunks(stride).all(is_quic) {
            return data.len();
        }
        let mut kept = 0;
        let mut offset = 0;
        while offset < data.len() {
            let end = (offset + stride).min(data.len());
            if is_quic(&data[offset..end]) {
                data.copy_within(offset..end, kept);
                kept += end - offset;
            } else {
                self.control.handle_control(&data[offset..end], meta.addr);
            }
            offset = end;
        }
        kept
    }
}
impl Socket {
    fn handle_control(&self, data: &[u8], source: SocketAddr) {
        if data.len() >= 20 && data[0] & 0xc0 == 0 && data[4..8] == STUN_COOKIE {
            self.handle_stun(data, source);
        } else if data.len() == PROBE_LEN && data.starts_with(PROBE) {
            self.handle_probe(data, source);
        }
    }
    fn handle_stun(&self, data: &[u8], source: SocketAddr) {
        if data.len() != 20 + usize::from(u16::from_be_bytes([data[2], data[3]])) {
            return;
        }
        let Ok(id) = <[u8; 12]>::try_from(&data[8..20]) else {
            return;
        };
        let stun = self.control.stun.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(pending) = stun.get(&id).filter(|p| p.source == source) {
            let _ = pending.tx.try_send(data.to_vec());
        }
    }
    fn handle_probe(&self, data: &[u8], source: SocketAddr) {
        if !valid_candidate(&source) {
            return;
        }
        let Ok(id) = <[u8; 16]>::try_from(&data[8..24]) else {
            return;
        };
        let reply = {
            let mut probes = self
                .control
                .probes
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let Some(probe) = probes
                .get_mut(&id)
                .filter(|p| p.expires > tokio::time::Instant::now())
            else {
                return;
            };
            let key = hmac::Key::new(hmac::HMAC_SHA256, &probe.key.0);
            if hmac::verify(&key, &data[..41], &data[41..]).is_err() {
                return;
            }
            match data[40] {
                0 if self.reply_allowed() => {
                    // A destination-dependent NAT may send from a different
                    // mapping than STUN found. Replying alone only validates
                    // the path for the sender. Challenge this source on our
                    // next probe tick before it can become a QUIC candidate.
                    if !probe.learned.contains(&source)
                        && !probe.triggered.contains(&source)
                        && probe.triggered.len() + probe.learned.len() < MAX_CANDIDATES
                    {
                        probe.triggered.push(source);
                    }
                    let nonce = data[24..40].try_into().unwrap_or([0; 16]);
                    packet(id, nonce, true, probe.key)
                }
                1 if data[24..40] == probe.nonce
                    && !probe.learned.contains(&source)
                    && probe.learned.len() < MAX_CANDIDATES =>
                {
                    probe.triggered.retain(|addr| *addr != source);
                    probe.learned.push(source);
                    probe
                        .triggered
                        .truncate(MAX_CANDIDATES - probe.learned.len());
                    return;
                }
                _ => return,
            }
        };
        let _ = self.inner.try_send_to(&reply, source);
    }
    fn reply_allowed(&self) -> bool {
        let mut budget = self
            .control
            .replies
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if budget.window.elapsed() > Duration::from_secs(1) {
            budget.window = tokio::time::Instant::now();
            budget.used = 0;
        }
        budget.used += 1;
        budget.used <= MAX_PROBE_REPLIES_PER_SECOND
    }
}
fn is_quic(segment: &[u8]) -> bool {
    segment.first().is_some_and(|byte| byte & 0x40 != 0)
}

impl Socket {
    pub fn bind(addr: SocketAddr) -> io::Result<(Self, Demux)> {
        let udp = std::net::UdpSocket::bind(addr)?;
        udp.set_nonblocking(true)?;
        // Share one socket and one reactor registration. Quinn owns its only
        // reader; control sends use Tokio's independent write waiters.
        let control_socket = Arc::new(tokio::net::UdpSocket::from_std(udp)?);
        let inner = quinn::TokioRuntime::wrap_tokio_udp_socket(control_socket.clone())?;
        let control = Arc::new(Control {
            probes: Mutex::new(HashMap::new()),
            stun: Mutex::new(HashMap::new()),
            replies: Mutex::new(ReplyBudget {
                window: tokio::time::Instant::now(),
                used: 0,
            }),
        });
        let socket = Self {
            inner: control_socket,
            control,
            #[cfg(test)]
            block_quic: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        };
        let quic = Demux {
            inner,
            control: socket.clone(),
        };
        Ok((socket, quic))
    }
    #[cfg(test)]
    pub fn block_quic(&self) {
        self.block_quic
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.inner.local_addr()
    }
    pub fn authorize(&self, id: SessionId, key: SessionSecret, expires: tokio::time::Instant) {
        let mut probes = self
            .control
            .probes
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        probes.retain(|_, p| p.expires > tokio::time::Instant::now());
        if probes.len() >= 1024 {
            return;
        }
        let nonce = pb_mapper_protocol::direct::random_session_id();
        probes.insert(
            id,
            ProbeState {
                key,
                nonce,
                expires,
                learned: Vec::new(),
                triggered: Vec::new(),
            },
        );
    }
    pub fn forget(&self, id: &SessionId) {
        self.control
            .probes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id);
    }
    /// Send one authenticated probe to each candidate of this address family
    /// and return the paths that have answered so far.
    pub fn probe(&self, id: SessionId, candidates: &[SocketAddr]) -> Vec<SocketAddr> {
        let (bytes, triggered, learned) = {
            let probes = self
                .control
                .probes
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let Some(probe) = probes
                .get(&id)
                .filter(|p| p.expires > tokio::time::Instant::now())
            else {
                return vec![];
            };
            (
                packet(id, probe.nonce, false, probe.key),
                probe.triggered.clone(),
                probe.learned.clone(),
            )
        };
        let family = self.local_addr().map(|local| local.is_ipv4());
        for addr in triggered
            .iter()
            .chain(candidates.iter().filter(|addr| !triggered.contains(addr)))
            .filter(|a| valid_candidate(a) && family.as_ref().is_ok_and(|v4| *v4 == a.is_ipv4()))
            .take(2 * MAX_CANDIDATES)
        {
            // Probes are retried every 120 ms. A congested or unreachable
            // candidate must never hold up cancellation or a healthy QUIC path.
            let _ = self.inner.try_send_to(&bytes, *addr);
        }
        learned
    }
    /// STUN Binding over this socket. Responses arrive through the Quinn
    /// endpoint driver, which runs for as long as the endpoint exists.
    pub async fn mapped_addr(&self, server: SocketAddr) -> io::Result<SocketAddr> {
        if self.local_addr()?.is_ipv4() != server.is_ipv4() {
            return Err(io::Error::other("STUN address family mismatch"));
        }
        let mut agent = StunAgent::builder(TransportType::Udp, self.local_addr()?).build();
        let mut message = Message::builder_request(BINDING, MessageWriteVec::new());
        message.add_fingerprint().map_err(io::Error::other)?;
        let bytes = message.finish();
        let txid: [u8; 12] = bytes[8..20].try_into().map_err(io::Error::other)?;
        let (tx, mut rx) = mpsc::channel(4);
        self.control
            .stun
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(txid, StunPending { source: server, tx });
        let _guard = StunGuard {
            control: self.control.clone(),
            id: txid,
        };
        let result = tokio::time::timeout(Duration::from_millis(700), async {
            let transmit = agent
                .send_request(bytes, server, Instant::from_std(std::time::Instant::now()))
                .map_err(io::Error::other)?;
            self.inner.send_to(&transmit.data, transmit.to).await?;
            loop {
                tokio::select! {
                    bytes = rx.recv() => {
                        let bytes = bytes.ok_or_else(|| io::Error::other("STUN channel closed"))?;
                        let message = Message::from_bytes(&bytes).map_err(io::Error::other)?;
                        if !message.has_method(BINDING) || !message.has_class(MessageClass::Success) {
                            continue;
                        }
                        let mapped = message
                            .attribute::<XorMappedAddress>()
                            .map_err(io::Error::other)?
                            .addr(message.transaction_id());
                        if mapped.is_ipv4() != server.is_ipv4()
                            || !pb_mapper_protocol::direct::valid_candidate(&mapped)
                        {
                            continue;
                        }
                        let now = Instant::from_std(std::time::Instant::now());
                        if agent.handle_stun_message_with_time(&message, server, now) {
                            return Ok(mapped);
                        }
                    }
                    () = tokio::time::sleep(Duration::from_millis(25)) => {
                        let now = Instant::from_std(std::time::Instant::now());
                        if let Some(transmit) = agent.poll_transmit(now) {
                            self.inner.send_to(transmit.data, transmit.to).await?;
                        }
                    }
                }
            }
        })
        .await;
        result.map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "STUN binding timed out"))?
    }
}
struct StunGuard {
    control: Arc<Control>,
    id: [u8; 12],
}
impl Drop for StunGuard {
    fn drop(&mut self) {
        self.control
            .stun
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.id);
    }
}

#[cfg(test)]
#[derive(Debug)]
struct TestSender {
    inner: Pin<Box<dyn UdpSender>>,
    blocked: Arc<std::sync::atomic::AtomicBool>,
}
#[cfg(test)]
impl UdpSender for TestSender {
    fn poll_send(
        mut self: Pin<&mut Self>,
        transmit: &Transmit<'_>,
        cx: &mut Context<'_>,
    ) -> Poll<io::Result<()>> {
        if self.blocked.load(std::sync::atomic::Ordering::Relaxed) {
            return Poll::Ready(Ok(()));
        }
        self.inner.as_mut().poll_send(transmit, cx)
    }
    fn max_transmit_segments(&self) -> usize {
        self.inner.max_transmit_segments()
    }
}
fn packet(id: SessionId, nonce: [u8; 16], pong: bool, secret: SessionSecret) -> [u8; PROBE_LEN] {
    let mut bytes = [0; PROBE_LEN];
    bytes[..8].copy_from_slice(PROBE);
    bytes[8..24].copy_from_slice(&id);
    bytes[24..40].copy_from_slice(&nonce);
    bytes[40] = u8::from(pong);
    let mac = hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, &secret.0), &bytes[..41]);
    bytes[41..].copy_from_slice(mac.as_ref());
    bytes
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use std::future::poll_fn;

    #[tokio::test]
    async fn authenticated_source_requires_our_return_path_challenge() {
        tokio::time::timeout(Duration::from_secs(3), async {
            let (consumer, _) = Socket::bind("127.0.0.1:0".parse().unwrap()).unwrap();
            let (provider, _) = Socket::bind("127.0.0.1:0".parse().unwrap()).unwrap();
            let id = [4; 16];
            let secret = SessionSecret([5; 32]);
            let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
            consumer.authorize(id, secret, deadline);
            provider.authorize(id, secret, deadline);
            consumer.inner.writable().await.unwrap();
            provider.inner.writable().await.unwrap();
            let mut buffer = [0; 1500];
            // Only the provider knows a working candidate. Its source address
            // was not advertised to the consumer (as with a changed NAT port).
            provider.probe(id, &[consumer.local_addr().unwrap()]);
            let (len, source) = consumer.inner.recv_from(&mut buffer).await.unwrap();
            consumer.handle_control(&buffer[..len], source);
            assert!(consumer.probe(id, &[]).is_empty());
            // The provider receives our Pong and then our own challenge.
            for _ in 0..2 {
                let (len, source) = provider.inner.recv_from(&mut buffer).await.unwrap();
                provider.handle_control(&buffer[..len], source);
            }
            let (len, source) = consumer.inner.recv_from(&mut buffer).await.unwrap();
            consumer.handle_control(&buffer[..len], source);
            assert_eq!(consumer.probe(id, &[]), [provider.local_addr().unwrap()]);
        })
        .await
        .unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn triggered_candidates_are_authenticated_bounded_and_expire() {
        let (socket, _) = Socket::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let id = [6; 16];
        let secret = SessionSecret([7; 32]);
        socket.authorize(
            id,
            secret,
            tokio::time::Instant::now() + Duration::from_secs(1),
        );
        let source = "127.0.0.1:9000".parse().unwrap();
        socket.handle_probe(&packet(id, [8; 16], false, SessionSecret([9; 32])), source);
        socket.handle_probe(&packet([0; 16], [8; 16], false, secret), source);
        assert!(
            socket.control.probes.lock().unwrap()[&id]
                .triggered
                .is_empty()
        );
        for port in 9000..9020 {
            socket.handle_probe(
                &packet(id, [8; 16], false, secret),
                SocketAddr::from(([127, 0, 0, 1], port)),
            );
        }
        assert_eq!(
            socket.control.probes.lock().unwrap()[&id].triggered.len(),
            MAX_CANDIDATES
        );
        // A valid MAC alone is not proof that our challenge reached this source.
        socket.handle_probe(&packet(id, [8; 16], true, secret), source);
        assert!(
            socket.control.probes.lock().unwrap()[&id]
                .learned
                .is_empty()
        );
        tokio::time::advance(Duration::from_secs(2)).await;
        let nonce = socket.control.probes.lock().unwrap()[&id].nonce;
        socket.handle_probe(&packet(id, nonce, true, secret), source);
        assert!(socket.probe(id, &[source]).is_empty());
        assert!(
            socket.control.probes.lock().unwrap()[&id]
                .learned
                .is_empty()
        );
        socket.forget(&id);
        socket.handle_probe(&packet(id, [8; 16], false, secret), source);
        assert!(socket.control.probes.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn control_and_quic_share_the_bound_socket() {
        tokio::time::timeout(Duration::from_secs(3), async {
            let (socket, mut quic) = Socket::bind("127.0.0.1:0".parse().unwrap()).unwrap();
            let peer = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
            let local = socket.local_addr().unwrap();
            let peer_addr = peer.local_addr().unwrap();
            let mut buffer = [0; 1500];
            let mut meta = [RecvMeta::default()];
            let (mapped, ()) = tokio::join!(socket.mapped_addr(peer_addr), async {
                let (len, from) = peer.recv_from(&mut buffer).await.unwrap();
                assert_eq!(from, local);
                let request = Message::from_bytes(&buffer[..len]).unwrap();
                let mut response = Message::builder_success(&request, MessageWriteVec::new());
                response
                    .add_attribute(&XorMappedAddress::new(from, request.transaction_id()))
                    .unwrap();
                response.add_fingerprint().unwrap();
                peer.send_to(&response.finish(), from).await.unwrap();
                let count = poll_fn(|cx| {
                    quic.poll_recv(cx, &mut [IoSliceMut::new(&mut buffer)], &mut meta)
                })
                .await
                .unwrap();
                assert_eq!(count, 1);
                assert_eq!(meta[0].len, 0);
            });
            assert_eq!(mapped.unwrap(), local);

            let packet = [0x40; 32];
            let transmit = Transmit {
                destination: peer_addr,
                ecn: None,
                contents: &packet,
                segment_size: None,
                src_ip: None,
            };
            let mut sender = quic.create_sender();
            poll_fn(|cx| sender.as_mut().poll_send(&transmit, cx))
                .await
                .unwrap();
            let (len, from) = peer.recv_from(&mut buffer).await.unwrap();
            assert_eq!(from, local);
            assert_eq!(&buffer[..len], &packet);
            peer.send_to(&packet, from).await.unwrap();
            poll_fn(|cx| quic.poll_recv(cx, &mut [IoSliceMut::new(&mut buffer)], &mut meta))
                .await
                .unwrap();
            assert_eq!(&buffer[..meta[0].len], &packet);
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn coalesced_control_segments_are_removed_in_place() {
        let (_, quic_socket) = Socket::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let quic = [0x40_u8; 4];
        let mut stun = [0_u8; 4];
        stun[0] = 0x01;
        let mut batch = [quic, stun, [0x41; 4]].concat();
        batch.extend_from_slice(&[0x42; 2]);
        let mut meta = RecvMeta::default();
        meta.len = batch.len();
        meta.stride = 4;
        let len = quic_socket.retain_quic(&mut batch, &meta);
        assert_eq!(
            &batch[..len],
            &[0x40, 0x40, 0x40, 0x40, 0x41, 0x41, 0x41, 0x41, 0x42, 0x42]
        );
    }
}
