//! The `register` and `connect` roles: their flags, the tunnel they start,
//! and the log they keep of it until it stops.

use std::error::Error;
use std::time::Duration;

use clap::{Args, ValueEnum};
use pb_mapper_client::sdk::{
    Client, ConnectOptions, ConnectRequest, LanTransport, PathPolicy, PortMapping, RegisterOptions,
    RegisterRequest, RelayTransport, TunnelDiagnostics, TunnelEventKind, TunnelEvents,
    TunnelMetrics, TunnelStatus,
};
use pb_mapper_core::config::{keep_alive_from_env, pb_mapper_server_addr};

#[derive(Debug, Args)]
pub(super) struct RegisterArgs {
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
    /// Probe a consumer's addresses in the order it offered them, rather than
    /// those on a local network shared with it first.
    #[arg(
        long = "no-lan-first",
        action = clap::ArgAction::SetFalse,
        conflicts_with = "relay_only"
    )]
    lan_first: bool,
    /// Refuse a consumer that asks for TCP within a local network shared
    /// with it; it then takes QUIC. Otherwise a TCP listener opens the first
    /// time one asks, on the direct port's number, offered on LANs only.
    #[arg(
        long = "no-lan-tcp",
        action = clap::ArgAction::SetFalse,
        conflicts_with = "relay_only"
    )]
    lan_tcp: bool,
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
pub(super) struct ConnectArgs {
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
    /// Dial every direct path as soon as it answers, rather than trying a
    /// local network shared with the publisher first.
    #[arg(
        long = "no-lan-first",
        action = clap::ArgAction::SetFalse,
        conflicts_with = "relay_only"
    )]
    lan_first: bool,
    /// What a path within a local network shared with the publisher runs
    /// over: `quic` (the default), or `tcp`, tried 300 ms ahead of QUIC and
    /// falling back to it. A path across the internet is always QUIC.
    #[arg(
        long,
        value_enum,
        default_value_t = LanTransportArg::Quic,
        conflicts_with = "relay_only"
    )]
    lan_transport: LanTransportArg,
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

impl From<RelayTransportArg> for RelayTransport {
    fn from(value: RelayTransportArg) -> Self {
        match value {
            RelayTransportArg::Tcp => Self::Tcp,
            RelayTransportArg::Quic => Self::Quic,
        }
    }
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
enum LanTransportArg {
    Quic,
    Tcp,
}

impl From<LanTransportArg> for LanTransport {
    fn from(value: LanTransportArg) -> Self {
        match value {
            LanTransportArg::Quic => Self::Quic,
            LanTransportArg::Tcp => Self::Tcp,
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

impl From<Transport> for pb_mapper_client::sdk::Transport {
    fn from(value: Transport) -> Self {
        match value {
            Transport::Tcp => Self::Tcp,
            Transport::Udp => Self::Udp,
        }
    }
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
    .with_relay_transport(relay.relay_transport.into())?)
}

pub(super) async fn run_register(args: RegisterArgs) -> Result<(), Box<dyn Error>> {
    let client = tunnel_client(&args.relay, args.namespace)?;
    let registration = client
        .register_with_options(
            RegisterRequest {
                key: args.key,
                local_addr: args.addr,
                transport: args.transport.into(),
                codec: args.codec,
                force_namespace: args.force,
            },
            RegisterOptions {
                allow_direct: !args.relay_only && RegisterOptions::default().allow_direct,
                idle_timeout: None,
                port_mapping: args.port_mapping.into(),
                lan_first: args.lan_first,
                lan_tcp: args.lan_tcp,
            },
        )
        .await?;
    monitor_tunnel(
        &client,
        registration.subscribe(),
        registration.subscribe_events(),
        || registration.metrics(),
        || registration.diagnostics(),
    )
    .await?;
    registration.stop().await?;
    Ok(())
}

pub(super) async fn run_connect(args: ConnectArgs) -> Result<(), Box<dyn Error>> {
    let client = tunnel_client(&args.relay, args.namespace)?;
    let connection = client
        .connect_with_options(
            ConnectRequest {
                key: args.key,
                local_addr: args.addr,
                transport: args.transport.into(),
            },
            ConnectOptions {
                path: match (args.relay_only, args.path) {
                    (true, _) | (false, PathArg::RelayOnly) => PathPolicy::RelayOnly,
                    // Auto in a build with `p2p`, relay-only in one without.
                    (false, PathArg::Auto) => PathPolicy::default(),
                },
                port_mapping: args.port_mapping.into(),
                lan_first: args.lan_first,
                lan_transport: args.lan_transport.into(),
            },
        )
        .await?;
    monitor_tunnel(
        &client,
        connection.subscribe(),
        connection.subscribe_events(),
        || connection.metrics(),
        || connection.diagnostics(),
    )
    .await?;
    connection.stop().await?;
    Ok(())
}

/// Log a running tunnel until it stops: its metrics, its diagnostics and the
/// relay's every 30 s, and as they happen each session that closes, with its
/// bytes, and each setup that found the LAN and went elsewhere. The
/// diagnostics carry what the metrics leave out: port mapping, the retry
/// state, recent events, and the direct link's setup timings and candidate
/// counts, which a LAN-first A/B compares.
async fn monitor_tunnel(
    client: &Client,
    mut status: tokio::sync::watch::Receiver<TunnelStatus>,
    mut events: TunnelEvents,
    metrics: impl Fn() -> TunnelMetrics,
    diagnostics: impl Fn() -> TunnelDiagnostics,
) -> Result<(), Box<dyn Error>> {
    let mut interval = tokio::time::interval(Duration::from_secs(30));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // One listener for the whole loop: one made per pass would be gone while
    // a pass logs, and a Ctrl-C arriving then would be lost.
    let mut ctrl_c = std::pin::pin!(tokio::signal::ctrl_c());
    let mut events_open = true;
    loop {
        let state = status.borrow_and_update().clone();
        match state {
            TunnelStatus::Failed(reason) => return Err(std::io::Error::other(reason).into()),
            TunnelStatus::Stopped => return Ok(()),
            _ => {}
        }
        tokio::select! {
            biased;
            result = &mut ctrl_c => { result?; return Ok(()); },
            result = status.changed() => { if result.is_err() { return Ok(()); } },
            event = events.recv(), if events_open => match event {
                Some(Ok(event)) => match &event.kind {
                    TunnelEventKind::SessionClosed { .. } => {
                        tracing::info!(event = "tunnel_session_closed", detail = %serde_json::to_string(&event.kind)?, "tunnel session closed");
                    }
                    TunnelEventKind::LanMissed { .. } => {
                        tracing::info!(event = "tunnel_lan_missed", detail = %serde_json::to_string(&event.kind)?, "direct setup found the LAN but did not stay on it");
                    }
                    TunnelEventKind::LanTcpMissed { .. } => {
                        tracing::info!(event = "tunnel_lan_tcp_missed", detail = %serde_json::to_string(&event.kind)?, "direct setup asked for TCP on the LAN and took another way");
                    }
                    _ => {}
                },
                Some(Err(lagged)) => tracing::debug!(%lagged, "tunnel events skipped"),
                None => events_open = false,
            },
            _ = interval.tick() => {
                tracing::info!(event = "tunnel_metrics", status = ?state, metrics = %serde_json::to_string(&metrics())?, diagnostics = %serde_json::to_string(&diagnostics())?, relay = %serde_json::to_string(&client.relay_diagnostics().await)?, "tunnel metrics");
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;
    use crate::{Cli, Command};

    const REGISTER: [&str; 7] = [
        "pb-mapper",
        "register",
        "tcp",
        "--key",
        "web",
        "--addr",
        "127.0.0.1:8080",
    ];

    const CONNECT: [&str; 7] = [
        "pb-mapper",
        "connect",
        "tcp",
        "--key",
        "web",
        "--addr",
        "127.0.0.1:9090",
    ];

    /// `base`, a whole command line, with `extra` appended.
    fn parse_with(base: &[&str], extra: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(base.iter().chain(extra))
    }

    fn register_args(extra: &[&str]) -> RegisterArgs {
        let cli = parse_with(&REGISTER, extra).expect("register flags should parse");
        let Command::Register(args) = cli.command else {
            panic!("expected register command");
        };
        args
    }

    fn connect_args(extra: &[&str]) -> ConnectArgs {
        let cli = parse_with(&CONNECT, extra).expect("connect flags should parse");
        let Command::Connect(args) = cli.command else {
            panic!("expected connect command");
        };
        args
    }

    #[test]
    fn register_flags_from_0_7_still_parse_and_direct_is_the_default() {
        let defaults = register_args(&[]);
        assert!(!defaults.relay_only);
        assert!(defaults.lan_first && defaults.lan_tcp);
        assert_eq!(defaults.relay.relay_transport, RelayTransportArg::Tcp);
        assert!(
            parse_with(&REGISTER, &["--allow-direct"]).is_ok(),
            "0.7 opt-in flag is accepted"
        );
        let no_tcp = register_args(&["--no-lan-tcp"]);
        assert!(!no_tcp.lan_tcp && no_tcp.lan_first);
        assert!(parse_with(&REGISTER, &["--relay-only", "--no-lan-tcp"]).is_err());
        assert!(!register_args(&["--no-lan-first"]).lan_first);
        assert!(parse_with(&REGISTER, &["--relay-only", "--no-lan-first"]).is_err());
        assert!(register_args(&["--relay-only"]).relay_only);
    }

    #[test]
    fn connect_flags_from_0_7_still_parse_and_direct_is_the_default() {
        let defaults = connect_args(&[]);
        assert_eq!(defaults.path, PathArg::Auto);
        assert_eq!(defaults.relay.relay_transport, RelayTransportArg::Tcp);
        assert!(!defaults.relay_only);
        assert!(parse_with(&CONNECT, &["--path", "auto"]).is_ok());
        assert!(parse_with(&CONNECT, &["--relay-only"]).is_ok());
        assert!(parse_with(&CONNECT, &["--relay-only", "--path", "auto"]).is_err());
        let mapping = |extra: &[&str]| connect_args(extra).port_mapping;
        assert_eq!(defaults.port_mapping, PortMappingArg::Auto);
        assert_eq!(mapping(&["--port-mapping", "off"]), PortMappingArg::Off);
        assert_eq!(mapping(&["--port-mapping"]), PortMappingArg::Always);
        assert_eq!(mapping(&["--port-mapping", "auto"]), PortMappingArg::Auto);
        assert!(defaults.lan_first);
        assert!(!connect_args(&["--no-lan-first"]).lan_first);
        assert!(parse_with(&CONNECT, &["--relay-only", "--no-lan-first"]).is_err());
        assert_eq!(defaults.lan_transport, LanTransportArg::Quic);
        let tcp = connect_args(&["--lan-transport", "tcp"]);
        assert_eq!(LanTransport::from(tcp.lan_transport), LanTransport::Tcp);
        assert!(parse_with(&CONNECT, &["--relay-only", "--lan-transport", "tcp"]).is_err());
    }
}
