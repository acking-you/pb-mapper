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
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::PathBuf;
use std::time::Duration;

use better_mimalloc_rs::MiMalloc;
use clap::{Args, Parser, Subcommand, ValueEnum};
use pb_mapper_auth::{
    AuthConfig, KeyPage, LegacyProtocolPolicy, MAX_TEMP_KEY_CAPACITY, MAX_TEMP_KEY_TTL,
    MIN_TEMP_KEY_TTL, acquire_state_dir_lock, discard_staged_admin_key, generate_admin_key,
    initialize_admin_key, stage_admin_key_candidate, write_admin_key_file,
};
use pb_mapper_client::client::handle_status_cli_scoped;
use pb_mapper_client::sdk::{
    Client, ConnectOptions, ConnectRequest, PathPolicy, PortMapping, RegisterOptions,
    RegisterRequest, TunnelDiagnostics, TunnelStatus,
};
use pb_mapper_core::checksum::set_process_msg_header_key;
use pb_mapper_core::checksum::{MACHINE_MSG_HEADER_KEY_PATH, setup_machine_msg_header_key};
use pb_mapper_core::config::{
    StatusOp, init_tracing, keep_alive_from_env, pb_mapper_server_addr,
    resolve_pb_mapper_server_async,
};
use pb_mapper_protocol::command::{
    AdminConnectionPage, AdminRequest, AdminResponse, AdminServicePage,
};

use tokio_util::sync::CancellationToken;

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
    /// Enable QUIC relay on a separate UDP address (e.g. 0.0.0.0:7667).
    #[arg(long)]
    quic_listen: Option<SocketAddr>,
    /// Public UDP port when NAT maps the QUIC listener to another port.
    #[arg(long, requires = "quic_listen", default_value_t = 0)]
    quic_advertise_port: u16,
    /// Port exposed to registering services and connecting clients. Direct
    /// coordination listens on the same port number over UDP.
    #[arg(short, long, visible_alias = "pb-mapper-port", default_value_t = 7666)]
    port: u16,
    /// Disable direct coordination; explicit --quic-listen remains available.
    #[arg(
        long,
        visible_alias = "disable-direct",
        conflicts_with_all = ["enable_direct", "direct_listen", "direct_advertise"]
    )]
    relay_only: bool,
    /// Accepted for 0.7 scripts. Direct is on by default; when this or a
    /// `--direct-*` flag is given, a UDP bind failure stops the relay instead
    /// of degrading it to relay-only.
    #[arg(long, hide = true)]
    enable_direct: bool,
    /// UDP address for direct coordination. Defaults to the relay's own
    /// address and port, over UDP.
    #[arg(long)]
    direct_listen: Option<SocketAddr>,
    /// Public IP and UDP port announced to clients. Defaults to the address
    /// each client dialed to reach the relay, with the direct port.
    #[arg(long)]
    direct_advertise: Option<SocketAddr>,
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
    /// Keep every session to this service on the relay; refuse direct ones.
    #[arg(long, conflicts_with = "allow_direct")]
    relay_only: bool,
    /// Accepted for 0.7 scripts: direct sessions are allowed by default.
    #[arg(long, hide = true)]
    allow_direct: bool,
    /// When to ask the router (PCP, NAT-PMP or UPnP) to map the direct UDP
    /// port: `auto` once punching through fails (the default), `always` from
    /// the start, or `off`. Alone, `always`.
    #[arg(
        long,
        value_enum,
        default_value_t = PortMappingArg::Auto,
        num_args = 0..=1,
        default_missing_value = "always",
        conflicts_with = "relay_only"
    )]
    port_mapping: PortMappingArg,
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
    /// Try direct first with automatic relay fallback, or use the relay only.
    #[arg(long, value_enum, default_value_t = PathArg::Auto)]
    path: PathArg,
    /// Shorthand for `--path relay-only`.
    #[arg(long, conflicts_with = "path")]
    relay_only: bool,
    /// When to ask the router (PCP, NAT-PMP or UPnP) to map the direct UDP
    /// port: `auto` once punching through fails (the default), `always` from
    /// the start, or `off`. Alone, `always`.
    #[arg(
        long,
        value_enum,
        default_value_t = PortMappingArg::Auto,
        num_args = 0..=1,
        default_missing_value = "always",
        conflicts_with = "relay_only"
    )]
    port_mapping: PortMappingArg,
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
    /// Carrier between this endpoint and the relay; independent of TCP/UDP service type.
    #[arg(long, value_enum, default_value_t = RelayTransportArg::Tcp)]
    relay_transport: RelayTransportArg,
    /// Relay address. Falls back to PB_MAPPER_SERVER.
    #[arg(short, long, visible_alias = "pb-mapper-server", value_name = "ADDR")]
    server: Option<String>,
    /// Enable TCP keep-alive. PB_MAPPER_KEEP_ALIVE=ON is also supported.
    #[arg(long, default_value_t = false)]
    keep_alive: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum RelayTransportArg {
    Tcp,
    Quic,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum PortMappingArg {
    Off,
    Auto,
    Always,
}

impl From<PortMappingArg> for PortMapping {
    fn from(value: PortMappingArg) -> Self {
        match value {
            PortMappingArg::Off => Self::Off,
            PortMappingArg::Auto => Self::Auto,
            PortMappingArg::Always => Self::Always,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum PathArg {
    RelayOnly,
    Auto,
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

/// Publishes CLI flags as environment variables, which is how the auth
/// subsystem reads its configuration.
///
/// # Safety note
///
/// Mutating the environment is unsafe in edition 2024 because it races
/// concurrent readers. Every call here happens during argument handling on the
/// main thread, before any runtime task or thread is spawned, so there is no
/// concurrent reader to race.
fn apply_server_auth_overrides(args: &ServerArgs) -> Result<(), Box<dyn Error>> {
    if let Some(auth_state_dir) = &args.auth_state_dir {
        unsafe { std::env::set_var("PB_MAPPER_AUTH_STATE_DIR", auth_state_dir) };
    }
    if let Some(max_temporary_keys) = args.max_temporary_keys {
        if !(1..=MAX_TEMP_KEY_CAPACITY).contains(&max_temporary_keys) {
            return Err(format!(
                "`--max-temporary-keys` must be between 1 and {MAX_TEMP_KEY_CAPACITY}"
            )
            .into());
        }
        unsafe {
            std::env::set_var(
                "PB_MAPPER_AUTH_MAX_TEMP_KEYS",
                max_temporary_keys.to_string(),
            )
        };
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
        unsafe {
            std::env::set_var(
                "PB_MAPPER_AUTH_MAX_TEMP_TTL_SECS",
                max_temporary_key_ttl.as_secs().to_string(),
            )
        };
    }
    Ok(())
}

async fn run_server(args: ServerArgs) -> Result<(), Box<dyn Error>> {
    apply_server_auth_overrides(&args)?;
    if let Some(legacy_protocol) = args.legacy_protocol {
        // SAFETY: as in `apply_server_auth_overrides` — this runs before the
        // server spawns anything that reads the environment.
        unsafe {
            std::env::set_var(
                "PB_MAPPER_LEGACY_PROTOCOL",
                match legacy_protocol {
                    LegacyProtocolArg::Allow => "allow",
                    LegacyProtocolArg::Deny => "deny",
                },
            )
        };
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
    let shutdown = CancellationToken::new();
    let keep_alive = args.keep_alive || keep_alive_from_env();
    let options = pb_mapper_server::RelayServerOptions {
        direct: direct_config(&args, ip_addr),
        quic: args
            .quic_listen
            .map(|listen| pb_mapper_server::QuicServerConfig {
                listen,
                advertise_port: args.quic_advertise_port,
            }),
    };
    let auth = pb_mapper_auth::AuthRuntime::from_process(auth_config).await?;
    let listener = tokio::net::TcpListener::bind((ip_addr, args.port)).await?;
    pb_mapper_server::run_server_on_listener_with_options(
        listener, shutdown, None, keep_alive, auth, options,
    )
    .await?;
    Ok(())
}

/// Where direct coordination listens, or `None` to serve the relay alone.
///
/// By default it takes the relay's own port over UDP and announces only that
/// port, so clients pair it with the relay address they dialed. A relay
/// started without any direct flag keeps serving if that UDP port is taken —
/// upgrading must not turn a running relay into one that will not start —
/// while one given an explicit direct flag fails as asked.
fn direct_config(
    args: &ServerArgs,
    ip_addr: IpAddr,
) -> Option<pb_mapper_server::DirectServerConfig> {
    let explicit =
        args.enable_direct || args.direct_listen.is_some() || args.direct_advertise.is_some();
    // A build without `p2p` has no direct service to offer by default; an
    // explicit request still fails at bind, saying so.
    if args.relay_only || (!explicit && !cfg!(feature = "p2p")) {
        return None;
    }
    let listen = args
        .direct_listen
        .unwrap_or(SocketAddr::new(ip_addr, args.port));
    let advertise = args
        .direct_advertise
        .unwrap_or(SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0));
    if !explicit && let Err(error) = std::net::UdpSocket::bind(listen) {
        tracing::warn!(
            event = "direct_service_unavailable",
            %listen,
            %error,
            "UDP port unavailable; serving relay only (pass --direct-listen to choose another)"
        );
        return None;
    }
    Some(pb_mapper_server::DirectServerConfig { listen, advertise })
}

fn tunnel_client(relay: &RelayArgs, namespace: Option<u64>) -> Result<Client, Box<dyn Error>> {
    let credential = pb_mapper_core::checksum::get_process_credential().map_err(|error| {
        std::io::Error::other(format!("tunnel credential is required: {error}"))
    })?;
    Ok(Client::from_credential(
        pb_mapper_server_addr(relay.server.as_deref())?,
        credential,
        relay.keep_alive || keep_alive_from_env(),
        namespace,
    )
    .with_relay_transport(match relay.relay_transport {
        RelayTransportArg::Tcp => pb_mapper_client::sdk::RelayTransport::Tcp,
        RelayTransportArg::Quic => pb_mapper_client::sdk::RelayTransport::Quic,
    })?)
}

fn sdk_transport(transport: Transport) -> pb_mapper_client::sdk::Transport {
    match transport {
        Transport::Tcp => pb_mapper_client::sdk::Transport::Tcp,
        Transport::Udp => pb_mapper_client::sdk::Transport::Udp,
    }
}

async fn run_register(args: RegisterArgs) -> Result<(), Box<dyn Error>> {
    let client = tunnel_client(&args.relay, args.namespace)?;
    let registration = client
        .register_with_options(
            RegisterRequest {
                key: args.key,
                local_addr: args.addr,
                transport: sdk_transport(args.transport),
                codec: args.codec,
                force_namespace: args.force,
            },
            RegisterOptions {
                allow_direct: !args.relay_only && RegisterOptions::default().allow_direct,
                port_mapping: args.port_mapping.into(),
                ..Default::default()
            },
        )
        .await?;
    monitor_tunnel(&client, registration.subscribe(), || {
        registration.diagnostics()
    })
    .await?;
    registration.stop().await?;
    Ok(())
}

async fn run_connect(args: ConnectArgs) -> Result<(), Box<dyn Error>> {
    let client = tunnel_client(&args.relay, args.namespace)?;
    let connection = client
        .connect_with_options(
            ConnectRequest {
                key: args.key,
                local_addr: args.addr,
                transport: sdk_transport(args.transport),
            },
            ConnectOptions {
                path: match (args.relay_only, args.path) {
                    (true, _) | (false, PathArg::RelayOnly) => PathPolicy::RelayOnly,
                    // Auto in a build with `p2p`, relay-only in one without.
                    (false, PathArg::Auto) => PathPolicy::default(),
                },
                port_mapping: args.port_mapping.into(),
            },
        )
        .await?;
    monitor_tunnel(&client, connection.subscribe(), || connection.diagnostics()).await?;
    connection.stop().await?;
    Ok(())
}

async fn monitor_tunnel(
    client: &Client,
    mut status: tokio::sync::watch::Receiver<TunnelStatus>,
    diagnostics: impl Fn() -> TunnelDiagnostics,
) -> Result<(), Box<dyn Error>> {
    let mut interval = tokio::time::interval(Duration::from_secs(30));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        let state = status.borrow_and_update().clone();
        match state {
            TunnelStatus::Failed(reason) => return Err(std::io::Error::other(reason).into()),
            TunnelStatus::Stopped => return Ok(()),
            _ => {}
        }
        tokio::select! {
            result = tokio::signal::ctrl_c() => { result?; return Ok(()); },
            result = status.changed() => { if result.is_err() { return Ok(()); } },
            _ = interval.tick() => {
                let snapshot = diagnostics();
                tracing::info!(event = "tunnel_diagnostics", status = ?state, diagnostics = %serde_json::to_string(&snapshot)?, relay = %serde_json::to_string(&client.relay_diagnostics().await)?, "tunnel recovery snapshot");
            },
        }
    }
}

async fn run_status(args: StatusArgs) -> Result<(), Box<dyn Error>> {
    let remote_addr = resolve_pb_mapper_server_async(args.server.as_deref()).await?;
    handle_status_cli_scoped(args.op, remote_addr.as_slice(), args.namespace).await
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
            ));
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

    fn server_args(args: &[&str]) -> ServerArgs {
        let cli = Cli::try_parse_from(["pb-mapper", "server"].iter().chain(args))
            .expect("server flags should parse");
        let Command::Server(args) = cli.command else {
            panic!("expected server command");
        };
        args
    }

    #[cfg(feature = "p2p")]
    #[test]
    fn direct_is_on_by_default_on_the_relay_port_over_udp() {
        let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
        // Port 0 so the UDP probe binds whatever is free on this machine.
        let config =
            direct_config(&server_args(&["--port", "0"]), ip).expect("direct is the default");
        assert_eq!(config.listen, SocketAddr::new(ip, 0));
        // Only the port is announced; clients add the relay IP they dialed.
        assert!(config.advertise.ip().is_unspecified());

        assert!(direct_config(&server_args(&["--relay-only"]), ip).is_none());
        assert!(direct_config(&server_args(&["--disable-direct"]), ip).is_none());
    }

    #[test]
    fn direct_flags_from_0_7_still_parse_and_override_the_defaults() {
        let args = server_args(&[
            "--enable-direct",
            "--direct-listen",
            "0.0.0.0:10811",
            "--direct-advertise",
            "203.0.113.7:10811",
        ]);
        let config = direct_config(&args, IpAddr::V4(Ipv4Addr::UNSPECIFIED))
            .expect("explicit direct configuration");
        assert_eq!(config.listen, "0.0.0.0:10811".parse().unwrap());
        assert_eq!(config.advertise, "203.0.113.7:10811".parse().unwrap());
        assert!(
            Cli::try_parse_from(["pb-mapper", "server", "--relay-only", "--enable-direct"])
                .is_err()
        );

        let register = [
            "pb-mapper",
            "register",
            "tcp",
            "--key",
            "web",
            "--addr",
            "127.0.0.1:8080",
        ];
        let Command::Register(defaults) = Cli::try_parse_from(register).unwrap().command else {
            panic!("expected register command");
        };
        assert!(!defaults.relay_only);
        assert_eq!(defaults.relay.relay_transport, RelayTransportArg::Tcp);
        assert!(
            Cli::try_parse_from(register.iter().chain(&["--allow-direct"])).is_ok(),
            "0.7 opt-in flag is accepted"
        );
        let Command::Register(opted_out) =
            Cli::try_parse_from(register.iter().chain(&["--relay-only"]))
                .unwrap()
                .command
        else {
            panic!("expected register command");
        };
        assert!(opted_out.relay_only);

        let connect = [
            "pb-mapper",
            "connect",
            "tcp",
            "--key",
            "web",
            "--addr",
            "127.0.0.1:9090",
        ];
        let Command::Connect(defaults) = Cli::try_parse_from(connect).unwrap().command else {
            panic!("expected connect command");
        };
        assert_eq!(defaults.path, PathArg::Auto);
        assert_eq!(defaults.relay.relay_transport, RelayTransportArg::Tcp);
        assert!(!defaults.relay_only);
        assert!(Cli::try_parse_from(connect.iter().chain(&["--path", "auto"])).is_ok());
        assert!(Cli::try_parse_from(connect.iter().chain(&["--relay-only"])).is_ok());
        assert!(
            Cli::try_parse_from(connect.iter().chain(&["--relay-only", "--path", "auto"])).is_err()
        );
        let mapping = |extra: &[&str]| match Cli::try_parse_from(connect.iter().chain(extra))
            .unwrap()
            .command
        {
            Command::Connect(args) => args.port_mapping,
            _ => panic!("expected connect command"),
        };
        assert_eq!(defaults.port_mapping, PortMappingArg::Auto);
        assert_eq!(mapping(&["--port-mapping", "off"]), PortMappingArg::Off);
        assert_eq!(mapping(&["--port-mapping"]), PortMappingArg::Always);
        assert_eq!(mapping(&["--port-mapping", "auto"]), PortMappingArg::Auto);
    }

    #[test]
    fn rejects_invalid_admin_paging_and_duration() {
        assert!(
            Cli::try_parse_from(["pb-mapper", "admin", "key", "list", "--page-size", "1001",])
                .is_err()
        );
        assert!(
            Cli::try_parse_from(["pb-mapper", "admin", "key", "issue", "--ttl", "1fortnight",])
                .is_err()
        );
        assert!(
            Cli::try_parse_from([
                "pb-mapper",
                "server",
                "--init-admin-key",
                "--use-machine-msg-header-key",
            ])
            .is_err()
        );
    }

    #[test]
    fn server_auth_options_only_override_environment_when_explicit() {
        let cli =
            Cli::try_parse_from(["pb-mapper", "server"]).expect("server defaults should parse");
        let Command::Server(defaults) = cli.command else {
            panic!("expected server command");
        };
        assert_eq!(defaults.auth_state_dir, None);
        assert_eq!(defaults.quic_listen, None);
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
