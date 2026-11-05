//! Renderer-free transport acceptance. Local endpoints through a deployed relay
//! are explicitly NOT a cross-NAT test. Credentials never leave process memory.
//!
//! cargo run -p pb-mapper-desktop --example transport_probe -- CONFIG --tcp
//! cargo run -p pb-mapper-desktop --example transport_probe -- --local
//! Optional: --mib 4 --case tls-split --nagle (host accepted socket only).
//! --consumer-server HOST:PORT changes only the consumer's relay address, for
//! example an existing SSH loopback forward. It never starts that forward.
use pb_mapper_client::sdk::{
    Client, ClientConfig, ConnectOptions, PathPolicy, RegisterOptions, RegisterRequest,
    RelayTransport, Transport,
};
use quinn::rustls::{self, pki_types::ServerName};
use serde::Deserialize;
use std::{
    io,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::TcpListener,
};
use tokio_rustls::{TlsAcceptor, TlsConnector};
use tokio_util::task::AbortOnDropHandle;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
const NAME: &str = "transport-probe.pb-mapper.invalid";
const BLOCK: usize = 16 * 1024;
const PINGS: usize = 12;

#[derive(Deserialize)]
struct Configuration {
    server_address: String,
    msg_header_key: String,
    #[serde(default)]
    relay_transport: RelayTransport,
}

#[derive(Clone, Copy)]
struct Case {
    name: &'static str,
    codec: bool,
    tls: bool,
    combined: bool,
}
const CASES: [Case; 4] = [
    Case {
        name: "raw",
        codec: false,
        tls: false,
        combined: false,
    },
    Case {
        name: "codec",
        codec: true,
        tls: false,
        combined: false,
    },
    Case {
        name: "tls-split",
        codec: true,
        tls: true,
        combined: false,
    },
    Case {
        name: "tls-combined",
        codec: true,
        tls: true,
        combined: true,
    },
];

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let local = args.iter().any(|arg| arg == "--local");
    let nagle = args.iter().any(|arg| arg == "--nagle");
    let value = |name| {
        args.windows(2)
            .find(|pair| pair[0] == name)
            .map(|pair| pair[1].as_str())
    };
    let mib: usize = value("--mib").unwrap_or("1").parse()?;
    if !(1..=64).contains(&mib) {
        return Err("--mib must be 1..64".into());
    }
    let selected = value("--case");
    if selected.is_some_and(|name| !CASES.iter().any(|case| case.name == name)) {
        return Err("--case must be raw, codec, tls-split or tls-combined".into());
    }
    let consumer_override = value("--consumer-server");
    if args.iter().any(|arg| arg == "--consumer-server") && consumer_override.is_none() {
        return Err("--consumer-server requires HOST:PORT".into());
    }
    let local_relay = if local {
        Some(pb_mapper_testkit::Relay::start("transport-probe").await)
    } else {
        None
    };
    let (server, admin_client) = if let Some(relay) = &local_relay {
        let server = relay.addr().to_string();
        (
            server.clone(),
            Client::from_credential(server, pb_mapper_testkit::admin_credential(), true, None),
        )
    } else {
        let path = args
            .first()
            .ok_or("configuration path or --local required")?;
        let config: Configuration = serde_json::from_slice(&std::fs::read(path)?)?;
        let transport = if args.iter().any(|arg| arg == "--tcp") {
            RelayTransport::Tcp
        } else {
            config.relay_transport
        };
        let client = Client::new(ClientConfig {
            server: config.server_address.clone(),
            credential: config.msg_header_key,
            keep_alive: true,
            namespace: None,
        })?
        .with_relay_transport(transport)?;
        (config.server_address, client)
    };
    let consumer_server = consumer_override.unwrap_or(&server).to_string();
    println!(
        "environment os={} arch={} sdk={} topology={} publisher_relay={} consumer_relay={} carrier={:?} host_nodelay={} bytes_per_direction={}",
        std::env::consts::OS,
        std::env::consts::ARCH,
        env!("CARGO_PKG_VERSION"),
        if consumer_override.is_some() {
            "same-machine-endpoints-via-separate-configured-relay-addresses"
        } else if local {
            "same-process-loopback-relay"
        } else {
            "same-machine-endpoints-via-deployed-relay"
        },
        server,
        consumer_server,
        admin_client.relay_transport(),
        !nagle,
        mib * 1024 * 1024
    );
    let admin = admin_client.admin()?;
    let key = admin
        .issue_key(
            Duration::from_secs(900),
            Some("transport-probe-ephemeral".into()),
        )
        .await?;
    let result = async {
        let publisher = Client::new(ClientConfig {
            server,
            credential: key.credential.clone(),
            keep_alive: true,
            namespace: None,
        })?
        .with_relay_transport(admin_client.relay_transport())?;
        let consumer = Client::new(ClientConfig {
            server: consumer_server,
            credential: key.credential.clone(),
            keep_alive: true,
            namespace: None,
        })?
        .with_relay_transport(admin_client.relay_transport())?;
        for case in CASES
            .into_iter()
            .filter(|case| selected.is_none_or(|name| case.name == name))
        {
            tokio::time::timeout(
                Duration::from_secs(180),
                run_case(&publisher, &consumer, case, mib, !nagle),
            )
            .await??;
        }
        Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
    }
    .await;
    // Even errors/timeouts revoke the dedicated scoped credential. The TTL also
    // bounds the grant if the process is forcibly terminated.
    admin.revoke_key(key.key_id).await?;
    println!("temporary_credential_revoked=true");
    result
}

fn tls_pair() -> Result<(TlsAcceptor, TlsConnector)> {
    let (rcgen::CertifiedKey { cert, .. }, key) =
        rcgen::generate_simple_self_signed(vec![NAME.into()])?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut server = rustls::ServerConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_no_client_auth()
        .with_single_cert(vec![cert.der().clone()], key)?;
    server.alpn_protocols = vec![b"pb-mapper/transport-probe/1".to_vec()];
    let mut roots = rustls::RootCertStore::empty();
    // The sole freshly generated root only authorizes this in-memory endpoint.
    roots.add(cert.der().clone())?;
    let mut client = rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_root_certificates(roots)
        .with_no_client_auth();
    client.alpn_protocols = server.alpn_protocols.clone();
    client.enable_early_data = false;
    Ok((
        TlsAcceptor::from(Arc::new(server)),
        TlsConnector::from(Arc::new(client)),
    ))
}

async fn run_case(
    publisher: &Client,
    consumer: &Client,
    case: Case,
    mib: usize,
    nodelay: bool,
) -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let service = format!(
        "desktop:transport-probe-{}-{}",
        std::process::id(),
        case.name
    );
    let registration = publisher
        .register_with_options(
            RegisterRequest {
                key: service.clone(),
                local_addr: listener.local_addr()?.to_string(),
                transport: Transport::Tcp,
                codec: case.codec,
                force_namespace: false,
            },
            RegisterOptions {
                allow_direct: false,
                ..Default::default()
            },
        )
        .await?;
    registration
        .wait_ready_timeout(Duration::from_secs(15))
        .await?;
    let blocks = mib * 1024 * 1024 / BLOCK;
    let (acceptor, connector) = tls_pair()?;
    let host = AbortOnDropHandle::new(tokio::spawn(async move {
        let (socket, _) = listener.accept().await?;
        socket.set_nodelay(nodelay)?;
        if case.tls {
            host_session(&mut acceptor.accept(socket).await?, case, blocks).await
        } else {
            host_session(&mut { socket }, case, blocks).await
        }
    }));
    let started = Instant::now();
    let (mut socket, connection) = consumer
        .connect_tcp(
            service,
            ConnectOptions {
                path: PathPolicy::RelayOnly,
                ..Default::default()
            },
        )
        .await?;
    let result = async {
        connection
            .wait_ready_timeout(Duration::from_secs(15))
            .await?;
        if case.tls {
            let mut tls = connector
                .connect(ServerName::try_from(NAME)?, socket)
                .await?;
            println!(
                "case={} setup_ms={:.2}",
                case.name,
                started.elapsed().as_secs_f64() * 1000.0
            );
            consumer_session(&mut tls, case, blocks).await
        } else {
            println!(
                "case={} setup_ms={:.2}",
                case.name,
                started.elapsed().as_secs_f64() * 1000.0
            );
            consumer_session(&mut socket, case, blocks).await
        }
    }
    .await;
    if result.is_err() {
        host.abort();
    }
    let host_result = host.await;
    connection.stop().await?;
    registration.stop().await?;
    result?;
    host_result??;
    Ok(())
}

async fn write_frame<W: AsyncWrite + Unpin>(
    io: &mut W,
    payload: &[u8],
    combined: bool,
) -> io::Result<()> {
    if combined {
        let mut wire = Vec::with_capacity(4 + payload.len());
        wire.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        wire.extend_from_slice(payload);
        io.write_all(&wire).await?;
    } else {
        io.write_u32(payload.len() as u32).await?;
        io.write_all(payload).await?;
    }
    io.flush().await
}

async fn read_frame<R: AsyncRead + Unpin>(io: &mut R, buffer: &mut [u8]) -> io::Result<()> {
    let size = io.read_u32().await? as usize;
    if size != buffer.len() {
        return Err(io::Error::other("unexpected probe frame length"));
    }
    io.read_exact(buffer).await?;
    Ok(())
}

fn payload(size: usize) -> Vec<u8> {
    (0..size)
        .map(|i| (i.wrapping_mul(73).wrapping_add(i / 251) % 256) as u8)
        .collect()
}

async fn receive_blocks<R: AsyncRead + Unpin>(io: &mut R, blocks: usize) -> io::Result<()> {
    let mut buffer = vec![0; BLOCK];
    let expected = payload(BLOCK);
    for _ in 0..blocks {
        read_frame(io, &mut buffer).await?;
        if buffer != expected {
            return Err(io::Error::other("probe payload corrupted"));
        }
    }
    Ok(())
}

async fn send_blocks<W: AsyncWrite + Unpin>(
    io: &mut W,
    blocks: usize,
    combined: bool,
) -> io::Result<()> {
    let bytes = payload(BLOCK);
    for _ in 0..blocks {
        write_frame(io, &bytes, combined).await?;
    }
    Ok(())
}

async fn host_session<S: AsyncRead + AsyncWrite + Unpin>(
    io: &mut S,
    case: Case,
    blocks: usize,
) -> Result<()> {
    let mut ping = [0; 64];
    for _ in 0..PINGS {
        read_frame(io, &mut ping).await?;
        write_frame(io, &ping, case.combined).await?;
    }
    receive_blocks(io, blocks).await?;
    io.write_u8(1).await?;
    io.flush().await?;
    if io.read_u8().await? != 2 {
        return Err("unexpected download start".into());
    }
    send_blocks(io, blocks, case.combined).await?;
    if io.read_u8().await? != 3 {
        return Err("missing download acknowledgement".into());
    }
    Ok(())
}

async fn consumer_session<S: AsyncRead + AsyncWrite + Unpin>(
    io: &mut S,
    case: Case,
    blocks: usize,
) -> Result<()> {
    let ping = payload(64);
    let mut reply = [0; 64];
    let mut rtts = Vec::with_capacity(PINGS);
    for _ in 0..PINGS {
        let start = Instant::now();
        write_frame(io, &ping, case.combined).await?;
        read_frame(io, &mut reply).await?;
        if reply.as_slice() != ping {
            return Err("ping payload corrupted".into());
        }
        rtts.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    rtts.sort_by(f64::total_cmp);
    println!(
        "case={} ping_64b_rtt_ms min={:.2} median={:.2} max={:.2}",
        case.name,
        rtts[0],
        rtts[PINGS / 2],
        rtts[PINGS - 1]
    );
    let start = Instant::now();
    send_blocks(io, blocks, case.combined).await?;
    let write_ms = start.elapsed().as_secs_f64() * 1000.0;
    if io.read_u8().await? != 1 {
        return Err("missing upload acknowledgement".into());
    }
    report(
        case,
        "consumer_to_publisher",
        blocks,
        start.elapsed(),
        write_ms,
    );
    let start = Instant::now();
    io.write_u8(2).await?;
    io.flush().await?;
    receive_blocks(io, blocks).await?;
    io.write_u8(3).await?;
    io.flush().await?;
    report(case, "publisher_to_consumer", blocks, start.elapsed(), 0.0);
    Ok(())
}

fn report(case: Case, direction: &str, blocks: usize, elapsed: Duration, queued_ms: f64) {
    let bytes = blocks * BLOCK;
    println!(
        "case={} direction={} verified_bytes={} delivery_ms={:.2} delivery_mbps={:.3} sender_queued_ms={:.2}",
        case.name,
        direction,
        bytes,
        elapsed.as_secs_f64() * 1000.0,
        bytes as f64 * 8.0 / elapsed.as_secs_f64() / 1e6,
        queued_ms
    );
}
