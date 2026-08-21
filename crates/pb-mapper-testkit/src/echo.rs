//! Echo servers a tunnel forwards to.
//!
//! Both take an already-bound socket, so the caller can learn the address without
//! a window in which another test could take the port.
//!
//! `tag` identifies which echo server answered. Two tunnels that share a service
//! name in different namespaces get different tags, so a test can tell which one
//! actually received the traffic — without it, a namespace leak would still
//! satisfy payload equality, since both servers echo identically.
//!
//! Where the tag goes differs by transport, because the transports differ in what
//! they preserve. UDP keeps datagram boundaries, so every reply carries it. TCP
//! keeps none: one write can arrive as several reads and several writes as one, so
//! a tag per read would inject bytes into the middle of a payload. The TCP server
//! therefore sends it **once, as the first byte of the connection**, and the reply
//! stream is `tag` followed by the echoed bytes verbatim. That is well defined
//! however the traffic happens to be chunked, and it is still enough to identify
//! the server — which is all the tag is for.

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, UdpSocket};
use uni_stream::udp::tune_udp_socket;

pub async fn tcp_echo_server(listener: TcpListener, tag: Option<u8>) {
    loop {
        let Ok((mut stream, peer)) = listener.accept().await else {
            return;
        };
        tokio::spawn(async move {
            let mut buf = vec![0u8; 4096];
            // Consumed by the first reply on this connection; see the module note.
            let mut pending_tag = tag;
            loop {
                match stream.read(&mut buf).await {
                    Ok(0) | Err(_) => return,
                    Ok(n) => {
                        let mut reply = Vec::with_capacity(n + 1);
                        reply.extend(pending_tag.take());
                        reply.extend_from_slice(&buf[..n]);
                        if stream.write_all(&reply).await.is_err() {
                            return;
                        }
                        tracing::debug!("echoed {n} bytes to {peer}");
                    }
                }
            }
        });
    }
}

pub async fn udp_echo_server(socket: UdpSocket, tag: Option<u8>) {
    tune_udp_socket(&socket);
    let mut buf = vec![0u8; 65_507];
    loop {
        let Ok((len, peer)) = socket.recv_from(&mut buf).await else {
            return;
        };
        let mut reply = Vec::with_capacity(len + 1);
        reply.extend(tag);
        reply.extend_from_slice(&buf[..len]);
        if let Err(err) = socket.send_to(&reply, peer).await {
            tracing::warn!("udp echo send error: {err}");
        }
    }
}
