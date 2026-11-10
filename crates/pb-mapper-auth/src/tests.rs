//! Authentication invariants exercised at the state-machine boundary.
//!
//! ```text
//! issue -> renew -> expire/revoke -> persist/restart
//!   |                                |
//!   +-> lease cancellation           +-> encrypted recovery
//! root rotate -> reject old key + reject already-authenticated old context
//! ```
//!
//! Protocol framing has its own tests in `pb-mapper-protocol`'s
//! `src/secure/tests.rs`; this module focuses on lifecycle, persistence, audit,
//! and replay. `schedule.rs` drives the lease schedule with explicit seconds, and
//! the timing wheel's own tests sit beside it in `timing_wheel/tests.rs`.

use pb_mapper_core::test_support::{
    PROCESS_CREDENTIAL_TEST_LOCK, TEST_ADMIN_KEY, TEST_ADMIN_KEY_BYTES, TempPath,
};

use super::*;
#[cfg(not(any(windows, target_os = "macos")))]
use crate::config::{
    linux_default_auth_state_dir, linux_system_auth_dir_usable, unix_effective_uid,
};
use crate::config::{parse_legacy_protocol_policy, platform_default_auth_state_dir};
use crate::persistence::{
    auth_snapshot_path, prepare_state_dir, read_instance_id_file, seal_blob,
    try_load_persisted_state,
};

mod namespaces;
mod schedule;

const HOUR: Duration = Duration::from_secs(3600);

/// A configuration for `state_dir` with room for `capacity` keys.
fn config(state_dir: &Path, capacity: usize) -> AuthConfig {
    AuthConfig {
        state_dir: state_dir.to_path_buf(),
        max_temporary_keys: capacity,
        max_temporary_key_ttl: HOUR,
        legacy_protocol: LegacyProtocolPolicy::Allow,
    }
}

/// Starts a runtime under [`TEST_ADMIN_KEY_BYTES`] and authenticates its administrator.
async fn start(config: &AuthConfig) -> (AuthRuntime, AuthContext) {
    let runtime = AuthRuntime::start(TEST_ADMIN_KEY_BYTES, config.clone())
        .await
        .unwrap();
    let admin = authenticate_for_test(&runtime, ADMIN_KEY_ID).unwrap();
    (runtime, admin)
}

/// Stops a runtime, waiting for its actor, so the directory is free again.
async fn stop(runtime: AuthRuntime) {
    runtime.shutdown_actor().await;
    drop(runtime);
}

/// Stops a runtime and brings the same configuration back up.
async fn restart(runtime: AuthRuntime) -> (AuthRuntime, AuthContext) {
    let config = runtime.config().clone();
    stop(runtime).await;
    start(&config).await
}

fn authenticate_for_test(runtime: &AuthRuntime, key_id: KeyId) -> Result<AuthContext, AuthFailure> {
    let key = runtime.derive_key(key_id)?;
    runtime.authenticate_presented(key_id, &key)
}

/// An empty snapshot of `slots` first-generation rows for `instance_id`.
fn snapshot_for_test(
    instance_id: [u8; INSTANCE_ID_LEN],
    slots: usize,
    root_epoch: u64,
) -> PersistedSnapshot {
    PersistedSnapshot {
        schema_version: SNAPSHOT_SCHEMA_VERSION,
        instance_id,
        generations: vec![Generation::FIRST; slots],
        entries: Vec::new(),
        legacy_protocol: LegacyProtocolPolicy::Allow,
        admin_replays: Vec::new(),
        audit_records: VecDeque::new(),
        root_epoch,
        namespace_high_water: 0,
    }
}

#[test]
fn initialize_admin_key_refuses_to_replace_a_key_when_encrypted_state_exists() {
    let state_dir = TempPath::new("force-init-state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let key_path = state_dir.join("admin.key");
    std::fs::write(&key_path, format!("{TEST_ADMIN_KEY}\n")).unwrap();
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
}

/// A staged candidate is the only copy of a key the relay may already have
/// installed, so a second rotation attempt must not overwrite it with a third
/// key: that would leave no way back into the relay.
#[test]
fn staging_a_second_rotation_candidate_refuses_to_replace_the_first() {
    let state_dir = TempPath::new("stage-candidate");
    std::fs::create_dir_all(&state_dir).unwrap();
    let key_path = state_dir.join("admin.key");
    let first = "abcdefghijklmnopqrstuvwxyz012345";
    let second = "zyxwvutsrqponmlkjihgfedcba543210";

    let staged = stage_admin_key_candidate(&key_path, first).unwrap();
    assert_eq!(staged, staged_admin_key_path(&key_path));
    assert_eq!(std::fs::read_to_string(&staged).unwrap().trim(), first);

    // Restaging the same key is a retry of the same rotation, so it is allowed.
    stage_admin_key_candidate(&key_path, first).unwrap();
    assert_eq!(std::fs::read_to_string(&staged).unwrap().trim(), first);

    let error = stage_admin_key_candidate(&key_path, second).unwrap_err();
    assert_eq!(error.code, "administrator_key_staged");
    assert!(!error.retryable);
    assert_eq!(
        std::fs::read_to_string(&staged).unwrap().trim(),
        first,
        "the first candidate must survive a rejected second attempt"
    );

    // Once resolved, the next rotation stages freely again.
    discard_staged_admin_key(&staged);
    stage_admin_key_candidate(&key_path, second).unwrap();
    assert_eq!(std::fs::read_to_string(&staged).unwrap().trim(), second);
}

/// Two rotations racing to stage a candidate: exactly one may win, and the
/// loser must be told rather than silently overwriting the winner's file.
///
/// The check-then-act this replaced let both pass the existence check, and a
/// rename always wins — so the loser destroyed the winner's candidate, which may
/// be the only copy of the key the relay had already installed.
#[test]
fn racing_rotations_stage_exactly_one_candidate() {
    let state_dir = TempPath::new("stage-candidate-race");
    std::fs::create_dir_all(&state_dir).unwrap();
    let key_path = state_dir.join("admin.key");
    let staged_path = staged_admin_key_path(&key_path);

    // Distinct keys, so whichever file survives names its winner unambiguously.
    let keys = [
        "abcdefghijklmnopqrstuvwxyz012345",
        "zyxwvutsrqponmlkjihgfedcba543210",
        "0123456789abcdefghijklmnopqrstuv",
        "vutsrqponmlkjihgfedcba9876543210",
    ];

    let barrier = std::sync::Arc::new(std::sync::Barrier::new(keys.len()));
    let outcomes: Vec<_> = std::thread::scope(|scope| {
        let handles: Vec<_> = keys
            .iter()
            .map(|key| {
                let key_path = key_path.clone();
                let barrier = barrier.clone();
                scope.spawn(move || {
                    barrier.wait();
                    stage_admin_key_candidate(&key_path, key).map(|_| *key)
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect()
    });

    let winners: Vec<_> = outcomes.iter().filter_map(|o| o.as_ref().ok()).collect();
    assert_eq!(
        winners.len(),
        1,
        "exactly one racer may claim the staged path, got {outcomes:?}"
    );
    assert_eq!(
        std::fs::read_to_string(&staged_path).unwrap().trim(),
        *winners[0],
        "the file must hold the winner's key, not a later racer's"
    );
    for outcome in &outcomes {
        if let Err(error) = outcome {
            assert_eq!(error.code, "administrator_key_staged");
            assert!(!error.retryable);
        }
    }
}

/// A staged candidate is a live administrator key, so it must never be readable
/// by another user — not even for the moment between creating it and writing it.
#[cfg(unix)]
#[test]
fn a_staged_candidate_is_owner_only_from_the_moment_it_exists() {
    use std::os::unix::fs::PermissionsExt;

    let state_dir = TempPath::new("stage-candidate-mode");
    std::fs::create_dir_all(&state_dir).unwrap();
    let key_path = state_dir.join("admin.key");

    let staged_path =
        stage_admin_key_candidate(&key_path, "abcdefghijklmnopqrstuvwxyz012345").unwrap();
    let mode = std::fs::metadata(&staged_path)
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(
        mode & 0o777,
        0o600,
        "staged key mode was {:o}",
        mode & 0o777
    );
}

/// `create_new_write` gives up atomic contents in exchange for claiming the
/// path, so a crash between the create and the write leaves a short file. That
/// file is somebody else's unresolved rotation, and staging over it would
/// destroy it — even though it does not parse as a key.
#[test]
fn a_truncated_staged_candidate_still_blocks_rotation() {
    let state_dir = TempPath::new("stage-candidate-truncated");
    std::fs::create_dir_all(&state_dir).unwrap();
    let key_path = state_dir.join("admin.key");
    let staged_path = staged_admin_key_path(&key_path);
    std::fs::write(&staged_path, b"abcdef").unwrap();

    let error =
        stage_admin_key_candidate(&key_path, "abcdefghijklmnopqrstuvwxyz012345").unwrap_err();
    assert_eq!(error.code, "administrator_key_staged");
    assert_eq!(
        std::fs::read_to_string(&staged_path).unwrap(),
        "abcdef",
        "a short candidate is still evidence of an unresolved rotation"
    );
}

#[tokio::test]
async fn shrinking_then_expanding_capacity_does_not_reuse_old_key_ids() {
    let state_dir = TempPath::new("capacity-shrink");
    let config_two = config(&state_dir, 2);
    let (runtime, admin) = start(&config_two).await;
    let first = runtime
        .issue(&admin, Duration::from_secs(60), Some("first".to_string()))
        .await
        .unwrap();
    let second = runtime
        .issue(&admin, Duration::from_secs(60), Some("second".to_string()))
        .await
        .unwrap();
    let first_id = first.metadata.key_id;
    let second_id = second.metadata.key_id;
    runtime.revoke(&admin, first_id).await.unwrap();
    runtime.revoke(&admin, second_id).await.unwrap();
    runtime.gc(&admin).await.unwrap();
    stop(runtime).await;

    let config_one = config(&state_dir, 1);
    let (runtime, admin) = start(&config_one).await;
    assert!(!runtime.status(&admin).await.unwrap().safe_mode);
    let _third = runtime
        .issue(&admin, Duration::from_secs(60), Some("third".to_string()))
        .await
        .unwrap();
    stop(runtime).await;

    let (runtime, admin) = start(&config_two).await;
    assert!(!runtime.status(&admin).await.unwrap().safe_mode);
    let fourth = runtime
        .issue(&admin, Duration::from_secs(60), Some("fourth".to_string()))
        .await
        .unwrap();
    let fourth_id = fourth.metadata.key_id;
    assert_ne!(fourth_id, second_id);
    stop(runtime).await;
}

#[tokio::test]
async fn gc_removes_inactive_high_slot_entries_and_keeps_their_generations() {
    let state_dir = TempPath::new("gc-high-slots");
    let config_two = config(&state_dir, 2);
    let (runtime, admin) = start(&config_two).await;
    let first = runtime
        .issue(&admin, Duration::from_secs(60), Some("first".to_string()))
        .await
        .unwrap();
    let second = runtime
        .issue(&admin, Duration::from_secs(60), Some("second".to_string()))
        .await
        .unwrap();
    let first_id = first.metadata.key_id;
    let second_id = second.metadata.key_id;
    runtime.revoke(&admin, first_id).await.unwrap();
    runtime.revoke(&admin, second_id).await.unwrap();
    stop(runtime).await;

    let config_one = config(&state_dir, 1);
    let (runtime, admin) = start(&config_one).await;
    assert_eq!(runtime.high_slot_entry_count(), 1);
    let removed = runtime.gc(&admin).await.unwrap();
    assert!(removed >= 1);
    assert_eq!(runtime.high_slot_entry_count(), 0);
    stop(runtime).await;

    let (runtime, admin) = start(&config_two).await;
    let _low = runtime
        .issue(&admin, Duration::from_secs(60), Some("low".to_string()))
        .await
        .unwrap();
    let high = runtime
        .issue(&admin, Duration::from_secs(60), Some("high".to_string()))
        .await
        .unwrap();
    let reused_id = high.metadata.key_id;
    assert_ne!(reused_id, second_id);
    stop(runtime).await;
}

#[tokio::test]
async fn admin_lifecycle_covers_high_slot_keys_after_capacity_shrink() {
    let state_dir = TempPath::new("high-slot-admin");
    let config_two = config(&state_dir, 2);
    let (runtime, admin) = start(&config_two).await;
    let first = runtime
        .issue(&admin, Duration::from_secs(60), Some("first".to_string()))
        .await
        .unwrap();
    let second = runtime
        .issue(&admin, Duration::from_secs(60), Some("second".to_string()))
        .await
        .unwrap();
    stop(runtime).await;

    let config_one = config(&state_dir, 1);
    let (runtime, admin) = start(&config_one).await;
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
    stop(runtime).await;

    let (runtime, admin) = start(&config_two).await;
    let restored = runtime.show(&admin, high_id, false).await.unwrap();
    assert_eq!(restored.metadata.state, "revoked");
    stop(runtime).await;
}

#[tokio::test]
async fn safe_mode_denies_legacy_protocol_instead_of_restoring_the_default() {
    let state_dir = TempPath::new("safe-mode-legacy");
    let config = config(&state_dir, 4);
    let (runtime, admin) = start(&config).await;
    runtime
        .set_legacy_protocol(&admin, LegacyProtocolPolicy::Deny)
        .await
        .unwrap();
    stop(runtime).await;
    std::fs::write(state_dir.join("auth.wal"), b"broken-wal").unwrap();

    let (runtime, admin) = start(&config).await;
    let status = runtime.status(&admin).await.unwrap();
    assert!(status.safe_mode);
    assert_eq!(status.legacy_protocol, LegacyProtocolPolicy::Deny);
    stop(runtime).await;
}

/// The app's stop path aborts a wedged actor and relies on the state being gone
/// once this returns.
#[tokio::test]
async fn abort_actor_returns_once_the_state_has_dropped() {
    let state_dir = TempPath::new("abort-actor");
    let config = config(&state_dir, 4);
    let runtime = AuthRuntime::start(TEST_ADMIN_KEY_BYTES, config)
        .await
        .unwrap();
    authenticate_for_test(&runtime, ADMIN_KEY_ID).unwrap();

    runtime.abort_actor().await.unwrap();
    assert_eq!(
        authenticate_for_test(&runtime, ADMIN_KEY_ID)
            .unwrap_err()
            .code,
        "auth_state_unavailable"
    );
    drop(runtime);
}

#[tokio::test]
async fn overlapping_runtimes_cannot_share_an_auth_state_directory() {
    let state_dir = TempPath::new("auth-dir-lock");
    let config = config(&state_dir, 4);
    let first = AuthRuntime::start(TEST_ADMIN_KEY_BYTES, config.clone())
        .await
        .unwrap();
    let error = match AuthRuntime::start(TEST_ADMIN_KEY_BYTES, config.clone()).await {
        Ok(_) => panic!("second runtime should not share the auth directory"),
        Err(error) => error,
    };
    assert_eq!(error.code, "auth_state_locked");
    stop(first).await;
    let recovered = AuthRuntime::start(TEST_ADMIN_KEY_BYTES, config)
        .await
        .unwrap();
    stop(recovered).await;
}

#[tokio::test]
async fn env_recovery_key_is_not_written_when_it_cannot_decrypt_existing_state() {
    let _process_credential_guard = PROCESS_CREDENTIAL_TEST_LOCK.lock().await;
    let state_dir = TempPath::new("env-key-must-match-snapshot");
    prepare_state_dir(&state_dir).unwrap();
    let good = TEST_ADMIN_KEY_BYTES;
    let bad = *b"abcdefghijklmnopqrstuvwxyz012345";
    let config = config(&state_dir, 1);
    let snapshot = snapshot_for_test([4_u8; INSTANCE_ID_LEN], 1, 0);
    write_snapshot_and_truncate_wal(&config, &good, &snapshot).unwrap();
    set_process_msg_header_key(Some(std::str::from_utf8(&bad).unwrap())).unwrap();
    let error = match AuthRuntime::from_process(config).await {
        Ok(_) => panic!("a mismatched recovery key must not start the runtime"),
        Err(error) => error,
    };
    set_process_msg_header_key(None).unwrap();
    assert_eq!(error.code, "administrator_key_invalid");
    assert!(
        !state_dir.join("admin.key").exists(),
        "a mismatched MSG_HEADER_KEY must not become the live administrator key"
    );
}

#[tokio::test]
async fn env_recovery_key_is_accepted_for_wal_only_state() {
    let _process_credential_guard = PROCESS_CREDENTIAL_TEST_LOCK.lock().await;
    let state_dir = TempPath::new("env-key-matches-wal");
    prepare_state_dir(&state_dir).unwrap();
    let good = TEST_ADMIN_KEY_BYTES;
    let config = config(&state_dir, 1);
    append_wal(
        &config,
        &good,
        &WalRecord::Audit(AuditRecord {
            at: 1,
            action: "issue".to_string(),
            key_id: None,
            label: None,
            namespace: None,
        }),
    )
    .unwrap();
    set_process_msg_header_key(Some(std::str::from_utf8(&good).unwrap())).unwrap();
    let started = AuthRuntime::from_process(config).await;
    set_process_msg_header_key(None).unwrap();
    started.expect("a matching recovery key must start from WAL-only state");
    assert!(
        state_dir.join("admin.key").exists(),
        "a matching MSG_HEADER_KEY should become the live administrator key"
    );
}

#[tokio::test]
async fn env_recovery_key_is_not_written_when_wal_only_state_does_not_match() {
    let _process_credential_guard = PROCESS_CREDENTIAL_TEST_LOCK.lock().await;
    let state_dir = TempPath::new("env-key-must-match-wal");
    prepare_state_dir(&state_dir).unwrap();
    let good = TEST_ADMIN_KEY_BYTES;
    let bad = *b"abcdefghijklmnopqrstuvwxyz012345";
    let config = config(&state_dir, 1);
    append_wal(
        &config,
        &good,
        &WalRecord::Audit(AuditRecord {
            at: 1,
            action: "issue".to_string(),
            key_id: None,
            label: None,
            namespace: None,
        }),
    )
    .unwrap();
    set_process_msg_header_key(Some(std::str::from_utf8(&bad).unwrap())).unwrap();
    let error = match AuthRuntime::from_process(config).await {
        Ok(_) => panic!("a mismatched recovery key must not start from WAL-only state"),
        Err(error) => error,
    };
    set_process_msg_header_key(None).unwrap();
    assert_eq!(error.code, "administrator_key_invalid");
    assert!(
        !state_dir.join("admin.key").exists(),
        "a mismatched MSG_HEADER_KEY must not become the live administrator key"
    );
}

#[tokio::test]
async fn from_isolated_state_takes_the_state_lock_before_creating_admin_key() {
    let state_dir = TempPath::new("lock-before-key");
    prepare_state_dir(&state_dir).unwrap();
    let _lock = acquire_state_dir_lock(&state_dir).unwrap();
    let config = config(&state_dir, 4);
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
}

#[tokio::test]
async fn reset_clears_retained_high_slot_entries() {
    let state_dir = TempPath::new("reset-high-slots");
    let config_two = config(&state_dir, 2);
    let (runtime, admin) = start(&config_two).await;
    let first = runtime
        .issue(&admin, Duration::from_secs(60), Some("first".to_string()))
        .await
        .unwrap();
    let second = runtime
        .issue(&admin, Duration::from_secs(60), Some("second".to_string()))
        .await
        .unwrap();
    stop(runtime).await;

    let config_one = config(&state_dir, 1);
    let (runtime, admin) = start(&config_one).await;
    runtime.reset(&admin).await.unwrap();
    stop(runtime).await;

    let (runtime, admin) = start(&config_two).await;
    let page = runtime.list(&admin, 0, 100).await.unwrap();
    assert!(page.items.is_empty());
    assert!(authenticate_for_test(&runtime, first.metadata.key_id).is_err());
    assert!(authenticate_for_test(&runtime, second.metadata.key_id).is_err());
    stop(runtime).await;
}

#[tokio::test]
async fn rotate_root_rejects_a_nul_containing_key() {
    let state_dir = TempPath::new("rotate-nul");
    let config = config(&state_dir, 4);
    let (runtime, admin) = start(&config).await;
    let mut bad = TEST_ADMIN_KEY_BYTES;
    bad[4] = 0;
    let error = runtime.rotate_root(&admin, bad).await.unwrap_err();
    assert_eq!(error.code, "administrator_key_invalid");
    stop(runtime).await;
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
                || dir == std::path::Path::new("/Library/Application Support/pb-mapper/auth"),
            "macos default auth dir should be under Application Support: {}",
            dir.display()
        );
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
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
fn key_id_packs_generation_and_slot_into_a_plain_integer() {
    let key_id = KeyId::new(Generation::from_u32(3), SlotIndex::from_index(2));
    assert_eq!(key_id.generation(), Generation::from_u32(3));
    assert_eq!(key_id.slot(), SlotIndex::from_index(2));
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
fn derived_key_is_bound_to_instance_and_key_id() {
    let instance_a = [1_u8; INSTANCE_ID_LEN];
    let instance_b = [2_u8; INSTANCE_ID_LEN];
    let key = derive_temporary_key(
        &TEST_ADMIN_KEY_BYTES,
        &instance_a,
        KeyId::new(Generation::from_u32(1), SlotIndex::from_index(7)),
    )
    .unwrap();
    assert_eq!(
        key,
        derive_temporary_key(
            &TEST_ADMIN_KEY_BYTES,
            &instance_a,
            KeyId::new(Generation::from_u32(1), SlotIndex::from_index(7))
        )
        .unwrap()
    );
    assert_ne!(
        key,
        derive_temporary_key(
            &TEST_ADMIN_KEY_BYTES,
            &instance_b,
            KeyId::new(Generation::from_u32(1), SlotIndex::from_index(7))
        )
        .unwrap()
    );
    assert_ne!(
        key,
        derive_temporary_key(
            &TEST_ADMIN_KEY_BYTES,
            &instance_a,
            KeyId::new(Generation::from_u32(2), SlotIndex::from_index(7))
        )
        .unwrap()
    );
}

#[tokio::test]
async fn isolated_runtime_preserves_remote_temporary_process_credential() {
    let _process_credential_guard = PROCESS_CREDENTIAL_TEST_LOCK.lock().await;
    let state_dir = TempPath::new("isolated-relay");
    let temporary_key_id = KeyId::new(Generation::from_u32(1), SlotIndex::from_index(0));
    let temporary_key = *b"temporary-remote-key-0123456789a";
    let temporary_credential =
        encode_temporary_credential(temporary_key_id.as_u64(), &temporary_key);
    set_process_msg_header_key(Some(&temporary_credential)).unwrap();
    let config = config(&state_dir, 4);

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
    stop(runtime).await;
}

#[tokio::test]
async fn issue_renew_revoke_and_persist() {
    let state_dir = TempPath::new("auth-lifecycle");
    let config = config(&state_dir, 8);
    let (runtime, admin) = start(&config).await;
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
    let persisted = try_load_persisted_state(&config, &TEST_ADMIN_KEY_BYTES, instance_id).unwrap();
    let revoked = persisted
        .entries
        .iter()
        .find(|entry| entry.key_id == issued.metadata.key_id)
        .unwrap();
    assert_eq!(revoked.state, SlotState::Revoked);
    assert!(revoked.tombstoned_at.is_some());
    stop(runtime).await;
    let restored = AuthRuntime::start(TEST_ADMIN_KEY_BYTES, config)
        .await
        .unwrap();
    assert_eq!(
        authenticate_for_test(&restored, issued.metadata.key_id)
            .unwrap_err()
            .code,
        "temporary_key_revoked"
    );
}

#[tokio::test]
async fn ensure_active_keeps_expiry_after_the_lease_is_cancelled() {
    let state_dir = TempPath::new("lease-expiry-reason");
    let config = config(&state_dir, 4);
    let (runtime, admin) = start(&config).await;
    let issued = runtime
        .issue(&admin, Duration::from_secs(60), Some("exp".to_string()))
        .await
        .unwrap();
    let context = authenticate_for_test(&runtime, issued.metadata.key_id).unwrap();
    let lease = context.ensure_active().unwrap();
    lease.set_expires_at(0);
    assert_eq!(
        context.ensure_active().unwrap_err().code,
        "temporary_key_expired"
    );
    assert_eq!(
        context.ensure_active().unwrap_err().code,
        "temporary_key_expired"
    );
    stop(runtime).await;
}

#[tokio::test]
async fn renew_replaces_a_lease_canceled_during_persistence() {
    let state_dir = TempPath::new("renew-canceled-lease");
    let config = config(&state_dir, 4);
    let (runtime, admin) = start(&config).await;
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

    stop(runtime).await;
}

#[tokio::test]
async fn reset_rotates_instance_and_prevents_old_key_id_reuse() {
    let state_dir = TempPath::new("auth-reset");
    let config = config(&state_dir, 1);
    let (runtime, admin) = start(&config).await;
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

    stop(runtime).await;
}

#[test]
fn recover_instance_id_promotes_next_when_snapshot_matches() {
    let state_dir = TempPath::new("instance-next-promote");
    prepare_state_dir(&state_dir).unwrap();
    let current = [1_u8; INSTANCE_ID_LEN];
    let next = [2_u8; INSTANCE_ID_LEN];
    atomic_write(&state_dir.join("server-instance-id"), &current, 0o600).unwrap();
    atomic_write(&state_dir.join("server-instance-id.next"), &next, 0o600).unwrap();
    let config = config(&state_dir, 1);
    let snapshot = snapshot_for_test(next, 1, 0);
    write_snapshot_and_truncate_wal(&config, &TEST_ADMIN_KEY_BYTES, &snapshot).unwrap();
    std::fs::write(state_dir.join("auth.wal"), b"old-instance-wal").unwrap();

    let recovered =
        recover_instance_id_after_reset(&state_dir, &TEST_ADMIN_KEY_BYTES, current).unwrap();
    assert_eq!(recovered, next);
    assert_eq!(
        read_instance_id_file(&state_dir.join("server-instance-id")).unwrap(),
        Some(next)
    );
    assert!(!state_dir.join("server-instance-id.next").exists());
    assert_eq!(std::fs::read(state_dir.join("auth.wal")).unwrap(), b"");
}

#[test]
fn reset_already_installed_accepts_matching_live_id_and_snapshot() {
    let state_dir = TempPath::new("reset-already-installed");
    prepare_state_dir(&state_dir).unwrap();
    let new_id = [9_u8; INSTANCE_ID_LEN];
    atomic_write(&state_dir.join("server-instance-id"), &new_id, 0o600).unwrap();
    let config = config(&state_dir, 1);
    let snapshot = snapshot_for_test(new_id, 1, 1);
    write_snapshot_and_truncate_wal(&config, &TEST_ADMIN_KEY_BYTES, &snapshot).unwrap();
    assert!(reset_already_installed(
        &state_dir,
        &TEST_ADMIN_KEY_BYTES,
        &new_id
    ));
    assert!(!reset_already_installed(
        &state_dir,
        &TEST_ADMIN_KEY_BYTES,
        &[8_u8; INSTANCE_ID_LEN]
    ));
}

#[test]
fn recover_instance_id_discards_stale_next_when_snapshot_still_matches_current() {
    let state_dir = TempPath::new("instance-next-stale");
    prepare_state_dir(&state_dir).unwrap();
    let current = [3_u8; INSTANCE_ID_LEN];
    let next = [4_u8; INSTANCE_ID_LEN];
    atomic_write(&state_dir.join("server-instance-id"), &current, 0o600).unwrap();
    atomic_write(&state_dir.join("server-instance-id.next"), &next, 0o600).unwrap();
    let config = config(&state_dir, 1);
    let snapshot = snapshot_for_test(current, 1, 0);
    write_snapshot_and_truncate_wal(&config, &TEST_ADMIN_KEY_BYTES, &snapshot).unwrap();

    let recovered =
        recover_instance_id_after_reset(&state_dir, &TEST_ADMIN_KEY_BYTES, current).unwrap();
    assert_eq!(recovered, current);
    assert!(!state_dir.join("server-instance-id.next").exists());
}

#[test]
fn recover_admin_key_discards_leftover_wal_from_the_old_key() {
    let state_dir = TempPath::new("admin-next-wal");
    prepare_state_dir(&state_dir).unwrap();
    let old_key = TEST_ADMIN_KEY_BYTES;
    let new_key = *b"abcdefghijklmnopqrstuvwxyz012345";
    let old_key_str = std::str::from_utf8(&old_key).unwrap();
    let new_key_str = std::str::from_utf8(&new_key).unwrap();
    write_admin_key(&state_dir, old_key_str).unwrap();
    write_admin_key_file(&state_dir.join("admin.key.next"), new_key_str, true).unwrap();
    let config = config(&state_dir, 1);
    let snapshot = snapshot_for_test([9_u8; INSTANCE_ID_LEN], 1, 0);
    write_snapshot_and_truncate_wal(&config, &new_key, &snapshot).unwrap();
    std::fs::write(state_dir.join("auth.wal"), b"old-key-wal").unwrap();

    let recovered = recover_admin_key_after_rotation(&state_dir, old_key_str).unwrap();
    assert_eq!(recovered.trim(), new_key_str);
    assert_eq!(std::fs::read(state_dir.join("auth.wal")).unwrap(), b"");
    assert!(!state_dir.join("admin.key.next").exists());
}

#[test]
fn rotation_finalize_requires_the_live_admin_key() {
    let state_dir = TempPath::new("rotate-requires-live-key");
    prepare_state_dir(&state_dir).unwrap();
    let old_key = TEST_ADMIN_KEY_BYTES;
    let new_key = *b"abcdefghijklmnopqrstuvwxyz012345";
    let old_key_str = std::str::from_utf8(&old_key).unwrap();
    let new_key_str = std::str::from_utf8(&new_key).unwrap();
    write_admin_key(&state_dir, old_key_str).unwrap();
    let config = config(&state_dir, 1);
    let snapshot = snapshot_for_test([3_u8; INSTANCE_ID_LEN], 1, 1);
    write_snapshot_and_truncate_wal(&config, &new_key, &snapshot).unwrap();
    assert!(!rotation_already_installed(&state_dir, new_key_str));
    write_admin_key(&state_dir, new_key_str).unwrap();
    assert!(rotation_already_installed(&state_dir, new_key_str));
}

#[tokio::test]
async fn interrupted_reset_recovers_the_staged_instance_id_on_restart() {
    let state_dir = TempPath::new("reset-recover");
    let config = config(&state_dir, 4);
    let (runtime, admin) = start(&config).await;
    let issued = runtime
        .issue(
            &admin,
            Duration::from_secs(60),
            Some("before-interrupted-reset".to_string()),
        )
        .await
        .unwrap();
    stop(runtime).await;

    let old_instance_id = load_or_create_instance_id(&state_dir).unwrap();
    let next = random_instance_id();
    atomic_write(&state_dir.join("server-instance-id.next"), &next, 0o600).unwrap();
    let snapshot = snapshot_for_test(next, 4, 0);
    write_snapshot_and_truncate_wal(&config, &TEST_ADMIN_KEY_BYTES, &snapshot).unwrap();
    std::fs::write(state_dir.join("auth.wal"), b"old-instance-wal").unwrap();
    atomic_write(
        &state_dir.join("server-instance-id"),
        &old_instance_id,
        0o600,
    )
    .unwrap();

    let (restored, restored_admin) = start(&config).await;
    let status = restored.status(&restored_admin).await.unwrap();
    assert!(!status.safe_mode);
    assert_eq!(status.server_instance_id, hex(&next));
    assert!(authenticate_for_test(&restored, issued.metadata.key_id).is_err());
    assert!(!state_dir.join("server-instance-id.next").exists());
    stop(restored).await;
}

#[tokio::test]
async fn corrupt_wal_fails_temporary_keys_closed_until_admin_reset() {
    let state_dir = TempPath::new("auth-safe-mode");
    let config = config(&state_dir, 4);
    let (runtime, admin) = start(&config).await;
    let issued = runtime
        .issue(
            &admin,
            Duration::from_secs(60),
            Some("corrupt-me".to_string()),
        )
        .await
        .unwrap();
    stop(runtime).await;
    std::fs::write(state_dir.join("auth.wal"), b"broken-wal").unwrap();

    let (recovered, recovered_admin) = start(&config).await;
    assert!(recovered.status(&recovered_admin).await.unwrap().safe_mode);
    assert_eq!(
        authenticate_for_test(&recovered, issued.metadata.key_id)
            .unwrap_err()
            .code,
        "temporary_key_store_unavailable"
    );
    recovered.reset(&recovered_admin).await.unwrap();
    assert!(!recovered.status(&recovered_admin).await.unwrap().safe_mode);

    stop(recovered).await;
}

#[tokio::test]
async fn root_rotation_rejects_old_key_and_in_flight_admin_context() {
    let _process_credential_guard = PROCESS_CREDENTIAL_TEST_LOCK.lock().await;
    let state_dir = TempPath::new("auth-root-rotation");
    let old_key = TEST_ADMIN_KEY_BYTES;
    let new_key = *b"abcdefghijklmnopqrstuvwxyz012345";
    let config = config(&state_dir, 4);
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

    stop(runtime).await;
    // The rotation synced the process credential; leave it as it was found.
    set_process_msg_header_key(None).unwrap();
}

#[tokio::test]
async fn admitted_admin_mutation_replay_survives_restart() {
    let state_dir = TempPath::new("admin-replay-restart");
    let config = config(&state_dir, 4);
    let fingerprint = [0x5a; 32];
    let timestamp = unix_seconds();
    let (runtime, admin) = start(&config).await;
    runtime
        .claim_admin_mutation(&admin, fingerprint, timestamp)
        .await
        .unwrap();
    stop(runtime).await;

    let (restored, restored_admin) = start(&config).await;
    assert_eq!(
        restored
            .claim_admin_mutation(&restored_admin, fingerprint, timestamp)
            .await
            .unwrap_err()
            .code,
        "admin_request_replayed"
    );

    stop(restored).await;
}

#[tokio::test]
async fn snapshot_compaction_preserves_audit_records() {
    let state_dir = TempPath::new("audit-compaction");
    let config = config(&state_dir, 4);
    let (runtime, admin) = start(&config).await;
    runtime
        .issue(&admin, Duration::from_secs(60), Some("audited".to_string()))
        .await
        .unwrap();
    runtime.gc(&admin).await.unwrap();

    let instance_id = load_or_create_instance_id(&state_dir).unwrap();
    let persisted = try_load_persisted_state(&config, &TEST_ADMIN_KEY_BYTES, instance_id).unwrap();
    let actions = persisted
        .audit_records
        .iter()
        .map(|record| record.action.as_str())
        .collect::<Vec<_>>();
    assert!(actions.contains(&"temporary_key_issue"));
    assert!(actions.contains(&"temporary_key_gc"));

    stop(runtime).await;
}

#[tokio::test]
async fn revoking_keeps_the_row_until_its_retention_elapses() {
    let state_dir = TempPath::new("revoke-retention");
    let config = config(&state_dir, 4);
    let (runtime, admin) = start(&config).await;
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
    assert!(
        runtime
            .list(&admin, 0, 100)
            .await
            .unwrap()
            .items
            .iter()
            .any(|item| item.key_id == key_id && item.state == "revoked")
    );

    stop(runtime).await;
}

/// Retention counts from the relay's own acceptance time, falling back to the
/// client's timestamp only for records written before that was kept.
#[test]
fn replay_pruning_removes_only_records_outside_the_retention_window() {
    let now = 10_000;
    let retention = ADMIN_REPLAY_RETENTION.as_secs();
    let record = |fingerprint: u8, client_timestamp: u64, accepted_at: u64| AdminReplayRecord {
        fingerprint: [fingerprint; 32],
        client_timestamp,
        accepted_at,
    };
    let records = [
        record(1, now - retention - 1, now - retention - 1),
        record(2, now, now),
        // Backdated by the client, but accepted recently.
        record(3, now - retention - 1, now - 1),
        // Future-dated by the client, but accepted too long ago.
        record(4, now + retention / 2, now - retention - 1),
        // Legacy records, without an acceptance time.
        record(5, now - retention - 1, 0),
        record(6, now, 0),
    ];
    // Restored at time 0, nothing has left the window yet.
    let mut replays = AdminReplays::restored(&records, 0);
    assert_eq!(replays.records().len(), records.len());

    replays.prune(now);

    let kept = replays
        .records()
        .iter()
        .map(|record| record.fingerprint[0])
        .collect::<Vec<_>>();
    assert_eq!(kept, [2, 3, 6]);
    for fingerprint in 1..=6 {
        assert_eq!(
            replays.admitted(&[fingerprint; 32]),
            kept.contains(&fingerprint),
            "fingerprint {fingerprint}"
        );
    }
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
        namespace: None,
    };
    let revoked_without_audit = PersistedEntry {
        key_id: KeyId::new(Generation::from_u32(1), SlotIndex::from_index(1)),
        state: SlotState::Revoked,
        issued_at: 100,
        expires_at: 20_000,
        label: None,
        tombstoned_at: None,
        namespace: None,
    };
    let audit_at = now - 30;
    let mut snapshot = PersistedSnapshot {
        generations: vec![Generation::from_u32(1), Generation::from_u32(1)],
        entries: vec![revoked_with_audit, revoked_without_audit],
        legacy_protocol: LegacyProtocolPolicy::Deny,
        audit_records: VecDeque::from([AuditRecord {
            at: audit_at,
            action: "temporary_key_revoke".to_string(),
            key_id: Some(KeyId::new(
                Generation::from_u32(1),
                SlotIndex::from_index(0),
            )),
            label: None,
            namespace: None,
        }]),
        ..snapshot_for_test([1; INSTANCE_ID_LEN], 0, 0)
    };

    assert!(normalize_tombstone_times(&mut snapshot, now));
    assert_eq!(snapshot.entries[0].tombstoned_at, Some(audit_at));
    assert_eq!(snapshot.entries[1].tombstoned_at, Some(now));
    assert!(!normalize_tombstone_times(&mut snapshot, now + 1));
    assert_eq!(snapshot.entries[1].tombstoned_at, Some(now));
}
