//! The public configuration, error, and administrator wire types.
use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LegacyProtocolPolicy {
    Allow,
    Deny,
}

impl LegacyProtocolPolicy {
    pub fn is_allowed(self) -> bool {
        matches!(self, Self::Allow)
    }
}

#[derive(Clone, Debug)]
pub struct AuthConfig {
    pub state_dir: PathBuf,
    pub max_temporary_keys: usize,
    pub max_temporary_key_ttl: Duration,
    pub legacy_protocol: LegacyProtocolPolicy,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AuthFailure {
    pub code: String,
    pub message: String,
    pub retryable: bool,
}

impl AuthFailure {
    pub fn new(code: impl Into<String>, message: impl Into<String>, retryable: bool) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            retryable,
        }
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(codes::AUTH_INTERNAL_ERROR, message, false)
    }

    // The failures more than one check reports, built in one place each.

    pub(crate) fn key_expired() -> Self {
        Self::new(
            codes::TEMPORARY_KEY_EXPIRED,
            "temporary key has expired",
            false,
        )
    }

    pub(crate) fn key_revoked() -> Self {
        Self::new(
            codes::TEMPORARY_KEY_REVOKED,
            "temporary key was revoked",
            false,
        )
    }

    pub(crate) fn key_rotated() -> Self {
        Self::new(
            codes::TEMPORARY_KEY_ROTATED,
            "temporary credential was invalidated by administrator root rotation or auth-state reset",
            false,
        )
    }

    pub(crate) fn key_material_mismatch() -> Self {
        Self::new(
            codes::TEMPORARY_KEY_INVALID,
            "temporary credential does not match the active relay key material",
            false,
        )
    }

    /// A key that looked active a moment earlier no longer is, so a retry sees
    /// how it ended. Distinct from the "credential lease ..." failures an
    /// [`AuthContext`] reports.
    pub(crate) fn key_lease_inactive() -> Self {
        Self::new(
            codes::TEMPORARY_KEY_INACTIVE,
            "temporary key lease is no longer active",
            true,
        )
    }

    pub(crate) fn state_manager_stopped() -> Self {
        Self::new(
            codes::AUTH_STATE_UNAVAILABLE,
            "authentication state manager is not running",
            true,
        )
    }
}

impl fmt::Display for AuthFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for AuthFailure {}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TemporaryKeyMetadata {
    pub key_id: KeyId,
    pub state: String,
    pub issued_at: u64,
    pub expires_at: u64,
    pub label: Option<String>,
    /// The namespace this key was issued into. Absent for a key that owns the one
    /// its id names, which is also how a relay without namespaces reports every
    /// key; [`Self::effective_namespace`] covers both.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<u64>,
}

impl TemporaryKeyMetadata {
    /// The namespace this key's credential operates in.
    pub fn effective_namespace(&self) -> u64 {
        self.key_id.namespace(self.namespace)
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct IssuedTemporaryKey {
    #[serde(flatten)]
    pub metadata: TemporaryKeyMetadata,
    /// The secret itself, or empty where the relay did not reveal it. Left out
    /// of `Debug`, as `Credential`'s own is.
    pub credential: String,
}

impl fmt::Debug for IssuedTemporaryKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self {
            metadata,
            credential,
        } = self;
        // Whether there is one, which is all a log needs.
        let credential = if credential.is_empty() {
            ""
        } else {
            "[redacted]"
        };
        formatter
            .debug_struct("IssuedTemporaryKey")
            .field("metadata", metadata)
            .field("credential", &credential)
            .finish()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KeyPage {
    pub schema_version: u16,
    pub items: Vec<TemporaryKeyMetadata>,
    pub next_page: Option<u32>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AuthStatus {
    pub schema_version: u16,
    pub safe_mode: bool,
    pub capacity: usize,
    pub active_keys: usize,
    pub expired_keys: usize,
    pub revoked_keys: usize,
    pub legacy_protocol: LegacyProtocolPolicy,
    pub active_legacy_connections: u64,
    pub last_legacy_connection_at: Option<u64>,
    pub auth_successes: u64,
    pub auth_failures: u64,
    pub server_instance_id: String,
    /// What this relay supports beyond the base protocol, such as
    /// [`CAPABILITY_KEY_NAMESPACES`]. A relay older than the field reports none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
}
