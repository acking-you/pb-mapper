use std::{
    collections::HashMap,
    net::IpAddr,
    time::{Duration, Instant},
};
use stun_proto::types::{
    attribute::XorMappedAddress,
    message::{BINDING, Message, MessageClass, MessageWrite, MessageWriteExt, MessageWriteVec},
};
use tokio::net::UdpSocket;

/// The receive buffer; a datagram that fills it is taken as truncated.
const RECV_BUFFER: usize = 1024;
/// The fixed STUN header, the shortest message worth parsing.
const STUN_HEADER_LEN: usize = 20;
/// The rate window, for all sources together and for each one.
const WINDOW: Duration = Duration::from_secs(1);
/// How long a quiet source keeps its slot.
const SOURCE_RETENTION: Duration = Duration::from_secs(2);
/// Requests served per window, from every source together.
const MAX_REQUESTS_PER_WINDOW: u32 = 1000;
/// Sources tracked at once.
const MAX_SOURCES: usize = 1024;
/// Requests served per window from one source.
const MAX_REQUESTS_PER_SOURCE: u32 = 20;

const _: () = assert!(
    SOURCE_RETENTION.as_secs() > WINDOW.as_secs(),
    "a source is forgotten only after its window ended"
);
const _: () = assert!(
    MAX_REQUESTS_PER_SOURCE < MAX_REQUESTS_PER_WINDOW,
    "one source cannot take the whole window"
);
const _: () = assert!(
    STUN_HEADER_LEN < RECV_BUFFER,
    "a buffer holds at least a header"
);

/// Binding only: bounded datagrams, no allocation until the per-source rate gate.
pub(super) async fn serve(socket: UdpSocket) -> std::io::Result<()> {
    let mut bytes = [0; RECV_BUFFER];
    let mut sources: HashMap<IpAddr, (Instant, u32)> = HashMap::new();
    let mut window = Instant::now();
    let mut total = 0;
    loop {
        let (len, from) = socket.recv_from(&mut bytes).await?;
        if window.elapsed() >= WINDOW {
            window = Instant::now();
            total = 0;
            sources.retain(|_, (at, _)| at.elapsed() < SOURCE_RETENTION);
        }
        total = u32::saturating_add(total, 1);
        if total > MAX_REQUESTS_PER_WINDOW || len < STUN_HEADER_LEN || len == bytes.len() {
            continue;
        }
        if sources.len() >= MAX_SOURCES && !sources.contains_key(&from.ip()) {
            continue;
        }
        let (at, count) = sources.entry(from.ip()).or_insert((window, 0));
        if at.elapsed() >= WINDOW {
            *at = window;
            *count = 0;
        }
        *count += 1;
        if *count > MAX_REQUESTS_PER_SOURCE {
            continue;
        }
        let Ok(request) = Message::from_bytes(&bytes[..len]) else {
            continue;
        };
        if !request.has_method(BINDING)
            || !request.has_class(MessageClass::Request)
            || request.as_bytes().len() != len
        {
            continue;
        }
        let mut response = Message::builder_success(&request, MessageWriteVec::new());
        if response
            .add_attribute(&XorMappedAddress::new(from, request.transaction_id()))
            .is_err()
            || response.add_fingerprint().is_err()
        {
            continue;
        }
        // ICMP errors for one peer must not disable discovery for everyone.
        if let Err(error) = socket.send_to(&response.finish(), from).await {
            tracing::debug!(%error, "STUN reply failed");
        }
    }
}
