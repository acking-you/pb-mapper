//! Explicitly authorized hosting owned by the installed Windows service.
//! The UI is a management client; closing it does not own or stop this host.
use super::{configuration, ipc, os};
use crate::{Result, SessionStatus, error};
use pb_mapper_client::sdk::RelayTransport;
use serde::{Deserialize, Serialize};

const PIPE: &str = r"\\.\pipe\pb-mapper.desktop-background.v1";
const CALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub service: String,
    pub display_id: u32,
    pub allow_control: bool,
    pub allow_direct: bool,
    pub auto_start: bool,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Options {
    pub server: String,
    pub credential: String,
    pub relay_transport: RelayTransport,
    pub settings: Settings,
}
impl std::fmt::Debug for Options {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Options")
            .field("server", &self.server)
            .field("credential", &"[redacted]")
            .field("relay_transport", &self.relay_transport)
            .field("settings", &self.settings)
            .finish()
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Snapshot {
    pub configured: bool,
    pub enabled: bool,
    pub host: Option<SessionStatus>,
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
    Configure { options: Options },
    Status,
    Start,
    Stop,
    Revoke,
    Permission { allow_control: bool },
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Response {
    snapshot: Option<Snapshot>,
    error: Option<String>,
}

async fn call(command: Command) -> Result<Snapshot> {
    let config = configuration()?;
    if !super::status().authorized_application {
        return Err(error(
            "Open the installed application to manage background hosting",
        ));
    }
    let mut pipe = ipc::connect(PIPE)?;
    os::verify_server(&pipe, &config.helper_executable, None)?;
    tokio::time::timeout(CALL_TIMEOUT, async {
        crate::wire::write_json(&mut pipe, &command).await?;
        let response: Response = crate::wire::read_json(&mut pipe).await?;
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
    call(Command::Configure { options }).await
}
pub async fn status() -> Result<Snapshot> {
    call(Command::Status).await
}
/// Restore the saved grant; a viewer still has to request input control.
pub async fn start() -> Result<Snapshot> {
    call(Command::Start).await
}
/// Stop active hosting and disable automatic startup, retaining the invitation.
pub async fn stop() -> Result<Snapshot> {
    call(Command::Stop).await
}
/// Stop active hosting and destroy the stored invitation and relay credential.
pub async fn revoke() -> Result<Snapshot> {
    call(Command::Revoke).await
}
pub async fn set_control_allowed(allow_control: bool) -> Result<Snapshot> {
    call(Command::Permission { allow_control }).await
}

#[cfg(feature = "windows-helper")]
#[path = "background_service.rs"]
mod service;
#[cfg(feature = "windows-helper")]
pub(super) use service::{revoke_installation, run};
