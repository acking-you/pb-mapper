//! The SDK side of a case: an administrator client, and a service published
//! or subscribed to, each ready before it is handed back.
//!
//! The helpers take the options a case passes, never a default of their own:
//! what a case exercises (`allow_direct`, the path policy) stays visible at
//! its call site.

use std::net::SocketAddr;

use pb_mapper_client::sdk::{
    self, Client, ConnectOptions, ConnectRequest, Connection, RegisterOptions, RegisterRequest,
    Registration,
};

use crate::{READY_TIMEOUT, Transport, admin_credential, reserve_addr};

/// A client of the relay at `relay`, authenticated as the administrator.
pub fn admin_client(relay: SocketAddr) -> Client {
    Client::from_credential(relay.to_string(), admin_credential(), false, None)
}

/// Publish `backend` as `key` and wait, up to [`READY_TIMEOUT`], until the
/// relay has accepted the registration.
pub async fn publish(
    client: &Client,
    key: &str,
    backend: SocketAddr,
    transport: sdk::Transport,
    codec: bool,
    options: RegisterOptions,
) -> Registration {
    let registration = client
        .register_with_options(
            RegisterRequest {
                key: key.into(),
                local_addr: backend.to_string(),
                transport,
                codec,
                force_namespace: false,
            },
            options,
        )
        .await
        .unwrap();
    registration
        .wait_ready_timeout(READY_TIMEOUT)
        .await
        .unwrap();
    registration
}

/// Subscribe to `key` on a reserved loopback port and wait, up to
/// [`READY_TIMEOUT`], until it forwards. Returns the port with the connection.
pub async fn subscribe(
    client: &Client,
    key: &str,
    transport: sdk::Transport,
    options: ConnectOptions,
) -> (Connection, SocketAddr) {
    let local_addr = reserve_addr(transport.into()).await;
    let connection = client
        .connect_with_options(
            ConnectRequest {
                key: key.into(),
                local_addr: local_addr.to_string(),
                transport,
            },
            options,
        )
        .await
        .unwrap();
    connection.wait_ready_timeout(READY_TIMEOUT).await.unwrap();
    (connection, local_addr)
}

impl From<sdk::Transport> for Transport {
    fn from(transport: sdk::Transport) -> Self {
        match transport {
            sdk::Transport::Tcp => Self::Tcp,
            sdk::Transport::Udp => Self::Udp,
        }
    }
}
