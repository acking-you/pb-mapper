//! FFI interface for pb-mapper UI.
#![allow(clippy::missing_safety_doc)]

mod client;
mod config;
mod handle;
mod logging;
mod response;
mod server;
mod service;
mod state;

// Re-export public FFI functions and handle type.
pub use client::{
    pb_mapper_connect_service, pb_mapper_delete_client_config, pb_mapper_disconnect_service,
    pb_mapper_get_client_configs_json, pb_mapper_get_client_status_json,
};
pub use config::{pb_mapper_get_config_json, pb_mapper_update_config};
pub use handle::{pb_mapper_create, pb_mapper_destroy, pb_mapper_set_app_dir, PbMapperHandle};
pub use logging::{pb_mapper_free_string, pb_mapper_init_logging, pb_mapper_set_log_callback};
pub use server::{
    pb_mapper_get_local_server_status_json, pb_mapper_get_server_status_detail_json,
    pb_mapper_start_server, pb_mapper_stop_server,
};
pub use service::{
    pb_mapper_delete_service_config, pb_mapper_get_service_configs_json,
    pb_mapper_get_service_status_json, pb_mapper_register_service, pb_mapper_unregister_service,
};
