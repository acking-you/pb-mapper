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

fn authenticate_for_test(runtime: &AuthRuntime, key_id: KeyId) -> Result<AuthContext, AuthFailure> {
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
    let error =
        write_admin_key_file(&key_path, "abcdefghijklmnopqrstuvwxyz012345", true).unwrap_err();
    assert_eq!(error.code, "administrator_key_state_exists");
    write_admin_key_file(
        &state_dir.join("admin.key.next"),
        "abcdefghijklmnopqrstuvwxyz012345",
        true,
    )
    .unwrap();
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
    let admin = authenticate_for_test(&runtime, ADMIN_KEY_ID).unwrap();
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
    runtime
        .revoke(&admin, KeyId::from_u64(first_id))
        .await
        .unwrap();
    runtime
        .revoke(&admin, KeyId::from_u64(second_id))
        .await
        .unwrap();
    runtime.gc(&admin).await.unwrap();
    drop(runtime);
    tokio::time::sleep(Duration::from_millis(20)).await;

    let config_one = AuthConfig {
        max_temporary_keys: 1,
        ..config_two.clone()
    };
    let runtime = AuthRuntime::start(admin_key, config_one).await.unwrap();
    let admin = authenticate_for_test(&runtime, ADMIN_KEY_ID).unwrap();
    assert!(!runtime.status(&admin).await.unwrap().safe_mode);
    let _third = runtime
        .issue(&admin, Duration::from_secs(60), Some("third".to_string()))
        .await
        .unwrap();
    drop(runtime);
    tokio::time::sleep(Duration::from_millis(20)).await;

    let runtime = AuthRuntime::start(admin_key, config_two).await.unwrap();
    let admin = authenticate_for_test(&runtime, ADMIN_KEY_ID).unwrap();
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
async fn gc_removes_inactive_high_slot_entries_and_keeps_their_generations() {
    let state_dir = temp_state_dir("gc-high-slots");
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
    let admin = authenticate_for_test(&runtime, ADMIN_KEY_ID).unwrap();
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
    runtime
        .revoke(&admin, KeyId::from_u64(first_id))
        .await
        .unwrap();
    runtime
        .revoke(&admin, KeyId::from_u64(second_id))
        .await
        .unwrap();
    drop(runtime);
    tokio::time::sleep(Duration::from_millis(20)).await;

    let config_one = AuthConfig {
        max_temporary_keys: 1,
        ..config_two.clone()
    };
    let runtime = AuthRuntime::start(admin_key, config_one).await.unwrap();
    let admin = authenticate_for_test(&runtime, ADMIN_KEY_ID).unwrap();
    assert_eq!(runtime.high_slot_entry_count(), 1);
    let removed = runtime.gc(&admin).await.unwrap();
    assert!(removed >= 1);
    assert_eq!(runtime.high_slot_entry_count(), 0);
    drop(runtime);
    tokio::time::sleep(Duration::from_millis(20)).await;

    let runtime = AuthRuntime::start(admin_key, config_two).await.unwrap();
    let admin = authenticate_for_test(&runtime, ADMIN_KEY_ID).unwrap();
    let _low = runtime
        .issue(&admin, Duration::from_secs(60), Some("low".to_string()))
        .await
        .unwrap();
    let high = runtime
        .issue(&admin, Duration::from_secs(60), Some("high".to_string()))
        .await
        .unwrap();
    let Credential::Temporary {
        key_id: reused_id, ..
    } = parse_credential(&high.credential).unwrap()
    else {
        panic!("expected temporary credential");
    };
    assert_ne!(reused_id, second_id);
    drop(runtime);
    tokio::time::sleep(Duration::from_millis(20)).await;
    let _ = std::fs::remove_dir_all(state_dir);
}

#[tokio::test]
async fn admin_lifecycle_covers_high_slot_keys_after_capacity_shrink() {
    let state_dir = temp_state_dir("high-slot-admin");
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
    let admin = authenticate_for_test(&runtime, ADMIN_KEY_ID).unwrap();
    let first = runtime
        .issue(&admin, Duration::from_secs(60), Some("first".to_string()))
        .await
        .unwrap();
    let second = runtime
        .issue(&admin, Duration::from_secs(60), Some("second".to_string()))
        .await
        .unwrap();
    drop(runtime);
    tokio::time::sleep(Duration::from_millis(20)).await;

    let config_one = AuthConfig {
        max_temporary_keys: 1,
        ..config_two.clone()
    };
    let runtime = AuthRuntime::start(admin_key, config_one).await.unwrap();
    let admin = authenticate_for_test(&runtime, ADMIN_KEY_ID).unwrap();
    assert_eq!(runtime.high_slot_entry_count(), 1);
    let high_id = [first.metadata.key_id, second.metadata.key_id]
        .into_iter()
        .find(|key_id| key_id.slot().as_index() >= 1)
        .expect("one issued key should land above the shrunken table");
    let page = runtime.list(&admin, 0, 100).await.unwrap();
    assert_eq!(page.items.len(), 2);
    assert!(page.items.iter().any(|item| item.key_id == high_id));
    let shown = runtime.show(&admin, high_id, false).await.unwrap();
    assert_eq!(shown.metadata.key_id, high_id);
    assert_eq!(shown.metadata.state, "active");
    assert_eq!(
        authenticate_for_test(&runtime, high_id).unwrap_err().code,
        "temporary_key_not_found"
    );
    let status = runtime.status(&admin).await.unwrap();
    assert_eq!(status.active_keys, 2);
    let renewed = runtime
        .renew(&admin, high_id, Duration::from_secs(120))
        .await
        .unwrap();
    assert!(renewed.metadata.expires_at > shown.metadata.expires_at);
    runtime.revoke(&admin, high_id).await.unwrap();
    let revoked = runtime.show(&admin, high_id, false).await.unwrap();
    assert_eq!(revoked.metadata.state, "revoked");
    drop(runtime);
    tokio::time::sleep(Duration::from_millis(20)).await;

    let runtime = AuthRuntime::start(admin_key, config_two).await.unwrap();
    let admin = authenticate_for_test(&runtime, ADMIN_KEY_ID).unwrap();
    let restored = runtime.show(&admin, high_id, false).await.unwrap();
    assert_eq!(restored.metadata.state, "revoked");
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
    let admin = authenticate_for_test(&runtime, ADMIN_KEY_ID).unwrap();
    runtime
        .set_legacy_protocol(&admin, LegacyProtocolPolicy::Deny)
        .await
        .unwrap();
    drop(runtime);
    tokio::time::sleep(Duration::from_millis(20)).await;
    std::fs::write(state_dir.join("auth.wal"), b"broken-wal").unwrap();

    let runtime = AuthRuntime::start(admin_key, config).await.unwrap();
    let admin = authenticate_for_test(&runtime, ADMIN_KEY_ID).unwrap();
    let status = runtime.status(&admin).await.unwrap();
    assert!(status.safe_mode);
    assert_eq!(status.legacy_protocol, LegacyProtocolPolicy::Deny);
    drop(runtime);
    tokio::time::sleep(Duration::from_millis(20)).await;
    let _ = std::fs::remove_dir_all(state_dir);
}

#[tokio::test]
async fn overlapping_runtimes_cannot_share_an_auth_state_directory() {
    let state_dir = temp_state_dir("auth-dir-lock");
    let admin_key = *b"0123456789abcdefghijklmnopqrstuv";
    let config = AuthConfig {
        state_dir: state_dir.clone(),
        max_temporary_keys: 4,
        max_temporary_key_ttl: Duration::from_secs(3600),
        legacy_protocol: LegacyProtocolPolicy::Allow,
    };
    let first = AuthRuntime::start(admin_key, config.clone()).await.unwrap();
    let error = match AuthRuntime::start(admin_key, config.clone()).await {
        Ok(_) => panic!("second runtime should not share the auth directory"),
        Err(error) => error,
    };
    assert_eq!(error.code, "auth_state_locked");
    drop(first);
    tokio::time::sleep(Duration::from_millis(20)).await;
    let recovered = AuthRuntime::start(admin_key, config).await.unwrap();
    drop(recovered);
    tokio::time::sleep(Duration::from_millis(20)).await;
    let _ = std::fs::remove_dir_all(state_dir);
}

#[tokio::test]
async fn env_recovery_key_is_not_written_when_it_cannot_decrypt_existing_state() {
    let _process_credential_guard = PROCESS_CREDENTIAL_TEST_LOCK.lock().await;
    let state_dir = temp_state_dir("env-key-must-match-snapshot");
    prepare_state_dir(&state_dir).unwrap();
    let good = *b"0123456789abcdefghijklmnopqrstuv";
    let bad = *b"abcdefghijklmnopqrstuvwxyz012345";
    let config = AuthConfig {
        state_dir: state_dir.clone(),
        max_temporary_keys: 1,
        max_temporary_key_ttl: Duration::from_secs(3600),
        legacy_protocol: LegacyProtocolPolicy::Allow,
    };
    let snapshot = PersistedSnapshot {
        schema_version: SNAPSHOT_SCHEMA_VERSION,
        instance_id: [4_u8; INSTANCE_ID_LEN],
        generations: vec![Generation::FIRST; 1],
        entries: Vec::new(),
        legacy_protocol: LegacyProtocolPolicy::Allow,
        admin_replays: Vec::new(),
        audit_records: VecDeque::new(),
        root_epoch: 0,
    };
    write_snapshot_and_truncate_wal(&config, &good, &snapshot).unwrap();
    set_process_msg_header_key(Some(std::str::from_utf8(&bad).unwrap())).unwrap();
    std::env::set_var(ENV_MSG_HEADER_KEY, std::str::from_utf8(&bad).unwrap());
    let error = match AuthRuntime::from_process(config).await {
        Ok(_) => panic!("a mismatched recovery key must not start the runtime"),
        Err(error) => error,
    };
    std::env::remove_var(ENV_MSG_HEADER_KEY);
    set_process_msg_header_key(None).unwrap();
    assert_eq!(error.code, "administrator_key_invalid");
    assert!(
        !state_dir.join("admin.key").exists(),
        "a mismatched MSG_HEADER_KEY must not become the live administrator key"
    );
    let _ = std::fs::remove_dir_all(state_dir);
}

#[tokio::test]
async fn env_recovery_key_is_accepted_for_wal_only_state() {
    let _process_credential_guard = PROCESS_CREDENTIAL_TEST_LOCK.lock().await;
    let state_dir = temp_state_dir("env-key-matches-wal");
    prepare_state_dir(&state_dir).unwrap();
    let good = *b"0123456789abcdefghijklmnopqrstuv";
    let config = AuthConfig {
        state_dir: state_dir.clone(),
        max_temporary_keys: 1,
        max_temporary_key_ttl: Duration::from_secs(3600),
        legacy_protocol: LegacyProtocolPolicy::Allow,
    };
    append_wal(
        &config,
        &good,
        &WalRecord::Audit(AuditRecord {
            at: 1,
            action: "issue".to_string(),
            key_id: None,
            label: None,
        }),
    )
    .unwrap();
    set_process_msg_header_key(Some(std::str::from_utf8(&good).unwrap())).unwrap();
    std::env::set_var(ENV_MSG_HEADER_KEY, std::str::from_utf8(&good).unwrap());
    let started = AuthRuntime::from_process(config).await;
    std::env::remove_var(ENV_MSG_HEADER_KEY);
    set_process_msg_header_key(None).unwrap();
    started.expect("a matching recovery key must start from WAL-only state");
    assert!(
        state_dir.join("admin.key").exists(),
        "a matching MSG_HEADER_KEY should become the live administrator key"
    );
    let _ = std::fs::remove_dir_all(state_dir);
}

#[tokio::test]
async fn env_recovery_key_is_not_written_when_wal_only_state_does_not_match() {
    let _process_credential_guard = PROCESS_CREDENTIAL_TEST_LOCK.lock().await;
    let state_dir = temp_state_dir("env-key-must-match-wal");
    prepare_state_dir(&state_dir).unwrap();
    let good = *b"0123456789abcdefghijklmnopqrstuv";
    let bad = *b"abcdefghijklmnopqrstuvwxyz012345";
    let config = AuthConfig {
        state_dir: state_dir.clone(),
        max_temporary_keys: 1,
        max_temporary_key_ttl: Duration::from_secs(3600),
        legacy_protocol: LegacyProtocolPolicy::Allow,
    };
    append_wal(
        &config,
        &good,
        &WalRecord::Audit(AuditRecord {
            at: 1,
            action: "issue".to_string(),
            key_id: None,
            label: None,
        }),
    )
    .unwrap();
    set_process_msg_header_key(Some(std::str::from_utf8(&bad).unwrap())).unwrap();
    std::env::set_var(ENV_MSG_HEADER_KEY, std::str::from_utf8(&bad).unwrap());
    let error = match AuthRuntime::from_process(config).await {
        Ok(_) => panic!("a mismatched recovery key must not start from WAL-only state"),
        Err(error) => error,
    };
    std::env::remove_var(ENV_MSG_HEADER_KEY);
    set_process_msg_header_key(None).unwrap();
    assert_eq!(error.code, "administrator_key_invalid");
    assert!(
        !state_dir.join("admin.key").exists(),
        "a mismatched MSG_HEADER_KEY must not become the live administrator key"
    );
    let _ = std::fs::remove_dir_all(state_dir);
}

#[tokio::test]
async fn from_isolated_state_takes_the_state_lock_before_creating_admin_key() {
    let state_dir = temp_state_dir("lock-before-key");
    prepare_state_dir(&state_dir).unwrap();
    let _lock = acquire_state_dir_lock(&state_dir).unwrap();
    let config = AuthConfig {
        state_dir: state_dir.clone(),
        max_temporary_keys: 4,
        max_temporary_key_ttl: Duration::from_secs(3600),
        legacy_protocol: LegacyProtocolPolicy::Allow,
    };
    let error = match AuthRuntime::from_isolated_state(config).await {
        Ok(_) => panic!("a locked start should not create a second runtime"),
        Err(error) => error,
    };
    assert_eq!(error.code, "auth_state_locked");
    assert!(
        !state_dir.join("admin.key").exists(),
        "a locked start must not create a competing administrator key"
    );
    drop(_lock);
    tokio::time::sleep(Duration::from_millis(20)).await;
    let _ = std::fs::remove_dir_all(state_dir);
}

#[test]
fn safe_mode_startup_does_not_allow_compaction() {
    assert!(!compaction_is_allowed(true));
    assert!(compaction_is_allowed(false));
}

#[tokio::test]
async fn reset_clears_retained_high_slot_entries() {
    let state_dir = temp_state_dir("reset-high-slots");
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
    let admin = authenticate_for_test(&runtime, ADMIN_KEY_ID).unwrap();
    let first = runtime
        .issue(&admin, Duration::from_secs(60), Some("first".to_string()))
        .await
        .unwrap();
    let second = runtime
        .issue(&admin, Duration::from_secs(60), Some("second".to_string()))
        .await
        .unwrap();
    drop(runtime);
    tokio::time::sleep(Duration::from_millis(20)).await;

    let config_one = AuthConfig {
        max_temporary_keys: 1,
        ..config_two.clone()
    };
    let runtime = AuthRuntime::start(admin_key, config_one).await.unwrap();
    let admin = authenticate_for_test(&runtime, ADMIN_KEY_ID).unwrap();
    runtime.reset(&admin).await.unwrap();
    drop(runtime);
    tokio::time::sleep(Duration::from_millis(20)).await;

    let runtime = AuthRuntime::start(admin_key, config_two).await.unwrap();
    let admin = authenticate_for_test(&runtime, ADMIN_KEY_ID).unwrap();
    let page = runtime.list(&admin, 0, 100).await.unwrap();
    assert!(page.items.is_empty());
    assert!(authenticate_for_test(&runtime, first.metadata.key_id).is_err());
    assert!(authenticate_for_test(&runtime, second.metadata.key_id).is_err());
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
    let admin = authenticate_for_test(&runtime, ADMIN_KEY_ID).unwrap();
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
        let expected = linux_default_auth_state_dir(
            unix_effective_uid(),
            linux_system_auth_dir_usable(),
            std::env::var_os("XDG_DATA_HOME").as_deref(),
            std::env::var_os("HOME").as_deref(),
        );
        assert_eq!(dir, expected);
        if unix_effective_uid() != 0 && !linux_system_auth_dir_usable() {
            assert_ne!(
                dir,
                PathBuf::from(DEFAULT_AUTH_STATE_DIR),
                "unprivileged Linux should not default to the system auth directory: {}",
                dir.display()
            );
        }
    }
}

#[test]
fn sync_parent_directory_succeeds_for_a_local_file() {
    let state_dir = temp_state_dir("dirsync");
    prepare_state_dir(&state_dir).unwrap();
    let path = state_dir.join("probe");
    std::fs::write(&path, b"x").unwrap();
    sync_parent_directory(&path).unwrap();
    let _ = std::fs::remove_dir_all(state_dir);
}

#[cfg(not(any(windows, target_os = "macos")))]
#[test]
fn linux_default_auth_state_dir_prefers_user_data_when_system_dir_is_unusable() {
    assert_eq!(
        linux_default_auth_state_dir(0, false, None, Some(std::ffi::OsStr::new("/home/op"))),
        PathBuf::from(DEFAULT_AUTH_STATE_DIR)
    );
    assert_eq!(
        linux_default_auth_state_dir(1000, true, None, Some(std::ffi::OsStr::new("/home/op"))),
        PathBuf::from(DEFAULT_AUTH_STATE_DIR)
    );
    assert_eq!(
        linux_default_auth_state_dir(
            1000,
            false,
            Some(std::ffi::OsStr::new("/xdg")),
            Some(std::ffi::OsStr::new("/home/op"))
        ),
        PathBuf::from("/xdg/pb-mapper/auth")
    );
    assert_eq!(
        linux_default_auth_state_dir(1000, false, None, Some(std::ffi::OsStr::new("/home/op"))),
        PathBuf::from("/home/op/.local/share/pb-mapper/auth")
    );
    assert_eq!(
        linux_default_auth_state_dir(1000, false, None, None),
        PathBuf::from(DEFAULT_AUTH_STATE_DIR)
    );
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
fn key_id_serializes_as_a_plain_integer() {
    let key_id = KeyId::new(Generation::from_u32(3), SlotIndex::from_index(2));
    assert_eq!(serde_json::to_string(&key_id).unwrap(), "12884901890");
    assert_eq!(
        serde_json::from_str::<KeyId>("12884901890").unwrap(),
        key_id
    );
    assert_eq!(
        serde_json::to_string(&Generation::from_u32(7)).unwrap(),
        "7"
    );
}

#[test]
fn key_id_round_trip() {
    let key_id = KeyId::new(Generation::from_u32(42), SlotIndex::from_index(65_535));
    assert_eq!(key_id.generation(), Generation::from_u32(42));
    assert_eq!(key_id.slot(), SlotIndex::from_index(65_535));
}

#[test]
fn derived_key_is_bound_to_instance_and_key_id() {
    let admin_key = *b"0123456789abcdefghijklmnopqrstuv";
    let instance_a = [1_u8; INSTANCE_ID_LEN];
    let instance_b = [2_u8; INSTANCE_ID_LEN];
    let key = derive_temporary_key(
        &admin_key,
        &instance_a,
        KeyId::new(Generation::from_u32(1), SlotIndex::from_index(7)),
    )
    .unwrap();
    assert_eq!(
        key,
        derive_temporary_key(
            &admin_key,
            &instance_a,
            KeyId::new(Generation::from_u32(1), SlotIndex::from_index(7))
        )
        .unwrap()
    );
    assert_ne!(
        key,
        derive_temporary_key(
            &admin_key,
            &instance_b,
            KeyId::new(Generation::from_u32(1), SlotIndex::from_index(7))
        )
        .unwrap()
    );
    assert_ne!(
        key,
        derive_temporary_key(
            &admin_key,
            &instance_a,
            KeyId::new(Generation::from_u32(2), SlotIndex::from_index(7))
        )
        .unwrap()
    );
}

#[tokio::test]
async fn isolated_runtime_preserves_remote_temporary_process_credential() {
    let _process_credential_guard = PROCESS_CREDENTIAL_TEST_LOCK.lock().await;
    let state_dir = temp_state_dir("isolated-relay");
    let temporary_key_id = KeyId::new(Generation::from_u32(1), SlotIndex::from_index(0));
    let temporary_key = *b"temporary-remote-key-0123456789a";
    let temporary_credential =
        encode_temporary_credential(temporary_key_id.as_u64(), &temporary_key);
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
            key_id: temporary_key_id.as_u64(),
            key: temporary_key,
        }
    );

    let local_admin_raw = std::fs::read_to_string(state_dir.join("admin.key")).unwrap();
    let Credential::Admin(local_admin_key) = parse_credential(local_admin_raw.trim()).unwrap()
    else {
        panic!("isolated relay key should be an administrator credential");
    };
    let local_admin = runtime
        .authenticate_presented(ADMIN_KEY_ID, &local_admin_key)
        .unwrap();
    runtime
        .rotate_root(&local_admin, *b"isolated-new-admin-key-012345678")
        .await
        .unwrap();
    assert_eq!(
        get_process_credential().unwrap(),
        Credential::Temporary {
            key_id: temporary_key_id.as_u64(),
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
    let admin = authenticate_for_test(&runtime, ADMIN_KEY_ID).unwrap();
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
    let presented = runtime.derive_key(issued.metadata.key_id).unwrap();
    runtime
        .revoke(&admin, issued.metadata.key_id)
        .await
        .unwrap();
    let mut mistyped = presented;
    mistyped[0] ^= 0x01;
    assert_eq!(
        runtime
            .authenticate_presented(issued.metadata.key_id, &mistyped)
            .unwrap_err()
            .code,
        "temporary_key_invalid"
    );
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
async fn ensure_active_keeps_expiry_after_the_lease_is_cancelled() {
    let state_dir = temp_state_dir("lease-expiry-reason");
    let admin_key = *b"0123456789abcdefghijklmnopqrstuv";
    let config = AuthConfig {
        state_dir: state_dir.clone(),
        max_temporary_keys: 4,
        max_temporary_key_ttl: Duration::from_secs(3600),
        legacy_protocol: LegacyProtocolPolicy::Allow,
    };
    let runtime = AuthRuntime::start(admin_key, config).await.unwrap();
    let admin = authenticate_for_test(&runtime, ADMIN_KEY_ID).unwrap();
    let issued = runtime
        .issue(&admin, Duration::from_secs(60), Some("exp".to_string()))
        .await
        .unwrap();
    let context = authenticate_for_test(&runtime, issued.metadata.key_id).unwrap();
    let lease = context.ensure_active().unwrap();
    lease.expire_now();
    assert_eq!(
        context.ensure_active().unwrap_err().code,
        "temporary_key_expired"
    );
    assert_eq!(
        context.ensure_active().unwrap_err().code,
        "temporary_key_expired"
    );
    drop(runtime);
    tokio::time::sleep(Duration::from_millis(20)).await;
    let _ = std::fs::remove_dir_all(state_dir);
}

#[tokio::test]
async fn renew_replaces_a_lease_canceled_during_persistence() {
    let state_dir = temp_state_dir("renew-canceled-lease");
    let admin_key = *b"0123456789abcdefghijklmnopqrstuv";
    let config = AuthConfig {
        state_dir: state_dir.clone(),
        max_temporary_keys: 4,
        max_temporary_key_ttl: Duration::from_secs(3600),
        legacy_protocol: LegacyProtocolPolicy::Allow,
    };
    let runtime = AuthRuntime::start(admin_key, config).await.unwrap();
    let admin = authenticate_for_test(&runtime, ADMIN_KEY_ID).unwrap();
    let issued = runtime
        .issue(&admin, Duration::from_secs(60), Some("renew".to_string()))
        .await
        .unwrap();
    let context = authenticate_for_test(&runtime, issued.metadata.key_id).unwrap();
    let canceled = context.cancellation_token().unwrap();
    canceled.cancel();
    assert!(canceled.is_cancelled());

    let renewed = runtime
        .renew(&admin, issued.metadata.key_id, Duration::from_secs(120))
        .await
        .unwrap();
    assert_eq!(renewed.metadata.key_id, issued.metadata.key_id);
    let restored = authenticate_for_test(&runtime, issued.metadata.key_id).unwrap();
    assert!(!restored.cancellation_token().unwrap().is_cancelled());
    assert!(canceled.is_cancelled());

    drop(runtime);
    tokio::time::sleep(Duration::from_millis(20)).await;
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
    let admin = authenticate_for_test(&runtime, ADMIN_KEY_ID).unwrap();
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
    let old_presented = runtime.derive_key(old.metadata.key_id).unwrap();

    runtime.reset(&admin).await.unwrap();

    let after = runtime.status(&admin).await.unwrap().server_instance_id;
    assert_ne!(after, before);
    assert!(old_cancellation.is_cancelled());
    assert_eq!(
        runtime
            .authenticate_presented(old.metadata.key_id, &old_presented)
            .unwrap_err()
            .code,
        "temporary_key_rotated"
    );
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

#[test]
fn recover_instance_id_promotes_next_when_snapshot_matches() {
    let state_dir = temp_state_dir("instance-next-promote");
    prepare_state_dir(&state_dir).unwrap();
    let admin_key = *b"0123456789abcdefghijklmnopqrstuv";
    let current = [1_u8; INSTANCE_ID_LEN];
    let next = [2_u8; INSTANCE_ID_LEN];
    atomic_write(&state_dir.join("server-instance-id"), &current, 0o600).unwrap();
    atomic_write(&state_dir.join("server-instance-id.next"), &next, 0o600).unwrap();
    let config = AuthConfig {
        state_dir: state_dir.clone(),
        max_temporary_keys: 1,
        max_temporary_key_ttl: Duration::from_secs(3600),
        legacy_protocol: LegacyProtocolPolicy::Allow,
    };
    let snapshot = PersistedSnapshot {
        schema_version: SNAPSHOT_SCHEMA_VERSION,
        instance_id: next,
        generations: vec![Generation::FIRST; 1],
        entries: Vec::new(),
        legacy_protocol: LegacyProtocolPolicy::Allow,
        admin_replays: Vec::new(),
        audit_records: VecDeque::new(),
        root_epoch: 0,
    };
    write_snapshot_and_truncate_wal(&config, &admin_key, &snapshot).unwrap();
    std::fs::write(state_dir.join("auth.wal"), b"old-instance-wal").unwrap();

    let recovered = recover_instance_id_after_reset(&state_dir, &admin_key, current).unwrap();
    assert_eq!(recovered, next);
    assert_eq!(
        read_instance_id_file(&state_dir.join("server-instance-id")).unwrap(),
        Some(next)
    );
    assert!(!state_dir.join("server-instance-id.next").exists());
    assert_eq!(std::fs::read(state_dir.join("auth.wal")).unwrap(), b"");
    let _ = std::fs::remove_dir_all(state_dir);
}

#[test]
fn reset_already_installed_accepts_matching_live_id_and_snapshot() {
    let state_dir = temp_state_dir("reset-already-installed");
    prepare_state_dir(&state_dir).unwrap();
    let admin_key = *b"0123456789abcdefghijklmnopqrstuv";
    let new_id = [9_u8; INSTANCE_ID_LEN];
    atomic_write(&state_dir.join("server-instance-id"), &new_id, 0o600).unwrap();
    let config = AuthConfig {
        state_dir: state_dir.clone(),
        max_temporary_keys: 1,
        max_temporary_key_ttl: Duration::from_secs(3600),
        legacy_protocol: LegacyProtocolPolicy::Allow,
    };
    let snapshot = PersistedSnapshot {
        schema_version: SNAPSHOT_SCHEMA_VERSION,
        instance_id: new_id,
        generations: vec![Generation::FIRST; 1],
        entries: Vec::new(),
        legacy_protocol: LegacyProtocolPolicy::Allow,
        admin_replays: Vec::new(),
        audit_records: VecDeque::new(),
        root_epoch: 1,
    };
    write_snapshot_and_truncate_wal(&config, &admin_key, &snapshot).unwrap();
    assert!(reset_already_installed(&state_dir, &admin_key, &new_id));
    assert!(!reset_already_installed(
        &state_dir,
        &admin_key,
        &[8_u8; INSTANCE_ID_LEN]
    ));
    let _ = std::fs::remove_dir_all(state_dir);
}

#[test]
fn recover_instance_id_discards_stale_next_when_snapshot_still_matches_current() {
    let state_dir = temp_state_dir("instance-next-stale");
    prepare_state_dir(&state_dir).unwrap();
    let admin_key = *b"0123456789abcdefghijklmnopqrstuv";
    let current = [3_u8; INSTANCE_ID_LEN];
    let next = [4_u8; INSTANCE_ID_LEN];
    atomic_write(&state_dir.join("server-instance-id"), &current, 0o600).unwrap();
    atomic_write(&state_dir.join("server-instance-id.next"), &next, 0o600).unwrap();
    let config = AuthConfig {
        state_dir: state_dir.clone(),
        max_temporary_keys: 1,
        max_temporary_key_ttl: Duration::from_secs(3600),
        legacy_protocol: LegacyProtocolPolicy::Allow,
    };
    let snapshot = PersistedSnapshot {
        schema_version: SNAPSHOT_SCHEMA_VERSION,
        instance_id: current,
        generations: vec![Generation::FIRST; 1],
        entries: Vec::new(),
        legacy_protocol: LegacyProtocolPolicy::Allow,
        admin_replays: Vec::new(),
        audit_records: VecDeque::new(),
        root_epoch: 0,
    };
    write_snapshot_and_truncate_wal(&config, &admin_key, &snapshot).unwrap();

    let recovered = recover_instance_id_after_reset(&state_dir, &admin_key, current).unwrap();
    assert_eq!(recovered, current);
    assert!(!state_dir.join("server-instance-id.next").exists());
    let _ = std::fs::remove_dir_all(state_dir);
}

#[test]
fn recover_admin_key_discards_leftover_wal_from_the_old_key() {
    let state_dir = temp_state_dir("admin-next-wal");
    prepare_state_dir(&state_dir).unwrap();
    let old_key = *b"0123456789abcdefghijklmnopqrstuv";
    let new_key = *b"abcdefghijklmnopqrstuvwxyz012345";
    let old_key_str = std::str::from_utf8(&old_key).unwrap();
    let new_key_str = std::str::from_utf8(&new_key).unwrap();
    write_admin_key(&state_dir, old_key_str).unwrap();
    write_admin_key_file(&state_dir.join("admin.key.next"), new_key_str, true).unwrap();
    let config = AuthConfig {
        state_dir: state_dir.clone(),
        max_temporary_keys: 1,
        max_temporary_key_ttl: Duration::from_secs(3600),
        legacy_protocol: LegacyProtocolPolicy::Allow,
    };
    let snapshot = PersistedSnapshot {
        schema_version: SNAPSHOT_SCHEMA_VERSION,
        instance_id: [9_u8; INSTANCE_ID_LEN],
        generations: vec![Generation::FIRST; 1],
        entries: Vec::new(),
        legacy_protocol: LegacyProtocolPolicy::Allow,
        admin_replays: Vec::new(),
        audit_records: VecDeque::new(),
        root_epoch: 0,
    };
    write_snapshot_and_truncate_wal(&config, &new_key, &snapshot).unwrap();
    std::fs::write(state_dir.join("auth.wal"), b"old-key-wal").unwrap();

    let recovered = recover_admin_key_after_rotation(&state_dir, old_key_str).unwrap();
    assert_eq!(recovered.trim(), new_key_str);
    assert_eq!(std::fs::read(state_dir.join("auth.wal")).unwrap(), b"");
    assert!(!state_dir.join("admin.key.next").exists());
    let _ = std::fs::remove_dir_all(state_dir);
}

#[test]
fn rotation_finalize_requires_the_live_admin_key() {
    let state_dir = temp_state_dir("rotate-requires-live-key");
    prepare_state_dir(&state_dir).unwrap();
    let old_key = *b"0123456789abcdefghijklmnopqrstuv";
    let new_key = *b"abcdefghijklmnopqrstuvwxyz012345";
    let old_key_str = std::str::from_utf8(&old_key).unwrap();
    let new_key_str = std::str::from_utf8(&new_key).unwrap();
    write_admin_key(&state_dir, old_key_str).unwrap();
    let config = AuthConfig {
        state_dir: state_dir.clone(),
        max_temporary_keys: 1,
        max_temporary_key_ttl: Duration::from_secs(3600),
        legacy_protocol: LegacyProtocolPolicy::Allow,
    };
    let snapshot = PersistedSnapshot {
        schema_version: SNAPSHOT_SCHEMA_VERSION,
        instance_id: [3_u8; INSTANCE_ID_LEN],
        generations: vec![Generation::FIRST; 1],
        entries: Vec::new(),
        legacy_protocol: LegacyProtocolPolicy::Allow,
        admin_replays: Vec::new(),
        audit_records: VecDeque::new(),
        root_epoch: 1,
    };
    write_snapshot_and_truncate_wal(&config, &new_key, &snapshot).unwrap();
    assert!(!rotation_already_installed(&state_dir, new_key_str));
    write_admin_key(&state_dir, new_key_str).unwrap();
    assert!(rotation_already_installed(&state_dir, new_key_str));
    let _ = std::fs::remove_dir_all(state_dir);
}

#[tokio::test]
async fn interrupted_reset_recovers_the_staged_instance_id_on_restart() {
    let state_dir = temp_state_dir("reset-recover");
    let admin_key = *b"0123456789abcdefghijklmnopqrstuv";
    let config = AuthConfig {
        state_dir: state_dir.clone(),
        max_temporary_keys: 4,
        max_temporary_key_ttl: Duration::from_secs(3600),
        legacy_protocol: LegacyProtocolPolicy::Allow,
    };
    let runtime = AuthRuntime::start(admin_key, config.clone()).await.unwrap();
    let admin = authenticate_for_test(&runtime, ADMIN_KEY_ID).unwrap();
    let issued = runtime
        .issue(
            &admin,
            Duration::from_secs(60),
            Some("before-interrupted-reset".to_string()),
        )
        .await
        .unwrap();
    drop(runtime);
    tokio::time::sleep(Duration::from_millis(20)).await;

    let old_instance_id = load_or_create_instance_id(&state_dir).unwrap();
    let next = random_instance_id();
    atomic_write(&state_dir.join("server-instance-id.next"), &next, 0o600).unwrap();
    let snapshot = PersistedSnapshot {
        schema_version: SNAPSHOT_SCHEMA_VERSION,
        instance_id: next,
        generations: vec![Generation::FIRST; 4],
        entries: Vec::new(),
        legacy_protocol: LegacyProtocolPolicy::Allow,
        admin_replays: Vec::new(),
        audit_records: VecDeque::new(),
        root_epoch: 0,
    };
    write_snapshot_and_truncate_wal(&config, &admin_key, &snapshot).unwrap();
    std::fs::write(state_dir.join("auth.wal"), b"old-instance-wal").unwrap();
    atomic_write(
        &state_dir.join("server-instance-id"),
        &old_instance_id,
        0o600,
    )
    .unwrap();

    let restored = AuthRuntime::start(admin_key, config).await.unwrap();
    let restored_admin = authenticate_for_test(&restored, ADMIN_KEY_ID).unwrap();
    let status = restored.status(&restored_admin).await.unwrap();
    assert!(!status.safe_mode);
    assert_eq!(status.server_instance_id, hex(&next));
    assert!(authenticate_for_test(&restored, issued.metadata.key_id).is_err());
    assert!(!state_dir.join("server-instance-id.next").exists());
    drop(restored);
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
    let admin = authenticate_for_test(&runtime, ADMIN_KEY_ID).unwrap();
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
    let recovered_admin = authenticate_for_test(&recovered, ADMIN_KEY_ID).unwrap();
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
    let old_admin = runtime
        .authenticate_presented(ADMIN_KEY_ID, &old_key)
        .unwrap();
    let issued = runtime
        .issue(
            &old_admin,
            Duration::from_secs(60),
            Some("before-rotate".to_string()),
        )
        .await
        .unwrap();
    let old_temporary = runtime.derive_key(issued.metadata.key_id).unwrap();
    let mut mistyped_temporary = old_temporary;
    mistyped_temporary[0] ^= 0x01;
    assert_eq!(
        runtime
            .authenticate_presented(issued.metadata.key_id, &mistyped_temporary)
            .unwrap_err()
            .code,
        "temporary_key_invalid"
    );
    let mistyped_key = *b"1123456789abcdefghijklmnopqrstuv";
    assert_eq!(
        runtime
            .authenticate_presented(ADMIN_KEY_ID, &mistyped_key)
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
            .authenticate_presented(issued.metadata.key_id, &old_temporary)
            .unwrap_err()
            .code,
        "temporary_key_rotated"
    );
    let new_admin = runtime
        .authenticate_presented(ADMIN_KEY_ID, &new_key)
        .unwrap();
    let _replacement = runtime
        .issue(
            &new_admin,
            Duration::from_secs(60),
            Some("after-rotate".to_string()),
        )
        .await
        .unwrap();
    assert_eq!(
        runtime
            .authenticate_presented(issued.metadata.key_id, &old_temporary)
            .unwrap_err()
            .code,
        "temporary_key_rotated"
    );

    assert_eq!(
        runtime
            .authenticate_presented(ADMIN_KEY_ID, &old_key)
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
    let admin = authenticate_for_test(&runtime, ADMIN_KEY_ID).unwrap();
    runtime
        .claim_admin_mutation(&admin, fingerprint, timestamp)
        .await
        .unwrap();
    drop(runtime);
    tokio::time::sleep(Duration::from_millis(20)).await;

    let restored = AuthRuntime::start(admin_key, config).await.unwrap();
    let restored_admin = authenticate_for_test(&restored, ADMIN_KEY_ID).unwrap();
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
    let admin = authenticate_for_test(&runtime, ADMIN_KEY_ID).unwrap();
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

#[tokio::test]
async fn revoking_keeps_the_row_until_its_retention_elapses() {
    let state_dir = temp_state_dir("revoke-retention");
    let admin_key = *b"0123456789abcdefghijklmnopqrstuv";
    let config = AuthConfig {
        state_dir: state_dir.clone(),
        max_temporary_keys: 4,
        max_temporary_key_ttl: Duration::from_secs(3600),
        legacy_protocol: LegacyProtocolPolicy::Allow,
    };
    let runtime = AuthRuntime::start(admin_key, config).await.unwrap();
    let admin = authenticate_for_test(&runtime, ADMIN_KEY_ID).unwrap();
    let issued = runtime
        .issue(&admin, Duration::from_secs(60), Some("revoked".to_string()))
        .await
        .unwrap();
    let key_id = issued.metadata.key_id;
    let presented = runtime.derive_key(key_id).unwrap();

    runtime.revoke(&admin, key_id).await.unwrap();

    // The credential stops working at once, but the row survives so the reason
    // is still reportable rather than degrading to "unknown key".
    assert_eq!(
        runtime
            .authenticate_presented(key_id, &presented)
            .unwrap_err()
            .code,
        "temporary_key_revoked"
    );
    assert!(runtime
        .list(&admin, 0, 100)
        .await
        .unwrap()
        .items
        .iter()
        .any(|item| item.key_id == key_id && item.state == "revoked"));

    drop(runtime);
    tokio::time::sleep(Duration::from_millis(20)).await;
    let _ = std::fs::remove_dir_all(state_dir);
}

/// Records which phases ran, so a test can assert on the callback's effects
/// rather than on a return value the wheel no longer produces.
#[derive(Clone, Default)]
struct PhaseLog(Arc<std::sync::Mutex<Vec<&'static str>>>);

impl PhaseLog {
    fn push(&self, phase: &'static str) {
        recover_lock(self.0.lock()).push(phase);
    }

    fn phases(&self) -> Vec<&'static str> {
        recover_lock(self.0.lock()).clone()
    }
}

/// Schedules the two-phase shape `Leases` uses: a first phase that asks for a
/// second one `gap` seconds later, then a final phase.
fn schedule_two_phases(
    wheel: &mut TimingWheel,
    key_id: KeyId,
    deadline: u64,
    gap: u64,
) -> PhaseLog {
    let log = PhaseLog::default();
    let recorder = log.clone();
    let mut first = true;
    wheel.schedule(key_id, deadline, move || {
        if std::mem::take(&mut first) {
            recorder.push("retire");
            return Some(deadline + gap);
        }
        recorder.push("reap");
        None
    });
    log
}

fn schedule_once(wheel: &mut TimingWheel, key_id: KeyId, deadline: u64) -> PhaseLog {
    let log = PhaseLog::default();
    let recorder = log.clone();
    wheel.schedule(key_id, deadline, move || {
        recorder.push("fired");
        None
    });
    log
}

#[test]
fn timing_wheel_runs_the_next_phase_at_the_deadline_it_asked_for() {
    let key_id = KeyId::new(Generation::from_u32(1), SlotIndex::from_index(0));
    let mut wheel = TimingWheel::new(1_000);
    let log = schedule_two_phases(&mut wheel, key_id, 1_005, 60);

    wheel.advance(1_004);
    assert!(log.phases().is_empty());
    wheel.advance(1_005);
    assert_eq!(log.phases(), ["retire"]);
    // The second phase waits for the deadline the first one returned.
    wheel.advance(1_064);
    assert_eq!(log.phases(), ["retire"]);
    wheel.advance(1_065);
    assert_eq!(log.phases(), ["retire", "reap"]);
    assert!(!wheel.holds(key_id));
}

#[test]
fn timing_wheel_cancel_runs_every_remaining_phase_at_once() {
    let key_id = KeyId::new(Generation::from_u32(1), SlotIndex::from_index(0));
    let mut wheel = TimingWheel::new(1_000);
    let log = schedule_two_phases(&mut wheel, key_id, 1_005, 60);

    wheel.cancel(key_id);
    assert_eq!(log.phases(), ["retire", "reap"]);
    assert!(!wheel.holds(key_id));
}

#[test]
fn timing_wheel_cancel_after_the_first_phase_runs_only_the_rest() {
    let key_id = KeyId::new(Generation::from_u32(1), SlotIndex::from_index(0));
    let mut wheel = TimingWheel::new(1_000);
    let log = schedule_two_phases(&mut wheel, key_id, 1_005, 60);

    wheel.advance(1_005);
    wheel.cancel(key_id);
    assert_eq!(log.phases(), ["retire", "reap"]);
}

#[test]
fn timing_wheel_drop_runs_every_remaining_phase() {
    let key_id = KeyId::new(Generation::from_u32(1), SlotIndex::from_index(0));
    let mut wheel = TimingWheel::new(1_000);
    let log = schedule_two_phases(&mut wheel, key_id, 1_005, 60);

    drop(wheel);
    assert_eq!(log.phases(), ["retire", "reap"]);
}

#[test]
fn timing_wheel_reschedule_moves_an_entry_without_running_it() {
    let key_id = KeyId::new(Generation::from_u32(1), SlotIndex::from_index(0));
    let mut wheel = TimingWheel::new(1_000);
    let log = schedule_once(&mut wheel, key_id, 1_005);

    assert!(wheel.reschedule(key_id, 1_020));
    wheel.advance(1_019);
    assert!(log.phases().is_empty());
    wheel.advance(1_020);
    assert_eq!(log.phases(), ["fired"]);
}

#[test]
fn timing_wheel_reschedule_reports_a_key_it_does_not_hold() {
    let mut wheel = TimingWheel::new(1_000);
    assert!(!wheel.reschedule(
        KeyId::new(Generation::from_u32(1), SlotIndex::from_index(0)),
        2_000
    ));
}

#[test]
fn timing_wheel_scheduling_over_an_entry_finishes_the_one_it_replaces() {
    let key_id = KeyId::new(Generation::from_u32(1), SlotIndex::from_index(0));
    let mut wheel = TimingWheel::new(1_000);
    let stale = schedule_two_phases(&mut wheel, key_id, 1_005, 60);
    let fresh = schedule_once(&mut wheel, key_id, 1_020);

    assert_eq!(stale.phases(), ["retire", "reap"]);
    wheel.advance(1_020);
    assert_eq!(fresh.phases(), ["fired"]);
}

#[test]
fn timing_wheel_fires_an_entry_scheduled_in_the_past_without_wrapping() {
    let key_id = KeyId::new(Generation::from_u32(1), SlotIndex::from_index(0));
    let mut wheel = TimingWheel::new(1_000);
    let log = schedule_once(&mut wheel, key_id, 999);

    wheel.advance(1_000);
    assert_eq!(log.phases(), ["fired"]);
}

#[test]
fn timing_wheel_cascades_an_entry_down_two_levels() {
    // A deadline two levels up has to reach level 0 before it can be drained.
    let now = 1_000;
    let deadline = now + (1 << (6 * 2));
    let key_id = KeyId::new(Generation::from_u32(1), SlotIndex::from_index(0));
    let mut wheel = TimingWheel::new(now);
    let log = schedule_once(&mut wheel, key_id, deadline);

    wheel.advance(deadline - 1);
    assert!(log.phases().is_empty());
    wheel.advance(deadline);
    assert_eq!(log.phases(), ["fired"]);
}

#[test]
fn timing_wheel_fires_a_boundary_deadline_without_an_extra_tick() {
    let key_id = KeyId::new(Generation::from_u32(1), SlotIndex::from_index(0));
    let mut wheel = TimingWheel::new(700);
    let log = schedule_once(&mut wheel, key_id, 1_024);

    wheel.advance(1_024);
    assert_eq!(log.phases(), ["fired"]);
}

#[test]
fn timing_wheel_drains_in_one_pass_when_the_clock_jumps_past_every_deadline() {
    // A correction longer than the longest schedulable delay leaves nothing
    // pending, so the wheel empties without ticking through the elapsed years.
    let now = 1_000;
    let key_id = KeyId::new(Generation::from_u32(1), SlotIndex::from_index(0));
    let mut wheel = TimingWheel::new(now);
    let log = schedule_two_phases(&mut wheel, key_id, now + 60, 60);

    wheel.advance(now + 4 * MAX_TEMP_KEY_TTL.as_secs());
    assert_eq!(log.phases(), ["retire", "reap"]);
    assert!(!wheel.holds(key_id));
}

#[test]
fn timing_wheel_keeps_its_position_when_the_clock_steps_backwards() {
    let key_id = KeyId::new(Generation::from_u32(1), SlotIndex::from_index(0));
    let mut wheel = TimingWheel::new(1_000);
    let log = schedule_once(&mut wheel, key_id, 1_030);

    wheel.advance(1_020);
    wheel.advance(1_005);
    assert!(log.phases().is_empty());
    wheel.advance(1_030);
    assert_eq!(log.phases(), ["fired"]);
}

#[test]
fn replay_pruning_removes_only_records_outside_the_retention_window() {
    let now = 10_000;
    let expired = AdminReplayRecord {
        fingerprint: [1; 32],
        client_timestamp: now - ADMIN_REPLAY_RETENTION.as_secs() - 1,
        accepted_at: now - ADMIN_REPLAY_RETENTION.as_secs() - 1,
    };
    let current = AdminReplayRecord {
        fingerprint: [2; 32],
        client_timestamp: now,
        accepted_at: now,
    };
    let mut replay_set = HashSet::from([expired.fingerprint, current.fingerprint]);
    let mut replay_order = VecDeque::from([expired, current.clone()]);

    super::actor::prune_expired_admin_replays(now, &mut replay_set, &mut replay_order);

    assert_eq!(replay_set, HashSet::from([current.fingerprint]));
    assert_eq!(replay_order.len(), 1);
    assert_eq!(replay_order[0].fingerprint, current.fingerprint);
}

#[test]
fn replay_pruning_uses_server_acceptance_not_client_timestamp() {
    let now = 10_000;
    let retention = ADMIN_REPLAY_RETENTION.as_secs();
    let backdated = AdminReplayRecord {
        fingerprint: [3; 32],
        client_timestamp: now - retention - 1,
        accepted_at: now - 1,
    };
    let future_dated_but_expired = AdminReplayRecord {
        fingerprint: [4; 32],
        client_timestamp: now + retention / 2,
        accepted_at: now - retention - 1,
    };
    let mut replay_set =
        HashSet::from([backdated.fingerprint, future_dated_but_expired.fingerprint]);
    let mut replay_order = VecDeque::from([backdated.clone(), future_dated_but_expired]);

    super::actor::prune_expired_admin_replays(now, &mut replay_set, &mut replay_order);

    assert_eq!(replay_set, HashSet::from([backdated.fingerprint]));
    assert_eq!(replay_order.len(), 1);
    assert_eq!(replay_order[0].fingerprint, backdated.fingerprint);
}

#[test]
fn replay_pruning_falls_back_to_client_timestamp_for_legacy_records() {
    let now = 10_000;
    let legacy_expired = AdminReplayRecord {
        fingerprint: [5; 32],
        client_timestamp: now - ADMIN_REPLAY_RETENTION.as_secs() - 1,
        accepted_at: 0,
    };
    let legacy_current = AdminReplayRecord {
        fingerprint: [6; 32],
        client_timestamp: now,
        accepted_at: 0,
    };
    let mut replay_set = HashSet::from([legacy_expired.fingerprint, legacy_current.fingerprint]);
    let mut replay_order = VecDeque::from([legacy_expired, legacy_current.clone()]);

    super::actor::prune_expired_admin_replays(now, &mut replay_set, &mut replay_order);

    assert_eq!(replay_set, HashSet::from([legacy_current.fingerprint]));
    assert_eq!(replay_order.len(), 1);
    assert_eq!(replay_order[0].fingerprint, legacy_current.fingerprint);
}

#[test]
fn tombstone_migration_prefers_audit_time_and_persists_fail_closed_fallback() {
    let now = 10_000;
    let revoked_with_audit = PersistedEntry {
        key_id: KeyId::new(Generation::from_u32(1), SlotIndex::from_index(0)),
        state: SlotState::Revoked,
        issued_at: 100,
        expires_at: 20_000,
        label: None,
        tombstoned_at: None,
    };
    let revoked_without_audit = PersistedEntry {
        key_id: KeyId::new(Generation::from_u32(1), SlotIndex::from_index(1)),
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
        generations: vec![Generation::from_u32(1), Generation::from_u32(1)],
        entries: vec![revoked_with_audit, revoked_without_audit],
        legacy_protocol: LegacyProtocolPolicy::Deny,
        admin_replays: Vec::new(),
        audit_records: VecDeque::from([AuditRecord {
            at: audit_at,
            action: "temporary_key_revoke".to_string(),
            key_id: Some(KeyId::new(
                Generation::from_u32(1),
                SlotIndex::from_index(0),
            )),
            label: None,
        }]),
        root_epoch: 0,
    };

    assert!(normalize_tombstone_times(&mut snapshot, now));
    assert_eq!(snapshot.entries[0].tombstoned_at, Some(audit_at));
    assert_eq!(snapshot.entries[1].tombstoned_at, Some(now));
    assert!(!normalize_tombstone_times(&mut snapshot, now + 1));
    assert_eq!(snapshot.entries[1].tombstoned_at, Some(now));
}
