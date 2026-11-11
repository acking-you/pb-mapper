//! What this computer saves: `config.json`, `services.json` and
//! `clients.json`, as the bytes on disk.
//!
//! Additive changes only: a field a build adds takes a default, so what any
//! earlier build wrote still loads; and a key this build does not know, which
//! a later build wrote, is kept as it was at each level this build rewrites
//! ([`Unknown`]), so saving here does not take it from that build.

use std::collections::HashMap;
use std::time::SystemTime;

use pb_mapper_client::sdk::{LanTransport, PortMapping, RelayTransport};
use pb_mapper_core::checksum::parse_credential;
use serde::{Deserialize, Serialize};

use crate::error::CtlError;

/// The keys of one object in a saved file that this build does not know, as
/// a later build wrote them. Written back after the known ones, and nothing
/// when there are none, so a file holding none is written as it always was.
pub type Unknown = serde_json::Map<String, serde_json::Value>;

pub(super) fn normalize_msg_header_key(msg_header_key: String) -> Result<String, CtlError> {
    let normalized = msg_header_key.trim().to_string();
    if normalized.is_empty() {
        return Ok(normalized);
    }
    parse_credential(&normalized).map_err(CtlError::invalid_argument)?;
    Ok(normalized)
}

#[derive(Serialize, Deserialize, Clone)]
pub struct ServiceConfigData {
    pub service_key: String,
    pub local_address: String,
    pub protocol: String,
    pub enable_encryption: bool,
    pub enable_keep_alive: bool,
    pub created_at: SystemTime,
    /// Whether a connection may reach it directly rather than through the
    /// relay. On unless turned off: a share saved before there was a choice
    /// allows it, as `register` does.
    #[serde(default = "direct_by_default")]
    pub allow_direct: bool,
    /// When to ask the router to map the direct UDP port. A tunnel saved
    /// before there was a choice does it when needed: once punching through
    /// has failed. Kept while direct is off, to apply when it is on again.
    #[serde(default = "map_when_needed", deserialize_with = "saved_port_mapping")]
    pub port_mapping: PortMapping,
    #[serde(flatten)]
    pub unknown: Unknown,
}

fn direct_by_default() -> bool {
    true
}

pub(crate) fn map_when_needed() -> PortMapping {
    PortMapping::Auto
}

/// A saved port mapping: its name, or the on/off a build of this change's
/// first draft saved (on was from the start).
fn saved_port_mapping<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<PortMapping, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Saved {
        Mode(PortMapping),
        Switch(bool),
    }
    Ok(match Saved::deserialize(deserializer)? {
        Saved::Mode(mode) => mode,
        Saved::Switch(true) => PortMapping::Always,
        Saved::Switch(false) => PortMapping::Off,
    })
}

/// How a tunnel may go past the relay, as it is saved with the tunnel.
#[derive(Clone, Copy)]
pub struct DirectOptions {
    pub allow_direct: bool,
    /// Only acted on while `allow_direct` is.
    pub port_mapping: PortMapping,
}

#[derive(Serialize, Deserialize, Default)]
pub struct ServiceConfigStore {
    pub services: HashMap<String, ServiceConfigData>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct ClientConfigData {
    pub service_key: String,
    pub local_address: String,
    pub protocol: String,
    pub enable_keep_alive: bool,
    pub created_at: SystemTime,
    /// The namespace the service is in, when it is not this credential's own:
    /// an administrator connecting to a temporary credential's service.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<u64>,
    /// Whether each session tries a direct path before the relay. Off is
    /// `connect --relay-only`.
    #[serde(default = "direct_by_default")]
    pub allow_direct: bool,
    /// As [`ServiceConfigData::port_mapping`].
    #[serde(default = "map_when_needed", deserialize_with = "saved_port_mapping")]
    pub port_mapping: PortMapping,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Serialize, Deserialize, Default)]
pub struct ClientConfigStore {
    pub clients: HashMap<String, ClientConfigData>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq)]
#[serde(default)]
pub struct AppConfig {
    pub server_address: String,
    pub keep_alive_enabled: bool,
    pub msg_header_key: String,
    /// How this computer reaches the relay: TCP, as a config saved before
    /// there was a choice, or QUIC. A tunnel keeps the one it started with.
    pub relay_transport: RelayTransport,
    /// Whether this computer tries a local network it shares with the other
    /// end first: on, as a config saved before there was a choice. Each
    /// tunnel, remote-desktop connection and host takes it as it starts.
    pub lan_first: bool,
    /// What a path within a local network this computer shares with the
    /// other end runs over, for what this computer dials: QUIC, as a config
    /// saved before there was a choice, or TCP. Each connection and
    /// remote-desktop connection takes it as it starts; as a provider this
    /// computer serves whichever the other end asks for.
    pub lan_transport: LanTransport,
    /// Carried through every save: one made from the window's fields takes
    /// it from the config it replaces.
    #[serde(flatten)]
    pub unknown: Unknown,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            server_address: "localhost:7666".to_string(),
            keep_alive_enabled: true,
            msg_header_key: String::new(),
            relay_transport: RelayTransport::Tcp,
            lan_first: true,
            lan_transport: LanTransport::Quic,
            unknown: Unknown::new(),
        }
    }
}

impl AppConfig {
    /// Whether `other` reaches the same relay the same way: the same
    /// address, keep-alive, credential and carrier. What the window keeps
    /// about the relay, its client and what it said about QUIC, is kept for
    /// these alone, so a preference of this computer's own, such as
    /// `lan_first` or `lan_transport`, changes neither.
    pub fn same_relay(&self, other: &Self) -> bool {
        let Self {
            server_address,
            keep_alive_enabled,
            msg_header_key,
            relay_transport,
            lan_first: _,
            lan_transport: _,
            unknown: _,
        } = self;
        *server_address == other.server_address
            && *keep_alive_enabled == other.keep_alive_enabled
            && *msg_header_key == other.msg_header_key
            && *relay_transport == other.relay_transport
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What this build writes when the files hold nothing it does not know,
    /// pinned byte for byte: a newer build's keys, kept when there are some,
    /// must leave it as it was.
    #[test]
    fn a_file_without_newer_keys_is_written_as_before() {
        const CONFIG: &str = r#"{
  "server_address": "localhost:7666",
  "keep_alive_enabled": true,
  "msg_header_key": "",
  "relay_transport": "tcp",
  "lan_first": true,
  "lan_transport": "quic"
}"#;
        const SERVICES: &str = r#"{
  "services": {
    "web": {
      "service_key": "web",
      "local_address": "127.0.0.1:8080",
      "protocol": "TCP",
      "enable_encryption": true,
      "enable_keep_alive": false,
      "created_at": {
        "secs_since_epoch": 1700000000,
        "nanos_since_epoch": 500000000
      },
      "allow_direct": true,
      "port_mapping": "auto"
    }
  }
}"#;
        const CLIENTS: &str = r#"{
  "clients": {
    "web": {
      "service_key": "web",
      "local_address": "127.0.0.1:9090",
      "protocol": "UDP",
      "enable_keep_alive": true,
      "created_at": {
        "secs_since_epoch": 1700000000,
        "nanos_since_epoch": 500000000
      },
      "namespace": 7,
      "allow_direct": false,
      "port_mapping": "always"
    }
  }
}"#;
        assert_eq!(
            serde_json::to_string_pretty(&AppConfig::default()).unwrap(),
            CONFIG
        );
        let config: AppConfig = serde_json::from_str(CONFIG).unwrap();
        assert_eq!(serde_json::to_string_pretty(&config).unwrap(), CONFIG);
        let services: ServiceConfigStore = serde_json::from_str(SERVICES).unwrap();
        assert_eq!(serde_json::to_string_pretty(&services).unwrap(), SERVICES);
        let clients: ClientConfigStore = serde_json::from_str(CLIENTS).unwrap();
        assert_eq!(serde_json::to_string_pretty(&clients).unwrap(), CLIENTS);
    }
}
