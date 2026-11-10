//! The lease schedule, driven by explicit seconds rather than the wall clock.
//!
//! Each test does what the actor does around a `Leases` call, then ticks the
//! schedule to the second that matters.

use super::*;

/// The second every schedule here starts at.
const T: u64 = 1_000_000;

/// A state with four free slots and an empty schedule at second `T`.
fn schedule() -> (Arc<AuthStateInner>, Leases) {
    restore(None)
}

/// The state and schedule a start at second `T` rebuilds from `snapshot`.
fn restore(snapshot: Option<&PersistedSnapshot>) -> (Arc<AuthStateInner>, Leases) {
    let config = AuthConfig {
        state_dir: PathBuf::new(),
        max_temporary_keys: 4,
        max_temporary_key_ttl: MAX_TEMP_KEY_TTL,
        legacy_protocol: LegacyProtocolPolicy::Allow,
    };
    let (restored, live) = RestoredState::from_snapshot(snapshot, false, &config, T);
    let admin_lease = Arc::new(AuthLease::new(ADMIN_KEY_ID, None, u64::MAX));
    let inner = Arc::new(AuthStateInner::new(
        TEST_ADMIN_KEY_BYTES,
        &admin_lease,
        false,
        [0; INSTANCE_ID_LEN],
        false,
        restored,
    ));
    let leases = Leases::restored(&inner, T, live);
    (inner, leases)
}

/// Issues a key into `slot` as `actor_issue` does. Returns its id and the
/// cancellation of its lease.
fn issue(
    inner: &AuthStateInner,
    leases: &mut Leases,
    slot: usize,
    expires_at: u64,
) -> (KeyId, CancellationToken) {
    let key_id = KeyId::new(Generation::from_u32(1), SlotIndex::from_index(slot));
    let lease = Arc::new(AuthLease::new(key_id, None, expires_at));
    inner.slots_mut()[slot] = SlotHot {
        generation: key_id.generation(),
        state: SlotState::Active,
        expires_at,
        lease: Arc::downgrade(&lease),
    };
    leases.issue(&lease, T, None, None);
    (key_id, lease.cancellation_token())
}

fn state_of(inner: &AuthStateInner, key_id: KeyId) -> SlotState {
    inner.slots()[key_id.slot().as_index()].state
}

/// A renewal can bring a deadline closer: the key ends at the new one, and its
/// row is recycled a retention window later.
#[test]
fn a_renewed_key_ends_at_its_new_deadline() {
    let (inner, mut leases) = schedule();
    let (key_id, cancellation) = issue(&inner, &mut leases, 0, T + 3600);

    // What `actor_renew` does for a live lease.
    let lease = {
        let mut slots = inner.slots_mut();
        slots[0].expires_at = T + 10;
        slots[0].lease.upgrade().unwrap()
    };
    lease.set_expires_at(T + 10);
    drop(lease);
    assert!(leases.renew(key_id, T + 10));

    leases.tick(T + 9);
    assert!(!cancellation.is_cancelled());
    assert_eq!(state_of(&inner, key_id), SlotState::Active);
    leases.tick(T + 10);
    assert!(cancellation.is_cancelled());
    assert_eq!(state_of(&inner, key_id), SlotState::Expired);

    leases.tick(T + 10 + TOMBSTONE_RETENTION.as_secs());
    assert_eq!(state_of(&inner, key_id), SlotState::Free);
    assert!(!inner.cold().contains_key(&key_id));
}

/// A renewal that had to replace a cancelled lease disarms the old lease's
/// stages: they name the key, so at the old deadline they would otherwise end
/// the key the renewal just extended. The WAL says that key is active, so a row
/// that said otherwise could not even be revoked. That the actor takes this path
/// is `a_renewal_that_replaced_a_cancelled_lease_outlives_the_old_deadline`.
#[test]
fn a_lease_adopted_by_a_renewal_outlives_the_old_deadline() {
    let (inner, mut leases) = schedule();
    let (key_id, old) = issue(&inner, &mut leases, 0, T + 10);
    old.cancel();

    // What `actor_renew` does for a cancelled lease.
    let replacement = Arc::new(AuthLease::new(key_id, None, T + 3600));
    {
        let mut slots = inner.slots_mut();
        slots[0].expires_at = T + 3600;
        slots[0].lease = Arc::downgrade(&replacement);
    }
    leases.adopt(&replacement);
    let cancellation = replacement.cancellation_token();
    drop(replacement);

    leases.tick(T + 10 + TOMBSTONE_RETENTION.as_secs() + 1);
    assert!(!cancellation.is_cancelled());
    assert_eq!(state_of(&inner, key_id), SlotState::Active);
    leases.tick(T + 3600);
    assert!(cancellation.is_cancelled());
}

/// A restart schedules each live key at the deadline it was persisted with.
#[test]
fn a_restored_key_ends_at_its_persisted_deadline() {
    let key_id = KeyId::new(Generation::from_u32(1), SlotIndex::from_index(2));
    let snapshot = PersistedSnapshot {
        generations: vec![
            Generation::FIRST,
            Generation::FIRST,
            key_id.generation(),
            Generation::FIRST,
        ],
        entries: vec![PersistedEntry {
            key_id,
            state: SlotState::Active,
            issued_at: T - 100,
            expires_at: T + 30,
            label: None,
            tombstoned_at: None,
            namespace: None,
        }],
        ..snapshot_for_test([0; INSTANCE_ID_LEN], 0, 0)
    };
    let (inner, mut leases) = restore(Some(&snapshot));
    let cancellation = inner.slots()[2]
        .lease
        .upgrade()
        .unwrap()
        .cancellation_token();

    leases.tick(T + 29);
    assert!(!cancellation.is_cancelled());
    assert_eq!(state_of(&inner, key_id), SlotState::Active);
    leases.tick(T + 30);
    assert!(cancellation.is_cancelled());
    assert_eq!(state_of(&inner, key_id), SlotState::Expired);
}

/// A dead row above the configured capacity keeps its own tombstone time
/// across a restart: one that died long before is recycled at once, not a
/// retention window after the restart.
#[test]
fn a_restored_dead_row_above_capacity_keeps_its_tombstone_time() {
    let key_id = KeyId::new(Generation::from_u32(1), SlotIndex::from_index(5));
    let mut generations = vec![Generation::FIRST; 6];
    generations[5] = key_id.generation();
    let snapshot = PersistedSnapshot {
        generations,
        entries: vec![PersistedEntry {
            key_id,
            state: SlotState::Revoked,
            issued_at: T - 7200,
            expires_at: T + 3600,
            label: None,
            tombstoned_at: Some(T - 3600),
            namespace: None,
        }],
        ..snapshot_for_test([0; INSTANCE_ID_LEN], 0, 0)
    };
    let (inner, mut leases) = restore(Some(&snapshot));

    leases.tick(T + 1);
    assert!(inner.high().is_empty());
}

/// A revoked key keeps its row for a retention window counted from the revoke,
/// not from the deadline it no longer has, and records when that window began.
#[test]
fn a_revoked_key_is_recycled_a_retention_window_after_the_revoke() {
    let (inner, mut leases) = schedule();
    let (key_id, cancellation) = issue(&inner, &mut leases, 0, T + 3600);

    // What `actor_revoke` does for a row in the table.
    inner.slots_mut()[0].state = SlotState::Revoked;
    leases.revoke(key_id, T);
    assert!(cancellation.is_cancelled());
    assert_eq!(inner.cold()[&key_id].tombstoned_at, T);

    leases.tick(T + TOMBSTONE_RETENTION.as_secs() - 1);
    assert_eq!(state_of(&inner, key_id), SlotState::Revoked);
    leases.tick(T + TOMBSTONE_RETENTION.as_secs());
    assert_eq!(state_of(&inner, key_id), SlotState::Free);
    assert!(!inner.cold().contains_key(&key_id));
}

/// No callback can reach the schedule's map, so a key that ended on schedule
/// is swept out of it rather than kept for as long as the relay runs. A key
/// whose reap is still to come stays, so collecting it can still end it early.
#[test]
fn keys_that_ended_on_schedule_are_forgotten() {
    let (inner, mut leases) = schedule();
    let (ended, _) = issue(&inner, &mut leases, 0, T + 10);
    let (retired, _) = issue(&inner, &mut leases, 1, T + 3590);
    let (live, _) = issue(&inner, &mut leases, 2, T + 7200);
    assert_eq!(leases.tracked_keys(), 3);

    leases.tick(T + 3600);
    assert_eq!(state_of(&inner, ended), SlotState::Free);
    assert_eq!(state_of(&inner, retired), SlotState::Expired);
    assert_eq!(state_of(&inner, live), SlotState::Active);
    assert_eq!(leases.tracked_keys(), 2);

    leases.end(retired);
    assert_eq!(state_of(&inner, retired), SlotState::Free);
    assert_eq!(leases.tracked_keys(), 1);
}
