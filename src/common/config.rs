use std::net::SocketAddr;
use std::sync::Once;
use std::time::Duration;

use clap::{Subcommand, ValueEnum};
use snafu::ResultExt;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::{fmt, EnvFilter, Layer};

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

/// Async socket address resolution for Tokio contexts.
/// Uses custom DNS servers first and falls back to the system resolver.
pub async fn get_sockaddr_async(addr: &str) -> Result<SocketAddr> {
    // First try direct parsing for IP addresses like "127.0.0.1:8080"
    match addr.parse::<SocketAddr>() {
        Ok(socket_addr) => Ok(socket_addr),
        Err(original_parse_error) => {
            // Check if it's localhost - use system resolver for localhost
            if addr.starts_with("localhost:") {
                match tokio::net::lookup_host(addr).await {
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
                use crate::utils::addr::get_socket_addrs_async;
                match get_socket_addrs_async(addr).await {
                    Ok(socket_addrs) => socket_addrs.into_iter().next().ok_or_else(|| {
                        super::error::Error::CfgParseSockAddr {
                            string: addr.to_string(),
                            source: original_parse_error,
                        }
                    }),
                    Err(_) => {
                        // If custom DNS resolution fails, fallback to system resolver
                        match tokio::net::lookup_host(addr).await {
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
pub const PB_MAPPER_CONTROL_IO_TIMEOUT: &str = "PB_MAPPER_CONTROL_IO_TIMEOUT";
pub const PB_MAPPER_STREAM_ACK_TIMEOUT: &str = "PB_MAPPER_STREAM_ACK_TIMEOUT";
pub const PB_MAPPER_STREAM_READY_TIMEOUT: &str = "PB_MAPPER_STREAM_READY_TIMEOUT";
pub const PB_MAPPER_STREAM_RECOVERY_TIMEOUT: &str = "PB_MAPPER_STREAM_RECOVERY_TIMEOUT";
pub const PB_MAPPER_CONTROL_CONN_POOL_SIZE: &str = "PB_MAPPER_CONTROL_CONN_POOL_SIZE";
pub const PB_MAPPER_CONTROL_HEARTBEAT_INTERVAL: &str = "PB_MAPPER_CONTROL_HEARTBEAT_INTERVAL";
pub const PB_MAPPER_CONTROL_HEARTBEAT_TOLERANCE: &str = "PB_MAPPER_CONTROL_HEARTBEAT_TOLERANCE";
pub const PB_MAPPER_CONTROL_SUSPECT_GRACE: &str = "PB_MAPPER_CONTROL_SUSPECT_GRACE";
pub const PB_MAPPER_REGISTRATION_PROBE_TIMEOUT: &str = "PB_MAPPER_REGISTRATION_PROBE_TIMEOUT";
pub const PB_MAPPER_SERVER_LEASE_TIMEOUT: &str = "PB_MAPPER_SERVER_LEASE_TIMEOUT";
pub const PB_MAPPER_CLIENT_HEALTH_CHECK_INTERVAL: &str = "PB_MAPPER_CLIENT_HEALTH_CHECK_INTERVAL";
pub const PB_MAPPER_CLIENT_HEALTH_CHECK_TIMEOUT: &str = "PB_MAPPER_CLIENT_HEALTH_CHECK_TIMEOUT";
pub const PB_MAPPER_CLIENT_HEALTH_FAILURE_THRESHOLD: &str =
    "PB_MAPPER_CLIENT_HEALTH_FAILURE_THRESHOLD";
pub const PB_MAPPER_LOG_FORMAT: &str = "PB_MAPPER_LOG_FORMAT";
const DEFAULT_CONTROL_IO_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_STREAM_ACK_TIMEOUT: Duration = Duration::from_millis(300);
const DEFAULT_STREAM_READY_TIMEOUT: Duration = Duration::from_secs(1);
const DEFAULT_STREAM_RECOVERY_TIMEOUT: Duration = Duration::from_secs(2);
const DEFAULT_CONTROL_CONN_POOL_SIZE: usize = 2;
const DEFAULT_CONTROL_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(2);
const DEFAULT_CONTROL_HEARTBEAT_TOLERANCE: Duration = Duration::from_secs(6);
const DEFAULT_CONTROL_SUSPECT_GRACE: Duration = Duration::from_secs(2);
const DEFAULT_REGISTRATION_PROBE_TIMEOUT: Duration = Duration::from_secs(1);
const DEFAULT_SERVER_LEASE_TIMEOUT: Duration = Duration::from_secs(15);
const DEFAULT_CLIENT_HEALTH_CHECK_INTERVAL: Duration = Duration::from_secs(15);
const DEFAULT_CLIENT_HEALTH_CHECK_TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_CLIENT_HEALTH_FAILURE_THRESHOLD: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    Pretty,
    Compact,
    Json,
}

pub fn parse_log_format(value: &str) -> LogFormat {
    match value.trim().to_ascii_lowercase().as_str() {
        "compact" => LogFormat::Compact,
        "json" => LogFormat::Json,
        _ => LogFormat::Pretty,
    }
}

fn log_format_from_env() -> LogFormat {
    std::env::var(PB_MAPPER_LOG_FORMAT)
        .ok()
        .map(|value| parse_log_format(&value))
        .unwrap_or(LogFormat::Pretty)
}

fn default_env_filter() -> EnvFilter {
    EnvFilter::builder()
        .with_default_directive(tracing::level_filters::LevelFilter::INFO.into())
        .from_env_lossy()
}

pub fn parse_duration(value: &str) -> Option<Duration> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if let Some(raw) = value.strip_suffix("ms") {
        return raw.trim().parse::<u64>().ok().map(Duration::from_millis);
    }
    if let Some(raw) = value.strip_suffix('s') {
        return raw.trim().parse::<u64>().ok().map(Duration::from_secs);
    }
    if let Some(raw) = value.strip_suffix('m') {
        return raw
            .trim()
            .parse::<u64>()
            .ok()
            .and_then(|minutes| minutes.checked_mul(60))
            .map(Duration::from_secs);
    }
    if let Some(raw) = value.strip_suffix('h') {
        return raw
            .trim()
            .parse::<u64>()
            .ok()
            .and_then(|hours| hours.checked_mul(60 * 60))
            .map(Duration::from_secs);
    }
    value.parse::<u64>().ok().map(Duration::from_secs)
}

pub fn duration_from_env(name: &str, default: Duration) -> Duration {
    std::env::var(name)
        .ok()
        .and_then(|value| parse_duration(&value))
        .unwrap_or(default)
}

pub fn control_io_timeout() -> Duration {
    duration_from_env(PB_MAPPER_CONTROL_IO_TIMEOUT, DEFAULT_CONTROL_IO_TIMEOUT)
}

pub fn stream_ack_timeout() -> Duration {
    duration_from_env(PB_MAPPER_STREAM_ACK_TIMEOUT, DEFAULT_STREAM_ACK_TIMEOUT)
}

pub fn stream_ready_timeout() -> Duration {
    duration_from_env(PB_MAPPER_STREAM_READY_TIMEOUT, DEFAULT_STREAM_READY_TIMEOUT)
}

pub fn stream_recovery_timeout() -> Duration {
    duration_from_env(
        PB_MAPPER_STREAM_RECOVERY_TIMEOUT,
        DEFAULT_STREAM_RECOVERY_TIMEOUT,
    )
}

pub fn control_conn_pool_size() -> usize {
    std::env::var(PB_MAPPER_CONTROL_CONN_POOL_SIZE)
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .filter(|size| *size > 0)
        .map(|size| size.min(16))
        .unwrap_or(DEFAULT_CONTROL_CONN_POOL_SIZE)
}

pub fn control_heartbeat_interval() -> Duration {
    duration_from_env(
        PB_MAPPER_CONTROL_HEARTBEAT_INTERVAL,
        DEFAULT_CONTROL_HEARTBEAT_INTERVAL,
    )
}

pub fn control_heartbeat_tolerance() -> Duration {
    duration_from_env(
        PB_MAPPER_CONTROL_HEARTBEAT_TOLERANCE,
        DEFAULT_CONTROL_HEARTBEAT_TOLERANCE,
    )
}

pub fn control_suspect_grace() -> Duration {
    duration_from_env(
        PB_MAPPER_CONTROL_SUSPECT_GRACE,
        DEFAULT_CONTROL_SUSPECT_GRACE,
    )
}

pub fn registration_probe_timeout() -> Duration {
    duration_from_env(
        PB_MAPPER_REGISTRATION_PROBE_TIMEOUT,
        DEFAULT_REGISTRATION_PROBE_TIMEOUT,
    )
}

pub fn server_lease_timeout() -> Duration {
    duration_from_env(PB_MAPPER_SERVER_LEASE_TIMEOUT, DEFAULT_SERVER_LEASE_TIMEOUT)
}

pub fn client_health_check_interval() -> Duration {
    duration_from_env(
        PB_MAPPER_CLIENT_HEALTH_CHECK_INTERVAL,
        DEFAULT_CLIENT_HEALTH_CHECK_INTERVAL,
    )
}

pub fn client_health_check_timeout() -> Duration {
    duration_from_env(
        PB_MAPPER_CLIENT_HEALTH_CHECK_TIMEOUT,
        DEFAULT_CLIENT_HEALTH_CHECK_TIMEOUT,
    )
}

pub fn client_health_failure_threshold() -> usize {
    std::env::var(PB_MAPPER_CLIENT_HEALTH_FAILURE_THRESHOLD)
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .filter(|threshold| *threshold > 0)
        .map(|threshold| threshold.min(100))
        .unwrap_or(DEFAULT_CLIENT_HEALTH_FAILURE_THRESHOLD)
}

/// Whether the environment asks for TCP keep-alive, from `PB_MAPPER_KEEP_ALIVE`.
///
/// A default for a process to read once at startup — not something the tunnels
/// consult. Keep-alive is a per-tunnel parameter because one process can run
/// many tunnels that disagree about it, which is why this is a function and not
/// the `LazyLock<bool>` it used to be: that froze on the first tunnel to touch
/// a socket and left every later one, including the UI's own toggle, unable to
/// change it.
pub fn keep_alive_from_env() -> bool {
    match std::env::var(PB_MAPPER_KEEP_ALIVE) {
        // The documented spelling is `ON`; the previous check was `is_ok()`,
        // which turned keep-alive on for any value at all — `OFF` included.
        Ok(value) => matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "on" | "1" | "true" | "yes"
        ),
        Err(_) => false,
    }
}

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

/// Async version of `get_pb_mapper_server` for Tokio contexts.
pub async fn get_pb_mapper_server_async(addr: Option<&str>) -> Result<SocketAddr> {
    match addr {
        Some(addr) => get_sockaddr_async(addr).await,
        None => {
            let addr = std::env::var(PB_MAPPER_SERVER).context(CfgPbServerEnvNotExistSnafu)?;
            get_sockaddr_async(&addr).await
        }
    }
}

pub fn init_tracing() {
    static INIT_TRACING: Once = Once::new();
    INIT_TRACING.call_once(|| {
        let result = match log_format_from_env() {
            LogFormat::Pretty => {
                let subscriber = tracing_subscriber::registry().with(
                    fmt::layer()
                        .pretty()
                        .with_writer(std::io::stdout)
                        .with_filter(default_env_filter()),
                );
                tracing::subscriber::set_global_default(subscriber)
            }
            LogFormat::Compact => {
                let subscriber = tracing_subscriber::registry().with(
                    fmt::layer()
                        .compact()
                        .with_writer(std::io::stdout)
                        .with_filter(default_env_filter()),
                );
                tracing::subscriber::set_global_default(subscriber)
            }
            LogFormat::Json => {
                let subscriber = tracing_subscriber::registry().with(
                    fmt::layer()
                        .json()
                        .flatten_event(true)
                        .with_writer(std::io::stdout)
                        .with_filter(default_env_filter()),
                );
                tracing::subscriber::set_global_default(subscriber)
            }
        };

        if let Err(e) = result {
            eprintln!("failed to initialize tracing subscriber: {e}");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_log_format_accepts_supported_values() {
        assert_eq!(parse_log_format("pretty"), LogFormat::Pretty);
        assert_eq!(parse_log_format("compact"), LogFormat::Compact);
        assert_eq!(parse_log_format("json"), LogFormat::Json);
        assert_eq!(parse_log_format(" JSON "), LogFormat::Json);
        assert_eq!(parse_log_format("unknown"), LogFormat::Pretty);
    }

    /// One test rather than several, because these share process-wide state and
    /// the test runner threads them.
    #[test]
    fn keep_alive_reads_the_environment_every_time() {
        let restore = std::env::var(PB_MAPPER_KEEP_ALIVE).ok();

        std::env::remove_var(PB_MAPPER_KEEP_ALIVE);
        assert!(!keep_alive_from_env(), "absent means off");

        std::env::set_var(PB_MAPPER_KEEP_ALIVE, "ON");
        assert!(keep_alive_from_env(), "the documented spelling");

        // The regression. This used to be a `LazyLock<bool>`, so the answer was
        // whatever the first caller in the process saw and could never change —
        // which is why the UI's per-service toggle did nothing after the first
        // tunnel started.
        std::env::set_var(PB_MAPPER_KEEP_ALIVE, "OFF");
        assert!(
            !keep_alive_from_env(),
            "OFF must mean off; the old check was `is_ok()`, so any value at \
             all — OFF included — turned keep-alive on"
        );

        for truthy in ["on", "1", "true", "yes", " ON "] {
            std::env::set_var(PB_MAPPER_KEEP_ALIVE, truthy);
            assert!(keep_alive_from_env(), "{truthy:?} should enable");
        }
        for falsy in ["", "off", "0", "false", "no"] {
            std::env::set_var(PB_MAPPER_KEEP_ALIVE, falsy);
            assert!(!keep_alive_from_env(), "{falsy:?} should not enable");
        }

        match restore {
            Some(value) => std::env::set_var(PB_MAPPER_KEEP_ALIVE, value),
            None => std::env::remove_var(PB_MAPPER_KEEP_ALIVE),
        }
    }
}
