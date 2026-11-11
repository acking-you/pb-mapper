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

/// Windows' `WSAEMSGSIZE`: a datagram larger than the receive buffer.
const WSAEMSGSIZE: i32 = 10040;
/// Windows' `WSAENETRESET`: on a datagram socket, an ICMP time-exceeded for
/// an earlier send.
const WSAENETRESET: i32 = 10052;

/// Whether a receive failed over one datagram, or over an ICMP error for an
/// earlier reply, leaving the socket fine for everyone else. Windows reports
/// both from `recv_from`: a port unreachable as a connection reset, and an
/// oversized datagram as an error instead of a truncation.
fn failed_one_datagram(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionRefused
    ) || (cfg!(windows) && matches!(error.raw_os_error(), Some(WSAEMSGSIZE | WSAENETRESET)))
}

/// Binding only: bounded datagrams, no allocation until the per-source rate gate.
pub(super) async fn serve(socket: UdpSocket) -> std::io::Result<()> {
    let mut bytes = [0; RECV_BUFFER];
    let mut sources: HashMap<IpAddr, (Instant, u32)> = HashMap::new();
    let mut window = Instant::now();
    let mut total = 0;
    loop {
        let (len, from) = match socket.recv_from(&mut bytes).await {
            Ok(received) => received,
            Err(error) if failed_one_datagram(&error) => {
                tracing::debug!(%error, "STUN receive failed for one datagram");
                continue;
            }
            Err(error) => return Err(error),
        };
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::SocketAddr;

    fn binding_request() -> Vec<u8> {
        Message::builder_request(BINDING, MessageWriteVec::new()).finish()
    }

    /// Asks `service` for this socket's address until it answers.
    async fn answers(client: &UdpSocket, service: SocketAddr) -> bool {
        let mut reply = [0; RECV_BUFFER];
        for _ in 0..20 {
            client.send_to(&binding_request(), service).await.unwrap();
            if let Ok(Ok(len)) =
                tokio::time::timeout(Duration::from_millis(100), client.recv(&mut reply)).await
            {
                return Message::from_bytes(&reply[..len]).is_ok();
            }
        }
        false
    }

    /// One peer cannot stop discovery for the others: neither a client that
    /// left before its reply (Windows hands the ICMP error to the next
    /// receive) nor a datagram larger than the buffer (which Windows fails
    /// rather than truncates). Each alone used to end the service.
    #[tokio::test]
    async fn one_peer_cannot_stop_the_service() {
        let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = socket.local_addr().unwrap();
        let service = tokio::spawn(serve(socket));
        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();

        let gone = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        gone.send_to(&binding_request(), addr).unwrap();
        drop(gone);
        client.send_to(&[0; RECV_BUFFER * 2], addr).await.unwrap();

        assert!(answers(&client, addr).await, "the service stopped");
        assert!(!service.is_finished());
        service.abort();
    }
}
