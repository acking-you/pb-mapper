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

/// Binding only: bounded datagrams, no allocation until the per-source rate gate.
pub(super) async fn serve(socket: UdpSocket) -> std::io::Result<()> {
    let mut bytes = [0; 1024];
    let mut sources: HashMap<IpAddr, (Instant, u32)> = HashMap::new();
    let mut window = Instant::now();
    let mut total = 0;
    loop {
        let (len, from) = socket.recv_from(&mut bytes).await?;
        if window.elapsed() >= Duration::from_secs(1) {
            window = Instant::now();
            total = 0;
            sources.retain(|_, (at, _)| at.elapsed() < Duration::from_secs(2));
        }
        total = u32::saturating_add(total, 1);
        if total > 1000 || len < 20 || len == bytes.len() {
            continue;
        }
        if sources.len() >= 1024 && !sources.contains_key(&from.ip()) {
            continue;
        }
        let (at, count) = sources.entry(from.ip()).or_insert((window, 0));
        if at.elapsed() >= Duration::from_secs(1) {
            *at = window;
            *count = 0;
        }
        *count += 1;
        if *count > 20 {
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
