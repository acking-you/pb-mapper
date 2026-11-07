//! FFI interface for pb-mapper UI.
#![allow(clippy::missing_safety_doc)]

mod callback;
mod cli;
mod client;
mod config;
mod ctl;
mod desktop;
#[cfg(target_os = "android")]
mod desktop_jni;
mod desktop_video;
mod disk_log;
mod error;
mod events;
mod galaxy;
mod handle;
mod logging;
mod relay;
mod response;
mod server;
mod service;
mod state;
mod tunnel_events;
mod update;

// Re-export public FFI functions and handle type.
use better_mimalloc_rs::MiMalloc;
pub use cli::{NOT_A_COMMAND, pb_mapper_cli_main};
pub use client::{
    pb_mapper_connect_service, pb_mapper_delete_client_config, pb_mapper_disconnect_service,
    pb_mapper_get_client_configs_json, pb_mapper_get_client_status_json,
};
pub use config::{
    pb_mapper_get_config_json, pb_mapper_reveal_isolated_admin_key, pb_mapper_update_config,
};
pub use desktop::{
    pb_mapper_desktop_call_json, pb_mapper_desktop_frame, pb_mapper_desktop_free_frame,
    pb_mapper_desktop_wait_frame,
};
#[cfg(target_os = "android")]
pub use desktop_jni::{
    Java_com_pb_1mapper_ui_DesktopVideoNative_attach,
    Java_com_pb_1mapper_ui_DesktopVideoNative_detach,
    Java_com_pb_1mapper_ui_DesktopVideoNative_instanceToken,
    Java_com_pb_1mapper_ui_DesktopVideoNative_keyframe,
    Java_com_pb_1mapper_ui_DesktopVideoNative_next,
    Java_com_pb_1mapper_ui_DesktopVideoNative_presented,
    Java_com_pb_1mapper_ui_DesktopVideoNative_report,
};
pub use desktop_video::{
    PbDesktopVideoMetaV1, pb_mapper_desktop_instance_token_v1, pb_mapper_desktop_video_attach_v1,
    pb_mapper_desktop_video_detach_v1, pb_mapper_desktop_video_keyframe_v1,
    pb_mapper_desktop_video_next_v1, pb_mapper_desktop_video_presented_v1,
    pb_mapper_desktop_video_report_v1,
};
#[cfg(feature = "desktop-test-source")]
pub use desktop_video::{
    pb_mapper_desktop_video_test_source_pause_v1, pb_mapper_desktop_video_test_source_stop_v1,
    pb_mapper_desktop_video_test_source_v1,
};
pub use events::pb_mapper_set_change_callback;
pub use galaxy::pb_mapper_galaxy_frame_v1;
pub use handle::{
    PbMapperHandle, pb_mapper_create, pb_mapper_destroy, pb_mapper_set_app_dir,
    pb_mapper_start_control_server,
};
pub use logging::{
    pb_mapper_free_string, pb_mapper_init_logging, pb_mapper_set_log_callback,
    pb_mapper_shutdown_logging,
};
pub use relay::pb_mapper_relay_call_json;
pub use server::{
    pb_mapper_force_refresh_server_status_json, pb_mapper_get_local_server_status_json,
    pb_mapper_get_server_status_detail_json, pb_mapper_start_server, pb_mapper_stop_server,
};
pub use service::{
    pb_mapper_delete_service_config, pb_mapper_get_service_configs_json,
    pb_mapper_get_service_status_json, pb_mapper_register_service, pb_mapper_unregister_service,
};
pub use tunnel_events::pb_mapper_tunnel_events_call_json;
pub use update::pb_mapper_ed25519_verify_v1;

#[global_allocator]
static GLOBAL_ALLOCATOR: MiMalloc = MiMalloc;
