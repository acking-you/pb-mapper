//! Unified command-line entry point for every pb-mapper role.
//!
//! ```text
//!                       +-> server   (relay)
//! process args -> clap -+-> register (publish a local service)
//!                       +-> connect  (open a local listener)
//!                       +-> status   (namespace-scoped inspection)
//!                       +-> admin    (credential/control plane)
//! ```
//!
//! Role-specific execution stays below this dispatch layer. Administrator parsing,
//! pagination, wire requests, and output rendering live in the `admin` module.

use std::error::Error;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::PathBuf;
use std::time::Duration;

use better_mimalloc_rs::MiMalloc;
use clap::{Args, Parser, Subcommand, ValueEnum};
use pb_mapper::common::auth::{
    acquire_state_dir_lock, generate_admin_key, initialize_admin_key, write_admin_key_file,
    AuthConfig, KeyPage, LegacyProtocolPolicy, MAX_TEMP_KEY_CAPACITY, MAX_TEMP_KEY_TTL,
    MIN_TEMP_KEY_TTL,
};
use pb_mapper::common::checksum::set_process_msg_header_key;
use pb_mapper::common::checksum::{setup_machine_msg_header_key, MACHINE_MSG_HEADER_KEY_PATH};
use pb_mapper::common::config::{
    control_io_timeout, get_pb_mapper_server_async, get_sockaddr_async, init_tracing,
    keep_alive_from_env, StatusOp,
};
use pb_mapper::common::message::command::{
    AdminConnectionPage, AdminRequest, AdminResponse, AdminServicePage, MessageSerializer,
    PbConnRequest, PbConnResponse,
};
use pb_mapper::common::message::forward::StreamForward;
use pb_mapper::common::message::secure::ClientHeaderSession;
use pb_mapper::common::message::MessageReader;
use pb_mapper::local::client::{handle_status_cli_scoped, run_client_side_cli_scoped};
use pb_mapper::local::server::{run_server_side_cli, ServerTunnelOptions};
use pb_mapper::pb_server::run_server_with_shutdown;
use tokio::net::TcpStream;
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
    /// Manage temporary credentials and inspect relay authentication state.
    Admin(AdminArgs),
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
    /// Directory containing encrypted authentication state and the administrator key file.
    /// Defaults to /var/lib/pb-mapper/auth for Linux services or a writable system
    /// directory; otherwise a user-writable application directory.
    #[arg(long)]
    auth_state_dir: Option<PathBuf>,
    /// Create a random administrator key before starting the relay.
    #[arg(
        long,
        conflicts_with = "use_machine_msg_header_key",
        default_value_t = false
    )]
    init_admin_key: bool,
    /// Replace an existing administrator key when used with --init-admin-key.
    #[arg(long, requires = "init_admin_key", default_value_t = false)]
    force_init_admin_key: bool,
    /// Maximum temporary-key slots allocated by the relay.
    #[arg(long)]
    max_temporary_keys: Option<usize>,
    /// Maximum accepted temporary-key TTL.
    #[arg(long, value_parser = parse_duration)]
    max_temporary_key_ttl: Option<Duration>,
    /// Allow or deny the legacy encrypted framing protocol.
    #[arg(long, value_enum)]
    legacy_protocol: Option<LegacyProtocolArg>,
}

#[path = "pb-mapper/admin.rs"]
mod admin;
use admin::AdminArgs;
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
    /// Administrator-only target namespace. Temporary credentials always use their own key id.
    #[arg(long)]
    namespace: Option<u64>,
    /// Required when an administrator registers a service inside a temporary-key namespace.
    #[arg(long, requires = "namespace", default_value_t = false)]
    force: bool,
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
    /// Administrator-only target namespace. Temporary credentials always use their own key id.
    #[arg(long)]
    namespace: Option<u64>,
}

#[derive(Debug, Args)]
struct StatusArgs {
    /// Status query to execute.
    #[arg(value_enum)]
    op: StatusOp,
    /// Relay address. Falls back to PB_MAPPER_SERVER.
    #[arg(short, long, visible_alias = "pb-mapper-server", value_name = "ADDR")]
    server: Option<String>,
    /// Administrator-only namespace to inspect.
    #[arg(long)]
    namespace: Option<u64>,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum LegacyProtocolArg {
    Allow,
    Deny,
}

impl From<LegacyProtocolArg> for LegacyProtocolPolicy {
    fn from(value: LegacyProtocolArg) -> Self {
        match value {
            LegacyProtocolArg::Allow => Self::Allow,
            LegacyProtocolArg::Deny => Self::Deny,
        }
    }
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
        Command::Admin(args) => admin::run_admin(args).await?,
    }
    Ok(())
}

fn apply_server_auth_overrides(args: &ServerArgs) -> Result<(), Box<dyn Error>> {
    if let Some(auth_state_dir) = &args.auth_state_dir {
        std::env::set_var("PB_MAPPER_AUTH_STATE_DIR", auth_state_dir);
    }
    if let Some(max_temporary_keys) = args.max_temporary_keys {
        if !(1..=MAX_TEMP_KEY_CAPACITY).contains(&max_temporary_keys) {
            return Err(format!(
                "`--max-temporary-keys` must be between 1 and {MAX_TEMP_KEY_CAPACITY}"
            )
            .into());
        }
        std::env::set_var(
            "PB_MAPPER_AUTH_MAX_TEMP_KEYS",
            max_temporary_keys.to_string(),
        );
    }
    if let Some(max_temporary_key_ttl) = args.max_temporary_key_ttl {
        if max_temporary_key_ttl < MIN_TEMP_KEY_TTL || max_temporary_key_ttl > MAX_TEMP_KEY_TTL {
            return Err(format!(
                "`--max-temporary-key-ttl` must be between {}s and {}d",
                MIN_TEMP_KEY_TTL.as_secs(),
                MAX_TEMP_KEY_TTL.as_secs() / 86_400
            )
            .into());
        }
        std::env::set_var(
            "PB_MAPPER_AUTH_MAX_TEMP_TTL_SECS",
            max_temporary_key_ttl.as_secs().to_string(),
        );
    }
    Ok(())
}

async fn run_server(args: ServerArgs) -> Result<(), Box<dyn Error>> {
    apply_server_auth_overrides(&args)?;
    if let Some(legacy_protocol) = args.legacy_protocol {
        std::env::set_var(
            "PB_MAPPER_LEGACY_PROTOCOL",
            match legacy_protocol {
                LegacyProtocolArg::Allow => "allow",
                LegacyProtocolArg::Deny => "deny",
            },
        );
    }
    let auth_config = AuthConfig::default();
    if args.init_admin_key {
        std::fs::create_dir_all(&auth_config.state_dir)?;
        let _lock = acquire_state_dir_lock(&auth_config.state_dir)?;
        let key_path = auth_config.state_dir.join("admin.key");
        let key = initialize_admin_key(&key_path, args.force_init_admin_key)?;
        drop(_lock);
        set_process_msg_header_key(Some(&key))?;
        eprintln!("administrator key initialized at {}", key_path.display());
    } else if args.use_machine_msg_header_key {
        let admin_key_path = auth_config.state_dir.join("admin.key");
        if admin_key_path.exists() {
            return Err(format!(
                "--use-machine-msg-header-key cannot replace `{}`; use `pb-mapper admin root-key rotate` to change the root key",
                admin_key_path.display()
            )
            .into());
        }
        tracing::warn!(
            "--use-machine-msg-header-key is a legacy compatibility option; prefer a random administrator key"
        );
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
        namespace: args.namespace,
        force_namespace: args.force,
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
    pb_mapper::common::checksum::get_process_credential().map_err(|error| {
        std::io::Error::other(format!("client credential is required: {error}"))
    })?;
    let local_addr = get_sockaddr_async(&args.addr).await?;
    let remote_addr = get_pb_mapper_server_async(args.relay.server.as_deref()).await?;
    let key = args.key.into();
    let keep_alive = args.relay.keep_alive || keep_alive_from_env();

    match args.transport {
        Transport::Tcp => {
            run_client_side_cli_scoped::<TcpListenerProvider, _>(
                local_addr,
                remote_addr,
                key,
                keep_alive,
                args.namespace,
            )
            .await;
        }
        Transport::Udp => {
            run_client_side_cli_scoped::<UdpListenerProvider, _>(
                local_addr,
                remote_addr,
                key,
                keep_alive,
                args.namespace,
            )
            .await;
        }
    }
    Ok(())
}

async fn run_status(args: StatusArgs) -> Result<(), Box<dyn Error>> {
    let remote_addr = get_pb_mapper_server_async(args.server.as_deref()).await?;
    handle_status_cli_scoped(args.op, remote_addr, args.namespace).await;
    Ok(())
}

fn parse_duration(raw: &str) -> Result<Duration, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("duration must not be empty".to_string());
    }
    let split = raw
        .find(|character: char| !character.is_ascii_digit())
        .unwrap_or(raw.len());
    let (number, unit) = raw.split_at(split);
    let value = number
        .parse::<u64>()
        .map_err(|_| format!("invalid duration `{raw}`"))?;
    let multiplier = match unit {
        "" | "s" => 1,
        "m" => 60,
        "h" => 60 * 60,
        "d" => 24 * 60 * 60,
        _ => {
            return Err(format!(
                "unsupported duration unit `{unit}`; use s, m, h, or d"
            ))
        }
    };
    value
        .checked_mul(multiplier)
        .map(Duration::from_secs)
        .ok_or_else(|| "duration is too large".to_string())
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
            vec![
                "pb-mapper",
                "admin",
                "--server",
                "relay:7666",
                "key",
                "issue",
                "--ttl",
                "30d",
                "--label",
                "build-agent",
            ],
            vec![
                "pb-mapper",
                "admin",
                "--output",
                "ndjson",
                "connection",
                "list",
                "--page-size",
                "1000",
                "--all",
            ],
            vec![
                "pb-mapper",
                "admin",
                "root-key",
                "rotate",
                "--key-file",
                "/tmp/pb-mapper-admin.key",
            ],
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
        Cli::try_parse_from([
            "pb-mapper",
            "register",
            "tcp",
            "--key",
            "web",
            "--addr",
            "127.0.0.1:8080",
            "--namespace",
            "4294967296",
            "--force",
        ])
        .expect("administrator namespace registration flags should parse");
    }

    #[test]
    fn rejects_invalid_admin_paging_and_duration() {
        assert!(
            Cli::try_parse_from(["pb-mapper", "admin", "key", "list", "--page-size", "1001",])
                .is_err()
        );
        assert!(Cli::try_parse_from(
            ["pb-mapper", "admin", "key", "issue", "--ttl", "1fortnight",]
        )
        .is_err());
        assert!(Cli::try_parse_from([
            "pb-mapper",
            "server",
            "--init-admin-key",
            "--use-machine-msg-header-key",
        ])
        .is_err());
    }

    #[test]
    fn server_auth_options_only_override_environment_when_explicit() {
        let cli =
            Cli::try_parse_from(["pb-mapper", "server"]).expect("server defaults should parse");
        let Command::Server(defaults) = cli.command else {
            panic!("expected server command");
        };
        assert_eq!(defaults.auth_state_dir, None);
        assert_eq!(defaults.max_temporary_keys, None);
        assert_eq!(defaults.max_temporary_key_ttl, None);
        assert_eq!(defaults.legacy_protocol, None);

        let cli = Cli::try_parse_from([
            "pb-mapper",
            "server",
            "--auth-state-dir",
            "/tmp/pb-mapper-auth",
            "--max-temporary-keys",
            "1024",
            "--max-temporary-key-ttl",
            "2h",
            "--legacy-protocol",
            "deny",
        ])
        .expect("explicit server authentication options should parse");
        let Command::Server(explicit) = cli.command else {
            panic!("expected server command");
        };
        assert_eq!(
            explicit.auth_state_dir,
            Some(PathBuf::from("/tmp/pb-mapper-auth"))
        );
        assert_eq!(explicit.max_temporary_keys, Some(1024));
        assert_eq!(
            explicit.max_temporary_key_ttl,
            Some(Duration::from_secs(2 * 60 * 60))
        );
        assert_eq!(explicit.legacy_protocol, Some(LegacyProtocolArg::Deny));
    }

    #[test]
    fn explicit_out_of_range_server_auth_flags_are_rejected() {
        let cli = Cli::try_parse_from(["pb-mapper", "server", "--max-temporary-keys", "0"])
            .expect("clap should accept the token before bounds checking");
        let Command::Server(args) = cli.command else {
            panic!("expected server command");
        };
        let error = apply_server_auth_overrides(&args).unwrap_err();
        assert!(error.to_string().contains("--max-temporary-keys"));

        let cli = Cli::try_parse_from(["pb-mapper", "server", "--max-temporary-key-ttl", "5s"])
            .expect("clap should accept the token before bounds checking");
        let Command::Server(args) = cli.command else {
            panic!("expected server command");
        };
        let error = apply_server_auth_overrides(&args).unwrap_err();
        assert!(error.to_string().contains("--max-temporary-key-ttl"));
    }
}
