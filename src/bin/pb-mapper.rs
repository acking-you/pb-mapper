use std::error::Error;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use better_mimalloc_rs::MiMalloc;
use clap::{Args, Parser, Subcommand, ValueEnum};
use pb_mapper::common::checksum::{setup_machine_msg_header_key, MACHINE_MSG_HEADER_KEY_PATH};
use pb_mapper::common::config::{
    get_pb_mapper_server_async, get_sockaddr_async, init_tracing, keep_alive_from_env, StatusOp,
};
use pb_mapper::common::message::forward::StreamForward;
use pb_mapper::local::client::{handle_status_cli, run_client_side_cli};
use pb_mapper::local::server::{run_server_side_cli, ServerTunnelOptions};
use pb_mapper::pb_server::run_server_with_shutdown;
use tokio_util::sync::CancellationToken;
use uni_stream::stream::{
    StreamProvider, TcpListenerProvider, TcpStreamProvider, UdpListenerProvider, UdpStreamProvider,
};

#[global_allocator]
static GLOBAL_MIMALLOC: MiMalloc = MiMalloc;

#[derive(Debug, Parser)]
#[command(
    author = "L_B__",
    version,
    about = "Expose and consume keyed TCP/UDP services through a pb-mapper relay",
    subcommand_required = true,
    arg_required_else_help = true
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run the public relay server.
    Server(ServerArgs),
    /// Register a local service with a relay.
    Register(RegisterArgs),
    /// Expose a registered service on a local listening address.
    Connect(ConnectArgs),
    /// Query relay status.
    Status(StatusArgs),
}

#[derive(Debug, Args)]
struct ServerArgs {
    /// Port exposed to registering services and connecting clients.
    #[arg(short, long, visible_alias = "pb-mapper-port", default_value_t = 7666)]
    port: u16,
    /// Listen on IPv6 (::) instead of IPv4 (0.0.0.0).
    #[arg(long, visible_alias = "use-ipv6", default_value_t = false)]
    ipv6: bool,
    /// Enable TCP keep-alive. PB_MAPPER_KEEP_ALIVE=ON is also supported.
    #[arg(long, default_value_t = false)]
    keep_alive: bool,
    /// Derive MSG_HEADER_KEY from this machine and persist it for other roles.
    #[arg(long, default_value_t = false)]
    use_machine_msg_header_key: bool,
}

#[derive(Debug, Args)]
struct RegisterArgs {
    /// Transport used by the local service.
    #[arg(value_enum)]
    transport: Transport,
    /// Service key registered with the relay.
    #[arg(short, long)]
    key: String,
    /// Local service address to forward to.
    #[arg(short, long, visible_alias = "local")]
    addr: String,
    #[command(flatten)]
    relay: RelayArgs,
    /// Encrypt forwarded traffic with the configured MSG_HEADER_KEY.
    #[arg(short, long, default_value_t = false)]
    codec: bool,
}

#[derive(Debug, Args)]
struct ConnectArgs {
    /// Transport exposed by the local listener.
    #[arg(value_enum)]
    transport: Transport,
    /// Registered service key to subscribe to.
    #[arg(short, long)]
    key: String,
    /// Local address on which downstream clients connect.
    #[arg(short, long, visible_alias = "local")]
    addr: String,
    #[command(flatten)]
    relay: RelayArgs,
}

#[derive(Debug, Args)]
struct StatusArgs {
    /// Status query to execute.
    #[arg(value_enum)]
    op: StatusOp,
    /// Relay address. Falls back to PB_MAPPER_SERVER.
    #[arg(short, long, visible_alias = "pb-mapper-server", value_name = "ADDR")]
    server: Option<String>,
}

#[derive(Debug, Args)]
struct RelayArgs {
    /// Relay address. Falls back to PB_MAPPER_SERVER.
    #[arg(short, long, visible_alias = "pb-mapper-server", value_name = "ADDR")]
    server: Option<String>,
    /// Enable TCP keep-alive. PB_MAPPER_KEEP_ALIVE=ON is also supported.
    #[arg(long, default_value_t = false)]
    keep_alive: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum Transport {
    Tcp,
    Udp,
}

#[tokio::main]
async fn main() {
    MiMalloc::init();
    let cli = Cli::parse();
    init_tracing();

    if let Err(error) = run(cli).await {
        tracing::error!(%error, "pb-mapper command failed");
        std::process::exit(1);
    }
}

async fn run(cli: Cli) -> Result<(), Box<dyn Error>> {
    match cli.command {
        Command::Server(args) => run_server(args).await?,
        Command::Register(args) => run_register(args).await?,
        Command::Connect(args) => run_connect(args).await?,
        Command::Status(args) => run_status(args).await?,
    }
    Ok(())
}

async fn run_server(args: ServerArgs) -> Result<(), Box<dyn Error>> {
    if args.use_machine_msg_header_key {
        setup_machine_msg_header_key()?;
        tracing::info!(
            path = MACHINE_MSG_HEADER_KEY_PATH,
            "derived and persisted machine MSG_HEADER_KEY"
        );
    }

    let ip_addr = if args.ipv6 {
        IpAddr::V6(Ipv6Addr::UNSPECIFIED)
    } else {
        IpAddr::V4(Ipv4Addr::UNSPECIFIED)
    };
    run_server_with_shutdown(
        (ip_addr, args.port),
        CancellationToken::new(),
        None,
        args.keep_alive || keep_alive_from_env(),
    )
    .await?;
    Ok(())
}

async fn run_register(args: RegisterArgs) -> Result<(), Box<dyn Error>> {
    let local_addr = get_sockaddr_async(&args.addr).await?;
    let remote_addr = get_pb_mapper_server_async(args.relay.server.as_deref()).await?;
    let options = ServerTunnelOptions {
        need_codec: args.codec,
        is_datagram: args.transport == Transport::Udp,
        keep_alive: args.relay.keep_alive || keep_alive_from_env(),
    };

    match args.transport {
        Transport::Tcp => {
            register::<TcpStreamProvider>(local_addr, remote_addr, args.key, options).await
        }
        Transport::Udp => {
            register::<UdpStreamProvider>(local_addr, remote_addr, args.key, options).await
        }
    }
    Ok(())
}

async fn register<LocalStream: StreamProvider + Send + 'static>(
    local_addr: std::net::SocketAddr,
    remote_addr: std::net::SocketAddr,
    key: String,
    options: ServerTunnelOptions,
) where
    LocalStream::Item: StreamForward,
{
    run_server_side_cli::<LocalStream, _>(local_addr, remote_addr, key.into(), options).await;
}

async fn run_connect(args: ConnectArgs) -> Result<(), Box<dyn Error>> {
    let local_addr = get_sockaddr_async(&args.addr).await?;
    let remote_addr = get_pb_mapper_server_async(args.relay.server.as_deref()).await?;
    let key = args.key.into();
    let keep_alive = args.relay.keep_alive || keep_alive_from_env();

    match args.transport {
        Transport::Tcp => {
            run_client_side_cli::<TcpListenerProvider, _>(local_addr, remote_addr, key, keep_alive)
                .await;
        }
        Transport::Udp => {
            run_client_side_cli::<UdpListenerProvider, _>(local_addr, remote_addr, key, keep_alive)
                .await;
        }
    }
    Ok(())
}

async fn run_status(args: StatusArgs) -> Result<(), Box<dyn Error>> {
    let remote_addr = get_pb_mapper_server_async(args.server.as_deref()).await?;
    handle_status_cli(args.op, remote_addr).await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_each_runtime_role() {
        let cases = [
            vec!["pb-mapper", "server", "--port", "7666", "--ipv6"],
            vec![
                "pb-mapper",
                "register",
                "tcp",
                "--key",
                "web",
                "--addr",
                "127.0.0.1:8080",
                "--server",
                "relay:7666",
                "--codec",
            ],
            vec![
                "pb-mapper",
                "connect",
                "udp",
                "--key",
                "game",
                "--addr",
                "127.0.0.1:8211",
                "--server",
                "relay:7666",
            ],
            vec!["pb-mapper", "status", "keys", "--server", "relay:7666"],
        ];

        for args in cases {
            Cli::try_parse_from(args).expect("unified command should parse");
        }
    }

    #[test]
    fn accepts_documented_option_aliases() {
        Cli::try_parse_from([
            "pb-mapper",
            "server",
            "--pb-mapper-port",
            "7666",
            "--use-ipv6",
        ])
        .expect("server aliases should parse");
        Cli::try_parse_from([
            "pb-mapper",
            "register",
            "tcp",
            "--key",
            "web",
            "--local",
            "127.0.0.1:8080",
            "--pb-mapper-server",
            "relay:7666",
        ])
        .expect("relay and local aliases should parse");
    }
}
