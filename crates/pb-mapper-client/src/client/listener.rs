//! A connect tunnel's local listeners.

use std::io;
use std::net::SocketAddr;

use tokio::net::TcpListener;
use tokio::sync::watch;
use uni_stream::addr::{ToSocketAddrs, each_addr};
use uni_stream::stream::{ListenerProvider, StreamAccept, TcpStreamImpl, UdpStreamImpl};
use uni_stream::udp::UdpListener;

/// Binds a connect tunnel's local TCP listener as uni-stream's
/// `TcpListenerProvider` does, but with the accept queue
/// [`pb_mapper_core::listener`] states: the tunnel's local connections wait
/// there while its setups are full.
pub struct LocalTcpListenerProvider;

/// A connect tunnel's local TCP listener, bound by
/// [`LocalTcpListenerProvider`].
pub struct LocalTcpListener(TcpListener);

impl LocalTcpListener {
    /// Where it is bound.
    pub(crate) fn local_addr(&self) -> io::Result<SocketAddr> {
        self.0.local_addr()
    }
}

impl ListenerProvider for LocalTcpListenerProvider {
    type Listener = LocalTcpListener;

    async fn bind<A: ToSocketAddrs + Send>(addr: A) -> io::Result<Self::Listener> {
        each_addr(addr, |addr| {
            std::future::ready(pb_mapper_core::listener::bind(addr))
        })
        .await
        .map(LocalTcpListener)
    }
}

impl StreamAccept for LocalTcpListener {
    type Item = TcpStreamImpl;

    async fn accept(&self) -> io::Result<(Self::Item, SocketAddr)> {
        let (stream, peer) = self.0.accept().await?;
        Ok((TcpStreamImpl::new(stream), peer))
    }
}

/// Binds a connect tunnel's local UDP listener as uni-stream's
/// `UdpListenerProvider` does, to a listener that can say where it is bound.
pub(crate) struct LocalUdpListenerProvider;

/// A connect tunnel's local UDP listener, bound by
/// [`LocalUdpListenerProvider`].
pub(crate) struct LocalUdpListener(UdpListener);

impl LocalUdpListener {
    /// Where it is bound.
    pub(crate) fn local_addr(&self) -> io::Result<SocketAddr> {
        self.0.local_addr()
    }
}

impl ListenerProvider for LocalUdpListenerProvider {
    type Listener = LocalUdpListener;

    async fn bind<A: ToSocketAddrs + Send>(addr: A) -> io::Result<Self::Listener> {
        UdpListener::bind(addr).await.map(LocalUdpListener)
    }
}

impl StreamAccept for LocalUdpListener {
    type Item = UdpStreamImpl;

    async fn accept(&self) -> io::Result<(Self::Item, SocketAddr)> {
        let (stream, peer) = self.0.accept().await?;
        Ok((UdpStreamImpl::new(stream), peer))
    }
}

/// Where a connect tunnel tells its handle the address its local listener is
/// bound to, read from the listener by `addr_of`: the port the system chose,
/// when the caller asked for port 0.
pub(crate) struct Bound<L> {
    pub(crate) addr_of: fn(&L) -> io::Result<SocketAddr>,
    pub(crate) to: watch::Sender<Option<SocketAddr>>,
}
