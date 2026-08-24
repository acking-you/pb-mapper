//! One tunnel over a [`Relay`]: echo server, `register`, and `connect`.

use std::net::SocketAddr;
use std::time::Duration;

use pb_mapper_client::client::run_client_side_cli_with_callback_scoped;
use pb_mapper_client::server::{ServerTunnelOptions, run_server_side_cli_with_pinned_credential};
use pb_mapper_core::checksum::Credential;
use tokio::net::{TcpListener, UdpSocket};
use tokio::task::JoinHandle;
use tokio::time::Instant;
use uni_stream::stream::{
    TcpListenerProvider, TcpStreamProvider, UdpListenerProvider, UdpStreamProvider,
};

use crate::echo::{tcp_echo_server, udp_echo_server};
use crate::relay::Relay;
use crate::traffic::{connected_udp_socket, probe_udp_socket, raw_tcp_probe};
use crate::{PROBE, READY_TIMEOUT, Transport, admin_credential, reserve_addr};

/// What kind of tunnel to build. Everything but the transport has a default that
/// matches the plain administrator case.
#[derive(Debug, Clone)]
pub struct TunnelSpec {
    transport: Transport,
    need_codec: bool,
    keep_alive: bool,
    credential: Option<Credential>,
    /// Overrides `credential` on the `connect` side only, for the case where one
    /// party registers a service and a different one subscribes to it.
    connect_credential: Option<Credential>,
    service_key: Option<String>,
    /// The namespace `register` and `connect` ask for. `None` means "the
    /// credential's own", which for a temporary credential is its key ID.
    namespace: Option<u64>,
    /// Required for an administrator registering into a temporary namespace.
    force_namespace: bool,
    /// A byte identifying the echo server: per datagram on UDP, once per
    /// connection on TCP.
    echo_tag: Option<u8>,
}

impl TunnelSpec {
    pub fn new(transport: Transport) -> Self {
        Self {
            transport,
            need_codec: false,
            keep_alive: false,
            credential: None,
            connect_credential: None,
            service_key: None,
            namespace: None,
            force_namespace: false,
            echo_tag: None,
        }
    }

    /// Encrypt forwarded traffic, as `register --codec` does.
    pub fn codec(mut self, need_codec: bool) -> Self {
        self.need_codec = need_codec;
        self
    }

    pub fn keep_alive(mut self, keep_alive: bool) -> Self {
        self.keep_alive = keep_alive;
        self
    }

    /// Authenticate as `credential` instead of the administrator.
    pub fn credential(mut self, credential: Credential) -> Self {
        self.credential = Some(credential);
        self
    }

    /// Subscribe as a different credential than the one that registered.
    ///
    /// A temporary credential may name its own namespace explicitly, so this
    /// composes with [`TunnelSpec::namespace`] without a second namespace knob.
    pub fn connect_credential(mut self, credential: Credential) -> Self {
        self.connect_credential = Some(credential);
        self
    }

    /// Use an explicit service name. Two tunnels may share one only if they are
    /// in different namespaces.
    pub fn service_key(mut self, key: impl Into<String>) -> Self {
        self.service_key = Some(key.into());
        self
    }

    /// Register and connect inside an explicit namespace.
    pub fn namespace(mut self, namespace: u64) -> Self {
        self.namespace = Some(namespace);
        self
    }

    /// Acknowledge an administrator registration into a temporary namespace.
    pub fn force_namespace(mut self, force: bool) -> Self {
        self.force_namespace = force;
        self
    }

    /// Have this tunnel's echo server identify itself with `tag`.
    ///
    /// Two tunnels sharing a service name across namespaces need distinct tags:
    /// without them, traffic reaching the wrong echo server would still come back
    /// byte-identical and the test would pass on a leak.
    ///
    /// A UDP server tags every datagram; a TCP server tags the connection once,
    /// ahead of its first echoed byte. See `echo.rs` for why a stream cannot carry
    /// it per reply.
    pub fn echo_tag(mut self, tag: u8) -> Self {
        self.echo_tag = Some(tag);
        self
    }

    fn label(&self) -> String {
        format!(
            "{}-{}",
            self.transport.name(),
            if self.need_codec { "codec" } else { "plain" }
        )
    }
}

/// A running tunnel. `Drop` aborts its tasks; the echo server, `register`, and
/// `connect` all stop with it.
pub struct Tunnel {
    tunnel_addr: SocketAddr,
    transport: Transport,
    service_key: String,
    echo_tag: Option<u8>,
    /// The socket every UDP probe reuses.
    ///
    /// The relay keys UDP streams by source address, so a fresh socket starts a
    /// fresh stream whose first datagram can be dropped during setup. Probing on
    /// one socket means a retry exercises the stream the previous attempt
    /// established, rather than opening another one — and it is what makes
    /// [`Self::wait_until_not_forwarding`] observe the established stream going
    /// away instead of a new stream failing to start.
    udp_probe_socket: Option<UdpSocket>,
    tasks: Vec<JoinHandle<()>>,
}

impl Tunnel {
    /// Build the tunnel and return once it carries traffic.
    pub async fn start(relay: &Relay, spec: TunnelSpec) -> Self {
        let relay_addr = relay.addr();
        let transport = spec.transport;
        let credential = spec.credential.unwrap_or_else(admin_credential);
        let connect_credential = spec.connect_credential.unwrap_or(credential);
        let label = spec.label();
        let service_key = spec.service_key.clone().unwrap_or(format!("echo-{label}"));
        let echo_tag = spec.echo_tag;
        let mut tasks = Vec::new();

        // The echo server owns its socket from the start, so its address is known
        // without a reservation window.
        let echo_addr = match transport {
            Transport::Tcp => {
                let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                let addr = listener.local_addr().unwrap();
                tasks.push(tokio::spawn(tcp_echo_server(listener, echo_tag)));
                addr
            }
            Transport::Udp => {
                let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
                let addr = socket.local_addr().unwrap();
                tasks.push(tokio::spawn(udp_echo_server(socket, echo_tag)));
                addr
            }
        };

        let options = ServerTunnelOptions {
            need_codec: spec.need_codec,
            is_datagram: transport.is_datagram(),
            keep_alive: spec.keep_alive,
            namespace: spec.namespace,
            force_namespace: spec.force_namespace,
        };
        let register_key = service_key.clone();
        tasks.push(tokio::spawn(async move {
            match transport {
                Transport::Tcp => {
                    run_server_side_cli_with_pinned_credential::<TcpStreamProvider, _>(
                        echo_addr,
                        relay_addr,
                        register_key.into(),
                        options,
                        None,
                        credential,
                    )
                    .await
                }
                Transport::Udp => {
                    run_server_side_cli_with_pinned_credential::<UdpStreamProvider, _>(
                        echo_addr,
                        relay_addr,
                        register_key.into(),
                        options,
                        None,
                        credential,
                    )
                    .await
                }
            }
        }));

        // `register` must have published the key before `connect` probes for it;
        // otherwise `connect` burns its backoff waiting.
        relay
            .wait_for_registration(&service_key, credential, spec.namespace)
            .await;

        let tunnel_addr = reserve_addr(transport).await;
        let connect_key = service_key.clone();
        let keep_alive = spec.keep_alive;
        let namespace = spec.namespace;
        tasks.push(tokio::spawn(async move {
            match transport {
                Transport::Tcp => {
                    run_client_side_cli_with_callback_scoped::<TcpListenerProvider, _>(
                        tunnel_addr,
                        relay_addr,
                        connect_key.into(),
                        keep_alive,
                        namespace,
                        None,
                        Some(connect_credential),
                    )
                    .await
                }
                Transport::Udp => {
                    run_client_side_cli_with_callback_scoped::<UdpListenerProvider, _>(
                        tunnel_addr,
                        relay_addr,
                        connect_key.into(),
                        keep_alive,
                        namespace,
                        None,
                        Some(connect_credential),
                    )
                    .await
                }
            }
        }));

        let udp_probe_socket = match transport {
            Transport::Tcp => None,
            Transport::Udp => Some(connected_udp_socket(tunnel_addr).await),
        };

        let tunnel = Self {
            tunnel_addr,
            transport,
            service_key,
            echo_tag,
            udp_probe_socket,
            tasks,
        };
        tunnel.wait_until_forwarding().await;
        tunnel
    }

    /// Where a test client sends its traffic — the address `connect` listens on.
    pub fn addr(&self) -> SocketAddr {
        self.tunnel_addr
    }

    pub fn transport(&self) -> Transport {
        self.transport
    }

    pub fn service_key(&self) -> &str {
        &self.service_key
    }

    /// The tag this tunnel's echo server identifies itself with, if any.
    pub fn echo_tag(&self) -> Option<u8> {
        self.echo_tag
    }

    /// What a reply to a single probe should look like, tag included.
    ///
    /// Correct for both transports because a probe is one datagram, or the first
    /// and only exchange on a fresh connection — which is exactly where a TCP
    /// tag sits.
    fn expected_echo(&self, payload: &[u8]) -> Vec<u8> {
        let mut expected = Vec::with_capacity(payload.len() + 1);
        expected.extend(self.echo_tag);
        expected.extend_from_slice(payload);
        expected
    }

    /// Poll until a payload round-trips, or fail the test.
    ///
    /// This is true end-to-end readiness — the relay, `register`'s control
    /// connection, `connect`'s local listener, and the echo server at once — which
    /// is why no case here needs a sleep.
    pub async fn wait_until_forwarding(&self) {
        let deadline = Instant::now() + READY_TIMEOUT;
        let mut last_error = String::from("no attempt completed");
        while Instant::now() < deadline {
            match self.probe_once().await {
                Ok(()) => return,
                Err(error) => last_error = error,
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!(
            "{} tunnel at {} never forwarded traffic: {last_error}",
            self.transport.name(),
            self.tunnel_addr
        );
    }

    /// Poll until a probe stops round-tripping, or fail the test.
    ///
    /// A credential's expiry or revocation cancels its lease, which drops the
    /// forwarding tasks; this is how a test observes that from the outside.
    pub async fn wait_until_not_forwarding(&self, within: Duration) {
        let deadline = Instant::now() + within;
        while Instant::now() < deadline {
            if self.probe_once().await.is_err() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!(
            "{} tunnel at {} still forwards traffic after {within:?}",
            self.transport.name(),
            self.tunnel_addr
        );
    }

    async fn probe_once(&self) -> Result<(), String> {
        match self.transport {
            Transport::Tcp => self.probe_tcp().await,
            Transport::Udp => self.probe_udp().await,
        }
    }

    /// Unframed on purpose: the tunnel's local side is byte-transparent, and a
    /// framed probe would misread an echo tag as part of the length header.
    async fn probe_tcp(&self) -> Result<(), String> {
        raw_tcp_probe(self.tunnel_addr, PROBE, &self.expected_echo(PROBE)).await
    }

    async fn probe_udp(&self) -> Result<(), String> {
        let socket = self
            .udp_probe_socket
            .as_ref()
            .ok_or_else(|| "no udp probe socket on a tcp tunnel".to_string())?;
        probe_udp_socket(socket, &self.expected_echo(PROBE)).await
    }
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

/// A relay and one tunnel on it, for the common case where a test wants a
/// complete flow and nothing else.
///
/// The field order matters: `Tunnel` is dropped before `Relay`, so `register` and
/// `connect` stop before the relay they talk to.
pub struct TunnelHarness {
    tunnel: Tunnel,
    relay: Relay,
}

impl TunnelHarness {
    /// The one-liner: an administrator-credentialed tunnel over its own relay.
    pub async fn start(transport: Transport, need_codec: bool) -> Self {
        Self::with_spec(TunnelSpec::new(transport).codec(need_codec)).await
    }

    pub async fn with_spec(spec: TunnelSpec) -> Self {
        let relay = Relay::start(&spec.label()).await;
        let tunnel = relay.start_tunnel(spec).await;
        Self { tunnel, relay }
    }

    pub fn relay(&self) -> &Relay {
        &self.relay
    }

    pub fn tunnel(&self) -> &Tunnel {
        &self.tunnel
    }

    /// Where a test client sends its traffic.
    pub fn tunnel_addr(&self) -> SocketAddr {
        self.tunnel.addr()
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    use super::*;
    use crate::{RAW_TCP_PAYLOAD_MAX, run_raw_tcp_echo};

    /// A tagged TCP tunnel carries a payload larger than one read intact.
    ///
    /// This is the case the harness got wrong: the echo server tagged every read,
    /// so a payload that arrived as two reads came back with a tag injected into
    /// its middle. It only showed up past roughly 4 KiB, which the driver's
    /// payloads did not reach — so the bug sat under passing tests, waiting for
    /// whoever raised the size.
    #[tokio::test]
    async fn a_tagged_tcp_tunnel_survives_a_payload_larger_than_one_read() {
        let relay = Relay::start("tag-fragmentation").await;
        let tunnel = relay
            .start_tunnel(TunnelSpec::new(Transport::Tcp).echo_tag(b'1'))
            .await;

        let mut stream = TcpStream::connect(tunnel.addr()).await.unwrap();
        // Past one segment and past the 8 KiB initial forwarding buffer, so the
        // echo server is guaranteed more than one read.
        let payload = vec![0xABu8; 64 * 1024];
        stream.write_all(&payload).await.unwrap();

        let mut echoed = vec![0u8; payload.len() + 1];
        tokio::time::timeout(Duration::from_secs(5), stream.read_exact(&mut echoed))
            .await
            .expect("tagged echo timed out")
            .expect("tagged echo failed");

        assert_eq!(echoed[0], b'1', "the reply should open with the tag");
        assert_eq!(
            &echoed[1..],
            &payload[..],
            "the payload came back altered, so a tag was injected mid-stream"
        );
    }

    /// The driver's payloads must be able to exceed one read, or the case below
    /// proves nothing about fragmentation.
    const _: () = assert!(RAW_TCP_PAYLOAD_MAX > 8 * 1024);

    /// The driver agrees with the server about where the tag goes, over enough
    /// rounds that its random sizes cross the fragmentation threshold.
    #[tokio::test]
    async fn the_raw_tcp_driver_agrees_with_a_tagged_server() {
        let relay = Relay::start("tag-driver").await;
        let tunnel = relay
            .start_tunnel(TunnelSpec::new(Transport::Tcp).echo_tag(b'7'))
            .await;
        run_raw_tcp_echo(tunnel.addr(), 12, Some(b'7')).await;
    }
}
