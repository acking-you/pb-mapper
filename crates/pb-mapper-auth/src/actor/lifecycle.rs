//! Issue, inspect, renew, revoke, and collect temporary keys.
use super::super::*;
use super::{
    audit, ensure_store_available, key_not_active, key_not_found, key_not_renewable,
    slot_state_name, validate_slot_identity,
};
use pb_mapper_core::paging::paginate;

/// The longest label a key may carry, after trimming.
const MAX_LABEL_BYTES: usize = 64;

fn validate_ttl(config: &AuthConfig, ttl: Duration) -> Result<u64, AuthFailure> {
    if ttl < MIN_TEMP_KEY_TTL {
        return Err(AuthFailure::new(
            codes::TEMPORARY_KEY_TTL_TOO_SHORT,
            format!(
                "temporary key TTL must be at least {} seconds",
                MIN_TEMP_KEY_TTL.as_secs()
            ),
            false,
        ));
    }
    if ttl > config.max_temporary_key_ttl {
        return Err(AuthFailure::new(
            codes::TEMPORARY_KEY_TTL_TOO_LONG,
            format!(
                "temporary key TTL exceeds the configured maximum of {} seconds",
                config.max_temporary_key_ttl.as_secs()
            ),
            false,
        ));
    }
    Ok(unix_seconds().saturating_add(ttl.as_secs()))
}

fn validate_label(label: Option<String>) -> Result<Option<String>, AuthFailure> {
    let label = label
        .map(|label| label.trim().to_string())
        .filter(|label| !label.is_empty());
    if label
        .as_ref()
        .is_some_and(|label| label.len() > MAX_LABEL_BYTES)
    {
        return Err(AuthFailure::new(
            codes::TEMPORARY_KEY_LABEL_TOO_LONG,
            format!("temporary key label must not exceed {MAX_LABEL_BYTES} UTF-8 bytes"),
            false,
        ));
    }
    Ok(label)
}

/// One key's lifecycle, read from wherever that key lives.
struct KeyState {
    state: SlotState,
    expires_at: u64,
    issued_at: u64,
    label: Option<String>,
    namespace: Option<u64>,
}

/// Reads a key's lifecycle from the slot table, falling back to the entries
/// retained for slots the configured capacity no longer covers. Every operation
/// that accepts any live key id needs both paths; see
/// `AuthStateInner::high_slot_generations`.
fn key_state(inner: &AuthStateInner, key_id: KeyId) -> Result<KeyState, AuthFailure> {
    let slots = inner.slots();
    if let Some(slot) = slots.get(key_id.slot().as_index()) {
        validate_slot_identity(slot, key_id)?;
        let metadata = inner
            .cold()
            .get(&key_id)
            .cloned()
            .ok_or_else(|| key_not_found(key_id))?;
        return Ok(KeyState {
            state: slot.state,
            expires_at: slot.expires_at,
            issued_at: metadata.issued_at,
            label: metadata.label.clone(),
            namespace: metadata.namespace,
        });
    }
    drop(slots);
    let high = inner.high();
    let entry = high_slot_entry(&high, key_id)?;
    Ok(KeyState {
        state: entry.state,
        expires_at: entry.expires_at,
        issued_at: entry.issued_at,
        label: entry.label.clone(),
        namespace: entry.namespace,
    })
}

/// The last namespace handed out, by whichever copy of the mark knows more. A damaged
/// file refuses rather than deferring to the other copy; see `persistence/namespaces.rs`.
fn namespace_high_water(inner: &AuthStateInner, config: &AuthConfig) -> Result<u64, AuthFailure> {
    Ok(read_namespace_high_water(&config.state_dir)?
        .max(inner.namespace_high_water.load(Ordering::Acquire)))
}

/// Hand out the next namespace.
///
/// The new high-water mark reaches the disk before the number leaves the actor, so
/// a crash can only skip a number, never repeat one.
pub(super) fn actor_allocate_namespace(
    inner: &Arc<AuthStateInner>,
    config: &AuthConfig,
) -> Result<u64, AuthFailure> {
    ensure_store_available(inner)?;
    let namespace = namespace_high_water(inner, config)?.saturating_add(1);
    if namespace > MAX_ALLOCATED_NAMESPACE {
        return Err(AuthFailure::new(
            codes::NAMESPACE_SPACE_EXHAUSTED,
            "every allocatable namespace has been handed out",
            false,
        ));
    }
    write_namespace_high_water(&config.state_dir, namespace)?;
    inner
        .namespace_high_water
        .store(namespace, Ordering::Release);
    append_audit(
        config,
        inner,
        AuditRecord {
            namespace: Some(namespace),
            ..audit("namespace_allocate", None, None)
        },
    )?;
    Ok(namespace)
}

/// Refuse a namespace the allocator never handed out. That covers namespace 0 and
/// every key's own namespace, whose ids all lie above the allocatable range.
fn ensure_namespace_allocated(
    inner: &AuthStateInner,
    config: &AuthConfig,
    namespace: u64,
) -> Result<(), AuthFailure> {
    if namespace == 0 || namespace > namespace_high_water(inner, config)? {
        return Err(AuthFailure::new(
            codes::NAMESPACE_NOT_ALLOCATED,
            format!("namespace {namespace} was not allocated by this relay"),
            false,
        ));
    }
    Ok(())
}

pub(super) fn actor_issue(
    inner: &Arc<AuthStateInner>,
    config: &AuthConfig,
    leases: &mut Leases,
    namespace: Option<u64>,
    ttl: Duration,
    label: Option<String>,
) -> Result<IssuedTemporaryKey, AuthFailure> {
    ensure_store_available(inner)?;
    let expires_at = validate_ttl(config, ttl)?;
    let label = validate_label(label)?;
    if let Some(namespace) = namespace {
        ensure_namespace_allocated(inner, config, namespace)?;
    }
    let issued_at = unix_seconds();
    let (index, generation, key_id, entry) = {
        let slots = inner.slots();
        // A row whose generation cannot advance is skipped rather than reused: it
        // has no unused identity left to hand out.
        let Some((slot_index, generation)) = slots.iter().enumerate().find_map(|(index, slot)| {
            (slot.state == SlotState::Free)
                .then(|| slot.generation.next())
                .flatten()
                .map(|generation| (SlotIndex::from_index(index), generation))
        }) else {
            return Err(AuthFailure::new(
                codes::TEMPORARY_KEY_CAPACITY_EXHAUSTED,
                "temporary key slot table is full",
                true,
            ));
        };
        let key_id = KeyId::new(generation, slot_index);
        (
            slot_index.as_index(),
            generation,
            key_id,
            PersistedEntry {
                key_id,
                state: SlotState::Active,
                issued_at,
                expires_at,
                label: label.clone(),
                tombstoned_at: None,
                namespace,
            },
        )
    };
    // Persist before taking the slot write lock. A fail-closed WAL error
    // cancels leases via slots.read() and must not nest under slots.write().
    append_mutation(
        config,
        inner,
        StateMutation::Issue(entry),
        audit("temporary_key_issue", Some(key_id), label.clone()),
    )?;
    let mut slots = inner.slots_mut();
    let slot = slots
        .get_mut(index)
        .ok_or_else(|| AuthFailure::internal("issued slot disappeared"))?;
    let lease = Arc::new(AuthLease::new(key_id, namespace, expires_at));
    slot.generation = generation;
    slot.state = SlotState::Active;
    slot.expires_at = expires_at;
    slot.lease = Arc::downgrade(&lease);
    drop(slots);
    leases.issue(&lease, issued_at, label, namespace);
    metadata_with_credential(inner, key_id, true)
}

pub(super) fn actor_list(
    inner: &Arc<AuthStateInner>,
    page: u32,
    page_size: u16,
) -> Result<KeyPage, AuthFailure> {
    let slots = inner.slots();
    let cold = inner.cold();
    let mut all = slots
        .iter()
        .enumerate()
        .filter_map(|(index, slot)| {
            if slot.state == SlotState::Free {
                return None;
            }
            let key_id = KeyId::new(slot.generation, SlotIndex::from_index(index));
            let cold = cold.get(&key_id)?;
            Some(TemporaryKeyMetadata {
                key_id,
                state: slot_state_name(slot.state).to_string(),
                issued_at: cold.issued_at,
                expires_at: slot.expires_at,
                label: cold.label.clone(),
                namespace: cold.namespace,
            })
        })
        .collect::<Vec<_>>();
    all.extend(
        inner
            .high()
            .iter()
            .filter(|entry| entry.state != SlotState::Free)
            .map(high_slot_metadata),
    );
    all.sort_by_key(|item| std::cmp::Reverse(item.issued_at));
    let (items, next_page) = paginate(&all, page, page_size);
    Ok(KeyPage {
        schema_version: ADMIN_SCHEMA_VERSION,
        items,
        next_page,
    })
}

pub(super) fn actor_show(
    inner: &Arc<AuthStateInner>,
    config: &AuthConfig,
    key_id: KeyId,
    reveal: bool,
) -> Result<IssuedTemporaryKey, AuthFailure> {
    let result = metadata_with_credential(inner, key_id, reveal)?;
    append_audit(
        config,
        inner,
        audit(
            if reveal {
                "temporary_key_reveal"
            } else {
                "temporary_key_show"
            },
            Some(key_id),
            result.metadata.label.clone(),
        ),
    )?;
    Ok(result)
}

pub(super) fn actor_renew(
    inner: &Arc<AuthStateInner>,
    config: &AuthConfig,
    leases: &mut Leases,
    key_id: KeyId,
    ttl: Duration,
) -> Result<IssuedTemporaryKey, AuthFailure> {
    ensure_store_available(inner)?;
    let expires_at = validate_ttl(config, ttl)?;
    let index = key_id.slot().as_index();
    let current = key_state(inner, key_id)?;
    if current.state != SlotState::Active || current.expires_at <= unix_seconds() {
        return Err(key_not_renewable());
    }
    let KeyState {
        label, namespace, ..
    } = current;
    append_mutation(
        config,
        inner,
        StateMutation::Renew { key_id, expires_at },
        audit("temporary_key_renew", Some(key_id), label.clone()),
    )?;
    let mut slots = inner.slots_mut();
    if let Some(slot) = slots.get_mut(index) {
        validate_slot_identity(slot, key_id)?;
        if slot.state != SlotState::Active {
            return Err(AuthFailure::key_lease_inactive());
        }
        slot.expires_at = expires_at;
        match slot.lease.upgrade() {
            Some(lease) if !lease.is_cancelled() => {
                lease.set_expires_at(expires_at);
                drop(slots);
                leases.renew(key_id, expires_at);
            }
            // A cancelled lease cannot be revived, so the renewal installs a
            // replacement; that drops the handle on the lease it succeeds.
            _ => {
                let lease = Arc::new(AuthLease::new(key_id, namespace, expires_at));
                slot.lease = Arc::downgrade(&lease);
                drop(slots);
                leases.adopt(&lease);
            }
        }
        return metadata_with_credential(inner, key_id, true);
    }
    drop(slots);
    {
        let mut high = inner.high_mut();
        let entry = high_slot_entry_mut(&mut high, key_id)?;
        if entry.state != SlotState::Active {
            return Err(AuthFailure::key_lease_inactive());
        }
        entry.expires_at = expires_at;
        entry.tombstoned_at = None;
    }
    metadata_with_credential(inner, key_id, true)
}

pub(super) fn actor_revoke(
    inner: &Arc<AuthStateInner>,
    config: &AuthConfig,
    leases: &mut Leases,
    key_id: KeyId,
) -> Result<TemporaryKeyMetadata, AuthFailure> {
    ensure_store_available(inner)?;
    let now = unix_seconds();
    let index = key_id.slot().as_index();
    let current = key_state(inner, key_id)?;
    if current.state != SlotState::Active {
        return Err(key_not_active());
    }
    let KeyState {
        label,
        issued_at,
        expires_at,
        namespace,
        ..
    } = current;
    append_mutation(
        config,
        inner,
        StateMutation::Revoke { key_id, at: now },
        audit("temporary_key_revoke", Some(key_id), label.clone()),
    )?;
    let mut slots = inner.slots_mut();
    if let Some(slot) = slots.get_mut(index) {
        validate_slot_identity(slot, key_id)?;
        slot.state = SlotState::Revoked;
        if let Some(lease) = slot.lease.upgrade() {
            lease.cancel_revoked();
        }
        let state = slot_state_name(slot.state).to_string();
        let expires_at = slot.expires_at;
        drop(slots);
        // Retire only: the row stays until its retention elapses, because the
        // slot table holds a `Weak` and a later request has to be able to read
        // the revoked reason rather than find a recycled row.
        leases.retire_now(key_id);
        let metadata = inner
            .cold()
            .get(&key_id)
            .cloned()
            .ok_or_else(|| key_not_found(key_id))?;
        return Ok(TemporaryKeyMetadata {
            key_id,
            state,
            issued_at: metadata.issued_at,
            expires_at,
            label: metadata.label.clone(),
            namespace: metadata.namespace,
        });
    }
    drop(slots);
    let mut high = inner.high_mut();
    let entry = high_slot_entry_mut(&mut high, key_id)?;
    if entry.state != SlotState::Active {
        return Err(key_not_active());
    }
    entry.state = SlotState::Revoked;
    entry.tombstoned_at = Some(now);
    let state = slot_state_name(entry.state).to_string();
    drop(high);
    leases.retire_now(key_id);
    Ok(TemporaryKeyMetadata {
        key_id,
        state,
        issued_at,
        expires_at,
        label,
        namespace,
    })
}

pub(super) fn actor_gc(
    inner: &Arc<AuthStateInner>,
    config: &AuthConfig,
    leases: &mut Leases,
    admin_replays: &VecDeque<AdminReplayRecord>,
) -> Result<u64, AuthFailure> {
    ensure_store_available(inner)?;
    let removed = leases.collect_garbage(unix_seconds());
    let gc_audit = audit("temporary_key_gc", None, Some(format!("removed={removed}")));
    let mut snapshot = build_snapshot(inner, admin_replays);
    push_persisted_audit(&mut snapshot.audit_records, gc_audit.clone());
    let admin_key = inner.admin_key();
    if let Err(error) = write_snapshot_and_truncate_wal(config, &admin_key, &snapshot) {
        inner.safe_mode.store(true, Ordering::Release);
        cancel_all_temporary_leases(inner);
        return Err(error);
    }
    push_audit_record(inner, gc_audit);
    Ok(removed)
}

fn high_slot_entry(high: &[PersistedEntry], key_id: KeyId) -> Result<&PersistedEntry, AuthFailure> {
    high.iter()
        .find(|entry| entry.key_id == key_id)
        .ok_or_else(|| key_not_found(key_id))
}

fn high_slot_entry_mut(
    high: &mut [PersistedEntry],
    key_id: KeyId,
) -> Result<&mut PersistedEntry, AuthFailure> {
    high.iter_mut()
        .find(|entry| entry.key_id == key_id)
        .ok_or_else(|| key_not_found(key_id))
}

fn high_slot_metadata(entry: &PersistedEntry) -> TemporaryKeyMetadata {
    TemporaryKeyMetadata {
        key_id: entry.key_id,
        state: slot_state_name(entry.state).to_string(),
        issued_at: entry.issued_at,
        expires_at: entry.expires_at,
        label: entry.label.clone(),
        namespace: entry.namespace,
    }
}

fn metadata_with_credential(
    inner: &Arc<AuthStateInner>,
    key_id: KeyId,
    reveal: bool,
) -> Result<IssuedTemporaryKey, AuthFailure> {
    let slots = inner.slots();
    let credential = if reveal {
        let key = derive_temporary_key(&inner.admin_key(), &inner.instance_id(), key_id)?;
        encode_temporary_credential(key_id.as_u64(), &key)
    } else {
        String::new()
    };
    if let Some(slot) = slots.get(key_id.slot().as_index()) {
        validate_slot_identity(slot, key_id)?;
        let cold = inner.cold();
        let cold = cold.get(&key_id).ok_or_else(|| key_not_found(key_id))?;
        return Ok(IssuedTemporaryKey {
            metadata: TemporaryKeyMetadata {
                key_id,
                state: slot_state_name(slot.state).to_string(),
                issued_at: cold.issued_at,
                expires_at: slot.expires_at,
                label: cold.label.clone(),
                namespace: cold.namespace,
            },
            credential,
        });
    }
    let high = inner.high();
    Ok(IssuedTemporaryKey {
        metadata: high_slot_metadata(high_slot_entry(&high, key_id)?),
        credential,
    })
}
