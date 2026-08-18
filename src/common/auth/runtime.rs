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
        Self::start_locked(admin_key, config, true, state_lock).await
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
        Self::start_locked(admin_key, config, false, state_lock).await
    }

    pub async fn start(admin_key: AesKeyType, config: AuthConfig) -> Result<Self, AuthFailure> {
        let state_lock = prepare_state_dir_and_lock(&config.state_dir)?;
        Self::start_locked(admin_key, config, true, state_lock).await
    }

    async fn start_locked(
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
        if let Some(state) = loaded.as_mut() {
            if normalize_tombstone_times(state, now) {
                write_snapshot_and_truncate_wal(&config, &admin_key, state)?;
            }
        }
        let mut slots = (0..config.max_temporary_keys)
            .map(|_| SlotHot::default())
            .collect::<Vec<_>>()
            .into_boxed_slice();
        let mut cold = HashMap::new();
        let mut wheel = TimingWheel::new(now);

        let admin_lease = Arc::new(AuthLease::new(0, u64::MAX));
        if let Some(state) = loaded.as_ref() {
            for (index, generation) in state.generations.iter().copied().enumerate() {
                if let Some(slot) = slots.get_mut(index) {
                    slot.generation = generation;
                }
            }
            for entry in &state.entries {
                let index = key_slot(entry.key_id) as usize;
                let Some(slot) = slots.get_mut(index) else {
                    continue;
                };
                if slot.generation != key_generation(entry.key_id) {
                    continue;
                }
                let state = if entry.state == SlotState::Active && entry.expires_at <= now {
                    SlotState::Expired
                } else {
                    entry.state
                };
                slot.state = state;
                slot.expires_at = entry.expires_at;
                cold.insert(
                    entry.key_id,
                    ColdMetadata {
                        issued_at: entry.issued_at,
                        label: entry.label.clone(),
                        tombstoned_at: match state {
                            SlotState::Expired => entry.tombstoned_at.unwrap_or(entry.expires_at),
                            SlotState::Revoked => entry.tombstoned_at.unwrap_or(now),
                            SlotState::Free | SlotState::Active => 0,
                        },
                    },
                );
                if state == SlotState::Active {
                    let lease = Arc::new(AuthLease::new(entry.key_id, entry.expires_at));
                    slot.lease = Arc::downgrade(&lease);
                    wheel.insert(lease);
                }
            }
        }

        let legacy_protocol = if safe_mode {
            LegacyProtocolPolicy::Deny
        } else {
            loaded
                .as_ref()
                .map(|state| state.legacy_protocol)
                .unwrap_or(config.legacy_protocol)
        };
        let mut admin_replay_order = loaded
            .as_ref()
            .map(|state| {
                state
                    .admin_replays
                    .iter()
                    .filter(|record| record.within_retention(now))
                    .cloned()
                    .collect::<VecDeque<_>>()
            })
            .unwrap_or_default();
        while admin_replay_order.len() > ADMIN_REPLAY_CAPACITY {
            admin_replay_order.pop_front();
        }
        let admin_replays = admin_replay_order
            .iter()
            .map(|record| record.fingerprint)
            .collect::<HashSet<_>>();
        let mut audit_records: VecDeque<AuditRecord> = loaded
            .as_ref()
            .map(|state| state.audit_records.iter().cloned().collect())
            .unwrap_or_default();
        while audit_records.len() > AUDIT_RECORD_CAPACITY {
            audit_records.pop_front();
        }
        let (high_slot_generations, mut high_slot_entries) = loaded
            .as_ref()
            .map(|state| split_high_slot_state(state, config.max_temporary_keys))
            .unwrap_or_default();
        for entry in &mut high_slot_entries {
            if entry.state == SlotState::Active && entry.expires_at <= now {
                entry.state = SlotState::Expired;
                entry.tombstoned_at = Some(entry.tombstoned_at.unwrap_or(entry.expires_at));
            }
        }
        let inner = Arc::new(AuthStateInner {
            admin: RwLock::new(AdminState {
                key: admin_key,
                lease: Arc::downgrade(&admin_lease),
            }),
            sync_process_credential,
            instance_id: RwLock::new(instance_id),
            slots: RwLock::new(slots),
            high_slot_generations: RwLock::new(high_slot_generations),
            high_slot_entries: RwLock::new(high_slot_entries),
            safe_mode: AtomicBool::new(safe_mode),
            legacy_protocol_allowed: AtomicBool::new(legacy_protocol.is_allowed()),
            active_legacy_connections: AtomicU64::new(0),
            last_legacy_connection_at: AtomicU64::new(0),
            auth_successes: AtomicU64::new(0),
            auth_failures: AtomicU64::new(0),
            root_epoch: AtomicU64::new(loaded.as_ref().map(|state| state.root_epoch).unwrap_or(0)),
            previous_root: RwLock::new(None),
            audit_records: RwLock::new(audit_records),
        });
        let (command_tx, command_rx) = mpsc::channel(256);
        let actor = tokio::spawn(run_auth_actor(
            inner.clone(),
            admin_lease,
            command_rx,
            config.clone(),
            AuthActorState::new(cold, wheel, admin_replays, admin_replay_order),
            state_lock.clone(),
        ));
        let runtime = Self {
            inner: Arc::downgrade(&inner),
            command_tx,
            config: config.clone(),
            _state_lock: state_lock.clone(),
            actor: Arc::new(std::sync::Mutex::new(Some(actor))),
        };
        Ok(runtime)
    }

    pub async fn shutdown_actor(&self) {
        let (response, receiver) = oneshot::channel();
        let _ = self
            .command_tx
            .send(AuthCommand::Shutdown { response })
            .await;
        let _ = receiver.await;
        let handle = self
            .actor
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(handle) = handle {
            let _ = handle.await;
        }
    }

    pub fn config(&self) -> &AuthConfig {
        &self.config
    }

    fn inner(&self) -> Result<Arc<AuthStateInner>, AuthFailure> {
        self.inner.upgrade().ok_or_else(|| {
            AuthFailure::new(
                "auth_state_unavailable",
                "authentication state manager is not running",
                true,
            )
        })
    }

    pub(crate) fn admin_key(&self) -> Result<AesKeyType, AuthFailure> {
        Ok(self.inner()?.admin_key())
    }

    pub(crate) fn derive_key(&self, key_id: u64) -> Result<AesKeyType, AuthFailure> {
        let inner = self.inner()?;
        if key_id == 0 {
            return Ok(inner.admin_key());
        }
        derive_temporary_key(&inner.admin_key(), &inner.instance_id(), key_id)
    }

    #[cfg(test)]
    pub(crate) fn high_slot_entry_count(&self) -> usize {
        self.inner()
            .map(|inner| {
                inner
                    .high_slot_entries
                    .read()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .len()
            })
            .unwrap_or(0)
    }

    pub(crate) fn derive_previous_key(&self, key_id: u64) -> Option<AesKeyType> {
        let inner = self.inner().ok()?;
        let previous = inner
            .previous_root
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()?;
        if key_id == 0 {
            Some(previous.admin_key)
        } else {
            derive_temporary_key(&previous.admin_key, &previous.instance_id, key_id).ok()
        }
    }

    pub fn authenticate_presented(
        &self,
        key_id: u64,
        presented_key: &AesKeyType,
    ) -> Result<AuthContext, AuthFailure> {
        let inner = self.inner()?;
        if key_id == 0 {
            let admin = inner
                .admin
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
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
            return Ok(AuthContext::from_lease(0, true, &lease));
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

        let index = key_slot(key_id) as usize;
        let generation = key_generation(key_id);
        let slots = inner
            .slots
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
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
                    lease.cancellation.cancel();
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
        self.command_tx.send(build(response)).await.map_err(|_| {
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
        let authority = authorization.admin_authority()?;
        self.request(|response| AuthCommand::Issue {
            authority,
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
        key_id: u64,
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
        key_id: u64,
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
        key_id: u64,
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
        key_id: Option<u64>,
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

fn temporary_key_material_mismatch(inner: &AuthStateInner, key_id: u64) -> AuthFailure {
    let index = key_slot(key_id) as usize;
    let generation = key_generation(key_id);
    let slots = inner
        .slots
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let current_generation = match slots.get(index) {
        Some(slot) => Some(slot.generation),
        None => {
            let high = inner
                .high_slot_generations
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            index
                .checked_sub(slots.len())
                .and_then(|offset| high.get(offset).copied())
        }
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
        && generation > 0
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
