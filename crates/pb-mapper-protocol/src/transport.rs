//! Relay carriers. Dispatch once before forwarding; control I/O may use the enum.
use serde::{Deserialize, Serialize};
use std::{
    io,
    net::SocketAddr,
    pin::Pin,
    task::{Context, Poll},
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::TcpStream,
};

/// Budget for setting a carrier up: QUIC discovery over TCP and the
/// handshake after it, on the client; admission of it, on the relay.
pub const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelayTransport {
    #[default]
    Tcp,
    Quic,
}

#[derive(Debug)]
pub enum RelayStream {
    Tcp(TcpStream),
    #[cfg(feature = "relay-quic")]
    Quic(Box<crate::quic::QuicStream>),
}

impl From<TcpStream> for RelayStream {
    fn from(stream: TcpStream) -> Self {
        Self::Tcp(stream)
    }
}

impl RelayStream {
    /// Process-local identity of the shared carrier, if this stream uses QUIC.
    pub fn carrier_id(&self) -> Option<u64> {
        match self {
            Self::Tcp(_) => None,
            #[cfg(feature = "relay-quic")]
            Self::Quic(stream) => Some(stream.carrier_id()),
        }
    }

    pub fn relay_addr(&self) -> io::Result<SocketAddr> {
        match self {
            Self::Tcp(stream) => stream.peer_addr(),
            #[cfg(feature = "relay-quic")]
            Self::Quic(stream) => Ok(stream.relay_addr()),
        }
    }
    pub fn peer_addr(&self) -> io::Result<SocketAddr> {
        match self {
            Self::Tcp(stream) => stream.peer_addr(),
            #[cfg(feature = "relay-quic")]
            Self::Quic(stream) => Ok(stream.peer_addr()),
        }
    }
    pub fn configure(&self, keep_alive: bool) -> io::Result<()> {
        match self {
            Self::Tcp(stream) => {
                let keep_alive_result = if keep_alive {
                    uni_stream::stream::set_tcp_keep_alive(stream)
                } else {
                    Ok(())
                };
                // Keep Nagle disabled even if the platform rejects keep-alive.
                stream.set_nodelay(true)?;
                keep_alive_result
            }
            #[cfg(feature = "relay-quic")]
            Self::Quic(_) => Ok(()),
        }
    }
    pub fn into_split(self) -> (RelayReadHalf, RelayWriteHalf) {
        match self {
            Self::Tcp(stream) => {
                let (read, write) = stream.into_split();
                (RelayReadHalf::Tcp(read), RelayWriteHalf::Tcp(write))
            }
            #[cfg(feature = "relay-quic")]
            Self::Quic(stream) => {
                let (read, write) = stream.into_split();
                (RelayReadHalf::Quic(read), RelayWriteHalf::Quic(write))
            }
        }
    }
}

pub enum RelayReadHalf {
    Tcp(tokio::net::tcp::OwnedReadHalf),
    #[cfg(feature = "relay-quic")]
    Quic(crate::quic::QuicReadHalf),
}
pub enum RelayWriteHalf {
    Tcp(tokio::net::tcp::OwnedWriteHalf),
    #[cfg(feature = "relay-quic")]
    Quic(crate::quic::QuicWriteHalf),
}

macro_rules! read_impl {
    ($ty:ty) => {
        impl AsyncRead for $ty {
            fn poll_read(
                mut self: Pin<&mut Self>,
                cx: &mut Context<'_>,
                buf: &mut ReadBuf<'_>,
            ) -> Poll<io::Result<()>> {
                match &mut *self {
                    Self::Tcp(stream) => Pin::new(stream).poll_read(cx, buf),
                    #[cfg(feature = "relay-quic")]
                    Self::Quic(stream) => Pin::new(stream).poll_read(cx, buf),
                }
            }
        }
    };
}
macro_rules! write_impl {
    ($ty:ty) => {
        impl AsyncWrite for $ty {
            fn poll_write(
                mut self: Pin<&mut Self>,
                cx: &mut Context<'_>,
                buf: &[u8],
            ) -> Poll<io::Result<usize>> {
                match &mut *self {
                    Self::Tcp(stream) => Pin::new(stream).poll_write(cx, buf),
                    #[cfg(feature = "relay-quic")]
                    Self::Quic(stream) => Pin::new(stream).poll_write(cx, buf),
                }
            }
            fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
                match &mut *self {
                    Self::Tcp(stream) => Pin::new(stream).poll_flush(cx),
                    #[cfg(feature = "relay-quic")]
                    Self::Quic(stream) => Pin::new(stream).poll_flush(cx),
                }
            }
            fn poll_shutdown(
                mut self: Pin<&mut Self>,
                cx: &mut Context<'_>,
            ) -> Poll<io::Result<()>> {
                match &mut *self {
                    Self::Tcp(stream) => Pin::new(stream).poll_shutdown(cx),
                    #[cfg(feature = "relay-quic")]
                    Self::Quic(stream) => Pin::new(stream).poll_shutdown(cx),
                }
            }
        }
    };
}
read_impl!(RelayStream);
read_impl!(RelayReadHalf);
write_impl!(RelayStream);
write_impl!(RelayWriteHalf);

/// Monomorphize the data path for the selected carrier. No per-I/O enum match.
#[cfg(feature = "relay-quic")]
#[macro_export]
macro_rules! with_relay_stream {
    ($stream:expr, $name:ident, $body:expr) => {
        match $stream {
            $crate::transport::RelayStream::Tcp($name) => $body,
            $crate::transport::RelayStream::Quic($name) => $body,
        }
    };
}

/// TCP-only builds erase carrier dispatch entirely, including in downstream crates.
#[cfg(not(feature = "relay-quic"))]
#[macro_export]
macro_rules! with_relay_stream {
    ($stream:expr, $name:ident, $body:expr) => {
        match $stream {
            $crate::transport::RelayStream::Tcp($name) => $body,
        }
    };
}
