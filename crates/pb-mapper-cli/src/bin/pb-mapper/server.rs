//! The `server` role: the relay's flags, its authentication settings, and
//! where direct coordination listens.

use std::error::Error;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::PathBuf;
use std::time::Duration;

use clap::{Args, ValueEnum};
use pb_mapper_auth::{
    AuthConfig, AuthConfigOverrides, LegacyProtocolPolicy, MAX_TEMP_KEY_CAPACITY, MAX_TEMP_KEY_TTL,
    MIN_TEMP_KEY_TTL, acquire_state_dir_lock, initialize_admin_key,
};
use pb_mapper_core::checksum::{
    MACHINE_MSG_HEADER_KEY_PATH, set_process_msg_header_key, setup_machine_msg_header_key,
};
use pb_mapper_core::config::keep_alive_from_env;
use tokio_util::sync::CancellationToken;

use crate::parse_duration;

#[derive(Debug, Args)]
pub(super) struct ServerArgs {
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

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub(super) enum LegacyProtocolArg {
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

/// A server flag outside the range the relay accepts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ServerFlagError {
    MaxTemporaryKeys,
    MaxTemporaryKeyTtl,
}

impl std::fmt::Display for ServerFlagError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MaxTemporaryKeys => write!(
                f,
                "`--max-temporary-keys` must be between 1 and {MAX_TEMP_KEY_CAPACITY}"
            ),
            Self::MaxTemporaryKeyTtl => write!(
                f,
                "`--max-temporary-key-ttl` must be between {}s and {}d",
                MIN_TEMP_KEY_TTL.as_secs(),
                MAX_TEMP_KEY_TTL.as_secs() / 86_400
            ),
        }
    }
}

impl Error for ServerFlagError {}

/// The relay's authentication configuration: the environment's, with each
/// explicit flag taking precedence.
///
/// The flags are checked first, so an out-of-range one fails before anything
/// else happens. A flag that is given replaces its environment variable,
/// which is then not read at all.
fn server_auth_config(args: &ServerArgs) -> Result<AuthConfig, ServerFlagError> {
    if let Some(max_temporary_keys) = args.max_temporary_keys
        && !(1..=MAX_TEMP_KEY_CAPACITY).contains(&max_temporary_keys)
    {
        return Err(ServerFlagError::MaxTemporaryKeys);
    }
    if let Some(max_temporary_key_ttl) = args.max_temporary_key_ttl
        && !(MIN_TEMP_KEY_TTL..=MAX_TEMP_KEY_TTL).contains(&max_temporary_key_ttl)
    {
        return Err(ServerFlagError::MaxTemporaryKeyTtl);
    }
    Ok(AuthConfig::from_env_with(AuthConfigOverrides {
        state_dir: args.auth_state_dir.clone(),
        max_temporary_keys: args.max_temporary_keys,
        max_temporary_key_ttl: args.max_temporary_key_ttl,
        legacy_protocol: args.legacy_protocol.map(Into::into),
    }))
}

pub(super) async fn run_server(args: ServerArgs) -> Result<(), Box<dyn Error>> {
    let auth_config = server_auth_config(&args)?;
    let admin_key_path = auth_config.state_dir.join("admin.key");
    if args.init_admin_key {
        std::fs::create_dir_all(&auth_config.state_dir)?;
        let lock = acquire_state_dir_lock(&auth_config.state_dir)?;
        let key = initialize_admin_key(&admin_key_path, args.force_init_admin_key)?;
        drop(lock);
        set_process_msg_header_key(Some(&key))?;
        eprintln!(
            "administrator key initialized at {}",
            admin_key_path.display()
        );
    } else if args.use_machine_msg_header_key {
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

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;
    use crate::{Cli, Command};

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
    fn server_direct_flags_from_0_7_still_parse_and_override_the_defaults() {
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
    }

    #[test]
    fn server_auth_options_only_override_environment_when_explicit() {
        let defaults = server_args(&[]);
        assert_eq!(defaults.auth_state_dir, None);
        assert_eq!(defaults.quic_listen, None);
        assert_eq!(defaults.max_temporary_keys, None);
        assert_eq!(defaults.max_temporary_key_ttl, None);
        assert_eq!(defaults.legacy_protocol, None);

        // Each value differs from its default, so each one shows its flag
        // reached the relay's configuration.
        let explicit = server_auth_config(&server_args(&[
            "--auth-state-dir",
            "/tmp/pb-mapper-auth",
            "--max-temporary-keys",
            "1024",
            "--max-temporary-key-ttl",
            "2h",
            "--legacy-protocol",
            "deny",
        ]))
        .unwrap();
        assert_eq!(explicit.state_dir, PathBuf::from("/tmp/pb-mapper-auth"));
        assert_eq!(explicit.max_temporary_keys, 1024);
        assert_eq!(
            explicit.max_temporary_key_ttl,
            Duration::from_secs(2 * 60 * 60)
        );
        assert_eq!(explicit.legacy_protocol, LegacyProtocolPolicy::Deny);
    }

    #[test]
    fn explicit_out_of_range_server_auth_flags_are_rejected() {
        // clap accepts both tokens; the bounds are checked afterwards, before
        // the configuration is built.
        assert_eq!(
            server_auth_config(&server_args(&["--max-temporary-keys", "0"])).unwrap_err(),
            ServerFlagError::MaxTemporaryKeys
        );
        assert_eq!(
            server_auth_config(&server_args(&["--max-temporary-key-ttl", "5s"])).unwrap_err(),
            ServerFlagError::MaxTemporaryKeyTtl
        );
    }
}
