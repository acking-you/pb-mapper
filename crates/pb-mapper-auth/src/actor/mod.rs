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
///
/// Fields drop in declaration order, and `inner` must come last: dropping the
/// schedule runs each live key's retire callback, which upgrades the state to
/// cancel the key's lease. So an actor that ends without `stop()` (aborted,
/// or unwinding from a panic) still cancels every live
/// temporary-key session and logs `temporary_key_expired` for it.
pub(super) struct Actor {
    config: AuthConfig,
    leases: Leases,
    /// The current administrator lease. The actor holds its only strong
    /// reference, so replacing it is what ends the previous one.
    admin_lease: Arc<AuthLease>,
    replays: AdminReplays,
    /// The wall-clock second of the last compaction, or of the actor's start.
    last_snapshot_at: u64,
    /// Keeps the state directory locked for as long as the actor runs.
    _state_lock: Arc<File>,
    /// The only strong reference to the state; see above for why it is last.
    inner: Arc<AuthStateInner>,
}

/// What the actor does next, off the runtime's workers.
enum Step {
    Command(AuthCommand),
    /// Compact, at this wall-clock second.
    Compact(u64),
    /// Every handle is gone.
    Stop,
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
            last_snapshot_at: unix_seconds(),
            _state_lock: state_lock,
        }
    }

    /// Serves commands until a shutdown or until every runtime handle is gone,
    /// advancing the schedule once a second in between.
    ///
    /// The schedule advances here. Every command and every compaction, which
    /// write and sync the state's files, runs on the blocking pool instead,
    /// with the actor moved there and back: one step at a time, in the order
    /// they came, each finished before the next begins. So their file work
    /// holds up no other task on the runtime, and each stays whole as if it
    /// ran here: an abort or a runtime shutdown lets a step under way finish
    /// and drops the actor after it, never inside one.
    pub(super) async fn run(self, mut command_rx: mpsc::Receiver<AuthCommand>) {
        let mut actor = self;
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            let step = tokio::select! {
                _ = tick.tick() => {
                    let now = unix_seconds();
                    actor.advance(now);
                    if !actor.compaction_due(now) {
                        continue;
                    }
                    Step::Compact(now)
                }
                command = command_rx.recv() => match command {
                    Some(command) => Step::Command(command),
                    None => Step::Stop,
                },
            };
            match off_runtime(move || actor.step(step)).await {
                Some(Some(next)) => actor = next,
                // Stopped, or the runtime is shutting down: either way the
                // actor has dropped.
                Some(None) | None => break,
            }
        }
    }

    /// Takes one step. Returns the actor while it serves on; once it stops,
    /// it drops here.
    fn step(mut self, step: Step) -> Option<Self> {
        match step {
            Step::Command(command) => {
                if self.handle(command).is_break() {
                    return None;
                }
            }
            Step::Compact(now) => self.compact(now),
            Step::Stop => {
                self.stop();
                return None;
            }
        }
        Some(self)
    }

    /// Runs the schedule and the replay window up to wall-clock second `now`.
    fn advance(&mut self, now: u64) {
        self.leases.tick(now);
        self.replays.prune(now);
    }

    fn compaction_due(&self, now: u64) -> bool {
        // WHY: A failed load starts safe mode with empty in-memory
        // generations. Compacting that reconstruction would replace the
        // damaged snapshot, truncate the WAL, and let the next start
        // exit safe mode without rotating the instance id.
        !self.inner.safe_mode.load(Ordering::Acquire)
            && now.saturating_sub(self.last_snapshot_at) >= SNAPSHOT_COMPACTION_INTERVAL.as_secs()
    }

    /// Replaces the snapshot with the state as of wall-clock second `now` and
    /// empties the WAL, or fails the store closed.
    fn compact(&mut self, now: u64) {
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
            self.last_snapshot_at = now;
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
    let (mut active_keys, mut expired_keys, mut revoked_keys) = (0, 0, 0);
    let states = slots.iter().map(|slot| slot.state);
    for state in states.chain(high.iter().map(|entry| entry.state)) {
        match state {
            SlotState::Free => {}
            SlotState::Active => active_keys += 1,
            SlotState::Expired => expired_keys += 1,
            SlotState::Revoked => revoked_keys += 1,
        }
    }
    let last_legacy_connection_at = inner.last_legacy_connection_at.load(Ordering::Acquire);
    AuthStatus {
        schema_version: ADMIN_SCHEMA_VERSION,
        safe_mode: inner.safe_mode.load(Ordering::Acquire),
        capacity: slots.len(),
        active_keys,
        expired_keys,
        revoked_keys,
        legacy_protocol: inner.legacy_policy(),
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

/// An audit record of `action`, stamped now. See [`AuditRecord::label`] for what
/// the last argument holds.
fn audit(action: &str, key_id: Option<KeyId>, label_or_detail: Option<String>) -> AuditRecord {
    AuditRecord {
        at: unix_seconds(),
        action: action.to_string(),
        key_id,
        label: label_or_detail,
        namespace: None,
    }
}
