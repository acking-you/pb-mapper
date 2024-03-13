use std::net::SocketAddr;

use snafu::ResultExt;
use tokio::net::TcpListener;

use super::error::{LsnListenerAcceptSnafu, LsnListenerBindSnafu, Result};
use super::stream::{NetworkStream, TcpStreamImpl, UdpStreamImpl};
use crate::utils::addr::{each_addr, ToSocketAddrs};
use crate::utils::udp::UdpListener;

pub trait ListenerProvider {
    type Listener: StreamAccept + 'static;

    fn bind<A: ToSocketAddrs + Send>(
        addr: A,
    ) -> impl std::future::Future<Output = Result<Self::Listener>> + Send;
}

pub trait StreamAccept {
    type Item: NetworkStream;

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
        Ok((TcpStreamImpl::new(stream), addr))
    }
}

impl ListenerProvider for TcpListenerProvider {
    type Listener = TcpListenerImpl;

    async fn bind<A: ToSocketAddrs + Send>(addr: A) -> Result<Self::Listener> {
        Ok(TcpListenerImpl(
            each_addr(addr, TcpListener::bind)
                .await
                .context(LsnListenerBindSnafu {
                    listener_type: "TCP",
                })?,
        ))
    }
}

pub struct UdpListenerProvider;

pub struct UdpListenerImpl(UdpListener);

impl StreamAccept for UdpListenerImpl {
    type Item = UdpStreamImpl;

    async fn accept(&self) -> Result<(Self::Item, SocketAddr)> {
        let (stream, addr) = self.0.accept().await.context(LsnListenerAcceptSnafu {
            listener_type: "UDP",
        })?;
        Ok((UdpStreamImpl::new(stream), addr))
    }
}

impl ListenerProvider for UdpListenerProvider {
    type Listener = UdpListenerImpl;

    async fn bind<A: ToSocketAddrs + Send>(addr: A) -> Result<Self::Listener> {
        Ok(UdpListenerImpl(
            each_addr(addr, UdpListener::bind)
                .await
                .context(LsnListenerBindSnafu {
                    listener_type: "UDP",
                })?,
        ))
    }
}
