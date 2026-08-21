//! Echo servers a tunnel forwards to.
//!
//! Both take an already-bound socket, so the caller can learn the address without
//! a window in which another test could take the port.
//!
//! `tag` prepends one byte to every reply. Two tunnels that share a service name
//! in different namespaces get different tags, so a test can tell which echo
//! server actually received the traffic — without it, a namespace leak would
//! still satisfy payload equality, since both servers echo identically.

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
            loop {
                match stream.read(&mut buf).await {
                    Ok(0) | Err(_) => return,
                    Ok(n) => {
                        let mut reply = Vec::with_capacity(n + 1);
                        reply.extend(tag);
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
