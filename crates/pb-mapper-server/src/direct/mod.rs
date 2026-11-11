//! Setup coordination is serialized by the existing routing manager. Established
//! direct sessions are observations only and never acquire an authorization lease.
use super::ImutableKey;
use pb_mapper_auth::AuthContext;
use pb_mapper_core::conn_id::RemoteConnId;
use pb_mapper_protocol::direct::*;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use tokio::sync::{mpsc, oneshot};

#[cfg(feature = "p2p")]
mod stun;

mod control;
mod state;

pub(crate) use control::handle_control;
pub(crate) use state::DirectState;

#[derive(Clone, Debug)]
pub struct DirectServerConfig {
    /// UDP address the STUN service binds.
    pub listen: SocketAddr,
    /// Public address announced to clients. Port 0 takes the bound port. An
    /// unspecified IP announces only the port: each client then uses the relay
    /// IP it dialed, which is right whenever the UDP port is forwarded like the
    /// TCP one, including behind cloud 1:1 NAT where the host never sees its
    /// public IP.
    pub advertise: SocketAddr,
}

impl DirectServerConfig {
    /// Direct coordination on the relay's own port number over UDP, announcing
    /// only that port: each client pairs it with the relay address it dialed.
    pub fn on_relay_port(ip: IpAddr, port: u16) -> Self {
        Self {
            listen: SocketAddr::new(ip, port),
            advertise: SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0),
        }
    }

    /// [`Self::on_relay_port`] if that UDP port can be bound now, so that a
    /// relay asked for no direct service in particular can still start,
    /// relay-only, when the port is taken. The probe socket is closed again
    /// before this returns.
    pub fn probe_relay_port(ip: IpAddr, port: u16) -> std::io::Result<Self> {
        let config = Self::on_relay_port(ip, port);
        drop(std::net::UdpSocket::bind(config.listen)?);
        Ok(config)
    }
}

/// The STUN service direct coordination runs: what it announces to clients,
/// and the task serving it.
pub(crate) struct StunService {
    pub(crate) capability: DirectCapability,
    pub(crate) task: tokio_util::task::AbortOnDropHandle<()>,
}

/// Start the STUN service `config` asks for, if any.
pub(crate) async fn bind(
    config: Option<DirectServerConfig>,
) -> std::io::Result<Option<StunService>> {
    let Some(config) = config else {
        return Ok(None);
    };
    #[cfg(not(feature = "p2p"))]
    {
        let _ = config;
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "binary built without p2p",
        ))
    }
    #[cfg(feature = "p2p")]
    {
        let socket = tokio::net::UdpSocket::bind(config.listen).await?;
        let mut advertise = config.advertise;
        if advertise.port() == 0 {
            advertise.set_port(socket.local_addr()?.port());
        }
        if !advertise.ip().is_unspecified() && !valid_candidate(&advertise) {
            return Err(std::io::Error::other(
                "direct advertise address must be reachable unicast IP:port",
            ));
        }
        let capability = DirectCapability {
            version: DIRECT_VERSION,
            stun_addr: advertise,
        };
        if advertise.ip().is_unspecified() {
            tracing::info!(
                event = "direct_service_listening",
                listen = %socket.local_addr()?,
                advertise_port = advertise.port(),
                "direct coordination announces its port; clients pair it with the relay address they dialed"
            );
        } else {
            tracing::info!(event = "direct_service_listening", listen = %socket.local_addr()?, %advertise);
        }
        let task = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
            if let Err(error) = stun::serve(socket).await {
                tracing::error!(%error, "STUN service stopped");
            }
        }));
        Ok(Some(StunService { capability, task }))
    }
}

pub type RelaySelection = (ImutableKey, RemoteConnId, u64);

pub enum DirectTask {
    Connect {
        connection: RemoteConnId,
        namespace: u64,
        auth: AuthContext,
        sender: mpsc::Sender<DirectEvent>,
    },
    Client {
        connection: RemoteConnId,
        message: DirectClientMessage,
    },
    Provider {
        connection: RemoteConnId,
        key: ImutableKey,
        message: DirectProviderMessage,
    },
    Disconnect {
        connection: RemoteConnId,
    },
    Relay {
        auth: AuthContext,
        namespace: u64,
        id: SessionId,
        secret: SessionSecret,
        response: oneshot::Sender<Option<RelaySelection>>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The default a relay offers: the relay's port, announced without an IP,
    /// and nothing at all where that UDP port is already taken.
    #[test]
    fn the_relay_port_default_yields_to_a_taken_port() {
        let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let held = std::net::UdpSocket::bind((ip, 0)).unwrap();
        let port = held.local_addr().unwrap().port();
        let config = DirectServerConfig::on_relay_port(ip, port);
        assert_eq!(config.listen, SocketAddr::new(ip, port));
        assert_eq!(
            config.advertise,
            SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0)
        );
        assert!(DirectServerConfig::probe_relay_port(ip, port).is_err());
        let free = DirectServerConfig::probe_relay_port(ip, 0).unwrap();
        assert_eq!(free.listen, SocketAddr::new(ip, 0));
    }
}
