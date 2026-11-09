//! The helper service's own host: configured by the app, owned by the
//! service.
pub use crate::platform::windows::helper::background::{
    Options, Settings, Snapshot, configure, configure_with_identity, end_session, revoke_scope,
    set_control_allowed, status, stop,
};
