//! The protected installation's record: the user who owns the helper, and
//! the application and helper executables it was installed for.
#[cfg(feature = "windows-helper")]
use super::HELPER_PROTOCOL;
use super::os;
use crate::{Result, error};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Configuration {
    /// 1: written by protocol 1 helpers, which record no protocol. 2: records
    /// the protocol.
    schema: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    protocol: Option<u32>,
    pub(super) owner_sid: String,
    pub(super) ui_executable: PathBuf,
    pub(super) helper_executable: PathBuf,
}
impl Configuration {
    #[cfg(feature = "windows-helper")]
    pub(super) fn current(
        owner_sid: String,
        ui_executable: PathBuf,
        helper_executable: PathBuf,
    ) -> Self {
        Self {
            schema: 2,
            protocol: Some(HELPER_PROTOCOL),
            owner_sid,
            ui_executable,
            helper_executable,
        }
    }
    pub(super) fn protocol(&self) -> u32 {
        self.protocol.unwrap_or(1)
    }
    pub(super) fn parse(data: &[u8]) -> Result<Self> {
        if data.len() > 8192 {
            return Err(error("invalid helper installation"));
        }
        let config: Self = serde_json::from_slice(data).map_err(error)?;
        match (config.schema, config.protocol) {
            (1, None) | (2, Some(_)) => Ok(config),
            _ => Err(error("unsupported helper installation")),
        }
    }
}
pub(super) fn configuration() -> Result<Configuration> {
    let path = os::install_root()?.join("installation.json");
    os::protected_file(&path)?;
    let config = Configuration::parse(&std::fs::read(path)?)?;
    os::protected_file(&config.helper_executable)?;
    os::protected_file(&config.ui_executable)?;
    Ok(config)
}
