//! Public authentication runtime facade and hot-path credential checks.
//!
//! ```text
//! process credential + persisted state
//!                 |
//!                 v
//!       hot slot table (Weak leases) <---- request authentication
//!                 |
//!                 +----> lifecycle actor (strong leases + time wheel)
//! ```
//!
//! Read-only authentication stays synchronous and allocation-light. Every administrator
//! API captures a weak authority lease and sends it to the actor, where it is compared
//! with the current lease immediately before the operation executes.

use super::*;

/// A handle on a running authentication state. Clones share one handle, so
/// cloning is cheap, and the actor stops once the last clone is dropped.
#[derive(Clone)]
pub struct AuthRuntime {
    shared: Arc<RuntimeShared>,
}

struct RuntimeShared {
    inner: Weak<AuthStateInner>,
    /// The only sender: dropping the last handle closes the channel, which is
    /// what stops an actor nobody shut down.
    command_tx: mpsc::Sender<AuthCommand>,
    config: AuthConfig,
    _state_lock: Arc<File>,
    actor: Mutex<Option<tokio::task::JoinHandle<()>>>,
    actor_abort: tokio::task::AbortHandle,
    /// Fails `changed()` once the state is dropped, for `abort_actor`.
    state_dropped: watch::Receiver<()>,
}

pub struct LegacyConnectionGuard {
    inner: Weak<AuthStateInner>,
}

impl Drop for LegacyConnectionGuard {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.upgrade() {
            inner
                .active_legacy_connections
                .fetch_sub(1, Ordering::AcqRel);
        }
    }
}

impl AuthRuntime {
    pub async fn from_process(config: AuthConfig) -> Result<Self, AuthFailure> {
        let state_lock = prepare_state_dir_and_lock(&config.state_dir)?;
        let credential = load_server_admin_credential(&config.state_dir)?;
        let Credential::Admin(admin_key) = credential else {
            return Err(AuthFailure::new(
                "administrator_key_required",
                "the relay server must start with the administrator credential",
                false,
            ));
        };
        Self::start_locked(admin_key, config, true, state_lock)
    }

    /// Start an embedded relay with an administrator key owned only by its state directory.
    ///
    /// This deliberately leaves the process credential untouched because the containing UI uses
    /// that credential for its outbound register, connect, status, and stream connections.
    pub async fn from_isolated_state(config: AuthConfig) -> Result<Self, AuthFailure> {
        let state_lock = prepare_state_dir_and_lock(&config.state_dir)?;
        let credential = load_isolated_server_admin_credential(&config.state_dir)?;
        let Credential::Admin(admin_key) = credential else {
            return Err(AuthFailure::new(
                "administrator_key_required",
                "the embedded relay must start with an administrator credential",
                false,
            ));
        };
        Self::start_locked(admin_key, config, false, state_lock)
    }

    pub async fn start(admin_key: AesKeyType, config: AuthConfig) -> Result<Self, AuthFailure> {
        let state_lock = prepare_state_dir_and_lock(&config.state_dir)?;
        Self::start_locked(admin_key, config, true, state_lock)
    }

    /// Restores the state under the directory lock already taken, and starts
    /// the actor that owns it.
    fn start_locked(
        admin_key: AesKeyType,
        config: AuthConfig,
        sync_process_credential: bool,
        state_lock: Arc<File>,
    ) -> Result<Self, AuthFailure> {
        let instance_id = load_or_create_instance_id(&config.state_dir)?;
        let instance_id =
            recover_instance_id_after_reset(&config.state_dir, &admin_key, instance_id)?;
        let (mut loaded, safe_mode) = load_persisted_state(&config, &admin_key, instance_id);
        let now = unix_seconds();
        if let Some(state) = loaded.as_mut()
            && normalize_tombstone_times(state, now)
        {
            write_snapshot_and_truncate_wal(&config, &admin_key, state)?;
        }
        let (mut restored, live) =
            RestoredState::from_snapshot(loaded.as_ref(), safe_mode, &config, now);
        let admin_replays = AdminReplays::restored(
            loaded.as_ref().map_or(&[], |state| &state.admin_replays),
            now,
        );
        restored.namespace_high_water =
            restore_namespace_high_water(&config.state_dir, restored.namespace_high_water);
        let admin_lease = Arc::new(AuthLease::new(ADMIN_KEY_ID, None, u64::MAX));
        let inner = Arc::new(AuthStateInner::new(
            admin_key,
            &admin_lease,
            sync_process_credential,
            instance_id,
            safe_mode,
            restored,
        ));
        // Before the spawn: a reap already due runs while scheduling.
        let leases = Leases::restored(&inner, now, live);
        let (command_tx, command_rx) = mpsc::channel(256);
        let actor = tokio::spawn(
            Actor::new(
                inner.clone(),
                admin_lease,
                config.clone(),
                leases,
                admin_replays,
                state_lock.clone(),
            )
            .run(command_rx),
        );
        let actor_abort = actor.abort_handle();
        let runtime = Self {
            shared: Arc::new(RuntimeShared {
                inner: Arc::downgrade(&inner),
                command_tx,
                config: config.clone(),
                _state_lock: state_lock.clone(),
                actor: Mutex::new(Some(actor)),
                actor_abort,
                state_dropped: inner.subscribe_dropped(),
            }),
        };
        Ok(runtime)
    }

    pub async fn shutdown_actor(&self) {
        let (response, receiver) = oneshot::channel();
        let _ = self
            .shared
            .command_tx
            .send(AuthCommand::Shutdown { response })
            .await;
        let _ = receiver.await;
        let handle = self.shared.actor.lock().take();
        if let Some(handle) = handle {
            let _ = handle.await;
        }
    }

    pub async fn abort_actor(&self) -> Result<(), AuthFailure> {
        self.shared.actor_abort.abort();
        let handle = self.shared.actor.lock().take();
        if let Some(handle) = handle {
            let _ = handle.await;
        }
        // Whatever still holds the state, such as a request mid-authentication,
        // lets go of it shortly after the actor has gone.
        let mut state_dropped = self.shared.state_dropped.clone();
        tokio::time::timeout(Duration::from_secs(5), async {
            // Nothing is ever sent, so this ends only when the state drops.
            while state_dropped.changed().await.is_ok() {}
        })
        .await
        .map_err(|_| {
            AuthFailure::new(
                "auth_state_unavailable",
                "authentication actor did not drop after abort",
                true,
            )
        })
    }

    pub fn config(&self) -> &AuthConfig {
        &self.shared.config
    }

    fn inner(&self) -> Result<Arc<AuthStateInner>, AuthFailure> {
        self.shared.inner.upgrade().ok_or_else(|| {
            AuthFailure::new(
                "auth_state_unavailable",
                "authentication state manager is not running",
                true,
            )
        })
    }

    pub fn admin_key(&self) -> Result<AesKeyType, AuthFailure> {
        Ok(self.inner()?.admin_key())
    }

    pub fn derive_key(&self, key_id: KeyId) -> Result<AesKeyType, AuthFailure> {
        let inner = self.inner()?;
        if key_id.is_admin() {
            return Ok(inner.admin_key());
        }
        derive_temporary_key(&inner.admin_key(), &inner.instance_id(), key_id)
    }

    #[cfg(test)]
    pub(crate) fn high_slot_entry_count(&self) -> usize {
        self.inner().map(|inner| inner.high().len()).unwrap_or(0)
    }

    pub fn derive_previous_key(&self, key_id: KeyId) -> Option<AesKeyType> {
        let inner = self.inner().ok()?;
        let previous = inner.previous_root.read().clone()?;
        if key_id.is_admin() {
            Some(previous.admin_key)
        } else {
            derive_temporary_key(&previous.admin_key, &previous.instance_id, key_id).ok()
        }
    }

    pub fn authenticate_presented(
        &self,
        key_id: KeyId,
        presented_key: &AesKeyType,
    ) -> Result<AuthContext, AuthFailure> {
        let inner = self.inner()?;
        if key_id.is_admin() {
            let admin = inner.admin.read();
            if !bool::from(presented_key.ct_eq(&admin.key)) {
                inner.auth_failures.fetch_add(1, Ordering::Relaxed);
                return Err(AuthFailure::new(
                    "administrator_key_invalid",
                    "administrator credential does not match the active root key",
                    false,
                ));
            }
            let lease = admin.lease.upgrade().ok_or_else(|| {
                AuthFailure::new(
                    "administrator_key_rotated",
                    "administrator credential was rotated",
                    false,
                )
            })?;
            inner.auth_successes.fetch_add(1, Ordering::Relaxed);
            return Ok(AuthContext::from_lease(ADMIN_KEY_ID, true, &lease));
        }
        if inner.safe_mode.load(Ordering::Acquire) {
            inner.auth_failures.fetch_add(1, Ordering::Relaxed);
            return Err(AuthFailure::new(
                "temporary_key_store_unavailable",
                "temporary key state is unavailable; administrator reset is required",
                false,
            ));
        }

        let expected_key = derive_temporary_key(&inner.admin_key(), &inner.instance_id(), key_id)?;
        if !bool::from(presented_key.ct_eq(&expected_key)) {
            inner.auth_failures.fetch_add(1, Ordering::Relaxed);
            return Err(temporary_key_material_mismatch(&inner, key_id));
        }

        let index = key_id.slot().as_index();
        let generation = key_id.generation();
        let slots = inner.slots();
        let Some(slot) = slots.get(index) else {
            inner.auth_failures.fetch_add(1, Ordering::Relaxed);
            return Err(AuthFailure::new(
                "temporary_key_not_found",
                "temporary key id is outside the configured slot table",
                false,
            ));
        };
        if slot.generation != generation {
            inner.auth_failures.fetch_add(1, Ordering::Relaxed);
            return Err(AuthFailure::new(
                "temporary_key_generation_mismatch",
                "temporary key generation does not match the current slot",
                false,
            ));
        }
        let failure = match slot.state {
            SlotState::Free => Some(AuthFailure::new(
                "temporary_key_not_found",
                "temporary key does not exist",
                false,
            )),
            SlotState::Expired => Some(AuthFailure::new(
                "temporary_key_expired",
                "temporary key has expired",
                false,
            )),
            SlotState::Revoked => Some(AuthFailure::new(
                "temporary_key_revoked",
                "temporary key was revoked",
                false,
            )),
            SlotState::Active if slot.expires_at <= unix_seconds() => {
                if let Some(lease) = slot.lease.upgrade() {
                    lease.cancel_expired();
                }
                Some(AuthFailure::new(
                    "temporary_key_expired",
                    "temporary key has expired",
                    false,
                ))
            }
            SlotState::Active => None,
        };
        if let Some(failure) = failure {
            inner.auth_failures.fetch_add(1, Ordering::Relaxed);
            return Err(failure);
        }
        let lease = slot.lease.upgrade().ok_or_else(|| {
            inner.auth_failures.fetch_add(1, Ordering::Relaxed);
            AuthFailure::new(
                "temporary_key_inactive",
                "temporary key lease is no longer active",
                true,
            )
        })?;
        inner.auth_successes.fetch_add(1, Ordering::Relaxed);
        Ok(AuthContext::from_lease(key_id, false, &lease))
    }

    pub fn legacy_protocol_allowed(&self) -> Result<bool, AuthFailure> {
        Ok(self
            .inner()?
            .legacy_protocol_allowed
            .load(Ordering::Acquire))
    }

    pub fn record_legacy_connection(&self) -> Result<LegacyConnectionGuard, AuthFailure> {
        let inner = self.inner()?;
        inner
            .active_legacy_connections
            .fetch_add(1, Ordering::AcqRel);
        inner
            .last_legacy_connection_at
            .store(unix_seconds(), Ordering::Release);
        Ok(LegacyConnectionGuard {
            inner: Arc::downgrade(&inner),
        })
    }

    async fn request<T>(
        &self,
        build: impl FnOnce(oneshot::Sender<Result<T, AuthFailure>>) -> AuthCommand,
    ) -> Result<T, AuthFailure> {
        let (response, receiver) = oneshot::channel();
        self.shared
            .command_tx
            .send(build(response))
            .await
            .map_err(|_| {
                AuthFailure::new(
                    "auth_state_unavailable",
                    "authentication state manager is not running",
                    true,
                )
            })?;
        receiver.await.map_err(|_| {
            AuthFailure::new(
                "auth_state_unavailable",
                "authentication state manager dropped the response",
                true,
            )
        })?
    }

    pub async fn claim_admin_mutation(
        &self,
        authorization: &AuthContext,
        fingerprint: [u8; 32],
        client_timestamp: u64,
    ) -> Result<(), AuthFailure> {
        let authority = authorization.admin_authority()?;
        self.request(|response| AuthCommand::ClaimAdminMutation {
            authority,
            fingerprint,
            client_timestamp,
            response,
        })
        .await
    }

    pub async fn issue(
        &self,
        authorization: &AuthContext,
        ttl: Duration,
        label: Option<String>,
    ) -> Result<IssuedTemporaryKey, AuthFailure> {
        self.issue_scoped(authorization, None, ttl, label).await
    }

    /// Reserve a namespace that no key owns, for [`Self::issue_in_namespace`].
    ///
    /// A number is handed out once, ever. A caller that loses the response wastes
    /// it and asks again.
    pub async fn allocate_namespace(
        &self,
        authorization: &AuthContext,
    ) -> Result<u64, AuthFailure> {
        let authority = authorization.admin_authority()?;
        self.request(|response| AuthCommand::AllocateNamespace {
            authority,
            response,
        })
        .await
    }

    /// Issue a key into a namespace from [`Self::allocate_namespace`].
    ///
    /// Every key in a namespace sees the same services and is revoked on its own.
    /// A namespace that was not allocated is refused, including another key's own.
    pub async fn issue_in_namespace(
        &self,
        authorization: &AuthContext,
        namespace: u64,
        ttl: Duration,
        label: Option<String>,
    ) -> Result<IssuedTemporaryKey, AuthFailure> {
        self.issue_scoped(authorization, Some(namespace), ttl, label)
            .await
    }

    async fn issue_scoped(
        &self,
        authorization: &AuthContext,
        namespace: Option<u64>,
        ttl: Duration,
        label: Option<String>,
    ) -> Result<IssuedTemporaryKey, AuthFailure> {
        let authority = authorization.admin_authority()?;
        self.request(|response| AuthCommand::Issue {
            authority,
            namespace,
            ttl,
            label,
            response,
        })
        .await
    }

    pub async fn list(
        &self,
        authorization: &AuthContext,
        page: u32,
        page_size: u16,
    ) -> Result<KeyPage, AuthFailure> {
        let authority = authorization.admin_authority()?;
        self.request(|response| AuthCommand::List {
            authority,
            page,
            page_size,
            response,
        })
        .await
    }

    pub async fn show(
        &self,
        authorization: &AuthContext,
        key_id: KeyId,
        reveal: bool,
    ) -> Result<IssuedTemporaryKey, AuthFailure> {
        let authority = authorization.admin_authority()?;
        self.request(|response| AuthCommand::Show {
            authority,
            key_id,
            reveal,
            response,
        })
        .await
    }

    pub async fn renew(
        &self,
        authorization: &AuthContext,
        key_id: KeyId,
        ttl: Duration,
    ) -> Result<IssuedTemporaryKey, AuthFailure> {
        let authority = authorization.admin_authority()?;
        self.request(|response| AuthCommand::Renew {
            authority,
            key_id,
            ttl,
            response,
        })
        .await
    }

    pub async fn revoke(
        &self,
        authorization: &AuthContext,
        key_id: KeyId,
    ) -> Result<TemporaryKeyMetadata, AuthFailure> {
        let authority = authorization.admin_authority()?;
        self.request(|response| AuthCommand::Revoke {
            authority,
            key_id,
            response,
        })
        .await
    }

    pub async fn gc(&self, authorization: &AuthContext) -> Result<u64, AuthFailure> {
        let authority = authorization.admin_authority()?;
        self.request(|response| AuthCommand::Gc {
            authority,
            response,
        })
        .await
    }

    pub async fn reset(&self, authorization: &AuthContext) -> Result<(), AuthFailure> {
        let authority = authorization.admin_authority()?;
        self.request(|response| AuthCommand::Reset {
            authority,
            response,
        })
        .await
    }

    pub async fn rotate_root(
        &self,
        authorization: &AuthContext,
        new_key: AesKeyType,
    ) -> Result<(), AuthFailure> {
        let authority = authorization.admin_authority()?;
        self.request(|response| AuthCommand::RotateRoot {
            authority,
            new_key,
            response,
        })
        .await
    }

    pub async fn set_legacy_protocol(
        &self,
        authorization: &AuthContext,
        policy: LegacyProtocolPolicy,
    ) -> Result<(), AuthFailure> {
        let authority = authorization.admin_authority()?;
        self.request(|response| AuthCommand::SetLegacyProtocol {
            authority,
            policy,
            response,
        })
        .await
    }

    pub async fn status(&self, authorization: &AuthContext) -> Result<AuthStatus, AuthFailure> {
        let authority = authorization.admin_authority()?;
        self.request(|response| AuthCommand::Status {
            authority,
            response,
        })
        .await
    }

    pub async fn audit_admin(
        &self,
        authorization: &AuthContext,
        action: impl Into<String>,
        key_id: Option<KeyId>,
        detail: Option<String>,
    ) -> Result<(), AuthFailure> {
        let authority = authorization.admin_authority()?;
        let action = action.into();
        self.request(|response| AuthCommand::Audit {
            authority,
            action,
            key_id,
            detail,
            response,
        })
        .await
    }
}

fn temporary_key_material_mismatch(inner: &AuthStateInner, key_id: KeyId) -> AuthFailure {
    let index = key_id.slot().as_index();
    let generation = key_id.generation();
    let slots = inner.slots();
    let current_generation = match slots.get(index) {
        Some(slot) => Some(slot.generation),
        None => index
            .checked_sub(slots.len())
            .and_then(|offset| inner.high_slot_generations.get(offset).copied()),
    };
    let slot_is_active = slots
        .get(index)
        .is_some_and(|slot| slot.state == SlotState::Active && slot.generation == generation);
    if slot_is_active {
        return AuthFailure::new(
            "temporary_key_invalid",
            "temporary credential does not match the active relay key material",
            false,
        );
    }
    let current_epoch = inner.root_epoch.load(Ordering::Acquire);
    if current_epoch > 0
        && generation > Generation::FIRST
        && current_generation.is_some_and(|issued| generation <= issued)
    {
        return AuthFailure::new(
            "temporary_key_rotated",
            "temporary credential was invalidated by administrator root rotation or auth-state reset",
            false,
        );
    }
    AuthFailure::new(
        "temporary_key_invalid",
        "temporary credential does not match the active relay key material",
        false,
    )
}
