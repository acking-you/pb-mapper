//! Conversions at the JS boundary: numbers narrowed both ways, enums to and
//! from their JS strings, and errors flattened into JS `Error`s.

use napi::{Error, Result};
use pb_mapper_client::sdk::{self, Transport, TunnelStatus};

/// Default page size for the `list*` calls that take an explicit page.
const DEFAULT_PAGE_SIZE: u32 = 100;

/// The largest integer a JS number holds exactly, `Number.MAX_SAFE_INTEGER`.
const MAX_SAFE_INTEGER: u64 = (1 << 53) - 1;

/// A JS-safe integer: `u64` values past 2^53 - 1 saturate rather than round.
pub(crate) fn number(value: u64) -> f64 {
    value.min(MAX_SAFE_INTEGER) as f64
}

/// A key id, namespace or generation as a JS number: one past 2^53 - 1
/// saturates, naming nothing, rather than rounding to a neighbour's id.
pub(crate) fn id(value: u64) -> i64 {
    // Exact: MAX_SAFE_INTEGER is below i64::MAX.
    value.min(MAX_SAFE_INTEGER) as i64
}

pub(crate) fn to_napi(error: sdk::Error) -> Error {
    Error::from_reason(error.to_string())
}

pub(crate) fn invalid_argument(message: impl Into<String>) -> Error {
    Error::from_reason(message.into())
}

/// Narrow a JS integer to the `u64` the SDK expects.
///
/// `field` names the argument, so a caller sees which one was negative rather
/// than a bare type complaint.
pub(crate) fn as_u64(field: &str, value: i64) -> Result<u64> {
    u64::try_from(value).map_err(|_| invalid_argument(format!("{field} must be non-negative")))
}

/// Same as [`as_u64`], for an optional argument.
pub(crate) fn as_opt_u64(field: &str, value: Option<i64>) -> Result<Option<u64>> {
    value.map(|value| as_u64(field, value)).transpose()
}

/// Narrow a JS page size to the `u16` the SDK expects.
///
/// Checked rather than cast: `as u16` silently wraps, so `pageSize: 65_537`
/// would have asked for a single-item page instead of failing.
pub(crate) fn narrow_page_size(value: Option<u32>) -> Result<u16> {
    let value = value.unwrap_or(DEFAULT_PAGE_SIZE);
    u16::try_from(value).map_err(|_| invalid_argument(format!("pageSize {value} is out of range")))
}

// The option parsers below take the SDK's own labels (`FromStr`, exact and
// case-sensitive). `transport` and the legacy policy are lowercased first, so
// they also take any case; every option keeps its JS default and message.

pub(crate) fn parse_transport(raw: &str) -> Result<Transport> {
    raw.to_ascii_lowercase()
        .parse()
        .map_err(|_| invalid_argument(format!("transport must be \"tcp\" or \"udp\", got {raw}")))
}

pub(crate) fn parse_relay_transport(raw: Option<&str>) -> Result<sdk::RelayTransport> {
    raw.unwrap_or("tcp")
        .parse()
        .map_err(|_| invalid_argument("relayTransport must be tcp or quic"))
}

pub(crate) fn parse_path(raw: Option<&str>) -> Result<sdk::PathPolicy> {
    raw.unwrap_or("auto")
        .parse()
        .map_err(|_| invalid_argument("path must be relay-only or auto"))
}

pub(crate) fn parse_legacy_protocol(raw: &str) -> Result<sdk::LegacyProtocol> {
    raw.to_ascii_lowercase()
        .parse()
        .map_err(|_| invalid_argument("legacy protocol must be \"allow\" or \"deny\""))
}

pub(crate) fn parse_lan_transport(raw: Option<&str>) -> Result<sdk::LanTransport> {
    let raw = raw.unwrap_or("quic");
    raw.parse().map_err(|_| {
        invalid_argument(format!(
            "lanTransport must be \"quic\" or \"tcp\", got {raw}"
        ))
    })
}

pub(crate) fn parse_port_mapping(raw: Option<&str>) -> Result<sdk::PortMapping> {
    let raw = raw.unwrap_or("auto");
    raw.parse().map_err(|_| {
        invalid_argument(format!(
            "portMapping must be \"off\", \"auto\" or \"always\", got {raw}"
        ))
    })
}

/// Render a tunnel status for JS.
///
/// `Failed` keeps its reason after the colon: it is the only description of a
/// permanent rejection a JS caller ever receives.
pub(crate) fn status_label(status: &TunnelStatus) -> String {
    match status {
        TunnelStatus::Starting => "starting".into(),
        TunnelStatus::Connected => "connected".into(),
        TunnelStatus::Retrying => "retrying".into(),
        TunnelStatus::Stopped => "stopped".into(),
        TunnelStatus::Failed(reason) => format!("failed:{reason}"),
    }
}
