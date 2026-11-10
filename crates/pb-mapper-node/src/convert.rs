//! Conversions at the JS boundary: numbers narrowed both ways, enums to and
//! from their JS strings, and errors flattened into JS `Error`s.

use napi::{Error, Result};
use pb_mapper_client::sdk::{self, Transport, TunnelStatus};

/// Default page size for the `list*` calls that take an explicit page.
const DEFAULT_PAGE_SIZE: u32 = 100;

/// A JS-safe integer: `u64` values past 2^53 - 1 saturate rather than round.
pub(crate) fn number(value: u64) -> f64 {
    value.min((1_u64 << 53) - 1) as f64
}

pub(crate) fn path_name(path: sdk::SessionPath) -> &'static str {
    match path {
        sdk::SessionPath::Direct => "direct",
        sdk::SessionPath::Relay => "relay",
    }
}

/// The name of a relay carrier, as the JS API spells it.
pub(crate) fn transport_name(transport: sdk::RelayTransport) -> String {
    match transport {
        sdk::RelayTransport::Tcp => "tcp",
        sdk::RelayTransport::Quic => "quic",
    }
    .into()
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

pub(crate) fn parse_transport(raw: &str) -> Result<Transport> {
    match raw.to_ascii_lowercase().as_str() {
        "tcp" => Ok(Transport::Tcp),
        "udp" => Ok(Transport::Udp),
        _ => Err(invalid_argument(format!(
            "transport must be \"tcp\" or \"udp\", got {raw}"
        ))),
    }
}

pub(crate) fn parse_relay_transport(raw: Option<&str>) -> Result<sdk::RelayTransport> {
    match raw.unwrap_or("tcp") {
        "tcp" => Ok(sdk::RelayTransport::Tcp),
        "quic" => Ok(sdk::RelayTransport::Quic),
        _ => Err(invalid_argument("relayTransport must be tcp or quic")),
    }
}

pub(crate) fn parse_path(raw: Option<&str>) -> Result<sdk::PathPolicy> {
    match raw.unwrap_or("auto") {
        "relay-only" => Ok(sdk::PathPolicy::RelayOnly),
        "auto" => Ok(sdk::PathPolicy::Auto),
        _ => Err(invalid_argument("path must be relay-only or auto")),
    }
}

pub(crate) fn parse_legacy_protocol(raw: &str) -> Result<sdk::LegacyProtocol> {
    match raw.to_ascii_lowercase().as_str() {
        "allow" => Ok(sdk::LegacyProtocol::Allow),
        "deny" => Ok(sdk::LegacyProtocol::Deny),
        _ => Err(invalid_argument(
            "legacy protocol must be \"allow\" or \"deny\"",
        )),
    }
}

pub(crate) fn parse_lan_transport(raw: Option<&str>) -> Result<sdk::LanTransport> {
    match raw.unwrap_or("quic") {
        "quic" => Ok(sdk::LanTransport::Quic),
        "tcp" => Ok(sdk::LanTransport::Tcp),
        other => Err(invalid_argument(format!(
            "lanTransport must be \"quic\" or \"tcp\", got {other}"
        ))),
    }
}

pub(crate) fn parse_port_mapping(raw: Option<&str>) -> Result<sdk::PortMapping> {
    match raw.unwrap_or("auto") {
        "off" => Ok(sdk::PortMapping::Off),
        "auto" => Ok(sdk::PortMapping::Auto),
        "always" => Ok(sdk::PortMapping::Always),
        other => Err(invalid_argument(format!(
            "portMapping must be \"off\", \"auto\" or \"always\", got {other}"
        ))),
    }
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
