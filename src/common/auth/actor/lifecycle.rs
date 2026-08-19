//! Issue, inspect, renew, revoke, and collect temporary keys.
use super::super::*;
use super::{
    audit, ensure_store_available, key_not_active, key_not_found, key_not_renewable,
    push_tombstone, slot_state_name, validate_slot_identity,
};

fn validate_ttl(config: &AuthConfig, ttl: Duration) -> Result<u64, AuthFailure> {
    if ttl < MIN_TEMP_KEY_TTL {
        return Err(AuthFailure::new(
            "temporary_key_ttl_too_short",
            format!(
                "temporary key TTL must be at least {} seconds",
                MIN_TEMP_KEY_TTL.as_secs()
            ),
            false,
        ));
    }
    if ttl > config.max_temporary_key_ttl {
        return Err(AuthFailure::new(
            "temporary_key_ttl_too_long",
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
    if label.as_ref().is_some_and(|label| label.len() > 64) {
        return Err(AuthFailure::new(
            "temporary_key_label_too_long",
            "temporary key label must not exceed 64 UTF-8 bytes",
            false,
        ));
    }
    Ok(label)
}

pub(super) fn actor_issue(
    inner: &Arc<AuthStateInner>,
    config: &AuthConfig,
    cold: &mut HashMap<u64, ColdMetadata>,
    wheel: &mut TimingWheel,
    ttl: Duration,
    label: Option<String>,
) -> Result<IssuedTemporaryKey, AuthFailure> {
    ensure_store_available(inner)?;
    let expires_at = validate_ttl(config, ttl)?;
    let label = validate_label(label)?;
    let issued_at = unix_seconds();
    let (index, generation, key_id, entry) = {
        let slots = inner
            .slots
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some((index, slot)) = slots
            .iter()
            .enumerate()
            .find(|(_, slot)| slot.state == SlotState::Free && slot.generation < u32::MAX)
        else {
            return Err(AuthFailure::new(
                "temporary_key_capacity_exhausted",
                "temporary key slot table is full",
                true,
            ));
        };
        let generation = slot.generation + 1;
        let key_id = make_key_id(generation, index as u32);
        (
            index,
            generation,
            key_id,
            PersistedEntry {
                key_id,
                state: SlotState::Active,
                issued_at,
                expires_at,
                label: label.clone(),
                tombstoned_at: None,
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
    let mut slots = inner
        .slots
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let slot = slots
        .get_mut(index)
        .ok_or_else(|| AuthFailure::internal("issued slot disappeared"))?;
    let lease = Arc::new(AuthLease::new(key_id, expires_at));
    slot.generation = generation;
    slot.state = SlotState::Active;
    slot.expires_at = expires_at;
    slot.issued_epoch = inner.root_epoch.load(Ordering::Acquire);
    slot.lease = Arc::downgrade(&lease);
    cold.insert(
        key_id,
        ColdMetadata {
            issued_at,
            label,
            tombstoned_at: 0,
        },
    );
    wheel.insert(lease);
    drop(slots);
    metadata_with_credential(inner, cold, key_id, true)
}

pub(super) fn actor_list(
    inner: &Arc<AuthStateInner>,
    cold: &HashMap<u64, ColdMetadata>,
    page: u32,
    page_size: u16,
) -> Result<KeyPage, AuthFailure> {
    let page_size = page_size.clamp(1, 1000) as usize;
    let start = (page as usize).saturating_mul(page_size);
    let slots = inner
        .slots
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut all = slots
        .iter()
        .enumerate()
        .filter_map(|(index, slot)| {
            if slot.state == SlotState::Free {
                return None;
            }
            let key_id = make_key_id(slot.generation, index as u32);
            let cold = cold.get(&key_id)?;
            Some(TemporaryKeyMetadata {
                key_id,
                state: slot_state_name(slot.state).to_string(),
                issued_at: cold.issued_at,
                expires_at: slot.expires_at,
                label: cold.label.clone(),
            })
        })
        .collect::<Vec<_>>();
    all.extend(
        inner
            .high_slot_entries
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .filter(|entry| entry.state != SlotState::Free)
            .map(high_slot_metadata),
    );
    all.sort_by_key(|item| std::cmp::Reverse(item.issued_at));
    let items = all.iter().skip(start).take(page_size).cloned().collect();
    let next_page = (start.saturating_add(page_size) < all.len()).then_some(page.saturating_add(1));
    Ok(KeyPage {
        schema_version: 1,
        items,
        next_page,
    })
}

pub(super) fn actor_show(
    inner: &Arc<AuthStateInner>,
    config: &AuthConfig,
    cold: &HashMap<u64, ColdMetadata>,
    key_id: u64,
    reveal: bool,
) -> Result<IssuedTemporaryKey, AuthFailure> {
    let result = metadata_with_credential(inner, cold, key_id, reveal)?;
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
    cold: &HashMap<u64, ColdMetadata>,
    wheel: &mut TimingWheel,
    key_id: u64,
    ttl: Duration,
) -> Result<IssuedTemporaryKey, AuthFailure> {
    ensure_store_available(inner)?;
    let expires_at = validate_ttl(config, ttl)?;
    let index = key_slot(key_id) as usize;
    {
        let slots = inner
            .slots
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(slot) = slots.get(index) {
            validate_slot_identity(slot, key_id)?;
            if slot.state != SlotState::Active || slot.expires_at <= unix_seconds() {
                return Err(key_not_renewable());
            }
        } else {
            let high = inner
                .high_slot_entries
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let entry = high_slot_entry(&high, key_id)?;
            if entry.state != SlotState::Active || entry.expires_at <= unix_seconds() {
                return Err(key_not_renewable());
            }
        }
    }
    let label = cold
        .get(&key_id)
        .and_then(|metadata| metadata.label.clone())
        .or_else(|| {
            inner
                .high_slot_entries
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .iter()
                .find(|entry| entry.key_id == key_id)
                .and_then(|entry| entry.label.clone())
        });
    append_mutation(
        config,
        inner,
        StateMutation::Renew { key_id, expires_at },
        audit("temporary_key_renew", Some(key_id), label),
    )?;
    let mut slots = inner
        .slots
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(slot) = slots.get_mut(index) {
        validate_slot_identity(slot, key_id)?;
        if slot.state != SlotState::Active {
            return Err(AuthFailure::new(
                "temporary_key_inactive",
                "temporary key lease is no longer active",
                true,
            ));
        }
        slot.expires_at = expires_at;
        let lease = match slot.lease.upgrade() {
            Some(lease) if !lease.cancellation_token().is_cancelled() => {
                lease.expires_at.store(expires_at, Ordering::Release);
                lease.wheel_version.fetch_add(1, Ordering::AcqRel);
                lease
            }
            _ => {
                let lease = Arc::new(AuthLease::new(key_id, expires_at));
                slot.lease = Arc::downgrade(&lease);
                lease
            }
        };
        wheel.release(key_id);
        wheel.insert(lease);
        drop(slots);
        return metadata_with_credential(inner, cold, key_id, true);
    }
    drop(slots);
    {
        let mut high = inner
            .high_slot_entries
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let entry = high_slot_entry_mut(&mut high, key_id)?;
        if entry.state != SlotState::Active {
            return Err(AuthFailure::new(
                "temporary_key_inactive",
                "temporary key lease is no longer active",
                true,
            ));
        }
        entry.expires_at = expires_at;
        entry.tombstoned_at = None;
    }
    metadata_with_credential(inner, cold, key_id, true)
}

pub(super) fn actor_revoke(
    inner: &Arc<AuthStateInner>,
    config: &AuthConfig,
    cold: &mut HashMap<u64, ColdMetadata>,
    tombstones: &mut VecDeque<(u64, u64)>,
    key_id: u64,
) -> Result<TemporaryKeyMetadata, AuthFailure> {
    ensure_store_available(inner)?;
    let now = unix_seconds();
    let index = key_slot(key_id) as usize;
    let (label, issued_at, expires_at) = {
        let slots = inner
            .slots
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(slot) = slots.get(index) {
            validate_slot_identity(slot, key_id)?;
            if slot.state != SlotState::Active {
                return Err(key_not_active());
            }
            let metadata = cold.get(&key_id).ok_or_else(|| key_not_found(key_id))?;
            (metadata.label.clone(), metadata.issued_at, slot.expires_at)
        } else {
            let high = inner
                .high_slot_entries
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let entry = high_slot_entry(&high, key_id)?;
            if entry.state != SlotState::Active {
                return Err(key_not_active());
            }
            (entry.label.clone(), entry.issued_at, entry.expires_at)
        }
    };
    append_mutation(
        config,
        inner,
        StateMutation::Revoke { key_id, at: now },
        audit("temporary_key_revoke", Some(key_id), label.clone()),
    )?;
    let mut slots = inner
        .slots
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(slot) = slots.get_mut(index) {
        validate_slot_identity(slot, key_id)?;
        slot.state = SlotState::Revoked;
        if let Some(lease) = slot.lease.upgrade() {
            lease.cancel_revoked();
        }
        let cold_metadata = cold.get_mut(&key_id).ok_or_else(|| key_not_found(key_id))?;
        cold_metadata.tombstoned_at = now;
        push_tombstone(tombstones, now, key_id);
        return Ok(TemporaryKeyMetadata {
            key_id,
            state: slot_state_name(slot.state).to_string(),
            issued_at: cold_metadata.issued_at,
            expires_at: slot.expires_at,
            label: cold_metadata.label.clone(),
        });
    }
    drop(slots);
    let mut high = inner
        .high_slot_entries
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let entry = high_slot_entry_mut(&mut high, key_id)?;
    if entry.state != SlotState::Active {
        return Err(key_not_active());
    }
    entry.state = SlotState::Revoked;
    entry.tombstoned_at = Some(now);
    if let Some(metadata) = cold.get_mut(&key_id) {
        metadata.tombstoned_at = now;
    }
    push_tombstone(tombstones, now, key_id);
    Ok(TemporaryKeyMetadata {
        key_id,
        state: slot_state_name(entry.state).to_string(),
        issued_at,
        expires_at,
        label,
    })
}

pub(super) fn actor_gc(
    inner: &Arc<AuthStateInner>,
    config: &AuthConfig,
    cold: &mut HashMap<u64, ColdMetadata>,
    wheel: &mut TimingWheel,
    tombstones: &mut VecDeque<(u64, u64)>,
    admin_replays: &VecDeque<AdminReplayRecord>,
) -> Result<u64, AuthFailure> {
    ensure_store_available(inner)?;
    let now = unix_seconds();
    let mut removed = 0_u64;
    let mut slots = inner
        .slots
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    for (index, slot) in slots.iter_mut().enumerate() {
        if matches!(slot.state, SlotState::Expired | SlotState::Revoked)
            || (slot.state == SlotState::Active && slot.expires_at <= now)
        {
            let key_id = make_key_id(slot.generation, index as u32);
            if let Some(lease) = slot.lease.upgrade() {
                if slot.state == SlotState::Revoked {
                    lease.cancel_revoked();
                } else {
                    lease.cancel_expired();
                }
            }
            slot.state = SlotState::Free;
            slot.expires_at = 0;
            slot.lease = Weak::new();
            cold.remove(&key_id);
            wheel.release(key_id);
            removed = removed.saturating_add(1);
        }
    }
    drop(slots);
    {
        let mut high = inner
            .high_slot_entries
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        high.retain(|entry| {
            let keep = match entry.state {
                SlotState::Active if entry.expires_at > now => true,
                SlotState::Active | SlotState::Expired | SlotState::Revoked | SlotState::Free => {
                    false
                }
            };
            if !keep {
                cold.remove(&entry.key_id);
                wheel.release(entry.key_id);
                removed = removed.saturating_add(1);
            }
            keep
        });
    }
    tombstones.clear();
    let gc_audit = audit("temporary_key_gc", None, Some(format!("removed={removed}")));
    let mut snapshot = build_snapshot(inner, cold, admin_replays);
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

fn high_slot_entry(high: &[PersistedEntry], key_id: u64) -> Result<&PersistedEntry, AuthFailure> {
    high.iter()
        .find(|entry| entry.key_id == key_id)
        .ok_or_else(|| key_not_found(key_id))
}

fn high_slot_entry_mut(
    high: &mut [PersistedEntry],
    key_id: u64,
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
    }
}

fn metadata_with_credential(
    inner: &Arc<AuthStateInner>,
    cold: &HashMap<u64, ColdMetadata>,
    key_id: u64,
    reveal: bool,
) -> Result<IssuedTemporaryKey, AuthFailure> {
    let slots = inner
        .slots
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let credential = if reveal {
        let key = derive_temporary_key(&inner.admin_key(), &inner.instance_id(), key_id)?;
        encode_temporary_credential(key_id, &key)
    } else {
        String::new()
    };
    if let Some(slot) = slots.get(key_slot(key_id) as usize) {
        validate_slot_identity(slot, key_id)?;
        let cold = cold.get(&key_id).ok_or_else(|| key_not_found(key_id))?;
        return Ok(IssuedTemporaryKey {
            metadata: TemporaryKeyMetadata {
                key_id,
                state: slot_state_name(slot.state).to_string(),
                issued_at: cold.issued_at,
                expires_at: slot.expires_at,
                label: cold.label.clone(),
            },
            credential,
        });
    }
    let high = inner
        .high_slot_entries
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    Ok(IssuedTemporaryKey {
        metadata: high_slot_metadata(high_slot_entry(&high, key_id)?),
        credential,
    })
}
