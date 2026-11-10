//! Snapshot construction, load, and mutation replay onto persisted entries.
use super::super::*;
use super::{auth_snapshot_path, auth_wal_path, open_blob, read_wal};

pub(crate) fn push_audit_record(inner: &AuthStateInner, record: AuditRecord) {
    let mut records = inner.audit_records.write();
    while records.len() >= AUDIT_RECORD_CAPACITY {
        records.pop_front();
    }
    records.push_back(record);
}

pub(crate) fn cancel_all_temporary_leases(inner: &AuthStateInner) {
    let slots = inner.slots();
    for lease in slots.iter().filter_map(|slot| slot.lease.upgrade()) {
        lease.cancel_rotated();
    }
}

/// Every row's generation, the indexable rows first. Takes the slot table the
/// caller already holds: a second read lock taken on the same thread can
/// deadlock behind a queued writer.
fn snapshot_generations(slots: &[SlotHot], high: &[Generation]) -> Vec<Generation> {
    let mut generations = Vec::with_capacity(slots.len() + high.len());
    generations.extend(slots.iter().map(|slot| slot.generation));
    generations.extend_from_slice(high);
    generations
}

pub(crate) fn split_high_slot_state(
    snapshot: &PersistedSnapshot,
    capacity: usize,
) -> (Vec<Generation>, Vec<PersistedEntry>) {
    let high_generations = snapshot.generations.get(capacity..).unwrap_or(&[]).to_vec();
    let high_entries = snapshot
        .entries
        .iter()
        .filter(|entry| entry.key_id.slot().as_index() >= capacity)
        .cloned()
        .collect();
    (high_generations, high_entries)
}

pub(crate) fn build_snapshot(
    inner: &AuthStateInner,
    admin_replays: &VecDeque<AdminReplayRecord>,
) -> PersistedSnapshot {
    let slots = inner.slots();
    let cold = inner.cold();
    let generations = snapshot_generations(&slots, &inner.high_slot_generations);
    let mut entries = slots
        .iter()
        .enumerate()
        .filter_map(|(index, slot)| {
            if slot.state == SlotState::Free {
                return None;
            }
            let key_id = KeyId::new(slot.generation, SlotIndex::from_index(index));
            let cold = cold.get(&key_id)?;
            Some(PersistedEntry {
                key_id,
                state: slot.state,
                issued_at: cold.issued_at,
                expires_at: slot.expires_at,
                label: cold.label.clone(),
                tombstoned_at: (cold.tombstoned_at != 0).then_some(cold.tombstoned_at),
                namespace: cold.namespace,
            })
        })
        .collect::<Vec<_>>();
    entries.extend(inner.high().iter().cloned());
    snapshot_with(
        inner,
        inner.instance_id(),
        generations,
        entries,
        admin_replays,
    )
}

pub(crate) fn normalize_tombstone_times(snapshot: &mut PersistedSnapshot, now: u64) -> bool {
    let mut changed = false;
    for entry in &mut snapshot.entries {
        if entry.tombstoned_at.is_some() {
            continue;
        }
        let tombstoned_at = match entry.state {
            SlotState::Expired => Some(entry.expires_at),
            SlotState::Revoked => snapshot
                .audit_records
                .iter()
                .rev()
                .find(|record| {
                    record.action == "temporary_key_revoke" && record.key_id == Some(entry.key_id)
                })
                .map(|record| record.at)
                .or(Some(now)),
            SlotState::Free | SlotState::Active => None,
        };
        if tombstoned_at.is_some() {
            entry.tombstoned_at = tombstoned_at;
            changed = true;
        }
    }
    changed
}

pub(crate) fn empty_snapshot(
    inner: &AuthStateInner,
    instance_id: [u8; INSTANCE_ID_LEN],
    admin_replays: &VecDeque<AdminReplayRecord>,
) -> PersistedSnapshot {
    let generations = snapshot_generations(&inner.slots(), &inner.high_slot_generations);
    snapshot_with(inner, instance_id, generations, Vec::new(), admin_replays)
}

fn snapshot_with(
    inner: &AuthStateInner,
    instance_id: [u8; INSTANCE_ID_LEN],
    generations: Vec<Generation>,
    entries: Vec<PersistedEntry>,
    admin_replays: &VecDeque<AdminReplayRecord>,
) -> PersistedSnapshot {
    PersistedSnapshot {
        schema_version: SNAPSHOT_SCHEMA_VERSION,
        instance_id,
        generations,
        entries,
        legacy_protocol: if inner.legacy_protocol_allowed.load(Ordering::Acquire) {
            LegacyProtocolPolicy::Allow
        } else {
            LegacyProtocolPolicy::Deny
        },
        admin_replays: admin_replays.iter().cloned().collect(),
        audit_records: inner.audit_records.read().clone(),
        root_epoch: inner.root_epoch.load(Ordering::Acquire),
        namespace_high_water: inner.namespace_high_water.load(Ordering::Acquire),
    }
}

pub(crate) fn load_persisted_state(
    config: &AuthConfig,
    admin_key: &AesKeyType,
    instance_id: [u8; INSTANCE_ID_LEN],
) -> (Option<PersistedSnapshot>, bool) {
    match try_load_persisted_state(config, admin_key, instance_id) {
        Ok(state) => (Some(state), false),
        Err(error) => {
            tracing::error!(
                event = "auth_state_safe_mode",
                auth_stage = "state_load",
                reason = %error.code,
                error = %error,
                "temporary key store failed closed in administrator safe mode"
            );
            (None, true)
        }
    }
}

pub(crate) fn try_load_persisted_state(
    config: &AuthConfig,
    admin_key: &AesKeyType,
    instance_id: [u8; INSTANCE_ID_LEN],
) -> Result<PersistedSnapshot, AuthFailure> {
    let snapshot_path = auth_snapshot_path(&config.state_dir);
    let mut snapshot = if snapshot_path.exists() {
        let bytes = std::fs::read(&snapshot_path).map_err(|error| {
            AuthFailure::new(
                "temporary_key_store_unavailable",
                format!("failed to read `{}`: {error}", snapshot_path.display()),
                false,
            )
        })?;
        let plain = open_blob(admin_key, &bytes)?;
        serde_json::from_slice::<PersistedSnapshot>(&plain).map_err(|error| {
            AuthFailure::new(
                "temporary_key_store_unavailable",
                format!("failed to decode auth snapshot: {error}"),
                false,
            )
        })?
    } else {
        PersistedSnapshot {
            schema_version: SNAPSHOT_SCHEMA_VERSION,
            instance_id,
            generations: vec![Generation::FIRST; config.max_temporary_keys],
            entries: Vec::new(),
            legacy_protocol: config.legacy_protocol,
            admin_replays: Vec::new(),
            audit_records: VecDeque::new(),
            root_epoch: 0,
            namespace_high_water: 0,
        }
    };
    if snapshot.schema_version != SNAPSHOT_SCHEMA_VERSION || snapshot.instance_id != instance_id {
        return Err(AuthFailure::new(
            "temporary_key_store_unavailable",
            "auth snapshot schema or server instance id does not match",
            false,
        ));
    }
    if snapshot.generations.len() < config.max_temporary_keys {
        snapshot
            .generations
            .resize(config.max_temporary_keys, Generation::FIRST);
    }

    let wal_path = auth_wal_path(&config.state_dir);
    if wal_path.exists() {
        for record in read_wal(&wal_path, admin_key)? {
            match record {
                WalRecord::Mutation { mutation, audit } => {
                    apply_persisted_mutation(&mut snapshot, mutation)?;
                    push_persisted_audit(&mut snapshot.audit_records, audit);
                }
                WalRecord::AdminReplay(record) => snapshot.admin_replays.push(record),
                WalRecord::Audit(audit) => {
                    snapshot.namespace_high_water = snapshot
                        .namespace_high_water
                        .max(audit.namespace.unwrap_or_default());
                    push_persisted_audit(&mut snapshot.audit_records, audit);
                }
            }
        }
    }
    Ok(snapshot)
}

pub(crate) fn apply_persisted_mutation(
    snapshot: &mut PersistedSnapshot,
    mutation: StateMutation,
) -> Result<(), AuthFailure> {
    match mutation {
        StateMutation::Issue(entry) => {
            let index = entry.key_id.slot().as_index();
            if snapshot.generations.len() <= index {
                snapshot.generations.resize(index + 1, Generation::FIRST);
            }
            snapshot.generations[index] = entry.key_id.generation();
            snapshot
                .entries
                .retain(|current| current.key_id.slot().as_index() != index);
            snapshot.entries.push(entry);
        }
        StateMutation::Renew { key_id, expires_at } => {
            let Some(entry) = replayed_entry_mut(snapshot, key_id, "renew")? else {
                return Ok(());
            };
            entry.expires_at = expires_at;
            entry.state = SlotState::Active;
            entry.tombstoned_at = None;
        }
        StateMutation::Revoke { key_id, at } => {
            let Some(entry) = replayed_entry_mut(snapshot, key_id, "revoke")? else {
                return Ok(());
            };
            entry.state = SlotState::Revoked;
            entry.tombstoned_at = Some(at);
        }
        StateMutation::LegacyProtocol(policy) => snapshot.legacy_protocol = policy,
    }
    Ok(())
}

/// The entry a replayed record applies to, or `None` for a key that is already over.
///
/// Compaction replaces the snapshot and then truncates the WAL, so a crash between the
/// two replays records the snapshot already includes, among them records for keys it
/// reaped afterwards. A key whose generation the row has reached and which has no entry
/// is such a key, and a dead key stays dead. A generation the row has not reached means
/// the record that issued it is gone, which really is a damaged store.
fn replayed_entry_mut<'a>(
    snapshot: &'a mut PersistedSnapshot,
    key_id: KeyId,
    operation: &str,
) -> Result<Option<&'a mut PersistedEntry>, AuthFailure> {
    let issued = snapshot
        .generations
        .get(key_id.slot().as_index())
        .is_some_and(|generation| *generation >= key_id.generation());
    match snapshot
        .entries
        .iter_mut()
        .find(|entry| entry.key_id == key_id)
    {
        Some(entry) => Ok(Some(entry)),
        None if issued => Ok(None),
        None => Err(AuthFailure::new(
            "temporary_key_store_unavailable",
            format!("WAL {operation} record references an unknown key"),
            false,
        )),
    }
}

pub(crate) fn push_persisted_audit(records: &mut VecDeque<AuditRecord>, record: AuditRecord) {
    while records.len() >= AUDIT_RECORD_CAPACITY {
        records.pop_front();
    }
    records.push_back(record);
}
