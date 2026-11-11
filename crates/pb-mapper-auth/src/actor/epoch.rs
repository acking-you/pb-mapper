//! Root rotation, auth-state reset, and live temporary-key wipe.
use super::super::*;
use super::{audit, ensure_store_available};

fn remember_previous_root(inner: &AuthStateInner) {
    *inner.previous_root.write() = Some(PreviousRoot {
        admin_key: inner.admin_key(),
        instance_id: inner.instance_id(),
    });
}

pub(super) fn actor_reset(
    inner: &Arc<AuthStateInner>,
    config: &AuthConfig,
    leases: &mut Leases,
    admin_replays: &VecDeque<AdminReplayRecord>,
) -> Result<(), AuthFailure> {
    let new_instance_id = random_instance_id();
    inner.root_epoch.fetch_add(1, Ordering::AcqRel);
    let reset_audit = audit(audit_action::AUTH_STATE_RESET, None, None);
    let mut snapshot = empty_snapshot(inner, new_instance_id, admin_replays);
    push_persisted_audit(&mut snapshot.audit_records, reset_audit.clone());
    let admin_key = inner.admin_key();
    let next_instance_path = staged_instance_id_path(&config.state_dir);
    if let Err(error) = atomic_write(&next_instance_path, &new_instance_id, 0o600)
        .and_then(|()| write_snapshot_and_truncate_wal(config, &admin_key, &snapshot))
        .and_then(|()| {
            atomic_write(
                &instance_id_path(&config.state_dir),
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
    leases.wipe(unix_seconds());
    let root = temporary_root(&admin_key, &new_instance_id);
    {
        let mut current = inner.temporary_root.write();
        *inner.instance_id.write() = new_instance_id;
        *current = root;
    }
    inner.safe_mode.store(false, Ordering::Release);
    Ok(())
}

pub(super) fn actor_rotate_root(
    inner: &Arc<AuthStateInner>,
    config: &AuthConfig,
    leases: &mut Leases,
    admin_lease: &mut Arc<AuthLease>,
    new_key: AesKeyType,
) -> Result<(), AuthFailure> {
    if new_key == inner.admin_key() {
        return Err(AuthFailure::new(
            codes::ADMINISTRATOR_KEY_UNCHANGED,
            "new administrator key must differ from the current key",
            false,
        ));
    }
    if !is_env_safe_admin_key(&new_key) {
        return Err(AuthFailure::new(
            codes::ADMINISTRATOR_KEY_INVALID,
            env_safe_admin_key_error(),
            false,
        ));
    }
    // Unreachable: `is_env_safe_admin_key` above accepts only printable ASCII.
    // Reported rather than panicked, since this already returns `Result`.
    let new_key_string = String::from_utf8(new_key.to_vec()).map_err(|_| {
        AuthFailure::new(
            codes::ADMINISTRATOR_KEY_INVALID,
            env_safe_admin_key_error(),
            false,
        )
    })?;
    // The key file is read back through `parse_credential`, which refuses some
    // printable keys too (one that starts like a temporary credential). Such a
    // key fails the first write below, and a failed write fails the store
    // closed, so it is refused here, before anything has changed.
    validate_admin_key(&new_key_string)?;

    inner.root_epoch.fetch_add(1, Ordering::AcqRel);
    let rotate_audit = audit(audit_action::ADMINISTRATOR_KEY_ROTATE, None, None);
    let mut snapshot = empty_snapshot(inner, inner.instance_id(), &VecDeque::new());
    push_persisted_audit(&mut snapshot.audit_records, rotate_audit.clone());
    let next_key_path = rotation_key_path(&config.state_dir);
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
    leases.wipe(unix_seconds());
    let old_admin_lease = admin_lease.clone();
    let new_admin_lease = Arc::new(AuthLease::new(ADMIN_KEY_ID, None, u64::MAX));
    let root = temporary_root(&new_key, &inner.instance_id());
    {
        let mut current = inner.temporary_root.write();
        *inner.admin.write() = AdminState {
            key: new_key,
            lease: Arc::downgrade(&new_admin_lease),
        };
        *current = root;
    }
    // The key is installed on disk and in memory by now, so a failure here must
    // not skip the rest: dropping `new_admin_lease` early would leave the relay
    // with no administrator at all. (It cannot fail today: both ways past the
    // writes above parsed the key as an administrator credential already.)
    let synced = if inner.sync_process_credential {
        set_process_msg_header_key(Some(&new_key_string)).map_err(AuthFailure::internal)
    } else {
        Ok(())
    };
    inner.safe_mode.store(false, Ordering::Release);
    old_admin_lease.cancel_rotated();
    *admin_lease = new_admin_lease;
    synced
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
        audit(
            audit_action::LEGACY_PROTOCOL_UPDATE,
            None,
            Some(policy_detail(policy).to_string()),
        ),
    )?;
    inner
        .legacy_protocol_allowed
        .store(policy.is_allowed(), Ordering::Release);
    Ok(())
}

/// How a policy change is written into its audit record. Spelled out rather
/// than taken from `Debug`, because the record is persisted.
fn policy_detail(policy: LegacyProtocolPolicy) -> &'static str {
    match policy {
        LegacyProtocolPolicy::Allow => "Allow",
        LegacyProtocolPolicy::Deny => "Deny",
    }
}
