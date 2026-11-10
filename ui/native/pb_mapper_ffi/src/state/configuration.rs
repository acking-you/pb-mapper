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
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::SystemTime;

use parking_lot::Mutex as SyncMutex;
use pb_mapper_core::checksum::set_process_msg_header_key;
use tokio::sync::RwLock;

use super::stored::normalize_msg_header_key;
use super::{
    AppConfig, ClientConfigData, ClientConfigStore, DirectOptions, LocalServerStatus,
    PbMapperState, ServiceConfigData, ServiceConfigStore,
};
use crate::error::CtlError;

impl PbMapperState {
    pub(crate) fn desktop_config_dir(&self) -> PathBuf {
        self.config_dir.clone()
    }
    pub(super) async fn reset_status_caches(&self) {
        {
            let mut cache = self.local_server_status_cache.write().await;
            *cache = LocalServerStatus::default();
        }
        {
            let mut last_update = self.local_server_status_last_update.write().await;
            *last_update = None;
        }
        self.local_server_status_refreshing
            .store(false, Ordering::Release);
    }
    /// A state that has read nothing: the default configuration and no
    /// credential, until `set_app_directory_path` names its directory. Every
    /// client sends that first (the app an empty path on desktop, meaning the
    /// platform's own), so the FFI handle starts here rather than reading a
    /// directory it may never be pointed at.
    pub fn unloaded() -> Self {
        let local_server_status_cache = Arc::new(RwLock::new(LocalServerStatus::default()));
        Self {
            server_handle: None,
            server_auth: None,
            server_shutdown_token: None,
            server_status_sender: None,
            server_start_time: None,
            server_port: None,
            server_quic_port: None,
            relay_client: SyncMutex::new(None),
            quic_offer: None,
            service_runtime: HashMap::new(),
            client_runtime: HashMap::new(),
            config: AppConfig::default(),
            config_dir: Self::get_config_dir(&None),
            app_directory_path: None,
            local_server_status_cache,
            local_server_status_last_update: Arc::new(RwLock::new(None)),
            local_server_status_refreshing: Arc::new(AtomicBool::new(false)),
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

    #[allow(unused_variables)]
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
        let path = self.get_service_config_path();
        match fs::read_to_string(&path) {
            Ok(content) => serde_json::from_str(&content).unwrap_or_else(|_| ServiceConfigStore {
                services: HashMap::new(),
            }),
            Err(_) => ServiceConfigStore {
                services: HashMap::new(),
            },
        }
    }

    pub fn save_service_configs(&self, store: &ServiceConfigStore) -> Result<(), CtlError> {
        let path = self.get_service_config_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| CtlError::io(format!("Failed to create config dir: {e}")))?;
        }

        let content = serde_json::to_string_pretty(store)
            .map_err(|e| CtlError::io(format!("Failed to serialize config: {e}")))?;

        fs::write(&path, content)
            .map_err(|e| CtlError::io(format!("Failed to write config file: {e}")))?;
        Ok(())
    }

    pub fn save_service_config(
        &self,
        service_key: &str,
        local_address: &str,
        protocol: &str,
        enable_encryption: bool,
        enable_keep_alive: bool,
        direct: DirectOptions,
    ) -> Result<(), CtlError> {
        let DirectOptions {
            allow_direct,
            port_mapping,
        } = direct;
        let mut store = self.load_service_configs();
        let now = SystemTime::now();

        let config = ServiceConfigData {
            service_key: service_key.to_string(),
            local_address: local_address.to_string(),
            protocol: protocol.to_string(),
            enable_encryption,
            enable_keep_alive,
            created_at: if store.services.contains_key(service_key) {
                store.services[service_key].created_at
            } else {
                now
            },
            allow_direct,
            port_mapping,
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
        let path = self.get_client_config_path();
        match fs::read_to_string(&path) {
            Ok(content) => serde_json::from_str(&content).unwrap_or_else(|_| ClientConfigStore {
                clients: HashMap::new(),
            }),
            Err(_) => ClientConfigStore {
                clients: HashMap::new(),
            },
        }
    }

    pub fn save_client_configs(&self, store: &ClientConfigStore) -> Result<(), CtlError> {
        let path = self.get_client_config_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| CtlError::io(format!("Failed to create config dir: {e}")))?;
        }

        let content = serde_json::to_string_pretty(store)
            .map_err(|e| CtlError::io(format!("Failed to serialize client config: {e}")))?;

        fs::write(&path, content)
            .map_err(|e| CtlError::io(format!("Failed to write client config file: {e}")))?;
        Ok(())
    }

    pub fn save_client_config(
        &self,
        service_key: &str,
        local_address: &str,
        protocol: &str,
        enable_keep_alive: bool,
        namespace: Option<u64>,
        direct: DirectOptions,
    ) -> Result<(), CtlError> {
        let DirectOptions {
            allow_direct,
            port_mapping,
        } = direct;
        let mut store = self.load_client_configs();
        let now = SystemTime::now();

        let config = ClientConfigData {
            service_key: service_key.to_string(),
            local_address: local_address.to_string(),
            protocol: protocol.to_string(),
            enable_keep_alive,
            created_at: if store.clients.contains_key(service_key) {
                store.clients[service_key].created_at
            } else {
                now
            },
            namespace,
            allow_direct,
            port_mapping,
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
