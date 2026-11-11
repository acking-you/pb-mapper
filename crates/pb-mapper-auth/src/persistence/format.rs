//! The persisted formats: the snapshot, the WAL records, and what they carry.
use super::super::*;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct PersistedEntry {
    pub(crate) key_id: KeyId,
    pub(crate) state: SlotState,
    pub(crate) issued_at: u64,
    pub(crate) expires_at: u64,
    pub(crate) label: Option<String>,
    #[serde(default)]
    pub(crate) tombstoned_at: Option<u64>,
    /// The namespace this key was issued into. Absent for a key that owns the one
    /// its id names, which is every entry written before namespaces existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) namespace: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct PersistedSnapshot {
    pub(crate) schema_version: u16,
    pub(crate) instance_id: [u8; INSTANCE_ID_LEN],
    pub(crate) generations: Vec<Generation>,
    pub(crate) entries: Vec<PersistedEntry>,
    pub(crate) legacy_protocol: LegacyProtocolPolicy,
    #[serde(default)]
    pub(crate) admin_replays: Vec<AdminReplayRecord>,
    #[serde(default)]
    pub(crate) audit_records: VecDeque<AuditRecord>,
    #[serde(default)]
    pub(crate) root_epoch: u64,
    /// The last namespace handed out as of this snapshot. See
    /// `persistence/namespaces.rs`.
    #[serde(default)]
    pub(crate) namespace_high_water: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct AdminReplayRecord {
    pub(crate) fingerprint: [u8; 32],
    pub(crate) client_timestamp: u64,
    /// Server receipt time used for retention. Older snapshots omit this field
    /// (`0` after serde default) and fall back to `client_timestamp`.
    #[serde(default)]
    pub(crate) accepted_at: u64,
}

impl AdminReplayRecord {
    pub(crate) fn within_retention(&self, now: u64) -> bool {
        let anchor = if self.accepted_at == 0 {
            self.client_timestamp
        } else {
            self.accepted_at
        };
        now.saturating_sub(anchor) <= ADMIN_REPLAY_RETENTION.as_secs()
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum StateMutation {
    Issue(PersistedEntry),
    Renew { key_id: KeyId, expires_at: u64 },
    Revoke { key_id: KeyId, at: u64 },
    LegacyProtocol(LegacyProtocolPolicy),
}

/// The [`AuditRecord::action`] values this crate writes. The administrator API
/// records actions of its own beside them, so the field stays a string.
pub(crate) mod audit_action {
    pub(crate) const TEMPORARY_KEY_ISSUE: &str = "temporary_key_issue";
    pub(crate) const TEMPORARY_KEY_SHOW: &str = "temporary_key_show";
    pub(crate) const TEMPORARY_KEY_REVEAL: &str = "temporary_key_reveal";
    pub(crate) const TEMPORARY_KEY_RENEW: &str = "temporary_key_renew";
    /// Also read back, to recover a revoked key's tombstone time.
    pub(crate) const TEMPORARY_KEY_REVOKE: &str = "temporary_key_revoke";
    pub(crate) const TEMPORARY_KEY_GC: &str = "temporary_key_gc";
    pub(crate) const NAMESPACE_ALLOCATE: &str = "namespace_allocate";
    pub(crate) const LEGACY_PROTOCOL_UPDATE: &str = "legacy_protocol_update";
    pub(crate) const AUTH_STATE_RESET: &str = "auth_state_reset";
    pub(crate) const ADMINISTRATOR_KEY_ROTATE: &str = "administrator_key_rotate";
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct AuditRecord {
    pub(crate) at: u64,
    pub(crate) action: String,
    pub(crate) key_id: Option<KeyId>,
    /// The key's label, or for an action on no key a detail of it: a GC's
    /// count, a legacy-protocol policy, or what the administrator API passed.
    pub(crate) label: Option<String>,
    /// The namespace an allocation handed out. Replaying this is how the encrypted
    /// state learns the allocator's mark, in a field an older relay ignores rather
    /// than a WAL record it could not decode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) namespace: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum WalRecord {
    Mutation {
        mutation: StateMutation,
        audit: AuditRecord,
    },
    Audit(AuditRecord),
    AdminReplay(AdminReplayRecord),
}
