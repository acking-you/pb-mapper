//! Echo servers a tunnel forwards to.
//!
//! Each binds its own socket before it returns, so the caller learns the
//! address without a window in which another test could take the port, and
//! each stops, connections and all, when its [`Echo`] is dropped.
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
//! therefore sends it **once, as the first byte of the connection**, as soon as it
//! accepts, and the reply stream is `tag` followed by the echoed bytes verbatim.
//! That is well defined however the traffic happens to be chunked, it is still
//! enough to identify the server — which is all the tag is for — and a client
//! may read it before it writes anything.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::task::JoinSet;
use tokio_util::task::AbortOnDropHandle;
use uni_stream::udp::tune_udp_socket;

/// A running echo server on loopback. Dropping it stops the server.
pub struct Echo {
    addr: SocketAddr,
    reached: Arc<AtomicUsize>,
    _task: AbortOnDropHandle<()>,
}

impl Echo {
    /// Where the server listens: the backend address to register.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// How often the server has been reached: connections accepted over TCP,
    /// datagrams received over UDP.
    pub fn reached(&self) -> usize {
        self.reached.load(Ordering::Relaxed)
    }
}

/// A TCP echo server. Each connection gets `tag`, if any, then its own bytes
/// back verbatim; once the peer's FIN arrives the server shuts its write half
/// down, so a peer that reads to EOF gets the whole tail.
pub async fn tcp_echo(tag: Option<u8>) -> Echo {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let reached = Arc::new(AtomicUsize::new(0));
    let task = tokio::spawn(serve_tcp(listener, tag, reached.clone()));
    Echo {
        addr,
        reached,
        _task: AbortOnDropHandle::new(task),
    }
}

/// A UDP echo server. Each datagram comes back to its sender, prefixed by `tag`.
pub async fn udp_echo(tag: Option<u8>) -> Echo {
    let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let addr = socket.local_addr().unwrap();
    let reached = Arc::new(AtomicUsize::new(0));
    let task = tokio::spawn(serve_udp(socket, tag, reached.clone()));
    Echo {
        addr,
        reached,
        _task: AbortOnDropHandle::new(task),
    }
}

/// The accept loop. Connections live in a set the loop owns, so they end with it.
async fn serve_tcp(listener: TcpListener, tag: Option<u8>, reached: Arc<AtomicUsize>) {
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
            accepted = listener.accept() => {
                let Ok((stream, _)) = accepted else {
                    return;
                };
                reached.fetch_add(1, Ordering::Relaxed);
                connections.spawn(echo_connection(stream, tag));
            }
        }
    }
}

async fn echo_connection(mut stream: TcpStream, tag: Option<u8>) {
    if let Some(tag) = tag
        && stream.write_u8(tag).await.is_err()
    {
        return;
    }
    let (mut read, mut write) = stream.split();
    let _ = tokio::io::copy(&mut read, &mut write).await;
    let _ = write.shutdown().await;
}

async fn serve_udp(socket: UdpSocket, tag: Option<u8>, reached: Arc<AtomicUsize>) {
    tune_udp_socket(&socket);
    let mut buf = vec![0u8; 65_507];
    loop {
        let Ok((len, peer)) = socket.recv_from(&mut buf).await else {
            return;
        };
        reached.fetch_add(1, Ordering::Relaxed);
        let mut reply = Vec::with_capacity(len + 1);
        reply.extend(tag);
        reply.extend_from_slice(&buf[..len]);
        if let Err(err) = socket.send_to(&reply, peer).await {
            tracing::warn!("udp echo send error: {err}");
        }
    }
}
