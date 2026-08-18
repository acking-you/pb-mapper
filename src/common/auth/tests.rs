//! Authentication invariants exercised at the state-machine boundary.
//!
//! ```text
//! issue -> renew -> expire/revoke -> persist/restart
//!   |                                |
//!   +-> lease cancellation           +-> encrypted recovery
//! root rotate -> reject old key + reject already-authenticated old context
//! ```
//!
//! Protocol framing has its own tests under `common::message::secure::tests`; this
//! module focuses on lifecycle, persistence, audit, replay, and timing-wheel behavior.

use super::*;

fn temp_state_dir(name: &str) -> PathBuf {
    let mut suffix = [0_u8; 8];
    let mut rng = rand::rng();
    for byte in &mut suffix {
        *byte = rng.random();
    }
    std::env::temp_dir().join(format!("pb-mapper-{name}-{}", hex(&suffix)))
}

fn authenticate_for_test(runtime: &AuthRuntime, key_id: u64) -> Result<AuthContext, AuthFailure> {
    let key = runtime.derive_key(key_id)?;
    runtime.authenticate_presented(key_id, &key)
}

#[test]
fn initialize_admin_key_refuses_to_replace_a_key_when_encrypted_state_exists() {
    let state_dir = temp_state_dir("force-init-state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let key_path = state_dir.join("admin.key");
    std::fs::write(&key_path, b"0123456789abcdefghijklmnopqrstuv\n").unwrap();
    std::fs::write(state_dir.join("auth.snapshot"), b"encrypted").unwrap();
    let error = initialize_admin_key(&key_path, true).unwrap_err();
    assert_eq!(error.code, "administrator_key_state_exists");
    let missing = state_dir.join("missing-admin.key");
    let error = initialize_admin_key(&missing, false).unwrap_err();
    assert_eq!(error.code, "administrator_key_state_exists");
    let _ = std::fs::remove_dir_all(state_dir);
}

#[tokio::test]
async fn shrinking_then_expanding_capacity_does_not_reuse_old_key_ids() {
    let state_dir = temp_state_dir("capacity-shrink");
    let admin_key = *b"0123456789abcdefghijklmnopqrstuv";
    let config_two = AuthConfig {
        state_dir: state_dir.clone(),
        max_temporary_keys: 2,
        max_temporary_key_ttl: Duration::from_secs(3600),
        legacy_protocol: LegacyProtocolPolicy::Allow,
    };
    let runtime = AuthRuntime::start(admin_key, config_two.clone())
        .await
        .unwrap();
    let admin = authenticate_for_test(&runtime, 0).unwrap();
    let first = runtime
        .issue(&admin, Duration::from_secs(60), Some("first".to_string()))
        .await
        .unwrap();
    let second = runtime
        .issue(&admin, Duration::from_secs(60), Some("second".to_string()))
        .await
        .unwrap();
    let Credential::Temporary {
        key_id: first_id, ..
    } = parse_credential(&first.credential).unwrap()
    else {
        panic!("expected temporary credential");
    };
    let Credential::Temporary {
        key_id: second_id, ..
    } = parse_credential(&second.credential).unwrap()
    else {
        panic!("expected temporary credential");
    };
    runtime.revoke(&admin, first_id).await.unwrap();
    runtime.revoke(&admin, second_id).await.unwrap();
    runtime.gc(&admin).await.unwrap();
    drop(runtime);
    tokio::time::sleep(Duration::from_millis(20)).await;

    let config_one = AuthConfig {
        max_temporary_keys: 1,
        ..config_two.clone()
    };
    let runtime = AuthRuntime::start(admin_key, config_one).await.unwrap();
    let admin = authenticate_for_test(&runtime, 0).unwrap();
    assert!(!runtime.status(&admin).await.unwrap().safe_mode);
    let _third = runtime
        .issue(&admin, Duration::from_secs(60), Some("third".to_string()))
        .await
        .unwrap();
    drop(runtime);
    tokio::time::sleep(Duration::from_millis(20)).await;

    let runtime = AuthRuntime::start(admin_key, config_two).await.unwrap();
    let admin = authenticate_for_test(&runtime, 0).unwrap();
    assert!(!runtime.status(&admin).await.unwrap().safe_mode);
    let fourth = runtime
        .issue(&admin, Duration::from_secs(60), Some("fourth".to_string()))
        .await
        .unwrap();
    let Credential::Temporary {
        key_id: fourth_id, ..
    } = parse_credential(&fourth.credential).unwrap()
    else {
        panic!("expected temporary credential");
    };
    assert_ne!(fourth_id, second_id);
    drop(runtime);
    tokio::time::sleep(Duration::from_millis(20)).await;
    let _ = std::fs::remove_dir_all(state_dir);
}

#[tokio::test]
async fn safe_mode_denies_legacy_protocol_instead_of_restoring_the_default() {
    let state_dir = temp_state_dir("safe-mode-legacy");
    let admin_key = *b"0123456789abcdefghijklmnopqrstuv";
    let config = AuthConfig {
        state_dir: state_dir.clone(),
        max_temporary_keys: 4,
        max_temporary_key_ttl: Duration::from_secs(3600),
        legacy_protocol: LegacyProtocolPolicy::Allow,
    };
    let runtime = AuthRuntime::start(admin_key, config.clone()).await.unwrap();
    let admin = authenticate_for_test(&runtime, 0).unwrap();
    runtime
        .set_legacy_protocol(&admin, LegacyProtocolPolicy::Deny)
        .await
        .unwrap();
    drop(runtime);
    tokio::time::sleep(Duration::from_millis(20)).await;
    std::fs::write(state_dir.join("auth.wal"), b"broken-wal").unwrap();

    let runtime = AuthRuntime::start(admin_key, config).await.unwrap();
    let admin = authenticate_for_test(&runtime, 0).unwrap();
    let status = runtime.status(&admin).await.unwrap();
    assert!(status.safe_mode);
    assert_eq!(status.legacy_protocol, LegacyProtocolPolicy::Deny);
    drop(runtime);
    tokio::time::sleep(Duration::from_millis(20)).await;
    let _ = std::fs::remove_dir_all(state_dir);
}

#[tokio::test]
async fn rotate_root_rejects_a_nul_containing_key() {
    let state_dir = temp_state_dir("rotate-nul");
    let admin_key = *b"0123456789abcdefghijklmnopqrstuv";
    let config = AuthConfig {
        state_dir: state_dir.clone(),
        max_temporary_keys: 4,
        max_temporary_key_ttl: Duration::from_secs(3600),
        legacy_protocol: LegacyProtocolPolicy::Allow,
    };
    let runtime = AuthRuntime::start(admin_key, config).await.unwrap();
    let admin = authenticate_for_test(&runtime, 0).unwrap();
    let mut bad = *b"0123456789abcdefghijklmnopqrstuv";
    bad[4] = 0;
    let error = runtime.rotate_root(&admin, bad).await.unwrap_err();
    assert_eq!(error.code, "administrator_key_invalid");
    drop(runtime);
    tokio::time::sleep(Duration::from_millis(20)).await;
    let _ = std::fs::remove_dir_all(state_dir);
}

#[test]
fn platform_default_auth_state_dir_is_writable_outside_linux_system_paths() {
    let dir = platform_default_auth_state_dir();
    #[cfg(windows)]
    {
        assert!(
            dir.ends_with(std::path::Path::new("pb-mapper").join("auth")),
            "windows default auth dir should be under a user-writable pb-mapper path: {}",
            dir.display()
        );
        assert_ne!(dir, PathBuf::from(r"\var\lib\pb-mapper\auth"));
    }
    #[cfg(target_os = "macos")]
    {
        assert!(
            dir.ends_with("Library/Application Support/pb-mapper/auth")
                || dir == PathBuf::from("/Library/Application Support/pb-mapper/auth"),
            "macos default auth dir should be under Application Support: {}",
            dir.display()
        );
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        assert_eq!(dir, PathBuf::from(DEFAULT_AUTH_STATE_DIR));
    }
}

#[test]
fn legacy_protocol_policy_trims_valid_values_and_rejects_unknown_values() {
    assert_eq!(
        parse_legacy_protocol_policy(" allow\n"),
        Some(LegacyProtocolPolicy::Allow)
    );
    assert_eq!(
        parse_legacy_protocol_policy(" DENY "),
        Some(LegacyProtocolPolicy::Deny)
    );
    assert_eq!(parse_legacy_protocol_policy("enabled"), None);
    assert_eq!(parse_legacy_protocol_policy(""), None);
}

#[test]
fn key_id_round_trip() {
    let key_id = make_key_id(42, 65_535);
    assert_eq!(key_generation(key_id), 42);
    assert_eq!(key_slot(key_id), 65_535);
}

#[test]
fn derived_key_is_bound_to_instance_and_key_id() {
    let admin_key = *b"0123456789abcdefghijklmnopqrstuv";
    let instance_a = [1_u8; INSTANCE_ID_LEN];
    let instance_b = [2_u8; INSTANCE_ID_LEN];
    let key = derive_temporary_key(&admin_key, &instance_a, make_key_id(1, 7)).unwrap();
    assert_eq!(
        key,
        derive_temporary_key(&admin_key, &instance_a, make_key_id(1, 7)).unwrap()
    );
    assert_ne!(
        key,
        derive_temporary_key(&admin_key, &instance_b, make_key_id(1, 7)).unwrap()
    );
    assert_ne!(
        key,
        derive_temporary_key(&admin_key, &instance_a, make_key_id(2, 7)).unwrap()
    );
}

#[tokio::test]
async fn isolated_runtime_preserves_remote_temporary_process_credential() {
    let _process_credential_guard = PROCESS_CREDENTIAL_TEST_LOCK.lock().await;
    let state_dir = temp_state_dir("isolated-relay");
    let temporary_key_id = make_key_id(1, 0);
    let temporary_key = *b"temporary-remote-key-0123456789a";
    let temporary_credential = encode_temporary_credential(temporary_key_id, &temporary_key);
    set_process_msg_header_key(Some(&temporary_credential)).unwrap();
    let config = AuthConfig {
        state_dir: state_dir.clone(),
        max_temporary_keys: 4,
        max_temporary_key_ttl: Duration::from_secs(3600),
        legacy_protocol: LegacyProtocolPolicy::Allow,
    };

    let runtime = AuthRuntime::from_isolated_state(config).await.unwrap();
    assert_eq!(
        get_process_credential().unwrap(),
        Credential::Temporary {
            key_id: temporary_key_id,
            key: temporary_key,
        }
    );

    let local_admin_raw = std::fs::read_to_string(state_dir.join("admin.key")).unwrap();
    let Credential::Admin(local_admin_key) = parse_credential(local_admin_raw.trim()).unwrap()
    else {
        panic!("isolated relay key should be an administrator credential");
    };
    let local_admin = runtime.authenticate_presented(0, &local_admin_key).unwrap();
    runtime
        .rotate_root(&local_admin, *b"isolated-new-admin-key-012345678")
        .await
        .unwrap();
    assert_eq!(
        get_process_credential().unwrap(),
        Credential::Temporary {
            key_id: temporary_key_id,
            key: temporary_key,
        }
    );

    set_process_msg_header_key(None).unwrap();
    drop(runtime);
    tokio::time::sleep(Duration::from_millis(20)).await;
    let _ = std::fs::remove_dir_all(state_dir);
}

#[tokio::test]
async fn issue_renew_revoke_and_persist() {
    let state_dir = temp_state_dir("auth-lifecycle");
    let admin_key = *b"0123456789abcdefghijklmnopqrstuv";
    let config = AuthConfig {
        state_dir: state_dir.clone(),
        max_temporary_keys: 8,
        max_temporary_key_ttl: Duration::from_secs(3600),
        legacy_protocol: LegacyProtocolPolicy::Allow,
    };
    let runtime = AuthRuntime::start(admin_key, config.clone()).await.unwrap();
    let admin = authenticate_for_test(&runtime, 0).unwrap();
    let issued = runtime
        .issue(&admin, Duration::from_secs(60), Some("demo".to_string()))
        .await
        .unwrap();
    assert!(issued.credential.starts_with("pbmt1_"));
    let context = authenticate_for_test(&runtime, issued.metadata.key_id).unwrap();
    assert!(!context.is_admin);
    let cancellation = context.cancellation_token().unwrap();
    let renewed = runtime
        .renew(&admin, issued.metadata.key_id, Duration::from_secs(120))
        .await
        .unwrap();
    assert_eq!(renewed.metadata.key_id, issued.metadata.key_id);
    assert_eq!(renewed.credential, issued.credential);
    assert!(renewed.metadata.expires_at > issued.metadata.expires_at);
    runtime
        .revoke(&admin, issued.metadata.key_id)
        .await
        .unwrap();
    assert!(cancellation.is_cancelled());
    assert_eq!(
        context.ensure_active().unwrap_err().code,
        "temporary_key_revoked"
    );
    assert_eq!(
        authenticate_for_test(&runtime, issued.metadata.key_id)
            .unwrap_err()
            .code,
        "temporary_key_revoked"
    );
    let instance_id = load_or_create_instance_id(&state_dir).unwrap();
    let persisted = try_load_persisted_state(&config, &admin_key, instance_id).unwrap();
    let revoked = persisted
        .entries
        .iter()
        .find(|entry| entry.key_id == issued.metadata.key_id)
        .unwrap();
    assert_eq!(revoked.state, SlotState::Revoked);
    assert!(revoked.tombstoned_at.is_some());
    drop(runtime);

    tokio::time::sleep(Duration::from_millis(20)).await;
    let restored = AuthRuntime::start(admin_key, config).await.unwrap();
    assert_eq!(
        authenticate_for_test(&restored, issued.metadata.key_id)
            .unwrap_err()
            .code,
        "temporary_key_revoked"
    );
    let _ = std::fs::remove_dir_all(state_dir);
}

#[tokio::test]
async fn reset_rotates_instance_and_prevents_old_key_id_reuse() {
    let state_dir = temp_state_dir("auth-reset");
    let admin_key = *b"0123456789abcdefghijklmnopqrstuv";
    let config = AuthConfig {
        state_dir: state_dir.clone(),
        max_temporary_keys: 1,
        max_temporary_key_ttl: Duration::from_secs(3600),
        legacy_protocol: LegacyProtocolPolicy::Allow,
    };
    let runtime = AuthRuntime::start(admin_key, config).await.unwrap();
    let admin = authenticate_for_test(&runtime, 0).unwrap();
    let before = runtime.status(&admin).await.unwrap().server_instance_id;
    let old = runtime
        .issue(
            &admin,
            Duration::from_secs(60),
            Some("before-reset".to_string()),
        )
        .await
        .unwrap();
    let old_context = authenticate_for_test(&runtime, old.metadata.key_id).unwrap();
    let old_cancellation = old_context.cancellation_token().unwrap();

    runtime.reset(&admin).await.unwrap();

    let after = runtime.status(&admin).await.unwrap().server_instance_id;
    assert_ne!(after, before);
    assert!(old_cancellation.is_cancelled());
    assert!(authenticate_for_test(&runtime, old.metadata.key_id).is_err());
    let replacement = runtime
        .issue(
            &admin,
            Duration::from_secs(60),
            Some("after-reset".to_string()),
        )
        .await
        .unwrap();
    assert_ne!(replacement.metadata.key_id, old.metadata.key_id);
    assert_ne!(replacement.credential, old.credential);

    drop(runtime);
    tokio::time::sleep(Duration::from_millis(20)).await;
    let _ = std::fs::remove_dir_all(state_dir);
}

#[tokio::test]
async fn corrupt_wal_fails_temporary_keys_closed_until_admin_reset() {
    let state_dir = temp_state_dir("auth-safe-mode");
    let admin_key = *b"0123456789abcdefghijklmnopqrstuv";
    let config = AuthConfig {
        state_dir: state_dir.clone(),
        max_temporary_keys: 4,
        max_temporary_key_ttl: Duration::from_secs(3600),
        legacy_protocol: LegacyProtocolPolicy::Allow,
    };
    let runtime = AuthRuntime::start(admin_key, config.clone()).await.unwrap();
    let admin = authenticate_for_test(&runtime, 0).unwrap();
    let issued = runtime
        .issue(
            &admin,
            Duration::from_secs(60),
            Some("corrupt-me".to_string()),
        )
        .await
        .unwrap();
    drop(runtime);
    tokio::time::sleep(Duration::from_millis(20)).await;
    std::fs::write(state_dir.join("auth.wal"), b"broken-wal").unwrap();

    let recovered = AuthRuntime::start(admin_key, config).await.unwrap();
    let recovered_admin = authenticate_for_test(&recovered, 0).unwrap();
    assert!(recovered.status(&recovered_admin).await.unwrap().safe_mode);
    assert_eq!(
        authenticate_for_test(&recovered, issued.metadata.key_id)
            .unwrap_err()
            .code,
        "temporary_key_store_unavailable"
    );
    recovered.reset(&recovered_admin).await.unwrap();
    assert!(!recovered.status(&recovered_admin).await.unwrap().safe_mode);

    drop(recovered);
    tokio::time::sleep(Duration::from_millis(20)).await;
    let _ = std::fs::remove_dir_all(state_dir);
}

#[tokio::test]
async fn root_rotation_rejects_old_key_and_in_flight_admin_context() {
    let _process_credential_guard = PROCESS_CREDENTIAL_TEST_LOCK.lock().await;
    let state_dir = temp_state_dir("auth-root-rotation");
    let old_key = *b"0123456789abcdefghijklmnopqrstuv";
    let new_key = *b"abcdefghijklmnopqrstuvwxyz012345";
    let config = AuthConfig {
        state_dir: state_dir.clone(),
        max_temporary_keys: 4,
        max_temporary_key_ttl: Duration::from_secs(3600),
        legacy_protocol: LegacyProtocolPolicy::Allow,
    };
    let runtime = AuthRuntime::start(old_key, config).await.unwrap();
    let old_admin = runtime.authenticate_presented(0, &old_key).unwrap();
    let mistyped_key = *b"1123456789abcdefghijklmnopqrstuv";
    assert_eq!(
        runtime
            .authenticate_presented(0, &mistyped_key)
            .unwrap_err()
            .code,
        "administrator_key_invalid"
    );

    runtime
        .rotate_root(&old_admin, new_key)
        .await
        .expect("root rotation should succeed");

    assert_eq!(
        runtime
            .authenticate_presented(0, &old_key)
            .unwrap_err()
            .code,
        "administrator_key_invalid"
    );
    assert_eq!(
        runtime
            .issue(&old_admin, Duration::from_secs(60), None)
            .await
            .unwrap_err()
            .code,
        "administrator_key_rotated"
    );
    let new_admin = runtime.authenticate_presented(0, &new_key).unwrap();
    assert!(runtime.status(&new_admin).await.is_ok());

    drop(runtime);
    tokio::time::sleep(Duration::from_millis(20)).await;
    let _ = std::fs::remove_dir_all(state_dir);
}

#[tokio::test]
async fn admitted_admin_mutation_replay_survives_restart() {
    let state_dir = temp_state_dir("admin-replay-restart");
    let admin_key = *b"0123456789abcdefghijklmnopqrstuv";
    let config = AuthConfig {
        state_dir: state_dir.clone(),
        max_temporary_keys: 4,
        max_temporary_key_ttl: Duration::from_secs(3600),
        legacy_protocol: LegacyProtocolPolicy::Allow,
    };
    let fingerprint = [0x5a; 32];
    let timestamp = unix_seconds();
    let runtime = AuthRuntime::start(admin_key, config.clone()).await.unwrap();
    let admin = authenticate_for_test(&runtime, 0).unwrap();
    runtime
        .claim_admin_mutation(&admin, fingerprint, timestamp)
        .await
        .unwrap();
    drop(runtime);
    tokio::time::sleep(Duration::from_millis(20)).await;

    let restored = AuthRuntime::start(admin_key, config).await.unwrap();
    let restored_admin = authenticate_for_test(&restored, 0).unwrap();
    assert_eq!(
        restored
            .claim_admin_mutation(&restored_admin, fingerprint, timestamp)
            .await
            .unwrap_err()
            .code,
        "admin_request_replayed"
    );

    drop(restored);
    tokio::time::sleep(Duration::from_millis(20)).await;
    let _ = std::fs::remove_dir_all(state_dir);
}

#[tokio::test]
async fn snapshot_compaction_preserves_audit_records() {
    let state_dir = temp_state_dir("audit-compaction");
    let admin_key = *b"0123456789abcdefghijklmnopqrstuv";
    let config = AuthConfig {
        state_dir: state_dir.clone(),
        max_temporary_keys: 4,
        max_temporary_key_ttl: Duration::from_secs(3600),
        legacy_protocol: LegacyProtocolPolicy::Allow,
    };
    let runtime = AuthRuntime::start(admin_key, config.clone()).await.unwrap();
    let admin = authenticate_for_test(&runtime, 0).unwrap();
    runtime
        .issue(&admin, Duration::from_secs(60), Some("audited".to_string()))
        .await
        .unwrap();
    runtime.gc(&admin).await.unwrap();

    let instance_id = load_or_create_instance_id(&state_dir).unwrap();
    let persisted = try_load_persisted_state(&config, &admin_key, instance_id).unwrap();
    let actions = persisted
        .audit_records
        .iter()
        .map(|record| record.action.as_str())
        .collect::<Vec<_>>();
    assert!(actions.contains(&"temporary_key_issue"));
    assert!(actions.contains(&"temporary_key_gc"));

    drop(runtime);
    tokio::time::sleep(Duration::from_millis(20)).await;
    let _ = std::fs::remove_dir_all(state_dir);
}

#[test]
fn timing_wheel_ignores_stale_renewal_entry() {
    let now = 1_000;
    let lease = Arc::new(AuthLease::new(make_key_id(1, 0), now + 5));
    let mut wheel = TimingWheel::new(now);
    wheel.insert(lease.clone());
    lease.expires_at.store(now + 20, Ordering::Release);
    lease.wheel_version.fetch_add(1, Ordering::AcqRel);
    wheel.insert(lease.clone());
    assert!(wheel.advance(now + 6).is_empty());
    assert_eq!(wheel.advance(now + 20).len(), 1);
}

#[test]
fn timing_wheel_fast_forwards_large_clock_jumps() {
    let now = 1_000;
    let target = now + 7 * 24 * 60 * 60;
    let expired = Arc::new(AuthLease::new(make_key_id(1, 0), now + 5));
    let future = Arc::new(AuthLease::new(make_key_id(1, 1), target + 20));
    let mut wheel = TimingWheel::new(now);
    wheel.insert(expired.clone());
    wheel.insert(future.clone());

    let due = wheel.advance(target);
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].key_id(), expired.key_id());
    assert!(wheel.advance(target + 19).is_empty());
    assert_eq!(wheel.advance(target + 20).len(), 1);
}

#[test]
fn timing_wheel_returns_already_expired_insert_without_wrapping() {
    let now = 1_000;
    let expired = Arc::new(AuthLease::new(make_key_id(1, 0), now - 1));
    let mut wheel = TimingWheel::new(now);
    wheel.insert(expired.clone());

    let due = wheel.advance(now);
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].key_id(), expired.key_id());
}

#[test]
fn timing_wheel_expires_cascaded_boundary_entry_without_an_extra_tick() {
    let now = 700;
    let expires_at = 1_024;
    let lease = Arc::new(AuthLease::new(make_key_id(1, 0), expires_at));
    let mut wheel = TimingWheel::new(now);
    wheel.insert(lease.clone());

    let due = wheel.advance(expires_at);
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].key_id(), lease.key_id());
}

#[test]
fn timing_wheel_clear_cancels_owned_leases() {
    let now = 1_000;
    let lease = Arc::new(AuthLease::new(make_key_id(1, 0), now + 60));
    let cancellation = lease.cancellation_token();
    let mut wheel = TimingWheel::new(now);
    wheel.insert(lease);

    wheel.clear(now + 1);
    assert!(cancellation.is_cancelled());
}

#[test]
fn replay_pruning_removes_only_records_outside_the_retention_window() {
    let now = 10_000;
    let expired = AdminReplayRecord {
        fingerprint: [1; 32],
        client_timestamp: now - ADMIN_REPLAY_RETENTION.as_secs() - 1,
    };
    let current = AdminReplayRecord {
        fingerprint: [2; 32],
        client_timestamp: now,
    };
    let mut replay_set = HashSet::from([expired.fingerprint, current.fingerprint]);
    let mut replay_order = VecDeque::from([expired, current.clone()]);

    super::actor::prune_expired_admin_replays(now, &mut replay_set, &mut replay_order);

    assert_eq!(replay_set, HashSet::from([current.fingerprint]));
    assert_eq!(replay_order.len(), 1);
    assert_eq!(replay_order[0].fingerprint, current.fingerprint);
}

#[test]
fn tombstone_migration_prefers_audit_time_and_persists_fail_closed_fallback() {
    let now = 10_000;
    let revoked_with_audit = PersistedEntry {
        key_id: make_key_id(1, 0),
        state: SlotState::Revoked,
        issued_at: 100,
        expires_at: 20_000,
        label: None,
        tombstoned_at: None,
    };
    let revoked_without_audit = PersistedEntry {
        key_id: make_key_id(1, 1),
        state: SlotState::Revoked,
        issued_at: 100,
        expires_at: 20_000,
        label: None,
        tombstoned_at: None,
    };
    let audit_at = now - 30;
    let mut snapshot = PersistedSnapshot {
        schema_version: SNAPSHOT_SCHEMA_VERSION,
        instance_id: [1; INSTANCE_ID_LEN],
        generations: vec![1, 1],
        entries: vec![revoked_with_audit, revoked_without_audit],
        legacy_protocol: LegacyProtocolPolicy::Deny,
        admin_replays: Vec::new(),
        audit_records: VecDeque::from([AuditRecord {
            at: audit_at,
            action: "temporary_key_revoke".to_string(),
            key_id: Some(make_key_id(1, 0)),
            label: None,
        }]),
    };

    assert!(normalize_tombstone_times(&mut snapshot, now));
    assert_eq!(snapshot.entries[0].tombstoned_at, Some(audit_at));
    assert_eq!(snapshot.entries[1].tombstoned_at, Some(now));
    assert!(!normalize_tombstone_times(&mut snapshot, now + 1));
    assert_eq!(snapshot.entries[1].tombstoned_at, Some(now));
}
