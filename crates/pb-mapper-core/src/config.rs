use std::net::SocketAddr;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Duration;

use snafu::ResultExt;

use crate::error::{CfgPbServerEnvNotExistSnafu, CfgResolveSockAddrSnafu, Result};

#[derive(Debug, Clone, Copy)]
pub enum StatusOp {
    /// Get active remote connection IDs.
    RemoteId,
    /// Get registered service keys.
    Keys,
    /// Get direct-session observations reported by endpoints in this namespace.
    Sessions,
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
    /// Build from the outcome of looking `name` up, rejecting an empty result.
    /// A failure keeps the resolver's reason.
    fn looked_up(name: &str, lookup: std::io::Result<Vec<SocketAddr>>) -> Result<Self> {
        let addrs = lookup.context(CfgResolveSockAddrSnafu { string: name })?;
        Self::from_candidates(addrs).ok_or_else(|| crate::error::Error::CfgResolveSockAddr {
            string: name.to_string(),
            source: std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "the resolver answered with no address",
            ),
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

/// What an address to resolve names.
enum Target<'a> {
    /// A literal `ip:port`, which needs no resolver.
    Literal(SocketAddr),
    /// A host and port to look up.
    Name(&'a str, u16),
}

impl<'a> Target<'a> {
    /// Splits `addr` as the system resolver does, so that whatever it would
    /// look up is looked up. Anything else is a malformed address, which fails
    /// with its parse error.
    fn of(addr: &'a str) -> Result<Self> {
        let parse_error = match addr.parse::<SocketAddr>() {
            Ok(socket_addr) => return Ok(Self::Literal(socket_addr)),
            Err(error) => error,
        };
        match addr
            .rsplit_once(':')
            .and_then(|(host, port)| Some((host, port.parse().ok()?)))
        {
            Some((host, port)) => Ok(Self::Name(host, port)),
            None => Err(crate::error::Error::CfgParseSockAddr {
                string: addr.to_string(),
                source: parse_error,
            }),
        }
    }
}

/// Resolve `addr` to every candidate it names.
///
/// A literal `ip:port` short-circuits; anything else goes to the system
/// resolver, once. Only [`resolve_addrs_async`] asks the configured DNS
/// servers first.
#[inline]
pub fn resolve_addrs(addr: &str) -> Result<ResolvedAddrs> {
    let (host, port) = match Target::of(addr)? {
        Target::Literal(literal) => return Ok(ResolvedAddrs::from(literal)),
        Target::Name(host, port) => (host, port),
    };
    let lookup = std::net::ToSocketAddrs::to_socket_addrs(&(host, port));
    ResolvedAddrs::looked_up(addr, lookup.map(Iterator::collect))
}

/// Async counterpart of [`resolve_addrs`], for Tokio contexts.
pub async fn resolve_addrs_async(addr: &str) -> Result<ResolvedAddrs> {
    let (host, port) = match Target::of(addr)? {
        Target::Literal(literal) => return Ok(ResolvedAddrs::from(literal)),
        Target::Name(host, port) => (host, port),
    };
    if host != "localhost"
        && let Ok(ips) = crate::addr::get_ip_addrs_async(host).await
        && let Some(addrs) = ResolvedAddrs::from_candidates(
            ips.into_iter()
                .map(|ip| SocketAddr::new(ip, port))
                .collect(),
        )
    {
        return Ok(addrs);
    }
    let lookup = tokio::net::lookup_host((host, port)).await;
    ResolvedAddrs::looked_up(addr, lookup.map(Iterator::collect))
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
pub const PB_MAPPER_TUNNEL_IDLE_TIMEOUT: &str = "PB_MAPPER_TUNNEL_IDLE_TIMEOUT";
pub const PB_MAPPER_HALF_CLOSE_IDLE_TIMEOUT: &str = "PB_MAPPER_HALF_CLOSE_IDLE_TIMEOUT";
pub const PB_MAPPER_NEW_STREAMS_PER_SECOND: &str = "PB_MAPPER_NEW_STREAMS_PER_SECOND";
/// What [`new_streams_per_second`] is when the variable is unset or not a
/// positive integer.
pub const DEFAULT_NEW_STREAMS_PER_SECOND: u32 = 100;
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
const DEFAULT_TUNNEL_IDLE_TIMEOUT: Duration = Duration::from_secs(60 * 60);
const DEFAULT_HALF_CLOSE_IDLE_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_CONTROL_CONN_POOL_SIZE: usize = 16;
const MAX_CLIENT_HEALTH_FAILURE_THRESHOLD: usize = 100;

const _: () = assert!(
    !DEFAULT_REGISTRATION_REJECT_BACKOFF_MIN.is_zero()
        && !DEFAULT_REGISTRATION_REJECT_BACKOFF_MAX.is_zero()
        && !DEFAULT_SERVER_LEASE_SWEEP_INTERVAL.is_zero(),
    "positive_duration_from_env falls back to these defaults, so they must be positive"
);
const _: () = assert!(
    DEFAULT_REGISTRATION_REJECT_BACKOFF_MIN.as_nanos()
        <= DEFAULT_REGISTRATION_REJECT_BACKOFF_MAX.as_nanos(),
    "the default registration reject backoff must be a valid RetryBackoff range"
);
const _: () = assert!(
    DEFAULT_CONTROL_HEARTBEAT_INTERVAL.as_nanos() < DEFAULT_CONTROL_HEARTBEAT_TOLERANCE.as_nanos(),
    "a control connection becomes suspect only after missing heartbeats"
);
const _: () = assert!(
    DEFAULT_CONTROL_HEARTBEAT_INTERVAL.as_nanos() < DEFAULT_SERVER_LEASE_TIMEOUT.as_nanos(),
    "a lease must outlast the heartbeat that renews it"
);
const _: () = assert!(
    DEFAULT_SERVER_LEASE_SWEEP_INTERVAL.as_nanos() * 3 == DEFAULT_SERVER_LEASE_TIMEOUT.as_nanos(),
    "server_lease_sweep_interval documents its default as a third of the lease"
);
const _: () = assert!(
    0 < DEFAULT_CONTROL_CONN_POOL_SIZE
        && DEFAULT_CONTROL_CONN_POOL_SIZE <= MAX_CONTROL_CONN_POOL_SIZE
        && 0 < DEFAULT_CLIENT_HEALTH_FAILURE_THRESHOLD
        && DEFAULT_CLIENT_HEALTH_FAILURE_THRESHOLD <= MAX_CLIENT_HEALTH_FAILURE_THRESHOLD,
    "a default count must be one bounded_count_from_env could return"
);

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

/// A positive count from `name`, capped at `max`, or `default` when it is
/// unset, unparsable or zero.
fn bounded_count_from_env(name: &str, max: usize, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .filter(|count| *count > 0)
        .map(|count| count.min(max))
        .unwrap_or(default)
}

pub fn control_conn_pool_size() -> usize {
    bounded_count_from_env(
        PB_MAPPER_CONTROL_CONN_POOL_SIZE,
        MAX_CONTROL_CONN_POOL_SIZE,
        DEFAULT_CONTROL_CONN_POOL_SIZE,
    )
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
/// 1024 ([`RetryBackoff::MAX_DOUBLINGS`]), a millisecond minimum would never
/// climb past about a second no matter how high the maximum, hammering the very
/// quota the ladder exists to let clear.
///
/// [`RetryBackoff`]: crate::timeout::RetryBackoff
/// [`RetryBackoff::new`]: crate::timeout::RetryBackoff::new
/// [`RetryBackoff::MAX_DOUBLINGS`]: crate::timeout::RetryBackoff::MAX_DOUBLINGS
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
    bounded_count_from_env(
        PB_MAPPER_CLIENT_HEALTH_FAILURE_THRESHOLD,
        MAX_CLIENT_HEALTH_FAILURE_THRESHOLD,
        DEFAULT_CLIENT_HEALTH_FAILURE_THRESHOLD,
    )
}

/// How long a TCP tunnel may carry nothing in either direction before it is
/// closed. Zero never closes it for idling.
pub fn tunnel_idle_timeout() -> Duration {
    duration_from_env(PB_MAPPER_TUNNEL_IDLE_TIMEOUT, DEFAULT_TUNNEL_IDLE_TIMEOUT)
}

/// How long a TCP tunnel one side has finished sending on may carry nothing
/// more before it is closed. Zero never closes it for idling.
pub fn half_close_idle_timeout() -> Duration {
    duration_from_env(
        PB_MAPPER_HALF_CLOSE_IDLE_TIMEOUT,
        DEFAULT_HALF_CLOSE_IDLE_TIMEOUT,
    )
}

/// New streams a namespace may open per second, sustained: the relay's
/// stream rate limit, and the basis of the first-flight replay budget. The
/// variable as a positive integer, untrimmed, or
/// [`DEFAULT_NEW_STREAMS_PER_SECOND`].
pub fn new_streams_per_second() -> NonZeroUsize {
    const DEFAULT: NonZeroUsize = match NonZeroUsize::new(DEFAULT_NEW_STREAMS_PER_SECOND as usize) {
        Some(rate) => rate,
        None => panic!("the default stream rate is positive"),
    };
    std::env::var(PB_MAPPER_NEW_STREAMS_PER_SECOND)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(DEFAULT)
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

    #[test]
    fn parse_duration_accepts_suffixes_and_plain_seconds() {
        assert_eq!(parse_duration("42"), Some(Duration::from_secs(42)));
        assert_eq!(parse_duration("500ms"), Some(Duration::from_millis(500)));
        assert_eq!(parse_duration("2s"), Some(Duration::from_secs(2)));
        assert_eq!(parse_duration("3m"), Some(Duration::from_secs(180)));
        assert_eq!(parse_duration("1h"), Some(Duration::from_secs(3600)));
        assert_eq!(parse_duration(""), None);
        assert_eq!(parse_duration("bad"), None);
        assert_eq!(parse_duration("18446744073709551615h"), None);
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

    /// A name that does not resolve reports the resolver's reason, not the
    /// parse error a malformed address gets, which would send whoever has to
    /// fix it to the address's syntax. A NUL byte fails every resolver before
    /// a query is sent.
    #[tokio::test]
    async fn a_name_that_does_not_resolve_reports_the_resolvers_reason() {
        let blocking = resolve_addrs("bad\0name:7666").expect_err("a NUL never resolves");
        let asynchronous = resolve_addrs_async("bad\0name:7666")
            .await
            .expect_err("a NUL never resolves");
        for error in [blocking, asynchronous] {
            assert!(
                matches!(error, crate::error::Error::CfgResolveSockAddr { .. }),
                "{error:?}"
            );
        }
        assert!(matches!(
            resolve_addrs_async("localhost").await,
            Err(crate::error::Error::CfgParseSockAddr { .. })
        ));
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
