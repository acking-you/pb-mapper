//! One UDP socket per address family carries QUIC, STUN and authenticated
//! probes. Quinn's endpoint driver is its only reader: control datagrams are
//! handled and removed in place inside the received batch, so QUIC packets
//! reach Quinn with no extra task, channel, allocation or copy.
use pb_mapper_protocol::direct::{SessionId, SessionSecret};
use quinn::{
    AsyncUdpSocket, Runtime, UdpPoller,
    udp::{RecvMeta, Transmit},
};
use ring::hmac;
use std::{
    collections::HashMap,
    future::poll_fn,
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
    inner: Arc<dyn AsyncUdpSocket>,
    control: Arc<Control>,
    quic: Arc<Demux>,
}
impl std::fmt::Debug for Socket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Socket")
            .field("local", &self.inner.local_addr())
            .finish()
    }
}

/// The socket Quinn sees. Sends pass straight through; receives are filtered.
struct Demux {
    #[cfg(test)]
    block_quic: std::sync::atomic::AtomicBool,
    inner: Arc<dyn AsyncUdpSocket>,
    control: Arc<Control>,
}
impl std::fmt::Debug for Demux {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DirectUdpSocket")
    }
}
impl AsyncUdpSocket for Demux {
    fn create_io_poller(self: Arc<Self>) -> Pin<Box<dyn UdpPoller>> {
        self.inner.clone().create_io_poller()
    }
    fn try_send(&self, transmit: &Transmit) -> io::Result<()> {
        #[cfg(test)]
        if self.block_quic.load(std::sync::atomic::Ordering::Relaxed) {
            return Ok(());
        }
        self.inner.try_send(transmit)
    }
    fn poll_recv(
        &self,
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
    fn max_transmit_segments(&self) -> usize {
        self.inner.max_transmit_segments()
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
                self.handle_control(&data[offset..end], meta.addr);
            }
            offset = end;
        }
        kept
    }
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
                    let nonce = data[24..40].try_into().unwrap_or([0; 16]);
                    packet(id, nonce, true, probe.key)
                }
                1 if data[24..40] == probe.nonce
                    && !probe.learned.contains(&source)
                    && probe.learned.len() < 8 =>
                {
                    probe.learned.push(source);
                    return;
                }
                _ => return,
            }
        };
        let _ = self.inner.try_send(&Transmit {
            destination: source,
            ecn: None,
            contents: &reply,
            segment_size: None,
            src_ip: None,
        });
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
    pub fn bind(addr: SocketAddr) -> io::Result<Self> {
        let udp = std::net::UdpSocket::bind(addr)?;
        udp.set_nonblocking(true)?;
        let inner = quinn::TokioRuntime.wrap_udp_socket(udp)?;
        let control = Arc::new(Control {
            probes: Mutex::new(HashMap::new()),
            stun: Mutex::new(HashMap::new()),
            replies: Mutex::new(ReplyBudget {
                window: tokio::time::Instant::now(),
                used: 0,
            }),
        });
        let quic = Arc::new(Demux {
            #[cfg(test)]
            block_quic: std::sync::atomic::AtomicBool::new(false),
            inner: inner.clone(),
            control: control.clone(),
        });
        Ok(Self {
            inner,
            control,
            quic,
        })
    }
    #[cfg(test)]
    pub fn block_quic(&self) {
        self.quic
            .block_quic
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }
    pub fn quic(&self) -> Arc<dyn AsyncUdpSocket> {
        self.quic.clone()
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
        let bytes = {
            let probes = self
                .control
                .probes
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let Some(probe) = probes.get(&id) else {
                return vec![];
            };
            packet(id, probe.nonce, false, probe.key)
        };
        let family = self.local_addr().map(|local| local.is_ipv4());
        for addr in candidates
            .iter()
            .filter(|a| family.as_ref().is_ok_and(|v4| *v4 == a.is_ipv4()))
            .take(8)
        {
            // Probes are retried every 120 ms. A congested or unreachable
            // candidate must never hold up cancellation or a healthy QUIC path.
            let _ = self.inner.try_send(&Transmit {
                destination: *addr,
                ecn: None,
                contents: &bytes,
                segment_size: None,
                src_ip: None,
            });
        }
        let probes = self
            .control
            .probes
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        probes
            .get(&id)
            .map(|p| p.learned.clone())
            .unwrap_or_default()
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
            send(&self.inner, &transmit.data, transmit.to).await?;
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
                            send(&self.inner, transmit.data, transmit.to).await?;
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

async fn send(
    socket: &Arc<dyn AsyncUdpSocket>,
    bytes: &[u8],
    destination: SocketAddr,
) -> io::Result<()> {
    let mut poller = socket.clone().create_io_poller();
    poll_fn(|cx| {
        loop {
            match socket.try_send(&Transmit {
                destination,
                ecn: None,
                contents: bytes,
                segment_size: None,
                src_ip: None,
            }) {
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    match poller.as_mut().poll_writable(cx) {
                        Poll::Pending => return Poll::Pending,
                        Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                        Poll::Ready(Ok(())) => {}
                    }
                }
                result => return Poll::Ready(result),
            }
        }
    })
    .await
}
fn packet(id: SessionId, nonce: [u8; 16], pong: bool, secret: SessionSecret) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(PROBE_LEN);
    bytes.extend_from_slice(PROBE);
    bytes.extend_from_slice(&id);
    bytes.extend_from_slice(&nonce);
    bytes.push(u8::from(pong));
    let mac = hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, &secret.0), &bytes);
    bytes.extend_from_slice(mac.as_ref());
    bytes
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[tokio::test]
    async fn coalesced_control_segments_are_removed_in_place() {
        let socket = Socket::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let quic = [0x40_u8; 4];
        let mut stun = [0_u8; 4];
        stun[0] = 0x01;
        let mut batch = [quic, stun, [0x41; 4]].concat();
        batch.extend_from_slice(&[0x42; 2]);
        let meta = RecvMeta {
            len: batch.len(),
            stride: 4,
            ..RecvMeta::default()
        };
        let len = socket.quic.retain_quic(&mut batch, &meta);
        assert_eq!(
            &batch[..len],
            &[0x40, 0x40, 0x40, 0x40, 0x41, 0x41, 0x41, 0x41, 0x42, 0x42]
        );
    }
}
