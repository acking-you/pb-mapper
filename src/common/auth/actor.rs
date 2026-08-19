//! Serialized owner of mutable authentication lifecycle state.
//!
//! ```text
//! authenticated admin command
//!           |
//!           v
//!   validate current admin lease
//!           |
//!           v
//! append encrypted WAL -> mutate slots / leases / timing wheel
//!           |
//!           +-> periodic snapshot + bounded replay/audit retention
//! ```
//!
//! Keeping authorization revalidation and mutations in one actor prevents a request
//! authenticated before root rotation from executing against the new administrator
//! state. The actor is also the sole strong owner of temporary-key leases.

use super::*;

pub(super) struct AuthActorState {
    cold: HashMap<u64, ColdMetadata>,
    wheel: TimingWheel,
    admin_replays: HashSet<[u8; 32]>,
    admin_replay_order: VecDeque<AdminReplayRecord>,
}

impl AuthActorState {
    pub(super) fn new(
        cold: HashMap<u64, ColdMetadata>,
        wheel: TimingWheel,
        admin_replays: HashSet<[u8; 32]>,
        admin_replay_order: VecDeque<AdminReplayRecord>,
    ) -> Self {
        Self {
            cold,
            wheel,
            admin_replays,
            admin_replay_order,
        }
    }
}

pub(super) async fn run_auth_actor(
    inner: Arc<AuthStateInner>,
    mut admin_lease: Arc<AuthLease>,
    mut command_rx: mpsc::Receiver<AuthCommand>,
    config: AuthConfig,
    state: AuthActorState,
    _state_lock: Arc<File>,
) {
    let AuthActorState {
        mut cold,
        mut wheel,
        mut admin_replays,
        mut admin_replay_order,
    } = state;
    let now = unix_seconds();
    let mut tombstones = inner
        .slots
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .iter()
        .enumerate()
        .filter_map(|(index, slot)| {
            if !matches!(slot.state, SlotState::Expired | SlotState::Revoked) {
                return None;
            }
            let key_id = make_key_id(slot.generation, index as u32);
            let tombstoned_at = cold
                .get(&key_id)
                .map(|metadata| metadata.tombstoned_at)
                .unwrap_or(now);
            Some((
                tombstoned_at.saturating_add(TOMBSTONE_RETENTION.as_secs()),
                key_id,
            ))
        })
        .collect::<Vec<_>>();
    tombstones.extend(
        inner
            .high_slot_entries
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .filter_map(|entry| {
                let expired_active = entry.state == SlotState::Active && entry.expires_at <= now;
                if !matches!(entry.state, SlotState::Expired | SlotState::Revoked)
                    && !expired_active
                {
                    return None;
                }
                let tombstoned_at = entry
                    .tombstoned_at
                    .or_else(|| {
                        cold.get(&entry.key_id)
                            .map(|metadata| metadata.tombstoned_at)
                    })
                    .unwrap_or(entry.expires_at.max(now));
                Some((
                    tombstoned_at.saturating_add(TOMBSTONE_RETENTION.as_secs()),
                    entry.key_id,
                ))
            }),
    );
    tombstones.sort_unstable_by_key(|(cleanup_at, _)| *cleanup_at);
    let mut tombstones = VecDeque::from(tombstones);
    let mut last_snapshot_at = unix_seconds();
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = tick.tick() => {
                let now = unix_seconds();
                for lease in wheel.advance(now) {
                    let key_id = lease.key_id();
                    let version = lease.wheel_version.load(Ordering::Acquire);
                    if lease.expires_at() > now {
                        wheel.insert_with_version(lease, version);
                        continue;
                    }
                    let mut slots = inner.slots.write().unwrap_or_else(|poisoned| poisoned.into_inner());
                    if let Some(slot) = slots.get_mut(key_slot(key_id) as usize) {
                        if slot.generation == key_generation(key_id) && slot.state == SlotState::Active {
                            slot.state = SlotState::Expired;
                            lease.cancel_expired();
                            let tombstoned_at = slot.expires_at;
                            if let Some(metadata) = cold.get_mut(&key_id) {
                                metadata.tombstoned_at = tombstoned_at;
                            }
                            push_tombstone(
                                &mut tombstones,
                                tombstoned_at,
                                key_id,
                            );
                            tracing::info!(
                                event = "temporary_key_expired",
                                auth_stage = "expiry",
                                key_id,
                                expires_at = lease.expires_at(),
                                "temporary key expired and active work was cancelled"
                            );
                        }
                    } else {
                        lease.cancel_expired();
                    }
                }
                expire_due_high_slots(&inner, &mut cold, &mut tombstones, now);
                while let Some((cleanup_at, key_id)) = tombstones.front().copied() {
                    if cleanup_at > now {
                        break;
                    }
                    tombstones.pop_front();
                    let mut slots = inner.slots.write().unwrap_or_else(|poisoned| poisoned.into_inner());
                    if let Some(slot) = slots.get_mut(key_slot(key_id) as usize) {
                        if slot.generation == key_generation(key_id) && matches!(slot.state, SlotState::Expired | SlotState::Revoked) {
                            slot.state = SlotState::Free;
                            slot.expires_at = 0;
                            slot.lease = Weak::new();
                            cold.remove(&key_id);
                            wheel.release(key_id);
                        }
                    } else {
                        let mut high = inner
                            .high_slot_entries
                            .write()
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        high.retain(|entry| entry.key_id != key_id);
                        cold.remove(&key_id);
                        wheel.release(key_id);
                    }
                }
                prune_expired_admin_replays(
                    now,
                    &mut admin_replays,
                    &mut admin_replay_order,
                );
                // WHY: A failed load starts safe mode with empty in-memory
                // generations. Compacting that reconstruction would replace the
                // damaged snapshot, truncate the WAL, and let the next start
                // exit safe mode without rotating the instance id.
                if compaction_is_allowed(inner.safe_mode.load(Ordering::Acquire))
                    && now.saturating_sub(last_snapshot_at)
                        >= SNAPSHOT_COMPACTION_INTERVAL.as_secs()
                {
                    let snapshot = build_snapshot(&inner, &cold, &admin_replay_order);
                    if let Err(error) = write_snapshot_and_truncate_wal(
                        &config,
                        &inner.admin_key(),
                        &snapshot,
                    ) {
                        inner.safe_mode.store(true, Ordering::Release);
                        cancel_all_temporary_leases(&inner);
                        tracing::error!(
                            event = "auth_state_safe_mode",
                            auth_stage = "snapshot_compaction",
                            reason = %error.code,
                            error = %error,
                            "authentication state compaction failed closed"
                        );
                    } else {
                        last_snapshot_at = now;
                    }
                }
            }
            command = command_rx.recv() => {
                let Some(command) = command else {
                    admin_lease.cancel_rotated();
                    cancel_all_temporary_leases(&inner);
                    break;
                };
                match command {
                    AuthCommand::ClaimAdminMutation {
                        authority,
                        fingerprint,
                        client_timestamp,
                        response,
                    } => {
                        let result = validate_admin_authority(&inner, &authority).and_then(|()| {
                            actor_claim_admin_mutation(
                                &inner,
                                &config,
                                &mut admin_replays,
                                &mut admin_replay_order,
                                fingerprint,
                                client_timestamp,
                            )
                        });
                        let _ = response.send(result);
                    }
                    AuthCommand::Issue { authority, ttl, label, response } => {
                        let result = validate_admin_authority(&inner, &authority)
                            .and_then(|()| actor_issue(&inner, &config, &mut cold, &mut wheel, ttl, label));
                        let _ = response.send(result);
                    }
                    AuthCommand::List { authority, page, page_size, response } => {
                        let result = validate_admin_authority(&inner, &authority)
                            .and_then(|()| actor_list(&inner, &cold, page, page_size));
                        let _ = response.send(result);
                    }
                    AuthCommand::Show { authority, key_id, reveal, response } => {
                        let result = validate_admin_authority(&inner, &authority)
                            .and_then(|()| actor_show(&inner, &config, &cold, key_id, reveal));
                        let _ = response.send(result);
                    }
                    AuthCommand::Renew { authority, key_id, ttl, response } => {
                        let result = validate_admin_authority(&inner, &authority)
                            .and_then(|()| actor_renew(&inner, &config, &cold, &mut wheel, key_id, ttl));
                        let _ = response.send(result);
                    }
                    AuthCommand::Revoke { authority, key_id, response } => {
                        let result = validate_admin_authority(&inner, &authority)
                            .and_then(|()| actor_revoke(&inner, &config, &mut cold, &mut tombstones, key_id));
                        let _ = response.send(result);
                    }
                    AuthCommand::Gc { authority, response } => {
                        let result = validate_admin_authority(&inner, &authority)
                            .and_then(|()| actor_gc(
                                &inner,
                                &config,
                                &mut cold,
                                &mut wheel,
                                &mut tombstones,
                                &admin_replay_order,
                            ));
                        let _ = response.send(result);
                    }
                    AuthCommand::Reset { authority, response } => {
                        let result = validate_admin_authority(&inner, &authority)
                            .and_then(|()| actor_reset(
                                &inner,
                                &config,
                                &mut cold,
                                &mut wheel,
                                &admin_replay_order,
                                "auth_state_reset",
                            ));
                        let _ = response.send(result);
                    }
                    AuthCommand::RotateRoot { authority, new_key, response } => {
                        let result = validate_admin_authority(&inner, &authority)
                            .and_then(|()| actor_rotate_root(&inner, &config, &mut cold, &mut wheel, &mut admin_lease, new_key));
                        if result.is_ok() {
                            admin_replays.clear();
                            admin_replay_order.clear();
                        }
                        let _ = response.send(result);
                    }
                    AuthCommand::SetLegacyProtocol { authority, policy, response } => {
                        let result = validate_admin_authority(&inner, &authority)
                            .and_then(|()| actor_set_legacy_protocol(&inner, &config, policy));
                        let _ = response.send(result);
                    }
                    AuthCommand::Status { authority, response } => {
                        let result = validate_admin_authority(&inner, &authority)
                            .map(|()| actor_status(&inner));
                        let _ = response.send(result);
                    }
                    AuthCommand::Audit { authority, action, key_id, detail, response } => {
                        let result = validate_admin_authority(&inner, &authority).and_then(|()| {
                            append_audit(
                                &config,
                                &inner,
                                audit(&action, key_id, detail),
                            )
                        });
                        let _ = response.send(result);
                    }
                    AuthCommand::Shutdown { response } => {
                        admin_lease.cancel_rotated();
                        cancel_all_temporary_leases(&inner);
                        let _ = response.send(());
                        break;
                    }
                }
            }
        }
    }
}

fn actor_claim_admin_mutation(
    inner: &AuthStateInner,
    config: &AuthConfig,
    admin_replays: &mut HashSet<[u8; 32]>,
    admin_replay_order: &mut VecDeque<AdminReplayRecord>,
    fingerprint: [u8; 32],
    client_timestamp: u64,
) -> Result<(), AuthFailure> {
    let now = unix_seconds();
    prune_expired_admin_replays(now, admin_replays, admin_replay_order);
    if admin_replays.contains(&fingerprint) {
        return Err(AuthFailure::new(
            "admin_request_replayed",
            "administrator mutation was already admitted",
            false,
        ));
    }
    if admin_replays.len() >= ADMIN_REPLAY_CAPACITY {
        return Err(AuthFailure::new(
            "admin_replay_capacity_exhausted",
            "administrator mutation replay window is full; retry after older claims expire",
            true,
        ));
    }
    if now.abs_diff(client_timestamp) > ADMIN_REPLAY_RETENTION.as_secs() / 2 {
        return Err(AuthFailure::new(
            "admin_request_timestamp_invalid",
            "administrator mutation timestamp is outside the accepted window",
            false,
        ));
    }
    let record = AdminReplayRecord {
        fingerprint,
        client_timestamp,
        accepted_at: now,
    };
    fail_closed_on_uncertain_wal(
        inner,
        append_wal(
            config,
            &inner.admin_key(),
            &WalRecord::AdminReplay(record.clone()),
        ),
    )?;
    admin_replays.insert(fingerprint);
    admin_replay_order.push_back(record);
    Ok(())
}

pub(super) fn prune_expired_admin_replays(
    now: u64,
    admin_replays: &mut HashSet<[u8; 32]>,
    admin_replay_order: &mut VecDeque<AdminReplayRecord>,
) {
    admin_replay_order.retain(|record| {
        let keep = record.within_retention(now);
        if !keep {
            admin_replays.remove(&record.fingerprint);
        }
        keep
    });
}

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

fn actor_issue(
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

fn actor_list(
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

fn actor_show(
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

fn actor_renew(
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

fn actor_revoke(
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

fn remember_previous_root(inner: &AuthStateInner) {
    *inner
        .previous_root
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(PreviousRoot {
        admin_key: inner.admin_key(),
        instance_id: inner.instance_id(),
    });
}

fn push_tombstone(tombstones: &mut VecDeque<(u64, u64)>, tombstoned_at: u64, key_id: u64) {
    let cleanup_at = tombstoned_at.saturating_add(TOMBSTONE_RETENTION.as_secs());
    let index = tombstones.partition_point(|(current, _)| *current <= cleanup_at);
    tombstones.insert(index, (cleanup_at, key_id));
}

fn actor_gc(
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

fn actor_reset(
    inner: &Arc<AuthStateInner>,
    config: &AuthConfig,
    cold: &mut HashMap<u64, ColdMetadata>,
    wheel: &mut TimingWheel,
    admin_replays: &VecDeque<AdminReplayRecord>,
    action: &str,
) -> Result<(), AuthFailure> {
    let new_instance_id = random_instance_id();
    inner.root_epoch.fetch_add(1, Ordering::AcqRel);
    let reset_audit = audit(action, None, None);
    let mut snapshot = empty_snapshot(inner, new_instance_id, admin_replays);
    push_persisted_audit(&mut snapshot.audit_records, reset_audit.clone());
    let admin_key = inner.admin_key();
    let next_instance_path = config.state_dir.join("server-instance-id.next");
    if let Err(error) = atomic_write(&next_instance_path, &new_instance_id, 0o600)
        .and_then(|()| write_snapshot_and_truncate_wal(config, &admin_key, &snapshot))
        .and_then(|()| {
            atomic_write(
                &config.state_dir.join("server-instance-id"),
                &new_instance_id,
                0o600,
            )
        })
    {
        if !reset_already_installed(&config.state_dir, &admin_key, &new_instance_id) {
            inner.safe_mode.store(true, Ordering::Release);
            cancel_all_temporary_leases(inner);
            return Err(error);
        }
        tracing::warn!(
            event = "auth_state_reset_finalized_after_sync_error",
            error = %error,
            "server-instance-id replacement reported an error, but the live id and snapshot already match the new instance; finishing in-memory reset"
        );
    }
    let _ = std::fs::remove_file(&next_instance_path);
    push_audit_record(inner, reset_audit);
    remember_previous_root(inner);

    cancel_all_temporary_leases(inner);
    {
        let mut slots = inner
            .slots
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for slot in slots.iter_mut() {
            slot.state = SlotState::Free;
            slot.expires_at = 0;
            slot.lease = Weak::new();
        }
    }
    *inner
        .instance_id
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = new_instance_id;
    cold.clear();
    wheel.clear(unix_seconds());
    clear_retained_high_slot_entries(inner);
    inner.safe_mode.store(false, Ordering::Release);
    Ok(())
}

fn actor_rotate_root(
    inner: &Arc<AuthStateInner>,
    config: &AuthConfig,
    cold: &mut HashMap<u64, ColdMetadata>,
    wheel: &mut TimingWheel,
    admin_lease: &mut Arc<AuthLease>,
    new_key: AesKeyType,
) -> Result<(), AuthFailure> {
    if new_key == inner.admin_key() {
        return Err(AuthFailure::new(
            "administrator_key_unchanged",
            "new administrator key must differ from the current key",
            false,
        ));
    }
    if !is_env_safe_admin_key(&new_key) {
        return Err(AuthFailure::new(
            "administrator_key_invalid",
            env_safe_admin_key_error(),
            false,
        ));
    }
    let new_key_string =
        String::from_utf8(new_key.to_vec()).expect("printable ASCII is valid UTF-8");

    inner.root_epoch.fetch_add(1, Ordering::AcqRel);
    let rotate_audit = audit("administrator_key_rotate", None, None);
    let mut snapshot = empty_snapshot(inner, inner.instance_id(), &VecDeque::new());
    push_persisted_audit(&mut snapshot.audit_records, rotate_audit.clone());
    let next_key_path = config.state_dir.join("admin.key.next");
    if let Err(error) = write_admin_key_file(&next_key_path, &new_key_string, true)
        .and_then(|()| write_snapshot_and_truncate_wal(config, &new_key, &snapshot))
        .and_then(|()| write_admin_key(&config.state_dir, &new_key_string))
    {
        if !key_matches_existing_snapshot(Some(&config.state_dir), &new_key_string) {
            inner.safe_mode.store(true, Ordering::Release);
            cancel_all_temporary_leases(inner);
            return Err(error);
        }
        tracing::warn!(
            event = "administrator_key_rotate_finalized_after_sync_error",
            error = %error,
            "admin.key replacement reported an error, but the new snapshot already decrypts with the new key; finishing in-memory rotation"
        );
    }
    let _ = std::fs::remove_file(&next_key_path);
    push_audit_record(inner, rotate_audit);
    remember_previous_root(inner);

    cancel_all_temporary_leases(inner);
    let old_admin_lease = admin_lease.clone();
    {
        let mut slots = inner
            .slots
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for slot in slots.iter_mut() {
            slot.state = SlotState::Free;
            slot.expires_at = 0;
            slot.lease = Weak::new();
        }
    }
    cold.clear();
    wheel.clear(unix_seconds());
    clear_retained_high_slot_entries(inner);
    let new_admin_lease = Arc::new(AuthLease::new(0, u64::MAX));
    *inner
        .admin
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = AdminState {
        key: new_key,
        lease: Arc::downgrade(&new_admin_lease),
    };
    if inner.sync_process_credential {
        set_process_msg_header_key(Some(&new_key_string)).map_err(AuthFailure::internal)?;
    }
    inner.safe_mode.store(false, Ordering::Release);
    old_admin_lease.cancel_rotated();
    *admin_lease = new_admin_lease;
    Ok(())
}

fn actor_set_legacy_protocol(
    inner: &Arc<AuthStateInner>,
    config: &AuthConfig,
    policy: LegacyProtocolPolicy,
) -> Result<(), AuthFailure> {
    ensure_store_available(inner)?;
    append_mutation(
        config,
        inner,
        StateMutation::LegacyProtocol(policy),
        audit("legacy_protocol_update", None, Some(format!("{policy:?}"))),
    )?;
    inner
        .legacy_protocol_allowed
        .store(policy.is_allowed(), Ordering::Release);
    Ok(())
}

fn actor_status(inner: &Arc<AuthStateInner>) -> AuthStatus {
    let slots = inner
        .slots
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let high = inner
        .high_slot_entries
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let active_keys = slots
        .iter()
        .filter(|slot| slot.state == SlotState::Active)
        .count()
        + high
            .iter()
            .filter(|entry| entry.state == SlotState::Active)
            .count();
    let expired_keys = slots
        .iter()
        .filter(|slot| slot.state == SlotState::Expired)
        .count()
        + high
            .iter()
            .filter(|entry| entry.state == SlotState::Expired)
            .count();
    let revoked_keys = slots
        .iter()
        .filter(|slot| slot.state == SlotState::Revoked)
        .count()
        + high
            .iter()
            .filter(|entry| entry.state == SlotState::Revoked)
            .count();
    let last_legacy_connection_at = inner.last_legacy_connection_at.load(Ordering::Acquire);
    AuthStatus {
        schema_version: 1,
        safe_mode: inner.safe_mode.load(Ordering::Acquire),
        capacity: slots.len(),
        active_keys,
        expired_keys,
        revoked_keys,
        legacy_protocol: if inner.legacy_protocol_allowed.load(Ordering::Acquire) {
            LegacyProtocolPolicy::Allow
        } else {
            LegacyProtocolPolicy::Deny
        },
        active_legacy_connections: inner.active_legacy_connections.load(Ordering::Acquire),
        last_legacy_connection_at: (last_legacy_connection_at != 0)
            .then_some(last_legacy_connection_at),
        auth_successes: inner.auth_successes.load(Ordering::Relaxed),
        auth_failures: inner.auth_failures.load(Ordering::Relaxed),
        server_instance_id: hex(&inner.instance_id()),
    }
}

fn validate_admin_authority(
    inner: &AuthStateInner,
    authority: &Weak<AuthLease>,
) -> Result<(), AuthFailure> {
    let presented = authority.upgrade().ok_or_else(|| {
        AuthFailure::new(
            "administrator_key_rotated",
            "administrator credential lease is no longer active",
            false,
        )
    })?;
    if presented.cancellation.is_cancelled() {
        return Err(AuthFailure::new(
            "administrator_key_rotated",
            "administrator credential lease has been cancelled",
            false,
        ));
    }
    let current = inner
        .admin
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .lease
        .upgrade()
        .ok_or_else(|| {
            AuthFailure::new(
                "administrator_key_rotated",
                "active administrator credential lease is unavailable",
                false,
            )
        })?;
    if !Arc::ptr_eq(&presented, &current) {
        return Err(AuthFailure::new(
            "administrator_key_rotated",
            "administrator request was authenticated before the latest root-key rotation",
            false,
        ));
    }
    Ok(())
}

fn ensure_store_available(inner: &AuthStateInner) -> Result<(), AuthFailure> {
    if inner.safe_mode.load(Ordering::Acquire) {
        Err(AuthFailure::new(
            "temporary_key_store_unavailable",
            "temporary key store is in administrator safe mode",
            false,
        ))
    } else {
        Ok(())
    }
}

fn validate_slot_identity(slot: &SlotHot, key_id: u64) -> Result<(), AuthFailure> {
    if slot.generation != key_generation(key_id) || slot.state == SlotState::Free {
        Err(key_not_found(key_id))
    } else {
        Ok(())
    }
}

fn key_not_found(key_id: u64) -> AuthFailure {
    AuthFailure::new(
        "temporary_key_not_found",
        format!("temporary key {key_id} does not exist"),
        false,
    )
}

fn key_not_renewable() -> AuthFailure {
    AuthFailure::new(
        "temporary_key_not_renewable",
        "only an active, unexpired temporary key can be renewed",
        false,
    )
}

fn key_not_active() -> AuthFailure {
    AuthFailure::new(
        "temporary_key_not_active",
        "temporary key is not active",
        false,
    )
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

fn expire_due_high_slots(
    inner: &Arc<AuthStateInner>,
    cold: &mut HashMap<u64, ColdMetadata>,
    tombstones: &mut VecDeque<(u64, u64)>,
    now: u64,
) {
    let mut high = inner
        .high_slot_entries
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut expired = Vec::new();
    for entry in high.iter_mut() {
        if entry.state == SlotState::Active && entry.expires_at <= now {
            entry.state = SlotState::Expired;
            let tombstoned_at = entry.expires_at;
            entry.tombstoned_at = Some(tombstoned_at);
            if let Some(metadata) = cold.get_mut(&entry.key_id) {
                metadata.tombstoned_at = tombstoned_at;
            }
            expired.push((tombstoned_at, entry.key_id, entry.expires_at));
        }
    }
    drop(high);
    for (tombstoned_at, key_id, expires_at) in expired {
        push_tombstone(tombstones, tombstoned_at, key_id);
        tracing::info!(
            event = "temporary_key_expired",
            auth_stage = "expiry",
            key_id,
            expires_at,
            "high-slot temporary key expired"
        );
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

fn slot_state_name(state: SlotState) -> &'static str {
    match state {
        SlotState::Free => "free",
        SlotState::Active => "active",
        SlotState::Expired => "expired",
        SlotState::Revoked => "revoked",
    }
}

fn audit(action: &str, key_id: Option<u64>, label: Option<String>) -> AuditRecord {
    AuditRecord {
        at: unix_seconds(),
        action: action.to_string(),
        key_id,
        label,
    }
}
