//! Owned mirrors of the wire types in [`pb_mapper_protocol::command`] and
//! [`pb_mapper_auth`].
//!
//! The SDK does not re-export the wire types: they carry serde derives and
//! protocol details a caller has no reason to depend on, and pinning them into
//! the public API would make every framing change a breaking one. These are
//! plain data, with a `From` impl per type as the single translation point.

use pb_mapper_auth::{AuthStatus, IssuedTemporaryKey, KeyPage, TemporaryKeyMetadata};
use pb_mapper_protocol::command::{
    AdminConnectionInfo, AdminConnectionPage, AdminServiceInfo, AdminServicePage,
};

use super::super::types::LegacyProtocol;

/// A temporary credential, with the secret the relay just minted.
///
/// `credential` is empty on every response but issue, renew, and reveal — the
/// relay does not hand back key material it has already delivered once.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IssuedKey {
    pub key_id: u64,
    pub state: String,
    pub issued_at: u64,
    pub expires_at: u64,
    pub label: Option<String>,
    pub credential: String,
}

/// A temporary credential without its secret.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KeyMetadata {
    pub key_id: u64,
    pub state: String,
    pub issued_at: u64,
    pub expires_at: u64,
    pub label: Option<String>,
}

/// A registered service as the relay sees it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServiceInfo {
    pub key_id: u64,
    pub namespace: u64,
    pub service_name: String,
    pub transport: String,
    pub codec_enabled: bool,
    pub connection_count: u32,
}

/// One control connection the relay holds for a registered service.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConnectionInfo {
    pub key_id: u64,
    pub namespace: u64,
    pub service_name: String,
    pub conn_id: u32,
    pub generation: u64,
    pub protocol_version: u16,
    pub healthy: bool,
    pub transport: String,
    pub codec_enabled: bool,
    pub last_rx_age_ms: u64,
}

/// Relay-side snapshot of the credential subsystem.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthStatusInfo {
    pub schema_version: u16,
    pub safe_mode: bool,
    pub capacity: usize,
    pub active_keys: usize,
    pub expired_keys: usize,
    pub revoked_keys: usize,
    pub legacy_protocol: LegacyProtocol,
    pub active_legacy_connections: u64,
    pub last_legacy_connection_at: Option<u64>,
    pub auth_successes: u64,
    pub auth_failures: u64,
    pub server_instance_id: String,
}

/// One page of a paginated administrator listing, as
/// [`super::collect_pages`] consumes it.
pub(super) trait Paged {
    type Item;

    /// This page's items, and the cursor for the next page if there is one.
    fn into_parts(self) -> (Vec<Self::Item>, Option<u32>);
}

/// Declares one paginated listing.
///
/// The three listings differ only in their item type: same schema version, same
/// items vector, same `next_page` cursor that is `None` on the last page.
macro_rules! admin_page {
    ($(#[$doc:meta])* $name:ident of $item:ty) => {
        $(#[$doc])*
        #[derive(Clone, Debug, Eq, PartialEq)]
        pub struct $name {
            pub schema_version: u16,
            pub items: Vec<$item>,
            /// The page to ask for next, or `None` when this was the last one.
            pub next_page: Option<u32>,
        }

        impl Paged for $name {
            type Item = $item;

            fn into_parts(self) -> (Vec<Self::Item>, Option<u32>) {
                (self.items, self.next_page)
            }
        }
    };
}

admin_page!(
    /// One page of temporary credentials.
    KeyListPage of KeyMetadata
);
admin_page!(
    /// One page of registered services.
    ServicePage of ServiceInfo
);
admin_page!(
    /// One page of live service connections.
    ConnectionPage of ConnectionInfo
);

/// Converts a wire page into its SDK mirror, mapping each item on the way.
macro_rules! impl_page_from {
    ($wire:ty => $owned:ty, $item:ty) => {
        impl From<$wire> for $owned {
            fn from(value: $wire) -> Self {
                Self {
                    schema_version: value.schema_version,
                    items: value.items.into_iter().map(<$item>::from).collect(),
                    next_page: value.next_page,
                }
            }
        }
    };
}

impl_page_from!(KeyPage => KeyListPage, KeyMetadata);
impl_page_from!(AdminServicePage => ServicePage, ServiceInfo);
impl_page_from!(AdminConnectionPage => ConnectionPage, ConnectionInfo);

impl From<IssuedTemporaryKey> for IssuedKey {
    fn from(value: IssuedTemporaryKey) -> Self {
        Self {
            key_id: value.metadata.key_id.as_u64(),
            state: value.metadata.state,
            issued_at: value.metadata.issued_at,
            expires_at: value.metadata.expires_at,
            label: value.metadata.label,
            credential: value.credential,
        }
    }
}

impl From<TemporaryKeyMetadata> for KeyMetadata {
    fn from(value: TemporaryKeyMetadata) -> Self {
        Self {
            key_id: value.key_id.as_u64(),
            state: value.state,
            issued_at: value.issued_at,
            expires_at: value.expires_at,
            label: value.label,
        }
    }
}

impl From<AdminServiceInfo> for ServiceInfo {
    fn from(value: AdminServiceInfo) -> Self {
        Self {
            key_id: value.key_id,
            namespace: value.namespace,
            service_name: value.service_name,
            transport: value.transport,
            codec_enabled: value.codec_enabled,
            connection_count: value.connection_count,
        }
    }
}

impl From<AdminConnectionInfo> for ConnectionInfo {
    fn from(value: AdminConnectionInfo) -> Self {
        Self {
            key_id: value.key_id,
            namespace: value.namespace,
            service_name: value.service_name,
            conn_id: value.conn_id,
            generation: value.generation,
            protocol_version: value.protocol_version,
            healthy: value.healthy,
            transport: value.transport,
            codec_enabled: value.codec_enabled,
            last_rx_age_ms: value.last_rx_age_ms,
        }
    }
}

impl From<AuthStatus> for AuthStatusInfo {
    fn from(value: AuthStatus) -> Self {
        Self {
            schema_version: value.schema_version,
            safe_mode: value.safe_mode,
            capacity: value.capacity,
            active_keys: value.active_keys,
            expired_keys: value.expired_keys,
            revoked_keys: value.revoked_keys,
            legacy_protocol: value.legacy_protocol.into(),
            active_legacy_connections: value.active_legacy_connections,
            last_legacy_connection_at: value.last_legacy_connection_at,
            auth_successes: value.auth_successes,
            auth_failures: value.auth_failures,
            server_instance_id: value.server_instance_id,
        }
    }
}
