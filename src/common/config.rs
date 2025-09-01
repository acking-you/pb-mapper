use std::net::SocketAddr;
use std::sync::LazyLock;

use clap::{Subcommand, ValueEnum};
use snafu::ResultExt;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::{fmt, Layer};

use super::error::{CfgPbServerEnvNotExistSnafu, Result};

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
    // First try direct parsing for IP addresses like "127.0.0.1:8080"
    match addr.parse::<SocketAddr>() {
        Ok(socket_addr) => Ok(socket_addr),
        Err(original_parse_error) => {
            // Check if it's localhost - use system resolver for localhost
            if addr.starts_with("localhost:") {
                // Use standard library for localhost resolution to avoid custom DNS resolver issues
                match std::net::ToSocketAddrs::to_socket_addrs(addr) {
                    Ok(mut socket_addrs) => {
                        socket_addrs
                            .next()
                            .ok_or_else(|| super::error::Error::CfgParseSockAddr {
                                string: addr.to_string(),
                                source: original_parse_error,
                            })
                    }
                    Err(_) => Err(super::error::Error::CfgParseSockAddr {
                        string: addr.to_string(),
                        source: original_parse_error,
                    }),
                }
            } else {
                // For other hostnames, use the custom DNS resolution
                use crate::utils::addr::get_socket_addrs;
                match get_socket_addrs(addr) {
                    Ok(socket_addrs) => {
                        // Return the first resolved address
                        socket_addrs.into_iter().next().ok_or_else(|| {
                            super::error::Error::CfgParseSockAddr {
                                string: addr.to_string(),
                                source: original_parse_error,
                            }
                        })
                    }
                    Err(_) => {
                        // If custom DNS resolution fails, fallback to system resolver
                        match std::net::ToSocketAddrs::to_socket_addrs(addr) {
                            Ok(mut socket_addrs) => socket_addrs.next().ok_or_else(|| {
                                super::error::Error::CfgParseSockAddr {
                                    string: addr.to_string(),
                                    source: original_parse_error,
                                }
                            }),
                            Err(_) => Err(super::error::Error::CfgParseSockAddr {
                                string: addr.to_string(),
                                source: original_parse_error,
                            }),
                        }
                    }
                }
            }
        }
    }
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
