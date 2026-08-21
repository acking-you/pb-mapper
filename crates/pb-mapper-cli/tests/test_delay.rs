// See the note in `regression.rs`: the whole file is test code.
#![allow(clippy::unwrap_used, clippy::expect_used)]

//! End-to-end tunnel tests over the full `server` + `register` + `connect` path.
//!
//! Every case builds its own relay, echo server, and tunnel on reserved loopback
//! ports, so the four transport/codec combinations run concurrently and none of
//! them collides with a relay already running on the machine.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use pb_mapper_auth::{AuthConfig, AuthRuntime, LegacyProtocolPolicy};
use pb_mapper_client::client::run_client_side_cli_with_pinned_credential;
use pb_mapper_client::client::status::get_status_with_credential;
use pb_mapper_client::server::{ServerTunnelOptions, run_server_side_cli_with_pinned_credential};
use pb_mapper_core::checksum::{Credential, set_process_msg_header_key};
use pb_mapper_core::config::init_tracing;
use pb_mapper_protocol::command::{PbConnStatusReq, PbConnStatusResp};
use pb_mapper_protocol::{MessageReader, MessageWriter, NormalMessageReader, NormalMessageWriter};
use pb_mapper_server::run_server_on_listener;
use rand::RngExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::task::JoinHandle;
use tokio::time::{Instant, timeout};
use tokio_util::sync::CancellationToken;
use uni_stream::addr::ToSocketAddrs;
use uni_stream::stream::{
    StreamProvider, StreamSplit, TcpListenerProvider, TcpStreamProvider, UdpListenerProvider,
    UdpStreamProvider,
};
use uni_stream::udp::tune_udp_socket;

const TEST_ADMIN_KEY: &str = "0123456789abcdefghijklmnopqrstuv";
const UDP_TEST_PAYLOAD_MAX: usize = 1200;
const PROBE: &[u8] = b"pb-mapper-probe";
/// Every readiness probe polls until this deadline before failing the test.
const READY_TIMEOUT: Duration = Duration::from_secs(10);

static TEST_ENV: LazyLock<()> = LazyLock::new(|| {
    init_tracing();
    // `run_echo_delay` frames its payload with the process credential's checksum,
    // so one must be configured before any case runs. Every case uses the same
    // administrator key, which keeps this a one-time write.
    set_process_msg_header_key(Some(TEST_ADMIN_KEY)).unwrap();
});

fn admin_credential() -> Credential {
    Credential::Admin(*TEST_ADMIN_KEY.as_bytes().first_chunk::<32>().unwrap())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Transport {
    Tcp,
    Udp,
}

impl Transport {
    fn name(self) -> &'static str {
        match self {
            Self::Tcp => "tcp",
            Self::Udp => "udp",
        }
    }

    fn is_datagram(self) -> bool {
        self == Self::Udp
    }
}

struct TimerTickGuard<'a> {
    ins: Instant,
    mut_duration: &'a mut Duration,
}

impl<'a> TimerTickGuard<'a> {
    fn new(mut_duration: &'a mut Duration) -> Self {
        Self {
            ins: Instant::now(),
            mut_duration,
        }
    }
}

impl Drop for TimerTickGuard<'_> {
    fn drop(&mut self) {
        let duration = Instant::now() - self.ins;
        *self.mut_duration += duration;
        println!("duration:{duration:?}");
    }
}

/// Reserve a loopback port by binding it and immediately dropping the socket.
///
/// TCP and UDP have separate port spaces, so the reservation has to use the same
/// protocol the caller will bind. The relay keeps its own listener (see
/// [`TunnelHarness::start`]); this is only for `connect`, whose bind happens
/// inside the client and cannot be handed a pre-bound socket.
async fn reserve_addr(transport: Transport) -> SocketAddr {
    match transport {
        Transport::Tcp => {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            drop(listener);
            addr
        }
        Transport::Udp => {
            let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
            let addr = socket.local_addr().unwrap();
            drop(socket);
            addr
        }
    }
}

fn auth_config(label: &str) -> AuthConfig {
    static SEQUENCE: AtomicUsize = AtomicUsize::new(0);
    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    AuthConfig {
        state_dir: std::env::temp_dir().join(format!(
            "pb-mapper-delay-{}-{label}-{sequence}",
            std::process::id()
        )),
        max_temporary_keys: 64,
        max_temporary_key_ttl: Duration::from_secs(3600),
        legacy_protocol: LegacyProtocolPolicy::Allow,
    }
}

async fn tcp_echo_server(listener: TcpListener) {
    loop {
        let Ok((mut stream, peer)) = listener.accept().await else {
            return;
        };
        tokio::spawn(async move {
            let mut buf = vec![0u8; 4096];
            loop {
                match stream.read(&mut buf).await {
                    Ok(0) | Err(_) => return,
                    Ok(n) => {
                        if stream.write_all(&buf[..n]).await.is_err() {
                            return;
                        }
                        tracing::debug!("echoed {n} bytes to {peer}");
                    }
                }
            }
        });
    }
}

async fn udp_echo_server(socket: UdpSocket) {
    tune_udp_socket(&socket);
    let mut buf = vec![0u8; 65_507];
    loop {
        let Ok((len, peer)) = socket.recv_from(&mut buf).await else {
            return;
        };
        if let Err(err) = socket.send_to(&buf[..len], peer).await {
            tracing::warn!("udp echo send error: {err}");
        }
    }
}

/// One complete tunnel: relay, echo server, `register`, and `connect`.
///
/// `start` returns only once the tunnel carries traffic, so the tests contain no
/// timing assumptions. `Drop` tears the four tasks down and removes the relay's
/// authentication state directory.
struct TunnelHarness {
    /// Where a test client sends its traffic — the address `connect` listens on.
    tunnel_addr: SocketAddr,
    transport: Transport,
    shutdown: CancellationToken,
    tasks: Vec<JoinHandle<()>>,
    state_dir: PathBuf,
}

impl TunnelHarness {
    async fn start(transport: Transport, need_codec: bool) -> Self {
        *TEST_ENV;
        let label = format!(
            "{}-{}",
            transport.name(),
            if need_codec { "codec" } else { "plain" }
        );
        let config = auth_config(&label);
        let _ = std::fs::remove_dir_all(&config.state_dir);
        let state_dir = config.state_dir.clone();
        let credential = admin_credential();

        // The relay gets its listener pre-bound, so nothing can take the port
        // between reserving it and the relay's own bind.
        let relay_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let relay_addr = relay_listener.local_addr().unwrap();
        let auth = AuthRuntime::start(
            *TEST_ADMIN_KEY.as_bytes().first_chunk::<32>().unwrap(),
            config,
        )
        .await
        .unwrap();

        let shutdown = CancellationToken::new();
        let relay_shutdown = shutdown.clone();
        let mut tasks = Vec::new();
        tasks.push(tokio::spawn(async move {
            if let Err(error) =
                run_server_on_listener(relay_listener, relay_shutdown, None, false, auth).await
            {
                tracing::error!("relay stopped: {error}");
            }
        }));

        // The echo server also owns its socket from the start.
        let echo_addr = match transport {
            Transport::Tcp => {
                let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                let addr = listener.local_addr().unwrap();
                tasks.push(tokio::spawn(tcp_echo_server(listener)));
                addr
            }
            Transport::Udp => {
                let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
                let addr = socket.local_addr().unwrap();
                tasks.push(tokio::spawn(udp_echo_server(socket)));
                addr
            }
        };

        let service_key = format!("echo-{label}");
        let options = ServerTunnelOptions {
            need_codec,
            is_datagram: transport.is_datagram(),
            keep_alive: false,
            namespace: None,
            force_namespace: false,
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
        wait_for_registration(relay_addr, &service_key, credential).await;

        let tunnel_addr = reserve_addr(transport).await;
        let connect_key = service_key.clone();
        tasks.push(tokio::spawn(async move {
            match transport {
                Transport::Tcp => {
                    run_client_side_cli_with_pinned_credential::<TcpListenerProvider, _>(
                        tunnel_addr,
                        relay_addr,
                        connect_key.into(),
                        false,
                        None,
                        credential,
                    )
                    .await
                }
                Transport::Udp => {
                    run_client_side_cli_with_pinned_credential::<UdpListenerProvider, _>(
                        tunnel_addr,
                        relay_addr,
                        connect_key.into(),
                        false,
                        None,
                        credential,
                    )
                    .await
                }
            }
        }));

        let harness = Self {
            tunnel_addr,
            transport,
            shutdown,
            tasks,
            state_dir,
        };
        harness.wait_until_forwarding().await;
        harness
    }
}

impl TunnelHarness {
    /// Send one probe payload through the tunnel and wait for it to come back.
    ///
    /// This replaces the fixed sleeps the earlier version of this file used: it is
    /// true end-to-end readiness, covering the relay, `register`'s control
    /// connection, `connect`'s local listener, and the echo server at once.
    async fn wait_until_forwarding(&self) {
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

    async fn probe_once(&self) -> Result<(), String> {
        match self.transport {
            Transport::Tcp => self.probe_tcp().await,
            Transport::Udp => self.probe_udp().await,
        }
    }

    async fn probe_tcp(&self) -> Result<(), String> {
        let mut stream = timeout(Duration::from_secs(1), TcpStream::connect(self.tunnel_addr))
            .await
            .map_err(|_| "tcp connect timed out".to_string())?
            .map_err(|error| format!("tcp connect failed: {error}"))?;
        let (mut reader, mut writer) = stream.split();
        let mut reader = NormalMessageReader::new(&mut reader);
        let mut writer = NormalMessageWriter::new(&mut writer);
        writer
            .write_msg(PROBE)
            .await
            .map_err(|error| format!("tcp probe write failed: {error}"))?;
        let echoed = timeout(Duration::from_secs(1), reader.read_msg())
            .await
            .map_err(|_| "tcp probe read timed out".to_string())?
            .map_err(|error| format!("tcp probe read failed: {error}"))?;
        if echoed == PROBE {
            Ok(())
        } else {
            Err(format!("tcp probe echoed {} bytes", echoed.len()))
        }
    }

    async fn probe_udp(&self) -> Result<(), String> {
        let socket = connected_udp_socket(self.tunnel_addr).await;
        probe_udp_socket(&socket).await
    }
}

async fn connected_udp_socket(addr: SocketAddr) -> UdpSocket {
    let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    tune_udp_socket(&socket);
    socket.connect(addr).await.unwrap();
    socket
}

/// Round-trip one probe datagram on `socket`.
///
/// The tunnel keys UDP streams by source address, so every new socket starts a
/// new stream and its first datagram can be dropped while the relay sets that
/// stream up. Callers that go on to assert payload equality warm their own
/// socket with this first.
async fn probe_udp_socket(socket: &UdpSocket) -> Result<(), String> {
    socket
        .send(PROBE)
        .await
        .map_err(|error| format!("udp probe send failed: {error}"))?;
    let mut buf = vec![0u8; 65_507];
    let len = timeout(Duration::from_millis(500), socket.recv(&mut buf))
        .await
        .map_err(|_| "udp probe timed out".to_string())?
        .map_err(|error| format!("udp probe recv failed: {error}"))?;
    if &buf[..len] == PROBE {
        Ok(())
    } else {
        Err(format!("udp probe echoed {len} bytes"))
    }
}

async fn wait_until_udp_socket_forwards(socket: &UdpSocket) {
    let deadline = Instant::now() + READY_TIMEOUT;
    let mut last_error = String::from("no attempt completed");
    while Instant::now() < deadline {
        match probe_udp_socket(socket).await {
            Ok(()) => return,
            Err(error) => last_error = error,
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("udp socket never forwarded through the tunnel: {last_error}");
}

impl Drop for TunnelHarness {
    fn drop(&mut self) {
        self.shutdown.cancel();
        for task in &self.tasks {
            task.abort();
        }
        let _ = std::fs::remove_dir_all(&self.state_dir);
    }
}

/// Poll the relay's `Keys` status until `register` has published `service_key`.
async fn wait_for_registration(relay_addr: SocketAddr, service_key: &str, credential: Credential) {
    let deadline = Instant::now() + READY_TIMEOUT;
    let mut last_error = String::from("no attempt completed");
    while Instant::now() < deadline {
        match registered_keys(relay_addr, credential).await {
            Ok(keys) => {
                if keys.iter().any(|key| key == service_key) {
                    return;
                }
                last_error = format!("relay reports keys {keys:?}");
            }
            Err(error) => last_error = error,
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("`{service_key}` was never registered: {last_error}");
}

async fn registered_keys(
    relay_addr: SocketAddr,
    credential: Credential,
) -> Result<Vec<String>, String> {
    let mut stream = timeout(Duration::from_secs(1), TcpStream::connect(relay_addr))
        .await
        .map_err(|_| "status connect timed out".to_string())?
        .map_err(|error| format!("status connect failed: {error}"))?;
    let response = timeout(
        Duration::from_secs(1),
        get_status_with_credential(&mut stream, PbConnStatusReq::Keys, None, &credential),
    )
    .await
    .map_err(|_| "status request timed out".to_string())?
    .map_err(|error| format!("status request failed: {error}"))?;
    match response {
        PbConnStatusResp::Keys(keys) => Ok(keys),
        other => Err(format!("unexpected status response: {other:?}")),
    }
}

/// Random payload; the length is random too, so framing is exercised at many sizes.
fn gen_random_msg(max_len: usize) -> Vec<u8> {
    let len = rand::rng().random_range(0_usize..max_len);
    let mut vec = Vec::with_capacity(len);
    for _ in 0..len {
        vec.push(rand::rng().random_range(0..212));
    }
    vec
}

async fn run_udp_datagram_echo(addr: SocketAddr, rounds: usize, burst: usize) {
    let socket = connected_udp_socket(addr).await;
    wait_until_udp_socket_forwards(&socket).await;
    let mut buf = vec![0u8; 65_507];

    for round in 0..rounds {
        for seq in 0..burst {
            let mut msg = (seq as u32).to_be_bytes().to_vec();
            if round == 0 && seq == 0 {
                msg.extend(vec![0u8; UDP_TEST_PAYLOAD_MAX]);
            } else {
                msg.extend(gen_random_msg(UDP_TEST_PAYLOAD_MAX));
            }
            socket.send(&msg).await.unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let wait = deadline.saturating_duration_since(Instant::now());
                let len = match timeout(wait, socket.recv(&mut buf)).await {
                    Ok(Ok(len)) => len,
                    Ok(Err(err)) => panic!("udp recv error: {err}"),
                    Err(_) => panic!("udp echo timeout; missing seq: {seq}"),
                };
                if len < 4 {
                    continue;
                }
                // A datagram from an earlier sequence number may still be in
                // flight; skip it rather than failing the comparison.
                if u32::from_be_bytes(buf[..4].try_into().unwrap()) != seq as u32 {
                    continue;
                }
                assert_eq!(msg, &buf[..len]);
                break;
            }
        }
    }
}

async fn run_echo_delay<P: StreamProvider, A: ToSocketAddrs + Send>(addr: A, times: usize) {
    let mut stream = P::from_addr(addr).await.unwrap();
    let (mut reader, mut writer) = stream.split();
    let mut reader = NormalMessageReader::new(&mut reader);
    let mut writer = NormalMessageWriter::new(&mut writer);
    let mut duration = Duration::default();
    for _ in 0..times {
        let expected = gen_random_msg(2000);
        for _ in 0..10 {
            let msg = {
                let _guard = TimerTickGuard::new(&mut duration);
                writer.write_msg(&expected).await.unwrap();
                reader.read_msg().await.unwrap()
            };

            assert_eq!(expected, msg);
        }
    }
    println!("{times} rounds of 10 random data echo delay tests each took a total of {duration:?}");
}

/// Push traffic through the tunnel and assert every payload comes back byte for byte.
///
/// These cases verify the logic. For latency numbers, run a separate binary.
async fn assert_tunnel_echoes(transport: Transport, need_codec: bool) {
    let harness = TunnelHarness::start(transport, need_codec).await;
    match transport {
        Transport::Tcp => run_echo_delay::<TcpStreamProvider, _>(harness.tunnel_addr, 10).await,
        Transport::Udp => run_udp_datagram_echo(harness.tunnel_addr, 10, 8).await,
    }
}

#[tokio::test]
async fn tcp_tunnel_echoes_without_codec() {
    assert_tunnel_echoes(Transport::Tcp, false).await;
}

#[tokio::test]
async fn tcp_tunnel_echoes_with_codec() {
    assert_tunnel_echoes(Transport::Tcp, true).await;
}

#[tokio::test]
async fn udp_tunnel_echoes_without_codec() {
    assert_tunnel_echoes(Transport::Udp, false).await;
}

#[tokio::test]
async fn udp_tunnel_echoes_with_codec() {
    assert_tunnel_echoes(Transport::Udp, true).await;
}
