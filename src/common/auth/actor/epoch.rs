//! Root rotation, auth-state reset, and live temporary-key wipe.
use super::super::*;
use super::{audit, ensure_store_available};

fn wipe_temporary_keys(
    inner: &AuthStateInner,
    cold: &mut HashMap<u64, ColdMetadata>,
    wheel: &mut TimingWheel,
) {
    cancel_all_temporary_leases(inner);
    let mut slots = inner
        .slots
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    for slot in slots.iter_mut() {
        slot.state = SlotState::Free;
        slot.expires_at = 0;
        slot.lease = Weak::new();
    }
    cold.clear();
    wheel.clear(unix_seconds());
    clear_retained_high_slot_entries(inner);
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

pub(super) fn actor_reset(
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
    wipe_temporary_keys(inner, cold, wheel);
    *inner
        .instance_id
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = new_instance_id;
    inner.safe_mode.store(false, Ordering::Release);
    Ok(())
}

pub(super) fn actor_rotate_root(
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
        if !rotation_already_installed(&config.state_dir, &new_key_string) {
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
    wipe_temporary_keys(inner, cold, wheel);
    let old_admin_lease = admin_lease.clone();
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

pub(super) fn actor_set_legacy_protocol(
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
