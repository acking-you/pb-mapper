use std::fmt::Debug;
use std::io::{self};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use bytes::{Buf, Bytes, BytesMut};
use hashbrown::HashMap;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::UdpSocket;
use tokio::time::Sleep;

use self::impl_inner::{get_sleep, UdpStreamReadContext, UdpStreamWriteContext};
use super::addr::{each_addr, ToSocketAddrs};
use crate::snafu_error_get_or_continue;

const UDP_CHANNEL_LEN: usize = 100;
const UDP_BUFFER_SIZE: usize = 16 * 1024;

type Result<T, E = std::io::Error> = std::result::Result<T, E>;

mod impl_inner {

    use std::time::Duration;

    use futures::{FutureExt, StreamExt};
    use tokio::time::{sleep, Instant, Sleep};

    use super::*;

    pub(super) trait UdpStreamReadContext {
        fn get_mut_remaining_bytes(&mut self) -> &mut Option<Bytes>;
        fn get_receiver_stream(&mut self) -> &mut flume::r#async::RecvStream<'static, Bytes>;
        fn get_sleep(&mut self) -> &mut Pin<Box<Sleep>>;
    }

    pub(super) trait UdpStreamWriteContext {
        fn get_socket(&self) -> &tokio::net::UdpSocket;
        fn get_peer_addr(&self) -> &SocketAddr;
    }

    pub(super) fn poll_read<T: UdpStreamReadContext>(
        mut read_ctx: T,
        cx: &mut Context,
        buf: &mut ReadBuf,
    ) -> Poll<Result<()>> {
        // timeout
        if read_ctx.get_sleep().poll_unpin(cx).is_ready() {
            buf.clear();
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("UdpStream timeout with duration:{:?}", get_timeout()),
            )));
        }

        let is_remaining_consume = if let Some(remaining) = read_ctx.get_mut_remaining_bytes() {
            if buf.remaining() < remaining.len() {
                buf.put_slice(&remaining.split_to(buf.remaining())[..]);
            } else {
                buf.put_slice(&remaining[..]);
                *read_ctx.get_mut_remaining_bytes() = None;
            }
            true
        } else {
            false
        };

        if is_remaining_consume {
            // update timeout
            read_ctx
                .get_sleep()
                .as_mut()
                .reset(Instant::now() + get_timeout());
            return Poll::Ready(Ok(()));
        }

        let remaining = match read_ctx.get_receiver_stream().poll_next_unpin(cx) {
            Poll::Ready(Some(mut inner_buf)) => {
                let remaining = if buf.remaining() < inner_buf.len() {
                    Some(inner_buf.split_off(buf.remaining()))
                } else {
                    None
                };
                buf.put_slice(&inner_buf[..]);
                remaining
            }
            Poll::Ready(None) => {
                return Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "Broken pipe",
                )));
            }
            Poll::Pending => return Poll::Pending,
        };

        // last update
        read_ctx
            .get_sleep()
            .as_mut()
            .reset(Instant::now() + get_timeout());
        *read_ctx.get_mut_remaining_bytes() = remaining;
        Poll::Ready(Ok(()))
    }

    pub(super) fn poll_write<T: UdpStreamWriteContext>(
        write_ctx: T,
        cx: &mut Context,
        buf: &[u8],
    ) -> Poll<Result<usize>> {
        write_ctx
            .get_socket()
            .poll_send_to(cx, buf, *write_ctx.get_peer_addr())
    }

    const DEFAULT_TIMEOUT: Duration = Duration::from_secs(20);

    pub(super) fn get_sleep() -> Sleep {
        sleep(get_timeout())
    }

    #[inline]
    pub(super) fn get_timeout() -> Duration {
        DEFAULT_TIMEOUT
    }
}

/// An I/O object representing a UDP socket listening for incoming connections.
///
/// This object can be converted into a stream of incoming connections for
/// various forms of processing.
///
/// # Examples
/// See [tokio::net::TcpListener]
pub struct UdpListener {
    handler: tokio::task::JoinHandle<()>,
    receiver: flume::Receiver<(UdpStream, SocketAddr)>,
    local_addr: SocketAddr,
}

impl Drop for UdpListener {
    fn drop(&mut self) {
        self.handler.abort();
    }
}

impl UdpListener {
    pub async fn bind(local_addr: SocketAddr) -> io::Result<Self> {
        let (listener_tx, listener_rx) = flume::bounded(UDP_CHANNEL_LEN);
        let udp_socket = UdpSocket::bind(local_addr).await?;
        let local_addr = udp_socket.local_addr()?;

        let handler = tokio::spawn(async move {
            let mut streams: HashMap<SocketAddr, flume::Sender<Bytes>> = HashMap::new();
            let socket = Arc::new(udp_socket);
            let (drop_tx, drop_rx) = flume::bounded(10);

            let mut buf = BytesMut::with_capacity(UDP_BUFFER_SIZE * 3);
            loop {
                if buf.capacity() < UDP_BUFFER_SIZE {
                    buf.reserve(UDP_BUFFER_SIZE * 3);
                }
                tokio::select! {
                    ret = drop_rx.recv_async() => {
                        let peer_addr = snafu_error_get_or_continue!(ret,"UDPListener clean conn");
                        streams.remove(&peer_addr);
                    }
                    ret = socket.recv_buf_from(&mut buf) => {
                        let (len,peer_addr) = snafu_error_get_or_continue!(ret,"UDPListener `recv_buf_from`");

                        match streams.get(&peer_addr) {
                            Some(tx) => {
                                if let Err(err) =  tx.send_async(buf.copy_to_bytes(len)).await{
                                    tracing::error!("UDPListener send msg to conn err:{:?}", err);
                                    streams.remove(&peer_addr);
                                    continue;
                                }
                            }
                            None => {
                                let (child_tx, child_rx) = flume::bounded(UDP_CHANNEL_LEN);
                                // pre send msg
                                snafu_error_get_or_continue!(
                                    child_tx.send_async(buf.copy_to_bytes(len)).await,
                                    "pre send msg"
                                );

                                let udp_stream = UdpStream {
                                    local_addr,
                                    peer_addr,
                                    recv_stream: child_rx.into_stream(),
                                    timeout: Box::pin(get_sleep()),
                                    socket: socket.clone(),
                                    _handler_guard: None,
                                    _listener_guard: Some(ListenerCleanGuard{sender:drop_tx.clone(),peer_addr}),
                                    remaining: None,
                                };
                                snafu_error_get_or_continue!(
                                    listener_tx.send_async((udp_stream, peer_addr)).await,
                                    "register UDPStream"
                                );
                                streams.insert(peer_addr, child_tx);
                            }
                        }
                    }
                }
            }
        });
        Ok(Self {
            handler,
            receiver: listener_rx,
            local_addr,
        })
    }

    /// Returns the local address that this socket is bound to.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        Ok(self.local_addr)
    }

    /// Accepts a new incoming UDP connection.
    pub async fn accept(&self) -> io::Result<(UdpStream, SocketAddr)> {
        self.receiver
            .recv_async()
            .await
            .map_err(|e| io::Error::new(io::ErrorKind::BrokenPipe, e))
    }
}

#[derive(Debug)]
struct TaskJoinHandleGuard(tokio::task::JoinHandle<()>);

#[derive(Debug, Clone)]
struct ListenerCleanGuard {
    sender: flume::Sender<SocketAddr>,
    peer_addr: SocketAddr,
}

impl Drop for ListenerCleanGuard {
    fn drop(&mut self) {
        _ = self.sender.try_send(self.peer_addr);
    }
}

impl Drop for TaskJoinHandleGuard {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// An I/O object representing a UDP stream connected to a remote endpoint.
///
/// A UDP stream can either be created by connecting to an endpoint, via the
/// [`connect`] method, or by accepting a connection from a listener.
pub struct UdpStream {
    local_addr: SocketAddr,
    peer_addr: SocketAddr,
    socket: Arc<tokio::net::UdpSocket>,
    recv_stream: flume::r#async::RecvStream<'static, Bytes>,
    remaining: Option<Bytes>,
    timeout: Pin<Box<Sleep>>,
    _handler_guard: Option<TaskJoinHandleGuard>,
    _listener_guard: Option<ListenerCleanGuard>,
}

impl UdpStream {
    /// Create a new UDP stream connected to the specified address.
    ///
    /// This function will create a new UDP socket and attempt to connect it to
    /// the `addr` provided. The returned future will be resolved once the
    /// stream has successfully connected, or it will return an error if one
    /// occurs.
    pub async fn connect<A: ToSocketAddrs>(addr: A) -> Result<Self> {
        each_addr(addr, UdpStream::connect_inner).await
    }

    async fn connect_inner(addr: SocketAddr) -> Result<Self> {
        let local_addr: SocketAddr = if addr.is_ipv4() {
            SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0)
        } else {
            SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0)
        };
        let socket = UdpSocket::bind(local_addr).await?;
        socket.connect(&addr).await?;
        Self::from_tokio(socket).await
    }

    /// Creates a new UdpStream from a tokio::net::UdpSocket.
    /// This function is intended to be used to wrap a UDP socket from the tokio library.
    /// Note: The UdpSocket must have the UdpSocket::connect method called before invoking this
    /// function.
    async fn from_tokio(socket: UdpSocket) -> Result<Self> {
        let socket = Arc::new(socket);

        let local_addr = socket.local_addr()?;
        let peer_addr = socket.peer_addr()?;

        let (tx, rx) = flume::bounded(UDP_CHANNEL_LEN);

        let socket_inner = socket.clone();

        let handler = tokio::spawn(async move {
            let mut buf = BytesMut::with_capacity(UDP_BUFFER_SIZE);
            while let Ok((len, received_addr)) = socket_inner.recv_buf_from(&mut buf).await {
                if received_addr != peer_addr {
                    continue;
                }
                if tx.send_async(buf.copy_to_bytes(len)).await.is_err() {
                    drop(tx);
                    break;
                }

                if buf.capacity() < UDP_BUFFER_SIZE {
                    buf.reserve(UDP_BUFFER_SIZE * 3);
                }
            }
        });

        Ok(UdpStream {
            local_addr,
            peer_addr,
            recv_stream: rx.into_stream(),
            socket,
            timeout: Box::pin(get_sleep()),
            _handler_guard: Some(TaskJoinHandleGuard(handler)),
            _listener_guard: None,
            remaining: None,
        })
    }

    pub fn peer_addr(&self) -> std::io::Result<SocketAddr> {
        Ok(self.peer_addr)
    }

    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        Ok(self.local_addr)
    }

    pub fn split(&self) -> (UdpStreamReadHalf<'static>, UdpStreamWriteHalf) {
        (
            UdpStreamReadHalf {
                recv_stream: self.recv_stream.clone(),
                remaining: self.remaining.clone(),
                timeout: Box::pin(get_sleep()),
            },
            UdpStreamWriteHalf {
                socket: &self.socket,
                peer_addr: self.peer_addr,
            },
        )
    }
}

impl UdpStreamReadContext for std::pin::Pin<&mut UdpStream> {
    fn get_mut_remaining_bytes(&mut self) -> &mut Option<Bytes> {
        &mut self.remaining
    }

    fn get_receiver_stream(&mut self) -> &mut flume::r#async::RecvStream<'static, Bytes> {
        &mut self.recv_stream
    }

    fn get_sleep(&mut self) -> &mut Pin<Box<Sleep>> {
        &mut self.timeout
    }
}

impl UdpStreamWriteContext for std::pin::Pin<&mut UdpStream> {
    fn get_socket(&self) -> &tokio::net::UdpSocket {
        &self.socket
    }

    fn get_peer_addr(&self) -> &SocketAddr {
        &self.peer_addr
    }
}

impl AsyncRead for UdpStream {
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context, buf: &mut ReadBuf) -> Poll<Result<()>> {
        impl_inner::poll_read(self, cx, buf)
    }
}

impl AsyncWrite for UdpStream {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context, buf: &[u8]) -> Poll<io::Result<usize>> {
        impl_inner::poll_write(self, cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

pub struct UdpStreamReadHalf<'a> {
    recv_stream: flume::r#async::RecvStream<'a, Bytes>,
    remaining: Option<Bytes>,
    timeout: Pin<Box<Sleep>>,
}

impl UdpStreamReadContext for std::pin::Pin<&mut UdpStreamReadHalf<'static>> {
    fn get_mut_remaining_bytes(&mut self) -> &mut Option<Bytes> {
        &mut self.remaining
    }

    fn get_receiver_stream(&mut self) -> &mut flume::r#async::RecvStream<'static, Bytes> {
        &mut self.recv_stream
    }

    fn get_sleep(&mut self) -> &mut Pin<Box<Sleep>> {
        &mut self.timeout
    }
}

impl AsyncRead for UdpStreamReadHalf<'static> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<Result<()>> {
        impl_inner::poll_read(self, cx, buf)
    }
}

pub struct UdpStreamWriteHalf<'a> {
    socket: &'a tokio::net::UdpSocket,
    peer_addr: SocketAddr,
}

impl UdpStreamWriteContext for std::pin::Pin<&mut UdpStreamWriteHalf<'_>> {
    fn get_socket(&self) -> &tokio::net::UdpSocket {
        self.socket
    }

    fn get_peer_addr(&self) -> &SocketAddr {
        &self.peer_addr
    }
}

impl AsyncWrite for UdpStreamWriteHalf<'_> {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<Result<usize>> {
        impl_inner::poll_write(self, cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<()>> {
        Poll::Ready(Ok(()))
    }
}
