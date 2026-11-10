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
    /// Preallocated, indexed directly by `key_slot(key_id)`. Documented on
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
    /// a key id that was already handed out, and their entries so a still-live
    /// credential in that range keeps working. This is the fallback path that
    /// operations take after missing in `slots`.
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
}

impl AuthStateInner {
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
}

#[derive(Clone)]
pub(crate) struct ColdMetadata {
    pub(crate) issued_at: u64,
    pub(crate) label: Option<String>,
    /// The namespace this key was issued into, if any. See [`KeyId::namespace`].
    pub(crate) namespace: Option<u64>,
    pub(crate) tombstoned_at: u64,
}
