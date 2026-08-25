//! Administrator RPCs: the credential lifecycle, the relay's auth state, and
//! the service and connection inventories.
//!
//! Split three ways — this file is the RPC surface, [`types`] holds the owned
//! response types and their wire conversions, and [`transport`] owns the
//! single-shot session each request runs over.

mod transport;
mod types;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use pb_mapper_auth::{
    discard_staged_admin_key, generate_admin_key, stage_admin_key_candidate, write_admin_key_file,
};
use pb_mapper_core::checksum::parse_credential;
use pb_mapper_core::config::control_io_timeout;
// The largest page the relay will serve, taken from the relay's own definition
// rather than restated here: an oversized request is rejected locally so it fails
// with a clear message instead of a protocol error, and that check has to agree
// with what the relay clamps to.
use pb_mapper_core::paging::MAX_PAGE_SIZE;
use pb_mapper_protocol::command::{AdminRequest, AdminResponse};

use self::transport::send_admin_request;
use self::types::Paged;
use super::Error;
use super::client::ClientInner;
use super::error::Result;
use super::types::LegacyProtocol;

pub use self::types::{
    AuthStatusInfo, ConnectionInfo, ConnectionPage, IssuedKey, KeyListPage, KeyMetadata,
    ServiceInfo, ServicePage,
};

/// Page size used by the `*_all` helpers, which page on the caller's behalf.
///
/// The largest page the relay serves, not a smaller round number, for two
/// reasons. A relay configured up to `MAX_TEMP_KEY_CAPACITY` (1,048,576) holds
/// more credentials than [`MAX_PAGES`] pages of 100 could carry, so `*_all`
/// would fail on a full inventory instead of returning it. And the relay
/// re-collects and re-sorts its whole table for every page it serves, so the
/// page count is what that work is multiplied by.
const COLLECT_PAGE_SIZE: u16 = MAX_PAGE_SIZE;

/// A cap on `*_all` pagination, so a relay that keeps handing back a `next_page`
/// cursor cannot spin the caller forever.
///
/// Ample at [`COLLECT_PAGE_SIZE`]: the relay's hard capacity needs 1,049 pages.
const MAX_PAGES: u32 = 10_000;

/// Administrator RPCs. Constructed via [`super::Client::admin`].
#[derive(Clone)]
pub struct Admin {
    pub(crate) inner: Arc<ClientInner>,
}

/// Declares an RPC that sends one request and expects one response variant.
///
/// Most administrator calls are exactly that: build the request, match the one
/// response that answers it, and convert. Written out, each is five lines of
/// which only two say anything, and the `unexpected` arm is easy to get subtly
/// wrong — naming the wrong expected variant in the error text.
macro_rules! admin_rpc {
    (
        $(#[$doc:meta])*
        $name:ident($($arg:ident: $arg_ty:ty),* $(,)?)
            -> $output:ty,
        request: $request:expr,
        response: $variant:ident($binding:pat) => $value:expr
    ) => {
        $(#[$doc])*
        pub async fn $name(&self, $($arg: $arg_ty),*) -> Result<$output> {
            match self.request($request).await? {
                AdminResponse::$variant($binding) => Ok($value),
                other => unexpected(stringify!($variant), &other),
            }
        }
    };
}

/// The struct-variant form of [`admin_rpc`], for the two responses that carry
/// named fields rather than a payload.
macro_rules! admin_rpc_struct {
    (
        $(#[$doc:meta])*
        $name:ident($($arg:ident: $arg_ty:ty),* $(,)?)
            -> $output:ty,
        request: $request:expr,
        response: $variant:ident { $($binding:tt)* } => $value:expr
    ) => {
        $(#[$doc])*
        pub async fn $name(&self, $($arg: $arg_ty),*) -> Result<$output> {
            match self.request($request).await? {
                AdminResponse::$variant { $($binding)* } => Ok($value),
                other => unexpected(stringify!($variant), &other),
            }
        }
    };
}

impl Admin {
    /// One-shot administrator RPC. CLI output rendering can call this directly.
    pub async fn request(&self, request: AdminRequest) -> Result<AdminResponse> {
        self.request_with_timeout(request, control_io_timeout())
            .await
    }

    /// [`Self::request`] with an explicit I/O bound, covering the connect as well
    /// as the exchange.
    pub async fn request_with_timeout(
        &self,
        request: AdminRequest,
        io_timeout: Duration,
    ) -> Result<AdminResponse> {
        send_admin_request(&self.inner.server, self.credential(), request, io_timeout).await
    }

    admin_rpc!(
        /// Mint a temporary credential valid for `ttl`.
        issue_key(ttl: Duration, label: Option<String>) -> IssuedKey,
        request: AdminRequest::KeyIssue { ttl_seconds: ttl.as_secs(), label },
        response: KeyIssued(issued) => IssuedKey::from(issued)
    );

    admin_rpc!(
        /// Metadata for one credential, without its secret.
        show_key(key_id: u64) -> IssuedKey,
        request: AdminRequest::KeyShow { key_id },
        response: KeyShown(issued) => IssuedKey::from(issued)
    );

    admin_rpc!(
        /// Metadata for one credential, *with* its secret.
        reveal_key(key_id: u64) -> IssuedKey,
        request: AdminRequest::KeyReveal { key_id },
        response: KeyShown(issued) => IssuedKey::from(issued)
    );

    admin_rpc!(
        /// Extend a credential's lifetime to `ttl` from now.
        renew_key(key_id: u64, ttl: Duration) -> IssuedKey,
        request: AdminRequest::KeyRenew { key_id, ttl_seconds: ttl.as_secs() },
        response: KeyRenewed(issued) => IssuedKey::from(issued)
    );

    admin_rpc!(
        /// Revoke a credential, taking effect on the relay immediately.
        revoke_key(key_id: u64) -> KeyMetadata,
        request: AdminRequest::KeyRevoke { key_id },
        response: KeyRevoked(meta) => KeyMetadata::from(meta)
    );

    admin_rpc!(
        /// The relay's credential-subsystem snapshot.
        auth_status() -> AuthStatusInfo,
        request: AdminRequest::AuthStatus,
        response: AuthStatus(status) => AuthStatusInfo::from(status)
    );

    admin_rpc_struct!(
        /// Drop expired and revoked credentials, returning how many were removed.
        gc_keys() -> u64,
        request: AdminRequest::KeyGc,
        response: KeyGc { removed } => removed
    );

    admin_rpc_struct!(
        /// Erase the relay's credential state. Every temporary credential stops
        /// authenticating; the administrator key is unaffected.
        reset_auth_state() -> (),
        request: AdminRequest::AuthStateReset { confirm: true },
        response: Ok { .. } => ()
    );

    admin_rpc_struct!(
        /// Allow or deny pre-v2 framing on new connections.
        set_legacy_protocol(policy: LegacyProtocol) -> (),
        request: AdminRequest::LegacyProtocolSet { policy: policy.into() },
        response: Ok { .. } => ()
    );

    admin_rpc!(
        /// One page of temporary credentials.
        list_keys(page: u32, page_size: u16) -> KeyListPage,
        request: AdminRequest::KeyList { page, page_size: validate_page_size(page_size)? },
        response: KeyList(page) => KeyListPage::from(page)
    );

    admin_rpc!(
        /// One page of registered services, optionally scoped to one credential.
        list_services(key_id: Option<u64>, page: u32, page_size: u16) -> ServicePage,
        request: AdminRequest::ServiceList {
            key_id,
            page,
            page_size: validate_page_size(page_size)?,
        },
        response: Services(page) => ServicePage::from(page)
    );

    admin_rpc!(
        /// One page of live connections, optionally scoped to one credential.
        list_connections(key_id: Option<u64>, page: u32, page_size: u16) -> ConnectionPage,
        request: AdminRequest::ConnectionList {
            key_id,
            page,
            page_size: validate_page_size(page_size)?,
        },
        response: Connections(page) => ConnectionPage::from(page)
    );

    admin_rpc_struct!(
        /// Drop registered control connections the relay is still holding for a
        /// service, returning how many it dropped.
        ///
        /// `conn_id` of `None` retires every connection the service has, which is
        /// what frees a connection quota filled by connections that should have
        /// gone away. A registration whose client is still healthy simply
        /// reconnects, so this is a nudge rather than a shutdown.
        retire_connections(key_id: Option<u64>, service_name: String, conn_id: Option<u32>) -> u32,
        request: AdminRequest::ConnectionRetire { key_id, service_name, conn_id },
        response: ConnectionsRetired { retired } => retired
    );

    /// Every temporary credential, paging until the relay stops handing back a
    /// cursor.
    pub async fn list_keys_all(&self) -> Result<Vec<KeyMetadata>> {
        collect_pages(|page| self.list_keys(page, COLLECT_PAGE_SIZE)).await
    }

    /// Every registered service, optionally scoped to one credential.
    pub async fn list_services_all(&self, key_id: Option<u64>) -> Result<Vec<ServiceInfo>> {
        collect_pages(|page| self.list_services(key_id, page, COLLECT_PAGE_SIZE)).await
    }

    /// Every live connection, optionally scoped to one credential.
    pub async fn list_connections_all(&self, key_id: Option<u64>) -> Result<Vec<ConnectionInfo>> {
        collect_pages(|page| self.list_connections(key_id, page, COLLECT_PAGE_SIZE)).await
    }

    /// Rotate the relay's administrator key, returning the key now in force.
    ///
    /// Generates a key when `new_key` is `None`. Rotation is not idempotent, so a
    /// caller who never learns the outcome cannot retry safely: if the relay
    /// committed the change and the response was lost, only the new key still
    /// authenticates. When the SDK generated that key, losing it locks the
    /// operator out — so any inconclusive result is reported as
    /// [`Error::RootRotationUncertain`], which carries the candidate. A caller
    /// who supplied `new_key` already holds it and gets the underlying error
    /// unchanged.
    ///
    /// Prefer [`Admin::rotate_root_key_to_file`], which persists the candidate
    /// before the request is ever sent.
    pub async fn rotate_root_key(&self, new_key: Option<String>) -> Result<String> {
        let caller_supplied = new_key.is_some();
        let new_key = new_key.unwrap_or_else(generate_admin_key);
        let parsed = parse_credential(new_key.trim()).map_err(Error::invalid_config)?;
        if !parsed.is_admin() {
            return Err(Error::invalid_config(
                "root rotation requires a 32-byte administrator key",
            ));
        }
        // A generated key exists nowhere but this frame, so every exit that
        // leaves the relay's state unknown has to hand it back.
        let preserve = |error: Error| {
            if caller_supplied {
                return error;
            }
            Error::RootRotationUncertain {
                candidate: new_key.clone(),
                message: error.to_string(),
            }
        };
        let response = self
            .request(AdminRequest::RootKeyRotate {
                new_admin_key: new_key.clone(),
            })
            .await
            .map_err(preserve)?;
        match response {
            AdminResponse::Ok { .. } => {
                *self
                    .inner
                    .credential
                    .write()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = parsed;
                Ok(new_key)
            }
            // A response that is not `Ok` still came from the relay, but nothing
            // here proves the rotation did not take effect.
            other => unexpected::<String>("Ok", &other).map_err(preserve),
        }
    }

    /// Rotate the root key and persist it to `path`.
    ///
    /// The candidate is staged at a sibling file before the request is sent, and
    /// `path` is only replaced once the relay has accepted the rotation and the new
    /// key has passed a post-rotation status check. A rotation that fails after the
    /// relay may have committed it therefore leaves both keys on disk rather than a
    /// `path` holding a key the relay never installed.
    ///
    /// An unresolved candidate from an earlier attempt is never overwritten — see
    /// [`stage_admin_key_candidate`] — so this fails until the operator establishes
    /// which of the two keys the relay accepts. Retrying with the same `new_key`
    /// is allowed.
    pub async fn rotate_root_key_to_file(
        &self,
        path: &Path,
        new_key: Option<String>,
    ) -> Result<String> {
        let new_key = new_key.unwrap_or_else(generate_admin_key);
        let staged_path = stage_admin_key_candidate(path, &new_key).map_err(auth_file_error)?;
        // Every failure past this point leaves the candidate on disk, and has to
        // say so: it may be the key the relay is now running on.
        let staged_note = || format!("the candidate key remains at `{}`", staged_path.display());

        let rotated = self.rotate_root_key(Some(new_key)).await.map_err(|error| {
            auth_file_message(format!(
                "root rotation request failed; {}: {error}",
                staged_note()
            ))
        })?;
        // `rotate_root_key` already swapped the in-memory credential, so this
        // proves the relay authenticates the key we are about to persist.
        self.auth_status().await.map_err(|error| {
            auth_file_message(format!(
                "new administrator key did not pass the post-rotation status check; {}: {error}",
                staged_note()
            ))
        })?;
        write_admin_key_file(path, &rotated, true).map_err(|error| {
            auth_file_message(format!(
                "administrator key rotated and verified, but `{}` could not be updated; recover \
                 the key from `{}`: {error}",
                path.display(),
                staged_path.display()
            ))
        })?;
        discard_staged_admin_key(&staged_path);
        Ok(rotated)
    }

    fn credential(&self) -> pb_mapper_core::checksum::Credential {
        *self
            .inner
            .credential
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Lift an administrator key-file failure into the SDK's error type.
fn auth_file_error(error: pb_mapper_auth::AuthFailure) -> Error {
    auth_file_message(error.to_string())
}

fn auth_file_message(message: impl Into<String>) -> Error {
    Error::AuthFile {
        message: message.into(),
    }
}

fn validate_page_size(page_size: u16) -> Result<u16> {
    if !(1..=MAX_PAGE_SIZE).contains(&page_size) {
        return Err(Error::invalid_config(format!(
            "page_size must be between 1 and {MAX_PAGE_SIZE}"
        )));
    }
    Ok(page_size)
}

fn unexpected<T>(expected: &str, actual: &AdminResponse) -> Result<T> {
    Err(Error::protocol(format!(
        "expected {expected}, got {actual:?}"
    )))
}

/// Drain a paginated listing into one vector.
///
/// Bounded by [`MAX_PAGES`]: the cursor comes from the relay, and a listing that
/// never terminates would otherwise hang the caller with no way to tell why.
async fn collect_pages<P, F, Fut>(mut fetch: F) -> Result<Vec<P::Item>>
where
    P: Paged,
    F: FnMut(u32) -> Fut,
    Fut: std::future::Future<Output = Result<P>>,
{
    let mut page = 0_u32;
    let mut items = Vec::new();
    for _ in 0..MAX_PAGES {
        let (chunk, next) = fetch(page).await?.into_parts();
        items.extend(chunk);
        match next {
            Some(next_page) => page = next_page,
            None => return Ok(items),
        }
    }
    Err(Error::protocol(format!(
        "pagination exceeded {MAX_PAGES} pages"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sdk::admin::types::KeyListPage;

    fn page(next_page: Option<u32>, ids: &[u64]) -> KeyListPage {
        KeyListPage {
            schema_version: 1,
            items: ids
                .iter()
                .map(|&key_id| KeyMetadata {
                    key_id,
                    state: "active".into(),
                    issued_at: 0,
                    expires_at: 0,
                    label: None,
                })
                .collect(),
            next_page,
        }
    }

    /// The `*_all` helpers have to be able to drain a relay filled to its hard
    /// capacity. At a smaller page size they cannot: 1,048,576 credentials over
    /// pages of 100 is 10,486 pages, past the [`MAX_PAGES`] guard, so a full
    /// inventory would come back as a pagination error instead of a listing.
    #[test]
    fn the_collect_page_size_can_drain_a_relay_at_capacity() {
        assert_eq!(
            COLLECT_PAGE_SIZE, MAX_PAGE_SIZE,
            "paging below the relay's maximum multiplies its per-page re-sort \
             and shrinks what `*_all` can drain"
        );
        let pages_at_capacity =
            pb_mapper_auth::MAX_TEMP_KEY_CAPACITY.div_ceil(usize::from(COLLECT_PAGE_SIZE));
        assert!(
            pages_at_capacity <= MAX_PAGES as usize,
            "a full inventory needs {pages_at_capacity} pages, over the {MAX_PAGES} cap"
        );
    }

    /// `validate_page_size` rejects locally, so an oversized request never
    /// reaches the relay to come back as an opaque protocol error.
    #[test]
    fn a_page_size_outside_the_relays_range_is_rejected_locally() {
        assert!(validate_page_size(0).is_err());
        assert!(validate_page_size(MAX_PAGE_SIZE + 1).is_err());
        assert_eq!(validate_page_size(1).unwrap(), 1);
        assert_eq!(validate_page_size(MAX_PAGE_SIZE).unwrap(), MAX_PAGE_SIZE);
    }

    #[tokio::test]
    async fn collect_pages_follows_the_cursor_and_concatenates() {
        let requested = std::sync::Mutex::new(Vec::new());
        let items = collect_pages::<KeyListPage, _, _>(|page_number| {
            requested.lock().expect("not poisoned").push(page_number);
            async move {
                Ok(match page_number {
                    0 => page(Some(7), &[1, 2]),
                    7 => page(Some(9), &[3]),
                    _ => page(None, &[4]),
                })
            }
        })
        .await
        .expect("three pages is well inside the cap");

        assert_eq!(
            items.iter().map(|item| item.key_id).collect::<Vec<_>>(),
            vec![1, 2, 3, 4]
        );
        assert_eq!(
            *requested.lock().expect("not poisoned"),
            vec![0, 7, 9],
            "the cursor the relay hands back is what gets asked for next"
        );
    }

    /// A relay that always hands back a cursor must not spin the caller: the
    /// cursor is remote input, so the loop is bounded here rather than trusted.
    #[tokio::test]
    async fn collect_pages_gives_up_on_a_cursor_that_never_ends() {
        let calls = std::sync::atomic::AtomicU32::new(0);
        let error = collect_pages::<KeyListPage, _, _>(|page_number| {
            calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            async move { Ok(page(Some(page_number + 1), &[u64::from(page_number)])) }
        })
        .await
        .expect_err("an endless cursor must fail rather than hang");

        assert!(error.to_string().contains("pagination exceeded"));
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::Relaxed),
            MAX_PAGES,
            "the cap is what stops it, and it stops exactly there"
        );
    }
}
