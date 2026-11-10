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
//! Role-specific execution stays below this dispatch layer, a module per role:
//! `server` (the relay, its authentication settings and direct coordination),
//! `tunnel` (`register` and `connect`, and the log kept of a running tunnel)
//! and `admin` (parsing, pagination, wire requests and output rendering).
//! `status` is one request and stays here; `logging` sets up the log output
//! every role shares.

use std::error::Error;
use std::time::Duration;

use better_mimalloc_rs::MiMalloc;
use clap::{Args, Parser, Subcommand, ValueEnum};
use pb_mapper_client::client::show_status_scoped;
use pb_mapper_core::config::resolve_pb_mapper_server_async;
use pb_mapper_protocol::command::PbConnStatusReq;

#[path = "pb-mapper/admin.rs"]
mod admin;
#[path = "pb-mapper/logging.rs"]
mod logging;
#[path = "pb-mapper/server.rs"]
mod server;
#[path = "pb-mapper/tunnel.rs"]
mod tunnel;

use admin::AdminArgs;
use server::{ServerArgs, run_server};
use tunnel::{ConnectArgs, RegisterArgs, run_connect, run_register};

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
struct StatusArgs {
    /// Status query to execute.
    #[arg(value_enum)]
    op: StatusOpArg,
    /// Relay address. Falls back to PB_MAPPER_SERVER.
    #[arg(short, long, visible_alias = "pb-mapper-server", value_name = "ADDR")]
    server: Option<String>,
    /// Administrator-only namespace to inspect.
    #[arg(long)]
    namespace: Option<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum StatusOpArg {
    /// Get active remote connection IDs.
    RemoteId,
    /// Get registered service keys.
    Keys,
    /// Get direct-session observations reported by endpoints in this namespace.
    Sessions,
}

impl From<StatusOpArg> for PbConnStatusReq {
    fn from(value: StatusOpArg) -> Self {
        match value {
            StatusOpArg::RemoteId => Self::RemoteId,
            StatusOpArg::Keys => Self::Keys,
            StatusOpArg::Sessions => Self::Sessions,
        }
    }
}

#[tokio::main]
async fn main() {
    MiMalloc::init();
    let cli = Cli::parse();
    logging::init_tracing();

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

async fn run_status(args: StatusArgs) -> Result<(), Box<dyn Error>> {
    let remote_addr = resolve_pb_mapper_server_async(args.server.as_deref()).await?;
    show_status_scoped(remote_addr.as_slice(), args.op.into(), args.namespace).await
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
        for filter in ["--namespace", "--key-id"] {
            Cli::try_parse_from(["pb-mapper", "admin", "service", "list", filter, "7"])
                .expect("a listing should filter by namespace under either name");
        }
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
}
