//! Service names and the routing keys that scope them to a namespace.

use std::sync::Arc;

use pb_mapper_auth::AuthContext;

use crate::routing::ImutableKey;

/// The longest service name, in bytes; the rejection below spells it out.
pub(crate) const MAX_SERVICE_NAME_LEN: usize = 1024;

pub(crate) fn scoped_service_key(
    context: &AuthContext,
    namespace: u64,
    service_name: &str,
) -> std::result::Result<ImutableKey, pb_mapper_auth::AuthFailure> {
    validate_service_name(service_name)?;
    if !context.is_admin
        && (service_name.len() > 128
            || !service_name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._:-".contains(&byte)))
    {
        return Err(pb_mapper_auth::AuthFailure::new(
            "service_name_invalid",
            "temporary-key service names must be 1-128 ASCII bytes from [A-Za-z0-9._:-]",
            false,
        ));
    }
    Ok(compose_service_key(namespace, service_name))
}

/// Reject a service name that cannot become an unambiguous routing key.
///
/// NUL is the separator [`compose_service_key`] writes between the namespace
/// prefix and the name, so a name containing one could spell out another
/// namespace's key. Every path that takes a name from the wire checks this,
/// including an administrator naming a service to retire: the name would
/// otherwise reach a different service than the one the audit record names.
pub(crate) fn validate_service_name(
    service_name: &str,
) -> std::result::Result<(), pb_mapper_auth::AuthFailure> {
    if service_name.is_empty()
        || service_name.len() > MAX_SERVICE_NAME_LEN
        || service_name.contains('\0')
    {
        return Err(pb_mapper_auth::AuthFailure::new(
            "service_name_invalid",
            "service names must be 1-1024 bytes and must not contain NUL",
            false,
        ));
    }
    Ok(())
}

/// Build the routing-map key for a service without validating the name.
///
/// The inverse of [`split_scoped_service_key`], and the one place the scoped-key
/// format is written. Callers that accept a service name from the wire go through
/// [`scoped_service_key`] instead, which validates it first; this is for callers
/// naming a service that is already registered — an administrator retiring one,
/// or a status query for one.
pub(crate) fn compose_service_key(namespace: u64, service_name: &str) -> ImutableKey {
    if namespace == 0 {
        Arc::from(service_name)
    } else {
        Arc::from(format!("@{namespace:016x}\u{0}{service_name}"))
    }
}

pub(crate) fn split_scoped_service_key(key: &str) -> (u64, &str) {
    let Some((prefix, name)) = key.split_once('\0') else {
        return (0, key);
    };
    let Some(hex) = prefix.strip_prefix('@') else {
        return (0, key);
    };
    match u64::from_str_radix(hex, 16) {
        Ok(namespace) => (namespace, name),
        Err(_) => (0, key),
    }
}
