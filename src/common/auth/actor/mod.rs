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
    leases: Leases,
    admin_replays: HashSet<[u8; 32]>,
    admin_replay_order: VecDeque<AdminReplayRecord>,
}

impl AuthActorState {
    pub(super) fn new(
        leases: Leases,
        admin_replays: HashSet<[u8; 32]>,
        admin_replay_order: VecDeque<AdminReplayRecord>,
    ) -> Self {
        Self {
            leases,
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
        mut leases,
        mut admin_replays,
        mut admin_replay_order,
    } = state;
    let mut last_snapshot_at = unix_seconds();
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = tick.tick() => {
                let now = unix_seconds();
                leases.tick(now);
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
                    let snapshot = build_snapshot(&inner, &admin_replay_order);
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
                            .and_then(|()| actor_issue(&inner, &config, &mut leases, ttl, label));
                        let _ = response.send(result);
                    }
                    AuthCommand::List { authority, page, page_size, response } => {
                        let result = validate_admin_authority(&inner, &authority)
                            .and_then(|()| actor_list(&inner, page, page_size));
                        let _ = response.send(result);
                    }
                    AuthCommand::Show { authority, key_id, reveal, response } => {
                        let result = validate_admin_authority(&inner, &authority)
                            .and_then(|()| actor_show(&inner, &config, key_id, reveal));
                        let _ = response.send(result);
                    }
                    AuthCommand::Renew { authority, key_id, ttl, response } => {
                        let result = validate_admin_authority(&inner, &authority)
                            .and_then(|()| actor_renew(&inner, &config, &mut leases, key_id, ttl));
                        let _ = response.send(result);
                    }
                    AuthCommand::Revoke { authority, key_id, response } => {
                        let result = validate_admin_authority(&inner, &authority)
                            .and_then(|()| actor_revoke(&inner, &config, &mut leases, key_id));
                        let _ = response.send(result);
                    }
                    AuthCommand::Gc { authority, response } => {
                        let result = validate_admin_authority(&inner, &authority)
                            .and_then(|()| actor_gc(&inner, &config, &mut leases, &admin_replay_order));
                        let _ = response.send(result);
                    }
                    AuthCommand::Reset { authority, response } => {
                        let result = validate_admin_authority(&inner, &authority)
                            .and_then(|()| actor_reset(&inner, &config, &mut leases, &admin_replay_order, "auth_state_reset"));
                        let _ = response.send(result);
                    }
                    AuthCommand::RotateRoot { authority, new_key, response } => {
                        let result = validate_admin_authority(&inner, &authority)
                            .and_then(|()| actor_rotate_root(&inner, &config, &mut leases, &mut admin_lease, new_key));
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

fn actor_status(inner: &Arc<AuthStateInner>) -> AuthStatus {
    let slots = inner.slots();
    let high = inner.high();
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
    let current = inner.admin.read().lease.upgrade().ok_or_else(|| {
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

fn validate_slot_identity(slot: &SlotHot, key_id: KeyId) -> Result<(), AuthFailure> {
    if slot.generation != key_id.generation() || slot.state == SlotState::Free {
        Err(key_not_found(key_id))
    } else {
        Ok(())
    }
}

fn key_not_found(key_id: KeyId) -> AuthFailure {
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

fn slot_state_name(state: SlotState) -> &'static str {
    match state {
        SlotState::Free => "free",
        SlotState::Active => "active",
        SlotState::Expired => "expired",
        SlotState::Revoked => "revoked",
    }
}

fn audit(action: &str, key_id: Option<KeyId>, label: Option<String>) -> AuditRecord {
    AuditRecord {
        at: unix_seconds(),
        action: action.to_string(),
        key_id,
        label,
    }
}
