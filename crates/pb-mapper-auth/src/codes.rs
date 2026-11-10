//! The [`AuthFailure::code`](crate::AuthFailure::code) values this crate
//! returns.
//!
//! Callers branch on these, and they reach clients unchanged in relay
//! responses, so each value is a contract: it is spelled once, here.

pub const AUTH_STATE_UNAVAILABLE: &str = "auth_state_unavailable";
pub const AUTH_STATE_LOCKED: &str = "auth_state_locked";
pub const AUTH_INTERNAL_ERROR: &str = "auth_internal_error";

pub const ADMINISTRATOR_KEY_REQUIRED: &str = "administrator_key_required";
pub const ADMINISTRATOR_KEY_INVALID: &str = "administrator_key_invalid";
pub const ADMINISTRATOR_KEY_ROTATED: &str = "administrator_key_rotated";
pub const ADMINISTRATOR_KEY_EXISTS: &str = "administrator_key_exists";
pub const ADMINISTRATOR_KEY_STAGED: &str = "administrator_key_staged";
pub const ADMINISTRATOR_KEY_STATE_EXISTS: &str = "administrator_key_state_exists";
pub const ADMINISTRATOR_KEY_UNCHANGED: &str = "administrator_key_unchanged";

pub const ADMIN_PERMISSION_REQUIRED: &str = "admin_permission_required";
pub const ADMIN_REQUEST_REPLAYED: &str = "admin_request_replayed";
pub const ADMIN_REPLAY_CAPACITY_EXHAUSTED: &str = "admin_replay_capacity_exhausted";
pub const ADMIN_REQUEST_TIMESTAMP_INVALID: &str = "admin_request_timestamp_invalid";

pub const TEMPORARY_KEY_EXPIRED: &str = "temporary_key_expired";
pub const TEMPORARY_KEY_REVOKED: &str = "temporary_key_revoked";
pub const TEMPORARY_KEY_ROTATED: &str = "temporary_key_rotated";
pub const TEMPORARY_KEY_INVALID: &str = "temporary_key_invalid";
pub const TEMPORARY_KEY_INACTIVE: &str = "temporary_key_inactive";
pub const TEMPORARY_KEY_NOT_FOUND: &str = "temporary_key_not_found";
pub const TEMPORARY_KEY_GENERATION_MISMATCH: &str = "temporary_key_generation_mismatch";
pub const TEMPORARY_KEY_STORE_UNAVAILABLE: &str = "temporary_key_store_unavailable";
pub const TEMPORARY_KEY_CAPACITY_EXHAUSTED: &str = "temporary_key_capacity_exhausted";
pub const TEMPORARY_KEY_TTL_TOO_SHORT: &str = "temporary_key_ttl_too_short";
pub const TEMPORARY_KEY_TTL_TOO_LONG: &str = "temporary_key_ttl_too_long";
pub const TEMPORARY_KEY_LABEL_TOO_LONG: &str = "temporary_key_label_too_long";
pub const TEMPORARY_KEY_NOT_RENEWABLE: &str = "temporary_key_not_renewable";
pub const TEMPORARY_KEY_NOT_ACTIVE: &str = "temporary_key_not_active";

pub const NAMESPACE_SPACE_EXHAUSTED: &str = "namespace_space_exhausted";
pub const NAMESPACE_NOT_ALLOCATED: &str = "namespace_not_allocated";
pub const NAMESPACE_ALLOCATOR_UNAVAILABLE: &str = "namespace_allocator_unavailable";
