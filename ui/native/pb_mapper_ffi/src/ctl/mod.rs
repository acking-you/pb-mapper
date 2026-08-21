//! The control channel: one command vocabulary, three consumers.
//!
//! [`Command`] is at once the clap subcommand tree, the wire format, and the
//! input to [`dispatch`]. A new subcommand therefore extends the protocol by
//! construction — there is no second place to add it and no marshalling layer
//! to keep in step, which is what keeps the attached and headless paths from
//! drifting apart.

pub mod endpoint;
pub mod proto;
pub mod server;

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::Mutex;

use crate::error::CtlError;
use crate::events::ChangeKind;
use crate::state::{self, PbMapperState};

/// Who asked. Not part of the wire format: the control server stamps `Cli` on
/// whatever arrives over the socket, the FFI stamps `Ui`, and the background
/// status refreshers stamp `Internal`. `PbMapperState` never has to know.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Origin {
    Ui,
    Cli,
    Internal,
}

#[derive(Debug, Clone, Serialize, Deserialize, clap::Args)]
#[serde(rename_all = "camelCase")]
pub struct RegisterArgs {
    /// The name clients will subscribe to.
    #[arg(long)]
    pub key: String,
    /// The local service to expose, as `ip:port`.
    #[arg(long)]
    pub addr: String,
    /// UDP instead of TCP.
    #[arg(long, default_value_t = false)]
    pub udp: bool,
    /// Forward without the encryption codec.
    #[arg(long, default_value_t = false)]
    pub no_encrypt: bool,
    #[arg(long, default_value_t = false)]
    pub keep_alive: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, clap::Args)]
#[serde(rename_all = "camelCase")]
pub struct ConnectArgs {
    /// The registered service to subscribe to.
    #[arg(long)]
    pub key: String,
    /// The local address to listen on, as `ip:port`.
    #[arg(long)]
    pub addr: String,
    /// UDP instead of TCP.
    #[arg(long, default_value_t = false)]
    pub udp: bool,
    #[arg(long, default_value_t = false)]
    pub keep_alive: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, clap::Subcommand)]
#[serde(tag = "cmd", rename_all = "kebab-case")]
pub enum Command {
    /// Protocol and version handshake.
    Hello,
    /// What the pb-mapper server knows.
    Status,
    /// Locally configured services and whether they are running.
    Services,
    /// Live connections the server holds for one service.
    Connections {
        #[arg(long)]
        key: String,
    },
    /// Locally configured client connections.
    Clients,
    /// Expose a local service through the pb-mapper server.
    Register(RegisterArgs),
    /// Stop a registered service.
    Unregister {
        #[arg(long)]
        key: String,
    },
    /// Open a local port that forwards to a registered service.
    Connect(ConnectArgs),
    /// Close a client connection.
    Disconnect {
        #[arg(long)]
        key: String,
    },
    /// Show the stored settings.
    ConfigGet,
}

impl Command {
    /// Whether this changes anything. Mutating commands default to attached and
    /// refuse to fall back, because the two modes differ in what happens to the
    /// tunnel afterwards; queries fall back freely, because either way the
    /// answer describes the same pb-mapper server.
    pub fn is_mutating(&self) -> bool {
        self.affects().is_some()
    }

    /// Which list this invalidates, and for which key. `None` for a query.
    ///
    /// Derived from the same enum the protocol uses, so a new mutating command
    /// cannot be added without deciding what it invalidates.
    pub fn affects(&self) -> Option<(ChangeKind, Option<String>)> {
        match self {
            Command::Register(args) => Some((ChangeKind::Services, Some(args.key.clone()))),
            Command::Unregister { key } => Some((ChangeKind::Services, Some(key.clone()))),
            Command::Connect(args) => Some((ChangeKind::Clients, Some(args.key.clone()))),
            Command::Disconnect { key } => Some((ChangeKind::Clients, Some(key.clone()))),
            Command::Hello
            | Command::Status
            | Command::Services
            | Command::Clients
            | Command::Connections { .. }
            | Command::ConfigGet => None,
        }
    }
}

fn protocol(udp: bool) -> String {
    if udp { "UDP" } else { "TCP" }.to_string()
}

/// Run one command against the state, whoever asked.
///
/// The control server and the headless path both come through here, which is
/// what makes "the same command means the same thing in both modes" a property
/// of the code rather than a promise in a document.
pub async fn dispatch(
    state: &Arc<Mutex<PbMapperState>>,
    command: Command,
    origin: Origin,
) -> proto::Response {
    let affected = command.affects();
    match run(state, command, origin).await {
        Ok(response) => {
            // Announce only what succeeded, and only from here — the boundary
            // is where the origin is known, so `PbMapperState` never has to
            // learn who called it.
            if let Some((kind, key)) = affected {
                crate::events::emit(kind, key.as_deref(), origin);
            }
            response
        }
        Err(error) => proto::Response::err(&error),
    }
}

async fn run(
    state: &Arc<Mutex<PbMapperState>>,
    command: Command,
    _origin: Origin,
) -> Result<proto::Response, CtlError> {
    match command {
        Command::Hello => Ok(proto::Response::ok(
            Some(json!({
                "protocolVersion": proto::PROTOCOL_VERSION,
                "appVersion": env!("CARGO_PKG_VERSION"),
                "pid": std::process::id(),
            })),
            None,
        )),

        Command::Status => {
            let detail = state.lock().await.get_server_status_detail().await?;
            Ok(proto::Response::ok(
                Some(serde_json::to_value(detail).unwrap_or_else(|_| json!({}))),
                None,
            ))
        }

        Command::Services => {
            let services = state.lock().await.get_service_configs().await;
            Ok(proto::Response::ok(
                Some(json!({ "services": services })),
                None,
            ))
        }

        Command::Clients => {
            let clients = state.lock().await.get_client_configs().await;
            Ok(proto::Response::ok(
                Some(json!({ "clients": clients })),
                None,
            ))
        }

        Command::Connections { key } => {
            let conns = state.lock().await.get_service_conns(key).await?;
            Ok(proto::Response::ok(
                Some(json!({ "connections": conns })),
                None,
            ))
        }

        Command::ConfigGet => {
            let guard = state.lock().await;
            let config = guard.get_config_status().await;
            let isolated_admin_key = guard.isolated_admin_key();
            Ok(proto::Response::ok(
                Some(json!({
                    "serverAddress": config.server_address,
                    "keepAliveEnabled": config.keep_alive_enabled,
                    "msgHeaderKeySet": !config.msg_header_key.is_empty(),
                    "isolatedRelayAdminKeySet": isolated_admin_key.is_some(),
                })),
                None,
            ))
        }

        Command::Register(args) => {
            state::register_service(
                state,
                args.key.clone(),
                args.addr,
                protocol(args.udp),
                !args.no_encrypt,
                args.keep_alive,
            )
            .await?;
            Ok(proto::Response::ok(
                None,
                Some(format!("registered '{}'", args.key)),
            ))
        }

        Command::Unregister { key } => {
            state.lock().await.unregister_service(key.clone()).await?;
            Ok(proto::Response::ok(None, Some(format!("stopped '{key}'"))))
        }

        Command::Connect(args) => {
            state::connect_service(
                state,
                args.key.clone(),
                args.addr,
                protocol(args.udp),
                args.keep_alive,
            )
            .await?;
            Ok(proto::Response::ok(
                None,
                Some(format!("connected to '{}'", args.key)),
            ))
        }

        Command::Disconnect { key } => {
            state.lock().await.disconnect_service(key.clone()).await?;
            Ok(proto::Response::ok(
                None,
                Some(format!("disconnected '{key}'")),
            ))
        }
    }
}
