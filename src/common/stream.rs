use std::os::fd::AsFd;
use std::time::Duration;

use snafu::ResultExt;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::tcp::{ReadHalf, WriteHalf};
use tokio::net::{TcpStream, ToSocketAddrs};

use super::error::{Result, StmConnectStreamSnafu};

pub trait NetStream: StreamSplit + AsyncReadExt + AsyncWriteExt + Send + Unpin + 'static {}

pub trait StreamProvider {
    type Item: NetStream;

    fn from_addr<A: ToSocketAddrs + Send>(
        addr: A,
    ) -> impl std::future::Future<Output = Result<Self::Item>> + Send;
}

pub trait StreamSplit {
    type ReaderRef<'a>: AsyncReadExt + Send + Unpin
    where
        Self: 'a;
    type WriterRef<'a>: AsyncWriteExt + Send + Unpin
    where
        Self: 'a;

    fn split(&mut self) -> (Self::ReaderRef<'_>, Self::WriterRef<'_>);
}

pub struct TcpStreamProvider;

pub struct TcpStreamImpl(pub(super) TcpStream);

impl NetStream for TcpStreamImpl {}

impl AsyncRead for TcpStreamImpl {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.0).poll_read(cx, buf)
    }
}

impl AsyncWrite for TcpStreamImpl {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::prelude::v1::Result<usize, std::io::Error>> {
        std::pin::Pin::new(&mut self.0).poll_write(cx, buf)
    }

    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::prelude::v1::Result<(), std::io::Error>> {
        std::pin::Pin::new(&mut self.0).poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::prelude::v1::Result<(), std::io::Error>> {
        std::pin::Pin::new(&mut self.0).poll_shutdown(cx)
    }
}

impl StreamSplit for TcpStreamImpl {
    type ReaderRef<'a> = ReadHalf<'a>
    where
        Self: 'a;
    type WriterRef<'a> = WriteHalf<'a>
    where
        Self: 'a;

    fn split(&mut self) -> (Self::ReaderRef<'_>, Self::WriterRef<'_>) {
        self.0.split()
    }
}

impl StreamProvider for TcpStreamProvider {
    type Item = TcpStreamImpl;

    async fn from_addr<A: ToSocketAddrs + Send>(addr: A) -> Result<Self::Item> {
        Ok(TcpStreamImpl(
            TcpStream::connect(addr)
                .await
                .context(StmConnectStreamSnafu { stream_type: "TCP" })?,
        ))
    }
}

/// How long it takes for TCP to start sending keepalive probe packets when no data is exchanged, in
/// Linux it is `tcp_keepalive_time`
const TCP_KEEPALIVE_TIME: Duration = Duration::from_secs(20);
/// Time interval between two consecutive keepalive probe packets,in Linux it is
/// `tcp_keepalive_intvl`
const TCP_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(20);
/// The number of times a keepalive probe packet is performed to be sent,in Linux it
/// is `tcp_keepalive_probes`
const TCP_KEEPALIVE_PROBES: u32 = 3;

/// Local and remote TCP connections must have keepalive enabled to ensure that the remote server's
/// resources are not wasted
pub fn set_tcp_keep_alive<S: AsFd>(stream: &S) -> std::result::Result<(), std::io::Error> {
    let sock_ref = socket2::SockRef::from(stream);
    let mut ka = socket2::TcpKeepalive::new();
    ka = ka.with_time(TCP_KEEPALIVE_TIME);
    ka = ka.with_interval(TCP_KEEPALIVE_INTERVAL);
    ka = ka.with_retries(TCP_KEEPALIVE_PROBES);

    sock_ref.set_tcp_keepalive(&ka)
}
