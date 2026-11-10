//! Client-owned direct sessions. Control sockets never own an established data task.
mod binding;
mod candidates;
mod conn;
mod consumer;
mod control;
mod endpoint;
mod identity;
mod lan;
mod link;
#[cfg(feature = "port-mapping")]
mod mapping;
mod provider;
mod reports;
mod socket;
mod tcp;

pub(crate) use consumer::Consumer;
use pb_mapper_protocol::direct::{SETUP_TTL_MS, SessionGrant};
pub(crate) use provider::{MAX_SESSIONS as MAX_PROVIDER_SESSIONS, Provider};

/// Whether a grant is for `key`'s service and of its kind, with a setup TTL
/// the protocol allows: what both ends check of every grant.
fn grant_fits(grant: &SessionGrant, key: &str, is_datagram: bool) -> bool {
    grant.service == key
        && grant.is_datagram == is_datagram
        && grant.ttl_ms != 0
        && grant.ttl_ms <= SETUP_TTL_MS
}

#[cfg(test)]
mod tests;
