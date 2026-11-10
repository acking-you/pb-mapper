//! What the desktop probes share: the relay configuration they read, their
//! `--name value` options and the SDK clients they build. Each probe includes
//! this module and uses part of it.

use pb_mapper_client::sdk::{self, Client, ClientConfig, RelayTransport};

/// The relay a probe reaches, as the app's configuration file names it.
#[allow(
    dead_code,
    reason = "deployed, viewer_probe and video_bench read no relay configuration"
)]
#[derive(serde::Deserialize)]
pub struct RelayConfig {
    pub server_address: String,
    pub msg_header_key: String,
    #[serde(default)]
    pub relay_transport: RelayTransport,
}

/// The value after `name` on the command line.
pub fn option<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.windows(2)
        .find(|pair| pair[0] == name)
        .map(|pair| pair[1].as_str())
}

/// A kept-alive client of `server` under `credential`, over `transport`.
#[allow(dead_code, reason = "video_bench reaches no relay")]
pub fn relay_client(
    server: String,
    credential: String,
    transport: RelayTransport,
) -> sdk::Result<Client> {
    Client::new(ClientConfig {
        server,
        credential,
        keep_alive: true,
        namespace: None,
    })?
    .with_relay_transport(transport)
}

/// As the app's `AppConfig::lan_first`, on for a file written before the
/// switch existed.
#[allow(
    dead_code,
    reason = "only deployed and viewer_probe read a lan_first setting"
)]
pub fn lan_first_by_default() -> bool {
    true
}
