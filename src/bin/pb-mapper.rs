use std::error::Error;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::PathBuf;
use std::time::Duration;

use better_mimalloc_rs::MiMalloc;
use clap::{Args, Parser, Subcommand, ValueEnum};
use pb_mapper::common::auth::{
    generate_admin_key, initialize_admin_key, write_admin_key_file, LegacyProtocolPolicy,
    DEFAULT_AUTH_STATE_DIR,
};
use pb_mapper::common::checksum::set_process_msg_header_key;
use pb_mapper::common::checksum::{setup_machine_msg_header_key, MACHINE_MSG_HEADER_KEY_PATH};
use pb_mapper::common::config::{
    get_pb_mapper_server_async, get_sockaddr_async, init_tracing, keep_alive_from_env, StatusOp,
};
use pb_mapper::common::message::command::{
    AdminRequest, AdminResponse, MessageSerializer, PbConnRequest, PbConnResponse,
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
    #[arg(long, default_value = DEFAULT_AUTH_STATE_DIR)]
    auth_state_dir: PathBuf,
    /// Create a random administrator key before starting the relay.
    #[arg(long, default_value_t = false)]
    init_admin_key: bool,
    /// Replace an existing administrator key when used with --init-admin-key.
    #[arg(long, requires = "init_admin_key", default_value_t = false)]
    force_init_admin_key: bool,
    /// Maximum temporary-key slots allocated by the relay.
    #[arg(long, default_value_t = 65_536)]
    max_temporary_keys: usize,
    /// Maximum accepted temporary-key TTL.
    #[arg(long, default_value = "30d", value_parser = parse_duration)]
    max_temporary_key_ttl: Duration,
    /// Allow or deny the legacy encrypted framing protocol.
    #[arg(long, value_enum, default_value_t = LegacyProtocolArg::Allow)]
    legacy_protocol: LegacyProtocolArg,
}

#[derive(Debug, Args)]
struct AdminArgs {
    /// Relay address. Falls back to PB_MAPPER_SERVER.
    #[arg(short, long, visible_alias = "pb-mapper-server", value_name = "ADDR")]
    server: Option<String>,
    /// Machine-readable output mode.
    #[arg(long, value_enum, default_value_t = OutputFormat::Human)]
    output: OutputFormat,
    #[command(subcommand)]
    command: AdminCommand,
}

#[derive(Debug, Subcommand)]
enum AdminCommand {
    /// Issue, inspect, renew, reveal, revoke, or collect temporary keys.
    Key(AdminKeyArgs),
    /// List relay connections across namespaces.
    Connection(AdminConnectionArgs),
    /// List registered services across namespaces.
    Service(AdminServiceArgs),
    /// Show authentication state and protocol counters.
    Status,
    /// Repair or reset encrypted temporary-key state.
    AuthState(AdminAuthStateArgs),
    /// Rotate the sole administrator key and invalidate every existing credential.
    RootKey(AdminRootKeyArgs),
    /// Change legacy protocol acceptance at runtime.
    LegacyProtocol(AdminLegacyProtocolArgs),
}

#[derive(Debug, Args)]
struct AdminKeyArgs {
    #[command(subcommand)]
    command: AdminKeyCommand,
}

#[derive(Debug, Subcommand)]
enum AdminKeyCommand {
    Issue {
        #[arg(long, value_parser = parse_duration)]
        ttl: Duration,
        #[arg(long)]
        label: Option<String>,
    },
    List {
        #[arg(long, default_value_t = 0)]
        page: u32,
        #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u16).range(1..=1000))]
        page_size: u16,
        #[arg(long, default_value_t = false)]
        all: bool,
    },
    Show {
        key_id: u64,
    },
    Reveal {
        key_id: u64,
    },
    Renew {
        key_id: u64,
        #[arg(long, value_parser = parse_duration)]
        ttl: Duration,
    },
    Revoke {
        key_id: u64,
    },
    Gc,
}

#[derive(Debug, Args)]
struct AdminConnectionArgs {
    #[command(subcommand)]
    command: AdminListCommand,
}

#[derive(Debug, Args)]
struct AdminServiceArgs {
    #[command(subcommand)]
    command: AdminListCommand,
}

#[derive(Debug, Clone, Subcommand)]
enum AdminListCommand {
    List {
        #[arg(long)]
        key_id: Option<u64>,
        #[arg(long, default_value_t = 0)]
        page: u32,
        #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u16).range(1..=1000))]
        page_size: u16,
        #[arg(long, default_value_t = false)]
        all: bool,
    },
}

#[derive(Debug, Args)]
struct AdminAuthStateArgs {
    #[command(subcommand)]
    command: AdminAuthStateCommand,
}

#[derive(Debug, Subcommand)]
enum AdminAuthStateCommand {
    Reset {
        #[arg(long, default_value_t = false)]
        confirm: bool,
    },
}

#[derive(Debug, Args)]
struct AdminRootKeyArgs {
    #[command(subcommand)]
    command: AdminRootKeyCommand,
}

#[derive(Debug, Subcommand)]
enum AdminRootKeyCommand {
    Rotate {
        /// New 32-byte administrator key. A cryptographically random printable key is generated when omitted.
        #[arg(long)]
        new_key: Option<String>,
        /// Save the new key here before asking the relay to rotate.
        #[arg(long, default_value = "/var/lib/pb-mapper/auth/admin.key")]
        key_file: PathBuf,
    },
}

#[derive(Debug, Args)]
struct AdminLegacyProtocolArgs {
    #[command(subcommand)]
    command: AdminLegacyProtocolCommand,
}

#[derive(Debug, Subcommand)]
enum AdminLegacyProtocolCommand {
    Set { policy: LegacyProtocolArg },
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
enum OutputFormat {
    Human,
    Json,
    Ndjson,
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
        Command::Admin(args) => run_admin(args).await?,
    }
    Ok(())
}

async fn run_server(args: ServerArgs) -> Result<(), Box<dyn Error>> {
    std::env::set_var("PB_MAPPER_AUTH_STATE_DIR", &args.auth_state_dir);
    std::env::set_var(
        "PB_MAPPER_AUTH_MAX_TEMP_KEYS",
        args.max_temporary_keys.to_string(),
    );
    std::env::set_var(
        "PB_MAPPER_AUTH_MAX_TEMP_TTL_SECS",
        args.max_temporary_key_ttl.as_secs().to_string(),
    );
    std::env::set_var(
        "PB_MAPPER_LEGACY_PROTOCOL",
        match args.legacy_protocol {
            LegacyProtocolArg::Allow => "allow",
            LegacyProtocolArg::Deny => "deny",
        },
    );
    if args.init_admin_key {
        let key_path = args.auth_state_dir.join("admin.key");
        let key = initialize_admin_key(&key_path, args.force_init_admin_key)?;
        set_process_msg_header_key(Some(&key))?;
        eprintln!("administrator key initialized at {}", key_path.display());
    }
    if args.use_machine_msg_header_key {
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

async fn run_admin(args: AdminArgs) -> Result<(), Box<dyn Error>> {
    let remote_addr = get_pb_mapper_server_async(args.server.as_deref()).await?;
    match args.command {
        AdminCommand::Key(AdminKeyArgs { command }) => match command {
            AdminKeyCommand::Issue { ttl, label } => {
                let response = send_admin_request(
                    remote_addr,
                    AdminRequest::KeyIssue {
                        ttl_seconds: ttl.as_secs(),
                        label,
                    },
                )
                .await?;
                print_admin_response(args.output, &response)?;
            }
            AdminKeyCommand::List {
                page,
                page_size,
                all,
            } => {
                stream_key_pages(remote_addr, args.output, page, page_size, all).await?;
            }
            AdminKeyCommand::Show { key_id } => {
                let response =
                    send_admin_request(remote_addr, AdminRequest::KeyShow { key_id }).await?;
                print_admin_response(args.output, &response)?;
            }
            AdminKeyCommand::Reveal { key_id } => {
                let response =
                    send_admin_request(remote_addr, AdminRequest::KeyReveal { key_id }).await?;
                print_admin_response(args.output, &response)?;
            }
            AdminKeyCommand::Renew { key_id, ttl } => {
                let response = send_admin_request(
                    remote_addr,
                    AdminRequest::KeyRenew {
                        key_id,
                        ttl_seconds: ttl.as_secs(),
                    },
                )
                .await?;
                print_admin_response(args.output, &response)?;
            }
            AdminKeyCommand::Revoke { key_id } => {
                let response =
                    send_admin_request(remote_addr, AdminRequest::KeyRevoke { key_id }).await?;
                print_admin_response(args.output, &response)?;
            }
            AdminKeyCommand::Gc => {
                let response = send_admin_request(remote_addr, AdminRequest::KeyGc).await?;
                print_admin_response(args.output, &response)?;
            }
        },
        AdminCommand::Connection(AdminConnectionArgs { command }) => {
            let AdminListCommand::List {
                key_id,
                page,
                page_size,
                all,
            } = command;
            stream_connection_pages(remote_addr, args.output, key_id, page, page_size, all).await?;
        }
        AdminCommand::Service(AdminServiceArgs { command }) => {
            let AdminListCommand::List {
                key_id,
                page,
                page_size,
                all,
            } = command;
            stream_service_pages(remote_addr, args.output, key_id, page, page_size, all).await?;
        }
        AdminCommand::Status => {
            let response = send_admin_request(remote_addr, AdminRequest::AuthStatus).await?;
            print_admin_response(args.output, &response)?;
        }
        AdminCommand::AuthState(AdminAuthStateArgs {
            command: AdminAuthStateCommand::Reset { confirm },
        }) => {
            let response =
                send_admin_request(remote_addr, AdminRequest::AuthStateReset { confirm }).await?;
            print_admin_response(args.output, &response)?;
        }
        AdminCommand::RootKey(AdminRootKeyArgs {
            command: AdminRootKeyCommand::Rotate { new_key, key_file },
        }) => {
            let new_key = new_key.unwrap_or_else(generate_admin_key);
            let staged_key_file = key_file.with_file_name(format!(
                ".{}.next",
                key_file
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("admin.key")
            ));
            write_admin_key_file(&staged_key_file, &new_key, true)?;
            let response = send_admin_request(
                remote_addr,
                AdminRequest::RootKeyRotate {
                    new_admin_key: new_key.clone(),
                },
            )
            .await
            .map_err(|error| {
                std::io::Error::other(format!(
                    "root rotation request failed; the candidate key remains at `{}`: {error}",
                    staged_key_file.display()
                ))
            })?;
            set_process_msg_header_key(Some(&new_key))?;
            let verification = send_admin_request(remote_addr, AdminRequest::AuthStatus).await?;
            if !matches!(verification, AdminResponse::AuthStatus(_)) {
                return Err(std::io::Error::other(
                    "new administrator key did not pass the post-rotation status check",
                )
                .into());
            }
            write_admin_key_file(&key_file, &new_key, true).map_err(|error| {
                std::io::Error::other(format!(
                    "administrator key rotated and verified, but `{}` could not be updated; recover the key from `{}`: {error}",
                    key_file.display(),
                    staged_key_file.display()
                ))
            })?;
            if let Err(error) = std::fs::remove_file(&staged_key_file) {
                tracing::warn!(
                    path = %staged_key_file.display(),
                    %error,
                    "administrator key was rotated, but the staged key file could not be removed"
                );
            }
            if args.output == OutputFormat::Human {
                println!("administrator key rotated and verified");
                println!("key file: {}", key_file.display());
            } else {
                print_admin_response(args.output, &response)?;
            }
        }
        AdminCommand::LegacyProtocol(AdminLegacyProtocolArgs {
            command: AdminLegacyProtocolCommand::Set { policy },
        }) => {
            let response = send_admin_request(
                remote_addr,
                AdminRequest::LegacyProtocolSet {
                    policy: policy.into(),
                },
            )
            .await?;
            print_admin_response(args.output, &response)?;
        }
    }
    Ok(())
}

async fn send_admin_request(
    remote_addr: std::net::SocketAddr,
    request: AdminRequest,
) -> Result<AdminResponse, Box<dyn Error>> {
    let encoded = PbConnRequest::Admin(request).encode()?;
    for attempt in 0..2 {
        let mut stream = TcpStream::connect(remote_addr).await?;
        let session = ClientHeaderSession::from_process()?;
        session.write_initial(&mut stream, &encoded).await?;
        let mut reader = session.response_reader(&mut stream)?;
        let message = reader.read_msg().await?;
        match PbConnResponse::decode(message)? {
            PbConnResponse::Admin(response) => return Ok(response),
            PbConnResponse::Error(error)
                if error.code == "connection_salt_replayed" && error.retryable && attempt == 0 =>
            {
                continue;
            }
            PbConnResponse::Error(error) => {
                return Err(std::io::Error::other(format!(
                    "{}: {} (retryable={})",
                    error.code, error.message, error.retryable
                ))
                .into());
            }
            response => {
                return Err(std::io::Error::other(format!(
                    "unexpected administrator response: {response:?}"
                ))
                .into());
            }
        }
    }
    Err(std::io::Error::other("connection salt replay retry was exhausted").into())
}

async fn stream_key_pages(
    remote_addr: std::net::SocketAddr,
    output: OutputFormat,
    mut page: u32,
    page_size: u16,
    all: bool,
) -> Result<(), Box<dyn Error>> {
    loop {
        let response =
            send_admin_request(remote_addr, AdminRequest::KeyList { page, page_size }).await?;
        let AdminResponse::KeyList(key_page) = &response else {
            return Err(std::io::Error::other("unexpected key-list response").into());
        };
        if all {
            for item in &key_page.items {
                println!("{}", serde_json::to_string(item)?);
            }
        } else {
            print_admin_response(output, &response)?;
        }
        let Some(next_page) = key_page.next_page else {
            break;
        };
        if !all {
            break;
        }
        page = next_page;
    }
    Ok(())
}

async fn stream_service_pages(
    remote_addr: std::net::SocketAddr,
    output: OutputFormat,
    key_id: Option<u64>,
    mut page: u32,
    page_size: u16,
    all: bool,
) -> Result<(), Box<dyn Error>> {
    loop {
        let response = send_admin_request(
            remote_addr,
            AdminRequest::ServiceList {
                key_id,
                page,
                page_size,
            },
        )
        .await?;
        let AdminResponse::Services(service_page) = &response else {
            return Err(std::io::Error::other("unexpected service-list response").into());
        };
        if all {
            for item in &service_page.items {
                println!("{}", serde_json::to_string(item)?);
            }
        } else {
            print_admin_response(output, &response)?;
        }
        let Some(next_page) = service_page.next_page else {
            break;
        };
        if !all {
            break;
        }
        page = next_page;
    }
    Ok(())
}

async fn stream_connection_pages(
    remote_addr: std::net::SocketAddr,
    output: OutputFormat,
    key_id: Option<u64>,
    mut page: u32,
    page_size: u16,
    all: bool,
) -> Result<(), Box<dyn Error>> {
    loop {
        let response = send_admin_request(
            remote_addr,
            AdminRequest::ConnectionList {
                key_id,
                page,
                page_size,
            },
        )
        .await?;
        let AdminResponse::Connections(connection_page) = &response else {
            return Err(std::io::Error::other("unexpected connection-list response").into());
        };
        if all {
            for item in &connection_page.items {
                println!("{}", serde_json::to_string(item)?);
            }
        } else {
            print_admin_response(output, &response)?;
        }
        let Some(next_page) = connection_page.next_page else {
            break;
        };
        if !all {
            break;
        }
        page = next_page;
    }
    Ok(())
}

fn print_admin_response(
    output: OutputFormat,
    response: &AdminResponse,
) -> Result<(), Box<dyn Error>> {
    match output {
        OutputFormat::Json => println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "schema_version": 1,
                "data": response,
            }))?
        ),
        OutputFormat::Ndjson => println!("{}", serde_json::to_string(response)?),
        OutputFormat::Human => print_human_admin_response(response),
    }
    Ok(())
}

fn print_human_admin_response(response: &AdminResponse) {
    match response {
        AdminResponse::KeyIssued(key)
        | AdminResponse::KeyShown(key)
        | AdminResponse::KeyRenewed(key) => {
            println!("key id: {}", key.metadata.key_id);
            println!("state: {}", key.metadata.state);
            println!("expires at: {}", key.metadata.expires_at);
            if let Some(label) = &key.metadata.label {
                println!("label: {label}");
            }
            if !key.credential.is_empty() {
                println!("credential: {}", key.credential);
            }
        }
        AdminResponse::KeyRevoked(key) => {
            println!("key {}: {}", key.key_id, key.state);
        }
        AdminResponse::KeyList(page) => {
            println!("KEY ID\tSTATE\tEXPIRES\tLABEL");
            for key in &page.items {
                println!(
                    "{}\t{}\t{}\t{}",
                    key.key_id,
                    key.state,
                    key.expires_at,
                    key.label.as_deref().unwrap_or("")
                );
            }
            if let Some(next) = page.next_page {
                println!("next page: {next}");
            }
        }
        AdminResponse::KeyGc { removed } => println!("removed {removed} inactive keys"),
        AdminResponse::AuthStatus(status) => {
            println!("safe mode: {}", status.safe_mode);
            println!(
                "keys: {} active / {} expired / {} revoked / {} capacity",
                status.active_keys, status.expired_keys, status.revoked_keys, status.capacity
            );
            println!("legacy protocol: {:?}", status.legacy_protocol);
            println!(
                "active legacy connections: {}",
                status.active_legacy_connections
            );
            println!(
                "last legacy connection: {}",
                status
                    .last_legacy_connection_at
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "never".to_string())
            );
            println!(
                "authentication: {} succeeded / {} failed",
                status.auth_successes, status.auth_failures
            );
            println!("server instance: {}", status.server_instance_id);
        }
        AdminResponse::Services(page) => {
            println!("KEY ID\tSERVICE\tTRANSPORT\tCODEC\tCONNECTIONS");
            for service in &page.items {
                println!(
                    "{}\t{}\t{}\t{}\t{}",
                    service.key_id,
                    service.service_name,
                    service.transport,
                    service.codec_enabled,
                    service.connection_count
                );
            }
        }
        AdminResponse::Connections(page) => {
            println!("KEY ID\tSERVICE\tCONN ID\tHEALTHY\tTRANSPORT\tCODEC");
            for connection in &page.items {
                println!(
                    "{}\t{}\t{}\t{}\t{}\t{}",
                    connection.key_id,
                    connection.service_name,
                    connection.conn_id,
                    connection.healthy,
                    connection.transport,
                    connection.codec_enabled
                );
            }
        }
        AdminResponse::Ok { action } => println!("ok: {action}"),
    }
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
    }
}
