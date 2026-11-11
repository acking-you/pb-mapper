//! A connect tunnel's local TCP listener.

use std::io;
use std::net::SocketAddr;

use tokio::net::TcpListener;
use uni_stream::addr::{ToSocketAddrs, each_addr};
use uni_stream::stream::{ListenerProvider, StreamAccept, TcpStreamImpl};

/// Binds a connect tunnel's local TCP listener as uni-stream's
/// `TcpListenerProvider` does, but with the accept queue
/// [`pb_mapper_core::listener`] states: the tunnel's local connections wait
/// there while its setups are full.
pub struct LocalTcpListenerProvider;

/// A connect tunnel's local TCP listener, bound by
/// [`LocalTcpListenerProvider`].
pub struct LocalTcpListener(TcpListener);

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
