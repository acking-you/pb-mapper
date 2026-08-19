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

mod epoch;
mod lifecycle;
use epoch::*;
use lifecycle::*;

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
                            push_tombstone(&mut tombstones, tombstoned_at, key_id);
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
                let mut due_high = Vec::new();
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
                        due_high.push(key_id);
                    }
                }
                if !due_high.is_empty() {
                    let due = due_high.iter().copied().collect::<HashSet<_>>();
                    inner
                        .high_slot_entries
                        .write()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .retain(|entry| !due.contains(&entry.key_id));
                    for key_id in due_high {
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

fn push_tombstone(tombstones: &mut VecDeque<(u64, u64)>, tombstoned_at: u64, key_id: u64) {
    let cleanup_at = tombstoned_at.saturating_add(TOMBSTONE_RETENTION.as_secs());
    let index = tombstones.partition_point(|(current, _)| *current <= cleanup_at);
    tombstones.insert(index, (cleanup_at, key_id));
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
    for entry in high.iter_mut() {
        if entry.state != SlotState::Active || entry.expires_at > now {
            continue;
        }
        entry.state = SlotState::Expired;
        let tombstoned_at = entry.expires_at;
        entry.tombstoned_at = Some(tombstoned_at);
        if let Some(metadata) = cold.get_mut(&entry.key_id) {
            metadata.tombstoned_at = tombstoned_at;
        }
        push_tombstone(tombstones, tombstoned_at, entry.key_id);
        tracing::info!(
            event = "temporary_key_expired",
            auth_stage = "expiry",
            key_id = entry.key_id,
            expires_at = entry.expires_at,
            "high-slot temporary key expired"
        );
    }
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
