//! User-writable configuration storage and in-memory state initialization.
//!
//! ```text
//! app directory -> pb-mapper-ui/config.json -> AppConfig -> process credential
//!               -> services.json / clients.json -> remembered tunnel definitions
//! ```
//!
//! An explicit Flutter app directory wins on every platform. This same directory is
//! the root for relay authentication state, so desktop and mobile UI processes never
//! depend on root-owned `/var/lib` paths.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use parking_lot::Mutex as SyncMutex;
use pb_mapper_core::checksum::set_process_msg_header_key;
use serde::Serialize;
use serde::de::DeserializeOwned;

use super::status::LocalStatusCache;
use super::stored::normalize_msg_header_key;
use super::{
    AppConfig, ClientConfigData, ClientConfigStore, ConnectSpec, DirectOptions, PbMapperState,
    ServiceConfigData, ServiceConfigStore, ShareSpec,
};
use crate::error::CtlError;

impl PbMapperState {
    pub(crate) fn desktop_config_dir(&self) -> PathBuf {
        self.config_dir.clone()
    }
    pub(super) fn reset_status_caches(&self) {
        *self.local_server_status.lock() = LocalStatusCache::default();
    }
    /// A state that has read nothing: the default configuration and no
    /// credential, until `set_app_directory_path` names its directory. Every
    /// client sends that first (the app an empty path on desktop, meaning the
    /// platform's own), so the FFI handle starts here rather than reading a
    /// directory it may never be pointed at.
    pub fn unloaded() -> Self {
        Self {
            embedded_relay: None,
            relay_client: SyncMutex::new(None),
            quic_offer: None,
            service_runtime: HashMap::new(),
            client_runtime: HashMap::new(),
            config: AppConfig::default(),
            config_dir: Self::get_config_dir(&None),
            app_directory_path: None,
            local_server_status: Arc::default(),
            registering: Arc::new(SyncMutex::new(HashSet::new())),
            connecting: Arc::new(SyncMutex::new(HashSet::new())),
        }
    }

    /// A state that loads the configuration in `app_directory_path` at once.
    #[cfg(test)]
    pub fn new(app_directory_path: Option<String>) -> Self {
        let mut state = Self::unloaded();
        if let Err(e) = state.set_app_directory_path(app_directory_path) {
            tracing::error!("Failed to apply MSG_HEADER_KEY during init: {}", e);
        }
        state
    }

    pub fn set_app_directory_path(&mut self, path: Option<String>) -> Result<(), CtlError> {
        self.app_directory_path = path;
        self.config_dir = Self::get_config_dir(&self.app_directory_path);
        tracing::info!("Using config directory: {:?}", self.config_dir);

        // Reload config from new location if exists
        match self.load_config() {
            Ok(config) => self.config = config,
            Err(e) => {
                tracing::warn!("Failed to reload config after setting app dir: {}", e);
            }
        }
        tracing::info!(
            "Loaded configuration: server_address={}, keep_alive={}, msg_header_key_set={}",
            self.config.server_address,
            self.config.keep_alive_enabled,
            !self.config.msg_header_key.is_empty()
        );
        self.apply_msg_header_key_env()?;

        Ok(())
    }

    pub(super) fn apply_msg_header_key_env(&self) -> Result<(), CtlError> {
        let key = (!self.config.msg_header_key.is_empty()).then_some(&*self.config.msg_header_key);
        // The library validates the key's length and shape; a rejection here is
        // the stored setting being wrong, not something going wrong.
        set_process_msg_header_key(key).map_err(CtlError::invalid_argument)
    }

    pub(crate) fn get_config_dir(app_directory_path: &Option<String>) -> PathBuf {
        // An explicit path wins everywhere. Mobile is where it normally comes
        // from — Flutter hands it over, because there is no OS config dir to
        // discover — but honouring it on desktop too is what lets a test point
        // a state at a temporary directory instead of the user's real config.
        if let Some(app_dir) = app_directory_path {
            let path = PathBuf::from(app_dir).join("pb-mapper-ui");
            tracing::info!("Using caller-provided app directory: {:?}", path);
            return path;
        }
        #[cfg(any(target_os = "android", target_os = "ios"))]
        {
            tracing::warn!("No app directory provided for mobile platform, using relative path");
            PathBuf::from("pb-mapper-ui")
        }
        #[cfg(not(any(target_os = "android", target_os = "ios")))]
        {
            if let Some(config_dir) = dirs::config_dir() {
                config_dir.join("pb-mapper-ui")
            } else if let Some(home_dir) = dirs::home_dir() {
                home_dir.join(".config").join("pb-mapper-ui")
            } else {
                tracing::warn!("Could not determine home directory, using current directory");
                std::env::current_dir()
                    .unwrap_or_else(|_| PathBuf::from("."))
                    .join("pb-mapper-ui-config")
            }
        }
    }

    fn get_config_file_path(&self) -> PathBuf {
        let config_dir = Self::get_config_dir(&self.app_directory_path);

        if let Err(e) = std::fs::create_dir_all(&config_dir) {
            tracing::warn!(
                "Failed to create config directory {:?}: {}, using current directory",
                config_dir,
                e
            );
            return PathBuf::from("pb_mapper_config.json");
        }

        let config_file = config_dir.join("config.json");
        tracing::info!("Using config file path: {:?}", config_file);
        config_file
    }

    pub fn load_config(&self) -> Result<AppConfig, CtlError> {
        let config_path = self.get_config_file_path();
        if config_path.exists() {
            let contents =
                fs::read_to_string(config_path).map_err(|e| CtlError::io(e.to_string()))?;
            let mut config: AppConfig =
                serde_json::from_str(&contents).map_err(|e| CtlError::io(e.to_string()))?;
            config.msg_header_key = normalize_msg_header_key(config.msg_header_key)?;
            Ok(config)
        } else {
            Ok(AppConfig::default())
        }
    }

    pub(super) fn write_config_file(&self, config: &AppConfig) -> Result<(), CtlError> {
        let config_path = self.get_config_file_path();
        let contents =
            serde_json::to_string_pretty(config).map_err(|e| CtlError::io(e.to_string()))?;
        fs::write(config_path, contents).map_err(|e| CtlError::io(e.to_string()))?;
        Ok(())
    }

    fn get_service_config_path(&self) -> PathBuf {
        self.config_dir.join("services.json")
    }

    fn get_client_config_path(&self) -> PathBuf {
        self.config_dir.join("clients.json")
    }

    pub fn load_service_configs(&self) -> ServiceConfigStore {
        read_store(&self.get_service_config_path())
    }

    pub fn save_service_configs(&self, store: &ServiceConfigStore) -> Result<(), CtlError> {
        write_store(&self.get_service_config_path(), store, "config")
    }

    pub fn save_service_config(&self, spec: &ShareSpec) -> Result<(), CtlError> {
        let ShareSpec {
            service_key,
            local_address,
            protocol,
            enable_encryption,
            enable_keep_alive,
            direct:
                DirectOptions {
                    allow_direct,
                    port_mapping,
                },
        } = spec;
        let mut store = self.load_service_configs();
        let now = SystemTime::now();

        let config = ServiceConfigData {
            service_key: service_key.to_string(),
            local_address: local_address.to_string(),
            protocol: protocol.to_string(),
            enable_encryption: *enable_encryption,
            enable_keep_alive: *enable_keep_alive,
            created_at: store
                .services
                .get(service_key)
                .map_or(now, |saved| saved.created_at),
            allow_direct: *allow_direct,
            port_mapping: *port_mapping,
        };

        store.services.insert(service_key.to_string(), config);
        self.save_service_configs(&store)
    }

    pub fn delete_service_config(&self, service_key: &str) -> Result<(), CtlError> {
        let mut store = self.load_service_configs();
        store.services.remove(service_key);
        self.save_service_configs(&store)
    }

    pub fn load_client_configs(&self) -> ClientConfigStore {
        read_store(&self.get_client_config_path())
    }

    pub fn save_client_configs(&self, store: &ClientConfigStore) -> Result<(), CtlError> {
        write_store(&self.get_client_config_path(), store, "client config")
    }

    pub fn save_client_config(&self, spec: &ConnectSpec) -> Result<(), CtlError> {
        let ConnectSpec {
            service_key,
            local_address,
            protocol,
            enable_keep_alive,
            namespace,
            direct:
                DirectOptions {
                    allow_direct,
                    port_mapping,
                },
        } = spec;
        let mut store = self.load_client_configs();
        let now = SystemTime::now();

        let config = ClientConfigData {
            service_key: service_key.to_string(),
            local_address: local_address.to_string(),
            protocol: protocol.to_string(),
            enable_keep_alive: *enable_keep_alive,
            created_at: store
                .clients
                .get(service_key)
                .map_or(now, |saved| saved.created_at),
            namespace: *namespace,
            allow_direct: *allow_direct,
            port_mapping: *port_mapping,
        };

        store.clients.insert(service_key.to_string(), config);
        self.save_client_configs(&store)
    }

    pub fn delete_client_config(&self, service_key: &str) -> Result<(), CtlError> {
        let mut store = self.load_client_configs();
        store.clients.remove(service_key);
        self.save_client_configs(&store)
    }
}

/// The store saved at `path`, or an empty one when there is none or it does
/// not parse.
fn read_store<T: DeserializeOwned + Default>(path: &Path) -> T {
    fs::read_to_string(path)
        .ok()
        .and_then(|content| serde_json::from_str(&content).ok())
        .unwrap_or_default()
}

/// Saves `store` at `path` as pretty JSON; `noun` names it in the errors.
fn write_store<T: Serialize>(path: &Path, store: &T, noun: &str) -> Result<(), CtlError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| CtlError::io(format!("Failed to create config dir: {e}")))?;
    }

    let content = serde_json::to_string_pretty(store)
        .map_err(|e| CtlError::io(format!("Failed to serialize {noun}: {e}")))?;

    fs::write(path, content)
        .map_err(|e| CtlError::io(format!("Failed to write {noun} file: {e}")))?;
    Ok(())
}
