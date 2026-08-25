use std::net::{AddrParseError, SocketAddr};
use std::sync::{Arc, Once};
use std::time::Duration;

use clap::ValueEnum;
use snafu::ResultExt;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::{EnvFilter, Layer, fmt};

use crate::error::{CfgPbServerEnvNotExistSnafu, Result};

#[derive(ValueEnum, Debug, Clone, Copy)]
pub enum StatusOp {
    /// Get active remote connection IDs.
    RemoteId,
    /// Get registered service keys.
    Keys,
}

/// Every address a name resolved to, in the order the resolver returned them.
///
/// Resolution used to end in `.next()`, which threw away everything after the
/// first candidate. That is enough for an `ip:port` literal, but a hostname with
/// several A/AAAA records — a relay behind round-robin DNS, or a dual-stack host
/// whose first record is an unreachable IPv6 address — resolved to one address
/// and then failed against it, with the working candidates never tried.
///
/// The dial loops below this were always able to try every address: both
/// `each_addr` and the `uni-stream` providers iterate a `ToSocketAddrs` and only
/// report the last error once every candidate has failed. They just never
/// received more than one. So this type is the whole of the fix: carry the list
/// to them, and they do the rest.
///
/// Guaranteed non-empty, so a caller never has to handle a resolution that
/// succeeded with nothing in it. Cheap to clone — the addresses are shared, not
/// copied — because every tunnel worker and every retry needs its own handle.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedAddrs {
    /// Non-empty by construction; see [`ResolvedAddrs::new`].
    addrs: Arc<[SocketAddr]>,
}

impl ResolvedAddrs {
    /// Build from resolver output, rejecting an empty result.
    ///
    /// `name` is what was resolved, and `parse_error` the failure from parsing it
    /// as a literal `ip:port`, so an empty resolution reports the same error a
    /// malformed address would.
    fn new(name: &str, addrs: Vec<SocketAddr>, parse_error: AddrParseError) -> Result<Self> {
        if addrs.is_empty() {
            return Err(crate::error::Error::CfgParseSockAddr {
                string: name.to_string(),
                source: parse_error,
            });
        }
        Ok(Self {
            addrs: Arc::from(addrs),
        })
    }

    /// Build from candidates already resolved elsewhere, rejecting an empty list.
    ///
    /// For callers that resolve through another address trait — the tunnel
    /// internals take a generic `ToSocketAddrs` at their public boundary — and
    /// need the result in this non-empty form.
    #[must_use]
    pub fn from_candidates(addrs: Vec<SocketAddr>) -> Option<Self> {
        if addrs.is_empty() {
            return None;
        }
        Some(Self {
            addrs: Arc::from(addrs),
        })
    }

    /// The candidates, for handing to `each_addr` or a `ToSocketAddrs` bound.
    ///
    /// `&[SocketAddr]` is what both address traits in play accept, and it is
    /// `Copy`, which the `A: ToSocketAddrs + Copy` bounds on the tunnel internals
    /// require. A `Vec` is neither.
    #[inline]
    #[must_use]
    pub fn as_slice(&self) -> &[SocketAddr] {
        &self.addrs
    }

    /// The first candidate.
    ///
    /// For the places that genuinely need one address rather than a list: a
    /// `SocketAddr` field on a status record, a preflight probe, a log line. Not
    /// for dialling — that is what [`Self::as_slice`] is for.
    #[inline]
    #[must_use]
    pub fn primary(&self) -> SocketAddr {
        // Non-empty by construction, so indexing cannot panic. `[0]` rather than
        // `first().unwrap()`: the invariant is the reason, and `unwrap` is denied.
        self.addrs[0]
    }
}

impl std::fmt::Display for ResolvedAddrs {
    /// Renders the primary address, with a count when candidates were dropped.
    ///
    /// Log lines pass these through `%`, and a bare Debug dump of a one-element
    /// list reads worse than the address itself.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.primary())?;
        match self.addrs.len() {
            1 => Ok(()),
            more => write!(formatter, " (+{} more)", more - 1),
        }
    }
}

impl From<SocketAddr> for ResolvedAddrs {
    fn from(addr: SocketAddr) -> Self {
        Self {
            addrs: Arc::from(vec![addr]),
        }
    }
}

/// Resolve `addr` to every candidate it names.
///
/// A literal `ip:port` short-circuits. Otherwise `localhost:` goes to the system
/// resolver — the custom DNS path does not answer for it — and any other hostname
/// tries the configured DNS servers first, falling back to the system resolver.
#[inline]
pub fn resolve_addrs(addr: &str) -> Result<ResolvedAddrs> {
    // The literal case returns here; otherwise the parse error is kept as the
    // failure reported below, so a caller sees what it asked for rather than a
    // resolver's internal complaint.
    let parse_error = match addr.parse::<SocketAddr>() {
        Ok(socket_addr) => return Ok(ResolvedAddrs::from(socket_addr)),
        Err(error) => error,
    };

    let system = || match std::net::ToSocketAddrs::to_socket_addrs(addr) {
        Ok(addrs) => addrs.collect(),
        Err(_) => Vec::new(),
    };

    let addrs = if addr.starts_with("localhost:") {
        system()
    } else {
        match crate::addr::get_socket_addrs(addr) {
            Ok(addrs) if !addrs.is_empty() => addrs,
            _ => system(),
        }
    };
    ResolvedAddrs::new(addr, addrs, parse_error)
}

/// Async counterpart of [`resolve_addrs`], for Tokio contexts.
pub async fn resolve_addrs_async(addr: &str) -> Result<ResolvedAddrs> {
    let parse_error = match addr.parse::<SocketAddr>() {
        Ok(socket_addr) => return Ok(ResolvedAddrs::from(socket_addr)),
        Err(error) => error,
    };

    async fn system(addr: &str) -> Vec<SocketAddr> {
        match tokio::net::lookup_host(addr).await {
            Ok(addrs) => addrs.collect(),
            Err(_) => Vec::new(),
        }
    }

    let addrs = if addr.starts_with("localhost:") {
        system(addr).await
    } else {
        match crate::addr::get_socket_addrs_async(addr).await {
            Ok(addrs) if !addrs.is_empty() => addrs,
            _ => system(addr).await,
        }
    };
    ResolvedAddrs::new(addr, addrs, parse_error)
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
pub const PB_MAPPER_REGISTRATION_REJECT_BACKOFF_MIN: &str =
    "PB_MAPPER_REGISTRATION_REJECT_BACKOFF_MIN";
pub const PB_MAPPER_REGISTRATION_REJECT_BACKOFF_MAX: &str =
    "PB_MAPPER_REGISTRATION_REJECT_BACKOFF_MAX";
pub const PB_MAPPER_SERVER_LEASE_TIMEOUT: &str = "PB_MAPPER_SERVER_LEASE_TIMEOUT";
pub const PB_MAPPER_SERVER_LEASE_SWEEP_INTERVAL: &str = "PB_MAPPER_SERVER_LEASE_SWEEP_INTERVAL";
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
const DEFAULT_REGISTRATION_REJECT_BACKOFF_MIN: Duration = Duration::from_secs(5);
const DEFAULT_REGISTRATION_REJECT_BACKOFF_MAX: Duration = Duration::from_secs(80);
const DEFAULT_SERVER_LEASE_TIMEOUT: Duration = Duration::from_secs(15);
const DEFAULT_SERVER_LEASE_SWEEP_INTERVAL: Duration = Duration::from_secs(5);
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

/// Read a duration that has to be positive, falling back to `default` on a zero.
///
/// Zero is never a usable value for these settings: a period of zero panics
/// `tokio::time::interval`, and a zero minimum trips [`RetryBackoff::new`]'s
/// assertion. Clamping a zero up to a millisecond avoids the panic, but trades it
/// for a hot loop — a rejected registration retried a thousand times a second, or
/// a full registration scan queued into the routing loop every millisecond. The
/// default is the only value that is both safe and useful, so a zero selects it
/// and says so once.
///
/// [`RetryBackoff::new`]: crate::timeout::RetryBackoff::new
fn positive_duration_from_env(name: &str, default: Duration) -> Duration {
    let value = duration_from_env(name, default);
    if value.is_zero() {
        tracing::warn!(
            event = "config_zero_duration_ignored",
            variable = name,
            default = ?default,
            "ignoring a zero duration and using the default instead"
        );
        return default;
    }
    value
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

/// The retry ladder for a registration the relay rejected but marked retryable.
///
/// Separate from the transport ladder, and far slower. A transport failure means
/// the relay could not be reached, so retrying quickly is how the tunnel comes
/// back. A retryable rejection is the opposite: the relay answered, and refused —
/// a per-service connection quota that is full, a namespace at its service limit.
/// Reconnecting a few times a second adds load to the very condition that has to
/// clear, and it produced thousands of identical reject lines a minute in
/// production. So the ladder starts at 5s and settles at 80s.
///
/// Both ends come back together because they are only valid as a pair: a zero
/// falls back to its default, and the maximum is then raised to the minimum,
/// rather than letting an environment typo reach [`RetryBackoff::new`]'s
/// assertions, which would abort the process at start-up.
///
/// Raising the maximum, but defaulting a zero minimum, because the two failures
/// are different. An inverted range is a legible request — wait exactly this long
/// — and collapsing it to a fixed delay honours it. A zero minimum is not: it
/// asks for no wait at all, and since [`RetryBackoff`] caps its multiplier at
/// 1024, a millisecond minimum would never climb past about a second no matter
/// how high the maximum, hammering the very quota the ladder exists to let clear.
///
/// [`RetryBackoff`]: crate::timeout::RetryBackoff
/// [`RetryBackoff::new`]: crate::timeout::RetryBackoff::new
pub fn registration_reject_backoff() -> (Duration, Duration) {
    let min = positive_duration_from_env(
        PB_MAPPER_REGISTRATION_REJECT_BACKOFF_MIN,
        DEFAULT_REGISTRATION_REJECT_BACKOFF_MIN,
    );
    let max = positive_duration_from_env(
        PB_MAPPER_REGISTRATION_REJECT_BACKOFF_MAX,
        DEFAULT_REGISTRATION_REJECT_BACKOFF_MAX,
    )
    .max(min);
    (min, max)
}

pub fn server_lease_timeout() -> Duration {
    duration_from_env(PB_MAPPER_SERVER_LEASE_TIMEOUT, DEFAULT_SERVER_LEASE_TIMEOUT)
}

/// How often the relay looks for registrations that stopped renewing their lease.
///
/// A third of [`server_lease_timeout`] by default, which bounds two things at
/// once: how long an expired registration can keep attracting subscribers, and —
/// because the sweep needs two passes to retire one — how much grace a live
/// connection gets to notice its own idle timeout first.
///
/// A zero falls back to the default: `tokio::time::interval` panics on a zero
/// period, and each tick queues a scan over every registration through the single
/// routing loop, so sweeping as fast as the timer allows would starve the traffic
/// the sweep exists to protect.
pub fn server_lease_sweep_interval() -> Duration {
    positive_duration_from_env(
        PB_MAPPER_SERVER_LEASE_SWEEP_INTERVAL,
        DEFAULT_SERVER_LEASE_SWEEP_INTERVAL,
    )
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

/// The relay address as configured: `addr` if given, else `PB_MAPPER_SERVER`.
///
/// Unresolved on purpose. A caller that hands the address to something which
/// resolves it itself — the SDK client, for one — should pass the name along
/// rather than resolve it here and stringify the result, which both duplicates
/// the lookup and narrows a multi-address name down to one entry on the way.
pub fn pb_mapper_server_addr(addr: Option<&str>) -> Result<String> {
    match addr {
        Some(addr) => Ok(addr.to_string()),
        None => std::env::var(PB_MAPPER_SERVER).context(CfgPbServerEnvNotExistSnafu),
    }
}

/// Every address the relay resolves to: `addr` if given, else `PB_MAPPER_SERVER`.
#[inline]
pub fn resolve_pb_mapper_server(addr: Option<&str>) -> Result<ResolvedAddrs> {
    match addr {
        Some(addr) => resolve_addrs(addr),
        None => {
            let addr = std::env::var(PB_MAPPER_SERVER).context(CfgPbServerEnvNotExistSnafu)?;
            resolve_addrs(&addr)
        }
    }
}

/// Async counterpart of [`resolve_pb_mapper_server`], for Tokio contexts.
pub async fn resolve_pb_mapper_server_async(addr: Option<&str>) -> Result<ResolvedAddrs> {
    match addr {
        Some(addr) => resolve_addrs_async(addr).await,
        None => {
            let addr = std::env::var(PB_MAPPER_SERVER).context(CfgPbServerEnvNotExistSnafu)?;
            resolve_addrs_async(&addr).await
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

        // SAFETY: mutating the environment is unsafe in edition 2024 because
        // it is process-global. This is the only test that touches
        // `PB_MAPPER_KEEP_ALIVE` — which is why it is one test and not several
        // — and it restores the original value before returning.
        unsafe {
            std::env::remove_var(PB_MAPPER_KEEP_ALIVE);
        }
        assert!(!keep_alive_from_env(), "absent means off");

        unsafe {
            std::env::set_var(PB_MAPPER_KEEP_ALIVE, "ON");
        }
        assert!(keep_alive_from_env(), "the documented spelling");

        // The regression. This used to be a `LazyLock<bool>`, so the answer was
        // whatever the first caller in the process saw and could never change —
        // which is why the UI's per-service toggle did nothing after the first
        // tunnel started.
        unsafe {
            std::env::set_var(PB_MAPPER_KEEP_ALIVE, "OFF");
        }
        assert!(
            !keep_alive_from_env(),
            "OFF must mean off; the old check was `is_ok()`, so any value at \
             all — OFF included — turned keep-alive on"
        );

        for truthy in ["on", "1", "true", "yes", " ON "] {
            unsafe {
                std::env::set_var(PB_MAPPER_KEEP_ALIVE, truthy);
            }
            assert!(keep_alive_from_env(), "{truthy:?} should enable");
        }
        for falsy in ["", "off", "0", "false", "no"] {
            unsafe {
                std::env::set_var(PB_MAPPER_KEEP_ALIVE, falsy);
            }
            assert!(!keep_alive_from_env(), "{falsy:?} should not enable");
        }

        unsafe {
            match restore {
                Some(value) => std::env::set_var(PB_MAPPER_KEEP_ALIVE, value),
                None => std::env::remove_var(PB_MAPPER_KEEP_ALIVE),
            }
        }
    }

    /// The point of [`ResolvedAddrs`]: a name with several records keeps them
    /// all, so the dial loops can try each one. Collapsing to the first is what
    /// made a multi-record relay unreachable whenever its first address was.
    #[test]
    fn resolved_addrs_keeps_every_candidate() {
        let first: SocketAddr = "127.0.0.1:7666".parse().expect("literal");
        let second: SocketAddr = "[::1]:7666".parse().expect("literal");
        let addrs = ResolvedAddrs::from_candidates(vec![first, second]).expect("non-empty");

        assert_eq!(addrs.as_slice(), [first, second]);
        assert_eq!(addrs.primary(), first, "order is the resolver's order");
    }

    /// An empty candidate list is not a resolution: nothing could be dialled, so
    /// it has to be rejected here rather than surface as a connect failure
    /// against an address the caller never gave.
    #[test]
    fn resolved_addrs_rejects_an_empty_candidate_list() {
        assert!(ResolvedAddrs::from_candidates(Vec::new()).is_none());
    }

    /// The rendering has to name one address — it goes into connect errors and
    /// log fields — while still admitting that others were available.
    #[test]
    fn resolved_addrs_renders_the_primary_and_the_rest_as_a_count() {
        let single: SocketAddr = "127.0.0.1:7666".parse().expect("literal");
        assert_eq!(ResolvedAddrs::from(single).to_string(), "127.0.0.1:7666");

        let second: SocketAddr = "[::1]:7666".parse().expect("literal");
        let both = ResolvedAddrs::from_candidates(vec![single, second]).expect("non-empty");
        assert_eq!(both.to_string(), "127.0.0.1:7666 (+1 more)");
    }

    /// A literal address needs no resolver, and must survive verbatim.
    #[test]
    fn resolve_addrs_passes_a_literal_through() {
        let addrs = resolve_addrs("127.0.0.1:7666").expect("a literal always resolves");
        assert_eq!(
            addrs.as_slice(),
            ["127.0.0.1:7666".parse().expect("literal")]
        );
    }

    /// `localhost` is the everyday multi-record name: it resolves to a v4 and a
    /// v6 loopback on most hosts, and both must reach the dial loop.
    #[test]
    fn resolve_addrs_keeps_every_localhost_record() {
        let addrs = resolve_addrs("localhost:7666").expect("localhost always resolves");
        assert!(
            addrs.as_slice().iter().all(|addr| addr.ip().is_loopback()),
            "localhost must resolve to loopback only, got {:?}",
            addrs.as_slice()
        );
    }

    /// A name that resolves to nothing is an error, not an empty success.
    ///
    /// Tested with a missing port rather than an unresolvable host: a resolver
    /// that answers every query with a wildcard address — WSL's NAT DNS, among
    /// others — makes "this host does not exist" untestable, while no resolver
    /// invents a port.
    #[test]
    fn resolve_addrs_fails_when_nothing_can_be_resolved() {
        assert!(resolve_addrs("127.0.0.1").is_err(), "no port");
        assert!(resolve_addrs("localhost").is_err(), "no port");
    }

    /// One test for all three variables, for the same reason as
    /// `keep_alive_reads_the_environment_every_time`: the environment is
    /// process-global and the runner threads these.
    #[test]
    fn reject_backoff_and_sweep_interval_survive_a_bad_environment() {
        let names = [
            PB_MAPPER_REGISTRATION_REJECT_BACKOFF_MIN,
            PB_MAPPER_REGISTRATION_REJECT_BACKOFF_MAX,
            PB_MAPPER_SERVER_LEASE_SWEEP_INTERVAL,
        ];
        let restore = names.map(|name| (name, std::env::var(name).ok()));

        // SAFETY: mutating the environment is unsafe in edition 2024 because it
        // is process-global. This is the only test that touches these three
        // names — which is why it is one test and not three — and it restores
        // their original values before returning.
        unsafe {
            for name in names {
                std::env::remove_var(name);
            }
        }
        assert_eq!(
            registration_reject_backoff(),
            (
                DEFAULT_REGISTRATION_REJECT_BACKOFF_MIN,
                DEFAULT_REGISTRATION_REJECT_BACKOFF_MAX
            ),
            "absent means the defaults"
        );
        assert_eq!(
            server_lease_sweep_interval(),
            DEFAULT_SERVER_LEASE_SWEEP_INTERVAL
        );

        unsafe {
            std::env::set_var(PB_MAPPER_REGISTRATION_REJECT_BACKOFF_MIN, "30s");
            std::env::set_var(PB_MAPPER_REGISTRATION_REJECT_BACKOFF_MAX, "2m");
        }
        assert_eq!(
            registration_reject_backoff(),
            (Duration::from_secs(30), Duration::from_secs(120)),
            "both ends come from the environment"
        );

        // A zero is not a usable setting at either end: `RetryBackoff::new`
        // asserts on a zero minimum, and a millisecond minimum would retry a
        // thousand times a second. Both fall back to their defaults instead.
        unsafe {
            std::env::set_var(PB_MAPPER_REGISTRATION_REJECT_BACKOFF_MIN, "0s");
            std::env::set_var(PB_MAPPER_REGISTRATION_REJECT_BACKOFF_MAX, "0s");
        }
        assert_eq!(
            registration_reject_backoff(),
            (
                DEFAULT_REGISTRATION_REJECT_BACKOFF_MIN,
                DEFAULT_REGISTRATION_REJECT_BACKOFF_MAX
            ),
            "a zero at either end selects the default, not a millisecond"
        );

        // A zero minimum with a usable maximum still defaults the minimum, and
        // the maximum given is kept.
        unsafe {
            std::env::set_var(PB_MAPPER_REGISTRATION_REJECT_BACKOFF_MIN, "0ms");
            std::env::set_var(PB_MAPPER_REGISTRATION_REJECT_BACKOFF_MAX, "3m");
        }
        assert_eq!(
            registration_reject_backoff(),
            (
                DEFAULT_REGISTRATION_REJECT_BACKOFF_MIN,
                Duration::from_secs(180)
            )
        );

        unsafe {
            std::env::set_var(PB_MAPPER_REGISTRATION_REJECT_BACKOFF_MIN, "1m");
            std::env::set_var(PB_MAPPER_REGISTRATION_REJECT_BACKOFF_MAX, "1s");
        }
        let (min, max) = registration_reject_backoff();
        assert_eq!(min, Duration::from_secs(60));
        assert_eq!(max, min, "an inverted range collapses to a fixed delay");

        unsafe {
            std::env::set_var(PB_MAPPER_SERVER_LEASE_SWEEP_INTERVAL, "0s");
        }
        assert_eq!(
            server_lease_sweep_interval(),
            DEFAULT_SERVER_LEASE_SWEEP_INTERVAL,
            "a zero period would panic `tokio::time::interval`, and a millisecond \
             one would queue a full scan every millisecond"
        );

        unsafe {
            for (name, value) in restore {
                match value {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
        }
    }

    /// The async path is the one the tunnels use, and has to agree with the
    /// blocking one on a literal.
    #[tokio::test]
    async fn resolve_addrs_async_matches_the_blocking_path_on_a_literal() {
        let expected = resolve_addrs("127.0.0.1:7666").expect("literal");
        let actual = resolve_addrs_async("127.0.0.1:7666")
            .await
            .expect("literal");
        assert_eq!(actual, expected);
    }
}
