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
    let admin = runtime.authenticate(0).unwrap();
    let issued = runtime
        .issue(&admin, Duration::from_secs(60), Some("demo".to_string()))
        .await
        .unwrap();
    assert!(issued.credential.starts_with("pbmt1_"));
    let context = runtime.authenticate(issued.metadata.key_id).unwrap();
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
        runtime
            .authenticate(issued.metadata.key_id)
            .unwrap_err()
            .code,
        "temporary_key_revoked"
    );
    drop(runtime);

    tokio::time::sleep(Duration::from_millis(20)).await;
    let restored = AuthRuntime::start(admin_key, config).await.unwrap();
    assert_eq!(
        restored
            .authenticate(issued.metadata.key_id)
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
    let admin = runtime.authenticate(0).unwrap();
    let before = runtime.status(&admin).await.unwrap().server_instance_id;
    let old = runtime
        .issue(
            &admin,
            Duration::from_secs(60),
            Some("before-reset".to_string()),
        )
        .await
        .unwrap();
    let old_context = runtime.authenticate(old.metadata.key_id).unwrap();
    let old_cancellation = old_context.cancellation_token().unwrap();

    runtime.reset(&admin).await.unwrap();

    let after = runtime.status(&admin).await.unwrap().server_instance_id;
    assert_ne!(after, before);
    assert!(old_cancellation.is_cancelled());
    assert!(runtime.authenticate(old.metadata.key_id).is_err());
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
    let admin = runtime.authenticate(0).unwrap();
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
    let recovered_admin = recovered.authenticate(0).unwrap();
    assert!(recovered.status(&recovered_admin).await.unwrap().safe_mode);
    assert_eq!(
        recovered
            .authenticate(issued.metadata.key_id)
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
    let admin = runtime.authenticate(0).unwrap();
    runtime
        .claim_admin_mutation(&admin, fingerprint, timestamp)
        .await
        .unwrap();
    drop(runtime);
    tokio::time::sleep(Duration::from_millis(20)).await;

    let restored = AuthRuntime::start(admin_key, config).await.unwrap();
    let restored_admin = restored.authenticate(0).unwrap();
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
    let admin = runtime.authenticate(0).unwrap();
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
