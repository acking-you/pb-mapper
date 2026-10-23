//! One receive owner for QUIC, STUN and authenticated probes, per address family.
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
    task::{Context, Poll},
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
struct Packet {
    bytes: Vec<u8>,
    meta: RecvMeta,
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

#[derive(Clone)]
pub(super) struct Socket {
    inner: Arc<dyn AsyncUdpSocket>,
    probes: Arc<Mutex<HashMap<SessionId, ProbeState>>>,
    stun: Arc<Mutex<HashMap<[u8; 12], StunPending>>>,
    quic: Arc<Demux>,
    task: Arc<tokio_util::task::AbortOnDropHandle<()>>,
}
impl std::fmt::Debug for Socket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Socket")
            .field("local", &self.inner.local_addr())
            .finish()
    }
}
#[derive(Debug)]
struct Demux {
    closing: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    block_quic: std::sync::atomic::AtomicBool,
    inner: Arc<dyn AsyncUdpSocket>,
    rx: Mutex<mpsc::Receiver<Packet>>,
}
impl std::fmt::Debug for Packet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Packet")
            .field("len", &self.bytes.len())
            .finish()
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
        let mut rx = self.rx.lock().unwrap_or_else(|e| e.into_inner());
        match rx.poll_recv(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(None) if self.closing.load(std::sync::atomic::Ordering::Acquire) => {
                Poll::Pending
            }
            Poll::Ready(None) => Poll::Ready(Err(io::Error::other("UDP driver stopped"))),
            Poll::Ready(Some(packet)) => {
                if packet.bytes.len() > bufs[0].len() {
                    return Poll::Ready(Err(io::Error::other("QUIC receive buffer too small")));
                }
                bufs[0][..packet.bytes.len()].copy_from_slice(&packet.bytes);
                meta[0] = packet.meta;
                Poll::Ready(Ok(1))
            }
        }
    }
    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.inner.local_addr()
    }
    fn max_transmit_segments(&self) -> usize {
        self.inner.max_transmit_segments()
    }
    fn max_receive_segments(&self) -> usize {
        1
    }
    fn may_fragment(&self) -> bool {
        self.inner.may_fragment()
    }
}
impl Socket {
    pub fn bind(addr: SocketAddr) -> io::Result<Self> {
        let udp = std::net::UdpSocket::bind(addr)?;
        udp.set_nonblocking(true)?;
        let inner = quinn::TokioRuntime.wrap_udp_socket(udp)?;
        let (tx, rx) = mpsc::channel(256);
        let quic = Arc::new(Demux {
            closing: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            block_quic: std::sync::atomic::AtomicBool::new(false),
            inner: inner.clone(),
            rx: Mutex::new(rx),
        });
        let probes = Arc::new(Mutex::new(HashMap::new()));
        let stun = Arc::new(Mutex::new(HashMap::new()));
        let task = Arc::new(tokio_util::task::AbortOnDropHandle::new(tokio::spawn(
            receive(inner.clone(), tx, probes.clone(), stun.clone()),
        )));
        Ok(Self {
            inner,
            probes,
            stun,
            quic,
            task,
        })
    }
    #[cfg(test)]
    pub fn block_quic(&self) {
        self.quic
            .block_quic
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }
    pub fn closing(&self) {
        self.quic
            .closing
            .store(true, std::sync::atomic::Ordering::Release);
    }
    pub fn quic(&self) -> Arc<dyn AsyncUdpSocket> {
        self.quic.clone()
    }
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.inner.local_addr()
    }
    pub fn authorize(&self, id: SessionId, key: SessionSecret, expires: tokio::time::Instant) {
        let mut probes = self.probes.lock().unwrap_or_else(|e| e.into_inner());
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
        self.probes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id);
    }
    pub async fn probe(
        &self,
        id: SessionId,
        candidates: &[SocketAddr],
    ) -> io::Result<Vec<SocketAddr>> {
        let bytes = {
            let state = self.probes.lock().unwrap_or_else(|e| e.into_inner());
            let Some(probe) = state.get(&id) else {
                return Ok(vec![]);
            };
            packet(id, probe.nonce, false, probe.key)
        };
        for addr in candidates
            .iter()
            .filter(|a| {
                self.local_addr()
                    .is_ok_and(|local| local.is_ipv4() == a.is_ipv4())
            })
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
        let state = self.probes.lock().unwrap_or_else(|e| e.into_inner());
        Ok(state
            .get(&id)
            .map(|p| p.learned.clone())
            .unwrap_or_default())
    }
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
        self.stun
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(txid, StunPending { source: server, tx });
        let _guard = StunGuard {
            map: self.stun.clone(),
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
                        if !message.has_method(BINDING) || !message.has_class(MessageClass::Success) { continue; }
                        let mapped = message.attribute::<XorMappedAddress>().map_err(io::Error::other)?.addr(message.transaction_id());
                        if mapped.is_ipv4() != server.is_ipv4() || !pb_mapper_protocol::direct::valid_candidate(&mapped) { continue; }
                        if agent.handle_stun_message_with_time(&message, server, Instant::from_std(std::time::Instant::now())) { return Ok(mapped); }
                    }
                    () = tokio::time::sleep(Duration::from_millis(25)) => {
                        let now = Instant::from_std(std::time::Instant::now());
                        if let Some(transmit) = agent.poll_transmit(now) { send(&self.inner, transmit.data, transmit.to).await?; }
                    }
                }
            }
        }).await;
        result.map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "STUN binding timed out"))?
    }
    pub fn alive(&self) -> bool {
        !self.task.is_finished()
    }
}
struct StunGuard {
    map: Arc<Mutex<HashMap<[u8; 12], StunPending>>>,
    id: [u8; 12],
}
impl Drop for StunGuard {
    fn drop(&mut self) {
        self.map
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
async fn receive(
    socket: Arc<dyn AsyncUdpSocket>,
    quic: mpsc::Sender<Packet>,
    probes: Arc<Mutex<HashMap<SessionId, ProbeState>>>,
    stun: Arc<Mutex<HashMap<[u8; 12], StunPending>>>,
) {
    let mut buffer = vec![0; 65536];
    let mut meta = [RecvMeta::default()];
    let mut response_window = tokio::time::Instant::now();
    let mut responses = 0u32;
    loop {
        let result =
            poll_fn(|cx| socket.poll_recv(cx, &mut [IoSliceMut::new(&mut buffer)], &mut meta))
                .await;
        if result.is_err() {
            break;
        }
        let received = meta[0];
        if response_window.elapsed() > Duration::from_secs(1) {
            responses = 0;
            response_window = tokio::time::Instant::now();
        }
        for data in buffer[..received.len].chunks(received.stride.max(1)) {
            if data.len() >= 20 && data[0] & 0xc0 == 0 && data[4..8] == [0x21, 0x12, 0xa4, 0x42] {
                if data.len() != 20 + u16::from_be_bytes([data[2], data[3]]) as usize {
                    continue;
                }
                let Ok(id) = <[u8; 12]>::try_from(&data[8..20]) else {
                    continue;
                };
                if let Some(pending) = stun
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(&id)
                    .filter(|p| p.source == received.addr)
                {
                    let _ = pending.tx.try_send(data.to_vec());
                }
            } else if data.starts_with(PROBE) {
                if data.len() != PROBE_LEN || responses >= 128 {
                    continue;
                }
                let Ok(id) = <[u8; 16]>::try_from(&data[8..24]) else {
                    continue;
                };
                let reply = {
                    let mut probes = probes.lock().unwrap_or_else(|e| e.into_inner());
                    let Some(probe) = probes
                        .get_mut(&id)
                        .filter(|p| p.expires > tokio::time::Instant::now())
                    else {
                        continue;
                    };
                    if hmac::verify(
                        &hmac::Key::new(hmac::HMAC_SHA256, &probe.key.0),
                        &data[..41],
                        &data[41..],
                    )
                    .is_err()
                    {
                        continue;
                    }
                    if data[40] == 0 {
                        responses += 1;
                        Some(packet(
                            id,
                            data[24..40].try_into().unwrap_or([0; 16]),
                            true,
                            probe.key,
                        ))
                    } else {
                        if data[40] == 1
                            && data[24..40] == probe.nonce
                            && !probe.learned.contains(&received.addr)
                            && probe.learned.len() < 8
                        {
                            probe.learned.push(received.addr);
                        }
                        None
                    }
                };
                if let Some(reply) = reply {
                    let _ = socket.try_send(&Transmit {
                        destination: received.addr,
                        ecn: None,
                        contents: &reply,
                        segment_size: None,
                        src_ip: None,
                    });
                }
            } else {
                let mut part = received;
                part.len = data.len();
                part.stride = data.len();
                let _ = quic.try_send(Packet {
                    bytes: data.to_vec(),
                    meta: part,
                });
            }
        }
    }
}
