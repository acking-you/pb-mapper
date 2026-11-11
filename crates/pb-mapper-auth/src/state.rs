//! The shared authentication state: the slot table and what hangs off it.
use super::*;

/// # The slot table
///
/// A temporary key is never stored. It is *derived* on demand from
/// `(root key, instance id, key id)`, so the server can verify a credential it
/// has no copy of. That makes the key id the whole identity of a key, and a
/// key id is a slot index plus a generation counter:
///
/// ```text
///  key_id: u64
/// ┌───────────────────────────┬───────────────────────────┐
/// │  generation (high 32)     │  slot index (low 32)      │
/// └───────────────────────────┴───────────────────────────┘
///        ^ bumped on reuse            ^ where the row lives
/// ```
///
/// The slot index is a direct offset into `AuthStateInner::slots`, a
/// preallocated `Box<[SlotHot]>`. So verifying a credential is an array index,
/// not a map lookup or a scan, and the table's memory does not grow with churn:
///
/// ```text
/// slots: [ SlotHot; max_temporary_keys ]
///   idx 0  gen 7  Active   expires_at=…  lease─┐
///   idx 1  gen 0  Free                         │  Weak, so the actor's
///   idx 2  gen 3  Expired  (tombstoned)        │  timing wheel is the
///   idx 3  gen 9  Active   expires_at=…  lease─┴─ only strong owner
/// ```
///
/// ## Why the generation counter
///
/// A freed slot is reused, so the index alone would let a *retired* credential
/// authenticate against the *new* tenant of that row. The generation bump makes
/// the old key id refer to a row that no longer exists:
///
/// ```text
/// issue   -> idx 2, gen 3  =>  key_id 0x0000_0003_0000_0002
/// expire  -> idx 2 retired, generation kept at 3
/// reissue -> idx 2, gen 4  =>  key_id 0x0000_0004_0000_0002
///            the old key id still names gen 3, which nothing matches
/// ```
///
/// This is why [`SlotHot::retire`] clears the row but preserves `generation`,
/// and why expiry, GC, root rotation and a state reset all keep it. The one
/// exception is a reset out of a damaged snapshot, which cannot know the generations
/// and starts them over. Only the rotated instance id then keeps an old credential
/// from authenticating, so a key id from before it may name a different key.
///
/// ## The lifecycle
///
/// ```text
///          issue                  deadline passes / revoke
///   Free ─────────> Active ──────────────────────────────> Expired
///     ^               │                                    Revoked
///     │               └── renew: same row, later expires_at    │
///     │                                                        │
///     └──────────── retire, after TOMBSTONE_RETENTION ─────────┘
/// ```
///
/// `Expired`/`Revoked` are tombstones, not garbage. A row lingers in that state
/// for `TOMBSTONE_RETENTION` so a client that presents a dead credential is told
/// *why* ("expired", "revoked") instead of receiving the indistinguishable
/// "unknown key" it would get from an already-recycled row. `Leases` owns that
/// delay; see `leases.rs`.
///
/// ## Slots above capacity
///
/// `max_temporary_keys` is configurable, so a restart can shrink the table below
/// what the persisted state used. Those rows cannot be indexed any more, but
/// their generations still have to be honoured — otherwise growing the table
/// again would reissue a key id that was already handed out. They are retained
/// out-of-line in `high_slot_generations` / `high_slot_entries`, which is why so
/// many operations check the array first and fall back to a scan of that vector.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SlotState {
    /// Never used, or retired and past its tombstone.
    Free,
    /// A live credential. `expires_at` is authoritative.
    Active,
    /// Dead. Retained for `TOMBSTONE_RETENTION` so the reason survives.
    Expired,
    Revoked,
}

/// One row of the slot table. `generation` outlives every other field.
pub(crate) struct SlotHot {
    pub(crate) generation: Generation,
    pub(crate) state: SlotState,
    pub(crate) expires_at: u64,
    /// `Weak`, because the actor's timing wheel holds the strong reference and is
    /// the single place a lease's lifetime ends. See `timing_wheel.rs`.
    pub(crate) lease: Weak<AuthLease>,
}

impl SlotHot {
    /// Whether this row still belongs to `key_id`'s generation. A row that has
    /// been reissued belongs to a newer tenant and must not be touched on the
    /// old one's behalf.
    pub(crate) fn holds(&self, key_id: KeyId) -> bool {
        self.generation == key_id.generation() && self.state != SlotState::Free
    }

    /// Whether a garbage collection should free this row: it is already dead, or
    /// it is active but past its deadline.
    pub(crate) fn is_collectable(&self, now: u64) -> bool {
        match self.state {
            SlotState::Expired | SlotState::Revoked => true,
            SlotState::Active => self.expires_at <= now,
            SlotState::Free => false,
        }
    }

    /// Frees the slot for reuse while keeping its generation, so a key id that
    /// has been handed out is never issued a second time.
    pub(crate) fn retire(&mut self) {
        *self = Self {
            generation: self.generation,
            ..Self::default()
        };
    }
}

impl Default for SlotHot {
    fn default() -> Self {
        Self {
            generation: Generation::FIRST,
            state: SlotState::Free,
            expires_at: 0,
            lease: Weak::new(),
        }
    }
}

// No `Debug` on the three types that hold the administrator key: a stray `{:?}`
// would print it. Write one by hand, with the key redacted, if it is ever needed.
pub(crate) struct AdminState {
    pub(crate) key: AesKeyType,
    pub(crate) lease: Weak<AuthLease>,
}

#[derive(Clone)]
pub(crate) struct PreviousRoot {
    pub(crate) admin_key: AesKeyType,
    pub(crate) instance_id: [u8; INSTANCE_ID_LEN],
}

pub(crate) struct AuthStateInner {
    pub(crate) admin: RwLock<AdminState>,
    pub(crate) sync_process_credential: bool,
    pub(crate) instance_id: RwLock<[u8; INSTANCE_ID_LEN]>,
    /// The [`temporary_root`] of the administrator key and the instance id. A
    /// change to either takes this lock first and replaces it before letting
    /// go, so no derivation pairs a new key or id with the old root.
    pub(crate) temporary_root: RwLock<Prk>,
    /// Preallocated, indexed directly by `key_id.slot()`. Documented on
    /// [`SlotState`].
    pub(crate) slots: RwLock<Box<[SlotHot]>>,
    /// Rows the configured capacity no longer covers, because a restart shrank
    /// the table below what the persisted state used:
    ///
    /// ```text
    ///  slots: [ 0 1 2 3 ]  <- indexable
    ///  high:          [ 4 5 ]  <- generations still honoured, out-of-line
    /// ```
    ///
    /// Their generations must be kept so growing the table again cannot reissue
    /// a key id that was already handed out, and their entries so a key in that
    /// range is still listed, renewed and revoked, and works again once a
    /// restart grows the table back over it; until then it cannot authenticate.
    /// Each is on the lease schedule like a row of the table. This is the
    /// fallback path that operations take after missing in `slots`.
    ///
    /// Fixed at startup: nothing issues into these rows, so the generations
    /// never change and need no lock.
    pub(crate) high_slot_generations: Box<[Generation]>,
    pub(crate) high_slot_entries: RwLock<Vec<PersistedEntry>>,
    /// Per-key description that no authentication check needs, kept out of the
    /// hot slot row. Lives here rather than inside the actor so a key's handle
    /// can drop it without the actor being involved; see `leases.rs`.
    pub(crate) cold: RwLock<HashMap<KeyId, ColdMetadata>>,
    pub(crate) safe_mode: AtomicBool,
    /// Whether anything a snapshot carries may have changed since the last
    /// compaction wrote one, so that the next compaction has something to
    /// write. See [`Self::mark_snapshot_stale`].
    pub(crate) snapshot_stale: AtomicBool,
    pub(crate) legacy_protocol_allowed: AtomicBool,
    pub(crate) active_legacy_connections: AtomicU64,
    pub(crate) last_legacy_connection_at: AtomicU64,
    pub(crate) auth_successes: AtomicU64,
    pub(crate) auth_failures: AtomicU64,
    pub(crate) root_epoch: AtomicU64,
    /// The encrypted state's copy of the allocator's mark, which snapshots carry. See
    /// `persistence/namespaces.rs` for the other copy.
    pub(crate) namespace_high_water: AtomicU64,
    pub(crate) previous_root: RwLock<Option<PreviousRoot>>,
    pub(crate) audit_records: RwLock<VecDeque<AuditRecord>>,
    /// Never sent on. It drops with the rest of the state, which is the signal
    /// a receiver from [`Self::subscribe_dropped`] waits for.
    dropped: watch::Sender<()>,
}

impl AuthStateInner {
    /// The state a start begins with: `admin_lease` as the current
    /// administrator lease, and the tables `restored` from the persisted state.
    pub(crate) fn new(
        admin_key: AesKeyType,
        admin_lease: &Arc<AuthLease>,
        sync_process_credential: bool,
        instance_id: [u8; INSTANCE_ID_LEN],
        safe_mode: bool,
        restored: RestoredState,
    ) -> Self {
        let RestoredState {
            slots,
            cold,
            high_slot_generations,
            high_slot_entries,
            legacy_protocol,
            audit_records,
            root_epoch,
            namespace_high_water,
        } = restored;
        Self {
            admin: RwLock::new(AdminState {
                key: admin_key,
                lease: Arc::downgrade(admin_lease),
            }),
            sync_process_credential,
            temporary_root: RwLock::new(temporary_root(&admin_key, &instance_id)),
            instance_id: RwLock::new(instance_id),
            slots: RwLock::new(slots),
            high_slot_generations,
            high_slot_entries: RwLock::new(high_slot_entries),
            cold: RwLock::new(cold),
            safe_mode: AtomicBool::new(safe_mode),
            // What a start restored may differ from the files it came from: a
            // key it found past its deadline, say.
            snapshot_stale: AtomicBool::new(true),
            legacy_protocol_allowed: AtomicBool::new(legacy_protocol.is_allowed()),
            active_legacy_connections: AtomicU64::new(0),
            last_legacy_connection_at: AtomicU64::new(0),
            auth_successes: AtomicU64::new(0),
            auth_failures: AtomicU64::new(0),
            root_epoch: AtomicU64::new(root_epoch),
            namespace_high_water: AtomicU64::new(namespace_high_water),
            previous_root: RwLock::new(None),
            audit_records: RwLock::new(audit_records),
            dropped: watch::Sender::new(()),
        }
    }

    /// Records that something a snapshot carries has changed, or may have.
    ///
    /// A compaction writes only the state, so one with nothing changed since
    /// the last would write what is already on disk, and is skipped. Every
    /// change outside a snapshot write marks it: each WAL append, a key's
    /// retirement or reap, and the replay window letting a record go. A
    /// restart would recompute those last two from an older snapshot only
    /// while the clock moves forward: after it steps back before a dead key's
    /// deadline, that snapshot would bring the key back.
    pub(crate) fn mark_snapshot_stale(&self) {
        self.snapshot_stale.store(true, Ordering::Release);
    }

    /// A receiver whose `changed()` fails once this state has been dropped.
    pub(crate) fn subscribe_dropped(&self) -> watch::Receiver<()> {
        self.dropped.subscribe()
    }

    /// Rows the configured capacity no longer covers. See the field's docs; the
    /// fallback is a scan because the range is small and rarely touched.
    pub(crate) fn high(&self) -> parking_lot::RwLockReadGuard<'_, Vec<PersistedEntry>> {
        self.high_slot_entries.read()
    }

    pub(crate) fn high_mut(&self) -> parking_lot::RwLockWriteGuard<'_, Vec<PersistedEntry>> {
        self.high_slot_entries.write()
    }

    pub(crate) fn slots(&self) -> parking_lot::RwLockReadGuard<'_, Box<[SlotHot]>> {
        self.slots.read()
    }

    pub(crate) fn slots_mut(&self) -> parking_lot::RwLockWriteGuard<'_, Box<[SlotHot]>> {
        self.slots.write()
    }

    pub(crate) fn cold(&self) -> parking_lot::RwLockReadGuard<'_, HashMap<KeyId, ColdMetadata>> {
        self.cold.read()
    }

    pub(crate) fn cold_mut(
        &self,
    ) -> parking_lot::RwLockWriteGuard<'_, HashMap<KeyId, ColdMetadata>> {
        self.cold.write()
    }

    pub(crate) fn admin_key(&self) -> AesKeyType {
        self.admin.read().key
    }

    pub(crate) fn instance_id(&self) -> [u8; INSTANCE_ID_LEN] {
        *self.instance_id.read()
    }

    /// The temporary key `key_id` names under the current root.
    pub(crate) fn derive_temporary_key(&self, key_id: KeyId) -> Result<AesKeyType, AuthFailure> {
        expand_temporary_key(&self.temporary_root.read(), key_id)
    }

    /// The legacy-protocol policy in force.
    pub(crate) fn legacy_policy(&self) -> LegacyProtocolPolicy {
        if self.legacy_protocol_allowed.load(Ordering::Acquire) {
            LegacyProtocolPolicy::Allow
        } else {
            LegacyProtocolPolicy::Deny
        }
    }
}

pub(crate) struct ColdMetadata {
    pub(crate) issued_at: u64,
    pub(crate) label: Option<String>,
    /// The namespace this key was issued into, if any. See [`KeyId::namespace`].
    pub(crate) namespace: Option<u64>,
    pub(crate) tombstoned_at: u64,
}

/// The leases of the keys still live, with their ids, in slot order.
pub(crate) type LiveLeases = Vec<(KeyId, Arc<AuthLease>)>;

/// What a start rebuilds from the persisted state, before it becomes an
/// [`AuthStateInner`].
pub(crate) struct RestoredState {
    pub(crate) slots: Box<[SlotHot]>,
    pub(crate) cold: HashMap<KeyId, ColdMetadata>,
    pub(crate) high_slot_generations: Box<[Generation]>,
    pub(crate) high_slot_entries: Vec<PersistedEntry>,
    pub(crate) legacy_protocol: LegacyProtocolPolicy,
    pub(crate) audit_records: VecDeque<AuditRecord>,
    pub(crate) root_epoch: u64,
    /// The encrypted state's copy of the allocator's mark, which the caller
    /// reconciles with the allocator file.
    pub(crate) namespace_high_water: u64,
}

impl RestoredState {
    /// Rebuilds the tables from `state`, or empty ones when nothing loaded.
    ///
    /// Also returns the leases of the keys still live, in slot order. Nothing
    /// else holds them, so the caller hands them to `Leases::restored`, which
    /// gives each to the timing wheel that owns it from then on.
    pub(crate) fn from_snapshot(
        state: Option<&PersistedSnapshot>,
        safe_mode: bool,
        config: &AuthConfig,
        now: u64,
    ) -> (Self, LiveLeases) {
        let capacity = config.max_temporary_keys;
        let (slots, cold, live) = restore_slots(state, capacity, now);
        let (high_slot_generations, high_slot_entries) = restore_high_slots(state, capacity, now);
        let legacy_protocol = if safe_mode {
            LegacyProtocolPolicy::Deny
        } else {
            state
                .map(|state| state.legacy_protocol)
                .unwrap_or(config.legacy_protocol)
        };
        let mut audit_records: VecDeque<AuditRecord> = state
            .map(|state| state.audit_records.iter().cloned().collect())
            .unwrap_or_default();
        while audit_records.len() > AUDIT_RECORD_CAPACITY {
            audit_records.pop_front();
        }
        let restored = Self {
            slots,
            cold,
            high_slot_generations,
            high_slot_entries,
            legacy_protocol,
            audit_records,
            root_epoch: state.map(|state| state.root_epoch).unwrap_or(0),
            namespace_high_water: state.map_or(0, |state| state.namespace_high_water),
        };
        (restored, live)
    }
}

/// The indexable rows: every generation the state recorded, and each entry
/// whose generation still matches its row. A key past its deadline comes back
/// expired.
fn restore_slots(
    state: Option<&PersistedSnapshot>,
    capacity: usize,
    now: u64,
) -> (Box<[SlotHot]>, HashMap<KeyId, ColdMetadata>, LiveLeases) {
    let mut slots = (0..capacity)
        .map(|_| SlotHot::default())
        .collect::<Vec<_>>()
        .into_boxed_slice();
    let mut cold = HashMap::new();
    // Every lease built below, alive until the table's own references to them
    // are collected at the end.
    let mut built = Vec::new();
    if let Some(state) = state {
        for (index, generation) in state.generations.iter().copied().enumerate() {
            if let Some(slot) = slots.get_mut(index) {
                slot.generation = generation;
            }
        }
        for entry in &state.entries {
            let index = entry.key_id.slot().as_index();
            let Some(slot) = slots.get_mut(index) else {
                continue;
            };
            if slot.generation != entry.key_id.generation() {
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
                    namespace: entry.namespace,
                    tombstoned_at: match state {
                        SlotState::Expired => entry.tombstoned_at.unwrap_or(entry.expires_at),
                        SlotState::Revoked => entry.tombstoned_at.unwrap_or(now),
                        SlotState::Free | SlotState::Active => 0,
                    },
                },
            );
            if state == SlotState::Active {
                let lease = Arc::new(AuthLease::new(
                    entry.key_id,
                    entry.namespace,
                    entry.expires_at,
                ));
                slot.lease = Arc::downgrade(&lease);
                built.push(lease);
            }
        }
    }
    let live = slots
        .iter()
        .enumerate()
        .filter(|(_, slot)| slot.state == SlotState::Active)
        .filter_map(|(index, slot)| {
            let key_id = KeyId::new(slot.generation, SlotIndex::from_index(index));
            slot.lease.upgrade().map(|lease| (key_id, lease))
        })
        .collect();
    drop(built);
    (slots, cold, live)
}

/// The rows above the configured capacity, which keep their generations and
/// entries out of line. A key past its deadline comes back expired.
fn restore_high_slots(
    state: Option<&PersistedSnapshot>,
    capacity: usize,
    now: u64,
) -> (Box<[Generation]>, Vec<PersistedEntry>) {
    let (high_slot_generations, mut high_slot_entries) = state
        .map(|state| split_high_slot_state(state, capacity))
        .unwrap_or_default();
    for entry in &mut high_slot_entries {
        if entry.state == SlotState::Active && entry.expires_at <= now {
            entry.state = SlotState::Expired;
            entry.tombstoned_at = Some(entry.tombstoned_at.unwrap_or(entry.expires_at));
        }
    }
    (high_slot_generations.into_boxed_slice(), high_slot_entries)
}
