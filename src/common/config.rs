use std::net::SocketAddr;
use std::sync::LazyLock;

use clap::{Subcommand, ValueEnum};
use snafu::ResultExt;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::{fmt, Layer};

use super::error::{CfgParseSockAddrSnafu, CfgPbServerEnvNotExistSnafu, Result};

#[derive(Debug, Subcommand)]
pub enum LocalService {
    /// UDP server
    UdpServer {
        /// [required] The key registered with the remote server to represent this service
        #[arg(short, long)]
        key: String,
        /// [required] addr(ip:port) for exposed
        #[arg(short, long)]
        addr: String,
    },
    /// TCP server
    TcpServer {
        /// [required] The key registered with the remote server to represent this service
        #[arg(short, long)]
        key: String,
        /// [required] addr(ip:port) for exposed
        #[arg(short, long)]
        addr: String,
    },
    /// Show Status
    Status {
        /// status operate
        op: StatusOp,
    },
}

#[derive(ValueEnum, Debug, Clone, Copy)]
pub enum StatusOp {
    /// get active remote id
    RemoteId,
    // get active service key
    Keys,
}

#[inline]
pub fn get_sockaddr(addr: &str) -> Result<SocketAddr> {
    addr.parse().with_context(|_| CfgParseSockAddrSnafu {
        string: addr.to_string(),
    })
}

const PB_MAPPER_SERVER: &str = "PB_MAPPER_SERVER";

/// Env to control whether the keep-alive option of TCP is enabled
pub const PB_MAPPER_KEEP_ALIVE: &str = "PB_MAPPER_KEEP_ALIVE";

/// Controls whether the keepalive option for TCP is enabled, depending on the value of the
/// environment variable `PB_MAPPER_KEEP_ALIVE`
pub static IS_KEEPALIVE: LazyLock<bool> = LazyLock::new(|| {
    if std::env::var(PB_MAPPER_KEEP_ALIVE).is_ok() {
        tracing::info!(
            "TCP keep-alive is already on, due to the setting of the env:`{PB_MAPPER_KEEP_ALIVE}` "
        );
        true
    } else {
        tracing::info!("By default TCP keep-alive is off");
        false
    }
});

#[inline]
pub fn get_pb_mapper_server(addr: Option<&str>) -> Result<SocketAddr> {
    match addr {
        Some(addr) => get_sockaddr(addr),
        None => {
            let addr = std::env::var(PB_MAPPER_SERVER).context(CfgPbServerEnvNotExistSnafu)?;
            get_sockaddr(&addr)
        }
    }
}

pub fn init_tracing() {
    let subcriber = tracing_subscriber::registry().with(
        fmt::layer()
            .pretty()
            .with_writer(std::io::stdout)
            .with_filter(
                tracing_subscriber::EnvFilter::builder()
                    .with_default_directive(tracing::level_filters::LevelFilter::INFO.into())
                    .from_env_lossy(),
            ),
    );
    tracing::subscriber::set_global_default(subcriber).expect("setting tracing default failed");
}
