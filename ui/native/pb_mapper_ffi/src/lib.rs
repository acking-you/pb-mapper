//! FFI interface for pb-mapper UI.
#![allow(clippy::missing_safety_doc)]

mod callback;
mod cli;
mod client;
mod config;
mod ctl;
mod error;
mod events;
mod handle;
mod logging;
mod response;
mod server;
mod service;
mod state;

// Re-export public FFI functions and handle type.
use better_mimalloc_rs::MiMalloc;
pub use cli::{pb_mapper_cli_main, NOT_A_COMMAND};
pub use client::{
    pb_mapper_connect_service, pb_mapper_delete_client_config, pb_mapper_disconnect_service,
    pb_mapper_get_client_configs_json, pb_mapper_get_client_status_json,
};
pub use config::{pb_mapper_get_config_json, pb_mapper_update_config};
pub use events::pb_mapper_set_change_callback;
pub use handle::{
    pb_mapper_create, pb_mapper_destroy, pb_mapper_set_app_dir, pb_mapper_start_control_server,
    PbMapperHandle,
};
pub use logging::{pb_mapper_free_string, pb_mapper_init_logging, pb_mapper_set_log_callback};
pub use server::{
    pb_mapper_force_refresh_server_status_json, pb_mapper_get_local_server_status_json,
    pb_mapper_get_server_status_detail_json, pb_mapper_start_server, pb_mapper_stop_server,
};
pub use service::{
    pb_mapper_delete_service_config, pb_mapper_get_service_configs_json,
    pb_mapper_get_service_status_json, pb_mapper_register_service, pb_mapper_unregister_service,
};

#[global_allocator]
static GLOBAL_ALLOCATOR: MiMalloc = MiMalloc;
