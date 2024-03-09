use std::net::SocketAddr;

use snafu::ResultExt;
use tokio::net::{TcpListener, ToSocketAddrs};

use super::error::{LsnListenerAcceptSnafu, LsnListenerBindSnafu, Result};
use super::stream::{NetStream, TcpStreamImpl};

pub trait ListenerProvider {
    type Listener: StreamAccept + 'static;

    fn bind<A: ToSocketAddrs + Send>(
        addr: A,
    ) -> impl std::future::Future<Output = Result<Self::Listener>> + Send;
}

pub trait StreamAccept {
    type Item: NetStream;

    fn accept(&self) -> impl std::future::Future<Output = Result<(Self::Item, SocketAddr)>> + Send;
}

pub struct TcpListenerProvider;

pub struct TcpListenerImpl(TcpListener);

impl StreamAccept for TcpListenerImpl {
    type Item = TcpStreamImpl;

    async fn accept(&self) -> Result<(Self::Item, SocketAddr)> {
        let (stream, addr) = self.0.accept().await.context(LsnListenerAcceptSnafu {
            listener_type: "TCP",
        })?;
        Ok((TcpStreamImpl(stream), addr))
    }
}

impl ListenerProvider for TcpListenerProvider {
    type Listener = TcpListenerImpl;

    async fn bind<A: ToSocketAddrs + Send>(addr: A) -> Result<Self::Listener> {
        Ok(TcpListenerImpl(TcpListener::bind(addr).await.context(
            LsnListenerBindSnafu {
                listener_type: "TCP",
            },
        )?))
    }
}
