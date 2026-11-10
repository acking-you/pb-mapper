//! Explicitly authorized hosting owned by the installed Windows service.
//! The UI is a management client; closing it does not own or stop this host.
use super::{configuration, ipc, os, pipe};
use crate::{HostStatus, Result, error};
use pb_mapper_client::sdk::RelayTransport;
use serde::{Deserialize, Serialize};

const PIPE: &str = r"\\.\pipe\pb-mapper.desktop-background.v7";
const CALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);
const FILE_NAME: &str = "background-host.dpapi";
const REVOKED_MARKER: &str = "background-host.revoked";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub service: String,
    pub display_id: u32,
    /// The display's device name (`\\.\DISPLAY1`), which finds it again
    /// when its id went stale.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_device: Option<String>,
    pub allow_control: bool,
    /// On when a saved grant does not say, as every path defaults: LAN
    /// first, then direct, then the relay.
    #[serde(default = "allow_direct_by_default")]
    pub allow_direct: bool,
    pub auto_start: bool,
}
fn allow_direct_by_default() -> bool {
    true
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Options {
    pub server: String,
    pub credential: String,
    pub relay_transport: RelayTransport,
    pub settings: Settings,
    /// The host's `HostOptions::lan_first`, as the application's setting
    /// was when sharing started. Written only when off, so a grant saved
    /// with the default stays readable by a helper from before it.
    #[serde(default = "lan_first_by_default", skip_serializing_if = "is_true")]
    pub lan_first: bool,
}
fn lan_first_by_default() -> bool {
    true
}
fn is_true(value: &bool) -> bool {
    *value
}
impl std::fmt::Debug for Options {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Options")
            .field("server", &self.server)
            .field("credential", &"[redacted]")
            .field("relay_transport", &self.relay_transport)
            .field("settings", &self.settings)
            .field("lan_first", &self.lan_first)
            .finish()
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Snapshot {
    pub configured: bool,
    pub enabled: bool,
    pub host: Option<HostStatus>,
    /// Returned only to the approved local application, never written to logs.
    pub invitation: Option<String>,
    pub settings: Option<Settings>,
    pub server: Option<String>,
    pub namespace: Option<u64>,
    pub message: String,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum Command {
    Configure {
        options: Options,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        identity: Option<crate::HostIdentity>,
    },
    // Struct variants throughout: serde ignores extra fields on an internally
    // tagged unit variant, which would undo `deny_unknown_fields`.
    Status {},
    /// The saved grant started again with the relay it was saved with. The
    /// application no longer sends it, since a resume configures from its
    /// current settings, but it stays part of `HELPER_PROTOCOL`'s pipe.
    Start {},
    Stop {},
    /// End the connected viewer's session; hosting continues.
    EndSession {},
    RevokeScope {
        server: String,
        namespace: Option<u64>,
    },
    Permission {
        allow_control: bool,
    },
    /// Put back the display mode a viewer changed.
    RestoreDisplayMode {},
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Response {
    snapshot: Option<Snapshot>,
    error: Option<String>,
}

async fn call(command: Command) -> Result<Snapshot> {
    super::status().ensure_ready()?;
    let config = configuration()?;
    let mut pipe = pipe::connect(PIPE)?;
    os::verify_server(&pipe, &config.helper_executable, None)?;
    tokio::time::timeout(CALL_TIMEOUT, async {
        ipc::write_json(&mut pipe, &command).await?;
        let response: Response = ipc::read_json(&mut pipe).await?;
        match (response.snapshot, response.error) {
            (_, Some(message)) => Err(error(message)),
            (Some(snapshot), None) => Ok(snapshot),
            _ => Err(error("Invalid background service response")),
        }
    })
    .await
    .map_err(error)?
}

/// Save explicit machine-service authorization and begin connecting. Network
/// readiness is asynchronous and visible through [`status`]. Validation or
/// storage failure leaves the previous configuration and host untouched.
pub async fn configure(options: Options) -> Result<Snapshot> {
    call(Command::Configure {
        options,
        identity: None,
    })
    .await
}
/// Transfer a local host into service ownership without changing its invitation.
/// An existing service identity for the same scope always wins: stale application
/// storage must never replace an established service-owned grant.
pub async fn configure_with_identity(
    options: Options,
    identity: crate::HostIdentity,
) -> Result<Snapshot> {
    call(Command::Configure {
        options,
        identity: Some(identity),
    })
    .await
}
pub async fn status() -> Result<Snapshot> {
    call(Command::Status {}).await
}
/// Disconnect the viewer; the background host keeps sharing.
pub async fn end_session() -> Result<Snapshot> {
    call(Command::EndSession {}).await
}
/// Stop active hosting and disable automatic startup, retaining the invitation.
pub async fn stop() -> Result<Snapshot> {
    call(Command::Stop {}).await
}
/// Revoke the current application's relay scope, including a just-completed
/// handoff that its UI has not observed. The service checks the scope atomically.
/// Older helpers reject this operation; never fall back to unscoped revocation.
pub async fn revoke_scope(server: String, namespace: Option<u64>) -> Result<Snapshot> {
    let helper = super::status();
    if !helper.installed || helper.startup_disabled {
        let root = os::install_root()?;
        // An absent/stopped helper with no durable grant has nothing to revoke.
        // A leftover authorization or failed-revocation marker requires the
        // service, even when this application has never observed it running.
        if !root.join(FILE_NAME).try_exists()? && !root.join(REVOKED_MARKER).try_exists()? {
            return Ok(Snapshot::default());
        }
    }
    call(Command::RevokeScope { server, namespace }).await
}
pub async fn set_control_allowed(allow_control: bool) -> Result<Snapshot> {
    call(Command::Permission { allow_control }).await
}
/// Put back the display mode a viewer of the background host changed.
pub async fn restore_display_mode() -> Result<Snapshot> {
    call(Command::RestoreDisplayMode {}).await
}

#[cfg(feature = "windows-helper")]
#[path = "background_service.rs"]
mod service;
#[cfg(feature = "windows-helper")]
pub(super) use service::{revoke_installation, run};

#[cfg(test)]
mod tests {
    use super::*;

    /// A grant saved with LAN-first on, the default, stays readable by a
    /// protocol 4 helper, whose `Options` refuse unknown fields; one saved
    /// before the field existed reads as on.
    #[test]
    fn lan_first_is_written_only_when_off() {
        let mut options = Options {
            server: "relay.example:7666".into(),
            credential: "background-test-key-000000000000".into(),
            relay_transport: RelayTransport::Tcp,
            settings: Settings {
                service: "desktop:study".into(),
                display_id: 1,
                display_device: None,
                allow_control: false,
                allow_direct: true,
                auto_start: true,
            },
            lan_first: true,
        };
        let on = serde_json::to_value(&options).unwrap();
        assert!(on.get("lan_first").is_none());
        assert!(serde_json::from_value::<Options>(on).unwrap().lan_first);
        options.lan_first = false;
        let off = serde_json::to_value(&options).unwrap();
        assert_eq!(off["lan_first"], false);
        assert!(!serde_json::from_value::<Options>(off).unwrap().lan_first);
    }
}
