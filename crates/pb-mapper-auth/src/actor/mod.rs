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

use std::ops::ControlFlow;

use super::*;

mod command;
mod epoch;
mod lifecycle;
mod replay;
pub(super) use command::AuthCommand;
use epoch::*;
use lifecycle::*;
pub(super) use replay::AdminReplays;

/// Everything the actor owns. The runtime builds it and spawns [`Actor::run`].
pub(super) struct Actor {
    inner: Arc<AuthStateInner>,
    config: AuthConfig,
    leases: Leases,
    /// The current administrator lease. The actor holds its only strong
    /// reference, so replacing it is what ends the previous one.
    admin_lease: Arc<AuthLease>,
    replays: AdminReplays,
    /// Keeps the state directory locked for as long as the actor runs.
    _state_lock: Arc<File>,
}

impl Actor {
    pub(super) fn new(
        inner: Arc<AuthStateInner>,
        admin_lease: Arc<AuthLease>,
        config: AuthConfig,
        leases: Leases,
        replays: AdminReplays,
        state_lock: Arc<File>,
    ) -> Self {
        Self {
            inner,
            config,
            leases,
            admin_lease,
            replays,
            _state_lock: state_lock,
        }
    }

    /// Serves commands until a shutdown or until every runtime handle is gone,
    /// advancing the schedule once a second in between.
    pub(super) async fn run(mut self, mut command_rx: mpsc::Receiver<AuthCommand>) {
        let mut last_snapshot_at = unix_seconds();
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = tick.tick() => self.on_tick(&mut last_snapshot_at),
                command = command_rx.recv() => {
                    let Some(command) = command else {
                        self.stop();
                        break;
                    };
                    if self.handle(command).is_break() {
                        break;
                    }
                }
            }
        }
    }

    fn on_tick(&mut self, last_snapshot_at: &mut u64) {
        let now = unix_seconds();
        self.leases.tick(now);
        self.replays.prune(now);
        // WHY: A failed load starts safe mode with empty in-memory
        // generations. Compacting that reconstruction would replace the
        // damaged snapshot, truncate the WAL, and let the next start
        // exit safe mode without rotating the instance id.
        if !self.inner.safe_mode.load(Ordering::Acquire)
            && now.saturating_sub(*last_snapshot_at) >= SNAPSHOT_COMPACTION_INTERVAL.as_secs()
        {
            let snapshot = build_snapshot(&self.inner, self.replays.records());
            if let Err(error) =
                write_snapshot_and_truncate_wal(&self.config, &self.inner.admin_key(), &snapshot)
            {
                self.inner.safe_mode.store(true, Ordering::Release);
                cancel_all_temporary_leases(&self.inner);
                tracing::error!(
                    event = "auth_state_safe_mode",
                    auth_stage = "snapshot_compaction",
                    reason = %error.code,
                    error = %error,
                    "authentication state compaction failed closed"
                );
            } else {
                *last_snapshot_at = now;
            }
        }
    }

    /// Runs one command. Breaks once the actor should stop.
    fn handle(&mut self, command: AuthCommand) -> ControlFlow<()> {
        match command {
            AuthCommand::ClaimAdminMutation {
                authority,
                fingerprint,
                client_timestamp,
                response,
            } => self.admin(&authority, response, |actor| {
                actor
                    .replays
                    .claim(&actor.inner, &actor.config, fingerprint, client_timestamp)
            }),
            AuthCommand::AllocateNamespace {
                authority,
                response,
            } => self.admin(&authority, response, |actor| {
                actor_allocate_namespace(&actor.inner, &actor.config)
            }),
            AuthCommand::Issue {
                authority,
                namespace,
                ttl,
                label,
                response,
            } => self.admin(&authority, response, |actor| {
                actor_issue(
                    &actor.inner,
                    &actor.config,
                    &mut actor.leases,
                    namespace,
                    ttl,
                    label,
                )
            }),
            AuthCommand::List {
                authority,
                page,
                page_size,
                response,
            } => self.admin(&authority, response, |actor| {
                actor_list(&actor.inner, page, page_size)
            }),
            AuthCommand::Show {
                authority,
                key_id,
                reveal,
                response,
            } => self.admin(&authority, response, |actor| {
                actor_show(&actor.inner, &actor.config, key_id, reveal)
            }),
            AuthCommand::Renew {
                authority,
                key_id,
                ttl,
                response,
            } => self.admin(&authority, response, |actor| {
                actor_renew(&actor.inner, &actor.config, &mut actor.leases, key_id, ttl)
            }),
            AuthCommand::Revoke {
                authority,
                key_id,
                response,
            } => self.admin(&authority, response, |actor| {
                actor_revoke(&actor.inner, &actor.config, &mut actor.leases, key_id)
            }),
            AuthCommand::Gc {
                authority,
                response,
            } => self.admin(&authority, response, |actor| {
                actor_gc(
                    &actor.inner,
                    &actor.config,
                    &mut actor.leases,
                    actor.replays.records(),
                )
            }),
            AuthCommand::Reset {
                authority,
                response,
            } => self.admin(&authority, response, |actor| {
                actor_reset(
                    &actor.inner,
                    &actor.config,
                    &mut actor.leases,
                    actor.replays.records(),
                )
            }),
            AuthCommand::RotateRoot {
                authority,
                new_key,
                response,
            } => self.admin(&authority, response, |actor| {
                let result = actor_rotate_root(
                    &actor.inner,
                    &actor.config,
                    &mut actor.leases,
                    &mut actor.admin_lease,
                    new_key,
                );
                if result.is_ok() {
                    actor.replays.clear();
                }
                result
            }),
            AuthCommand::SetLegacyProtocol {
                authority,
                policy,
                response,
            } => self.admin(&authority, response, |actor| {
                actor_set_legacy_protocol(&actor.inner, &actor.config, policy)
            }),
            AuthCommand::Status {
                authority,
                response,
            } => self.admin(&authority, response, |actor| Ok(actor_status(&actor.inner))),
            AuthCommand::Audit {
                authority,
                action,
                key_id,
                detail,
                response,
            } => self.admin(&authority, response, |actor| {
                append_audit(&actor.config, &actor.inner, audit(&action, key_id, detail))
            }),
            AuthCommand::Shutdown { response } => {
                self.stop();
                let _ = response.send(());
                return ControlFlow::Break(());
            }
        }
        ControlFlow::Continue(())
    }

    /// Runs an administrator command, checking the authority it was sent with
    /// against the current administrator lease immediately beforehand, and
    /// sends the outcome back.
    fn admin<T>(
        &mut self,
        authority: &Weak<AuthLease>,
        response: oneshot::Sender<Result<T, AuthFailure>>,
        command: impl FnOnce(&mut Self) -> Result<T, AuthFailure>,
    ) {
        let result = validate_admin_authority(&self.inner, authority).and_then(|()| command(self));
        let _ = response.send(result);
    }

    /// Ends the administrator lease and every temporary one, for either way
    /// the actor stops.
    fn stop(&mut self) {
        self.admin_lease.cancel_rotated();
        cancel_all_temporary_leases(&self.inner);
    }
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
        schema_version: ADMIN_SCHEMA_VERSION,
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
        capabilities: vec![CAPABILITY_KEY_NAMESPACES.to_string()],
    }
}

fn validate_admin_authority(
    inner: &AuthStateInner,
    authority: &Weak<AuthLease>,
) -> Result<(), AuthFailure> {
    let presented = authority.upgrade().ok_or_else(|| {
        AuthFailure::new(
            codes::ADMINISTRATOR_KEY_ROTATED,
            "administrator credential lease is no longer active",
            false,
        )
    })?;
    if presented.is_cancelled() {
        return Err(AuthFailure::new(
            codes::ADMINISTRATOR_KEY_ROTATED,
            "administrator credential lease has been cancelled",
            false,
        ));
    }
    let current = inner.admin.read().lease.upgrade().ok_or_else(|| {
        AuthFailure::new(
            codes::ADMINISTRATOR_KEY_ROTATED,
            "active administrator credential lease is unavailable",
            false,
        )
    })?;
    if !Arc::ptr_eq(&presented, &current) {
        return Err(AuthFailure::new(
            codes::ADMINISTRATOR_KEY_ROTATED,
            "administrator request was authenticated before the latest root-key rotation",
            false,
        ));
    }
    Ok(())
}

fn ensure_store_available(inner: &AuthStateInner) -> Result<(), AuthFailure> {
    if inner.safe_mode.load(Ordering::Acquire) {
        Err(AuthFailure::new(
            codes::TEMPORARY_KEY_STORE_UNAVAILABLE,
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
        codes::TEMPORARY_KEY_NOT_FOUND,
        format!("temporary key {key_id} does not exist"),
        false,
    )
}

fn key_not_renewable() -> AuthFailure {
    AuthFailure::new(
        codes::TEMPORARY_KEY_NOT_RENEWABLE,
        "only an active, unexpired temporary key can be renewed",
        false,
    )
}

fn key_not_active() -> AuthFailure {
    AuthFailure::new(
        codes::TEMPORARY_KEY_NOT_ACTIVE,
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
        namespace: None,
    }
}
