//! Allocated namespaces, the keys issued into them, and shortening a key's life.

use super::*;

const ADMIN_KEY: [u8; 32] = *b"0123456789abcdefghijklmnopqrstuv";
const HOUR: Duration = Duration::from_secs(3600);

fn config_for(state_dir: &Path) -> AuthConfig {
    AuthConfig {
        state_dir: state_dir.to_path_buf(),
        max_temporary_keys: 8,
        max_temporary_key_ttl: HOUR,
        legacy_protocol: LegacyProtocolPolicy::Allow,
    }
}

async fn start(state_dir: &Path) -> (AuthRuntime, AuthContext) {
    let runtime = AuthRuntime::start(ADMIN_KEY, config_for(state_dir))
        .await
        .unwrap();
    let admin = authenticate_for_test(&runtime, ADMIN_KEY_ID).unwrap();
    (runtime, admin)
}

/// Stop a runtime and bring the same state directory back up.
async fn restart(runtime: AuthRuntime, state_dir: &Path) -> (AuthRuntime, AuthContext) {
    runtime.shutdown_actor().await;
    drop(runtime);
    start(state_dir).await
}

#[tokio::test]
async fn keys_in_a_namespace_share_it_and_are_revoked_one_at_a_time() {
    let state_dir = temp_state_dir("namespace-shared");
    let (runtime, admin) = start(&state_dir).await;
    let namespace = runtime.allocate_namespace(&admin).await.unwrap();
    let first = runtime
        .issue_in_namespace(&admin, namespace, HOUR, Some("first".into()))
        .await
        .unwrap();
    let second = runtime
        .issue_in_namespace(&admin, namespace, HOUR, Some("second".into()))
        .await
        .unwrap();
    assert_ne!(first.metadata.key_id, second.metadata.key_id);
    assert_eq!(first.metadata.effective_namespace(), namespace);
    assert_eq!(second.metadata.effective_namespace(), namespace);

    let first_context = authenticate_for_test(&runtime, first.metadata.key_id).unwrap();
    let second_context = authenticate_for_test(&runtime, second.metadata.key_id).unwrap();
    assert_eq!(first_context.namespace, namespace);
    assert_eq!(second_context.namespace, namespace);

    runtime.revoke(&admin, first.metadata.key_id).await.unwrap();
    assert_eq!(
        first_context.ensure_active().unwrap_err().code,
        "temporary_key_revoked"
    );
    second_context.ensure_active().unwrap();
    assert_eq!(
        authenticate_for_test(&runtime, second.metadata.key_id)
            .unwrap()
            .namespace,
        namespace
    );

    // A namespace outlives its keys: a later key lands in the same one.
    let third = runtime
        .issue_in_namespace(&admin, namespace, HOUR, None)
        .await
        .unwrap();
    assert_eq!(third.metadata.effective_namespace(), namespace);
    let _ = std::fs::remove_dir_all(state_dir);
}

#[tokio::test]
async fn a_key_keeps_its_namespace_through_wal_replay_compaction_and_lease_replacement() {
    let state_dir = temp_state_dir("namespace-durable");
    let (runtime, admin) = start(&state_dir).await;
    let namespace = runtime.allocate_namespace(&admin).await.unwrap();
    let issued = runtime
        .issue_in_namespace(&admin, namespace, HOUR, None)
        .await
        .unwrap();
    let key_id = issued.metadata.key_id;

    // Nothing compacted yet, so this reads the key back from the WAL.
    let (runtime, admin) = restart(runtime, &state_dir).await;
    assert_eq!(
        authenticate_for_test(&runtime, key_id).unwrap().namespace,
        namespace
    );

    // A collection compacts into a snapshot and truncates the WAL.
    runtime.gc(&admin).await.unwrap();
    let persisted = try_load_persisted_state(
        &config_for(&state_dir),
        &ADMIN_KEY,
        load_or_create_instance_id(&state_dir).unwrap(),
    )
    .unwrap();
    assert_eq!(
        persisted
            .entries
            .iter()
            .find(|entry| entry.key_id == key_id)
            .unwrap()
            .namespace,
        Some(namespace)
    );
    let (runtime, admin) = restart(runtime, &state_dir).await;
    let context = authenticate_for_test(&runtime, key_id).unwrap();
    assert_eq!(context.namespace, namespace);

    // A renewal that has to replace a cancelled lease must not fall back to the
    // key's own namespace.
    context.ensure_active().unwrap().cancel_expired();
    let renewed = runtime.renew(&admin, key_id, HOUR).await.unwrap();
    assert_eq!(renewed.metadata.effective_namespace(), namespace);
    assert_eq!(
        authenticate_for_test(&runtime, key_id).unwrap().namespace,
        namespace
    );
    let _ = std::fs::remove_dir_all(state_dir);
}

/// Allocation outlives everything that replaces the encrypted state, including a
/// reset out of a damaged snapshot that could not say what had been handed out.
#[tokio::test]
async fn an_allocated_namespace_is_never_handed_out_again() {
    let _process_credential_guard = PROCESS_CREDENTIAL_TEST_LOCK.lock().await;
    let state_dir = temp_state_dir("namespace-never-reused");
    let (runtime, admin) = start(&state_dir).await;
    let mut allocated = vec![runtime.allocate_namespace(&admin).await.unwrap()];

    let (runtime, admin) = restart(runtime, &state_dir).await;
    allocated.push(runtime.allocate_namespace(&admin).await.unwrap());

    runtime.gc(&admin).await.unwrap();
    allocated.push(runtime.allocate_namespace(&admin).await.unwrap());

    runtime.reset(&admin).await.unwrap();
    allocated.push(runtime.allocate_namespace(&admin).await.unwrap());

    let rotated_key = *b"abcdefghijklmnopqrstuvwxyz012345";
    runtime.rotate_root(&admin, rotated_key).await.unwrap();
    let rotated_admin = runtime
        .authenticate_presented(ADMIN_KEY_ID, &rotated_key)
        .unwrap();
    allocated.push(runtime.allocate_namespace(&rotated_admin).await.unwrap());
    runtime.shutdown_actor().await;
    drop(runtime);

    std::fs::write(state_dir.join("auth.snapshot"), b"damaged").unwrap();
    let runtime = AuthRuntime::start(rotated_key, config_for(&state_dir))
        .await
        .unwrap();
    let admin = runtime
        .authenticate_presented(ADMIN_KEY_ID, &rotated_key)
        .unwrap();
    assert!(runtime.status(&admin).await.unwrap().safe_mode);
    assert_eq!(
        runtime.allocate_namespace(&admin).await.unwrap_err().code,
        "temporary_key_store_unavailable"
    );
    runtime.reset(&admin).await.unwrap();
    allocated.push(runtime.allocate_namespace(&admin).await.unwrap());

    assert_eq!(allocated, [1, 2, 3, 4, 5, 6]);
    // Generation 0 is what keeps these apart from every key's own namespace.
    let issued = runtime.issue(&admin, HOUR, None).await.unwrap();
    assert_ne!(issued.metadata.key_id.generation(), Generation::FIRST);
    for namespace in allocated {
        assert_eq!(
            KeyId::from_u64(namespace).generation(),
            Generation::FIRST,
            "namespace {namespace} must sit in the generation no key uses"
        );
    }
    set_process_msg_header_key(None).unwrap();
    let _ = std::fs::remove_dir_all(state_dir);
}

#[tokio::test]
async fn only_the_administrator_issues_and_only_into_an_allocated_namespace() {
    let state_dir = temp_state_dir("namespace-refusals");
    let (runtime, admin) = start(&state_dir).await;
    let own = runtime.issue(&admin, HOUR, None).await.unwrap();
    let allocated = runtime.allocate_namespace(&admin).await.unwrap();

    for refused in [0, allocated + 1, own.metadata.key_id.as_u64()] {
        assert_eq!(
            runtime
                .issue_in_namespace(&admin, refused, HOUR, None)
                .await
                .unwrap_err()
                .code,
            "namespace_not_allocated",
            "namespace {refused} must be refused"
        );
    }

    let tenant = authenticate_for_test(&runtime, own.metadata.key_id).unwrap();
    assert_eq!(
        runtime.allocate_namespace(&tenant).await.unwrap_err().code,
        "admin_permission_required"
    );
    assert_eq!(
        runtime
            .issue_in_namespace(&tenant, allocated, HOUR, None)
            .await
            .unwrap_err()
            .code,
        "admin_permission_required"
    );
    let _ = std::fs::remove_dir_all(state_dir);
}

/// A damaged allocator stops namespace work and nothing else: guessing could
/// repeat a number, and existing keys carry their namespace elsewhere.
#[tokio::test]
async fn an_unreadable_allocator_refuses_namespaces_and_leaves_keys_working() {
    let state_dir = temp_state_dir("namespace-allocator-damaged");
    let (runtime, admin) = start(&state_dir).await;
    let namespace = runtime.allocate_namespace(&admin).await.unwrap();
    let issued = runtime
        .issue_in_namespace(&admin, namespace, HOUR, None)
        .await
        .unwrap();

    std::fs::write(state_dir.join("namespace-allocator"), b"not a count").unwrap();
    assert_eq!(
        runtime.allocate_namespace(&admin).await.unwrap_err().code,
        "namespace_allocator_unavailable"
    );
    assert_eq!(
        runtime
            .issue_in_namespace(&admin, namespace, HOUR, None)
            .await
            .unwrap_err()
            .code,
        "namespace_allocator_unavailable"
    );
    assert_eq!(
        authenticate_for_test(&runtime, issued.metadata.key_id)
            .unwrap()
            .namespace,
        namespace
    );
    runtime
        .renew(&admin, issued.metadata.key_id, HOUR)
        .await
        .unwrap();
    runtime.issue(&admin, HOUR, None).await.unwrap();
    let _ = std::fs::remove_dir_all(state_dir);
}

/// State written before namespaces existed, exactly as an older relay wrote it.
#[tokio::test]
async fn keys_from_an_older_relay_stay_in_their_key_id_namespace() {
    let state_dir = temp_state_dir("namespace-legacy");
    std::fs::create_dir_all(&state_dir).unwrap();
    let config = config_for(&state_dir);
    let instance_id = load_or_create_instance_id(&state_dir).unwrap();
    let now = unix_seconds();
    let expires_at = now + 600;

    let in_snapshot = KeyId::new(Generation::from_u32(4), SlotIndex::from_index(0));
    let in_wal = KeyId::new(Generation::from_u32(1), SlotIndex::from_index(1));
    let old_entry = |key_id: KeyId| {
        format!(
            r#"{{"key_id":{key_id},"state":"active","issued_at":{now},"expires_at":{expires_at},"label":"old"}}"#
        )
    };
    let snapshot = format!(
        r#"{{"schema_version":1,"instance_id":{instance_id:?},"generations":[4,0,0,0,0,0,0,0],"entries":[{}],"legacy_protocol":"allow"}}"#,
        old_entry(in_snapshot)
    );
    atomic_write(
        &auth_snapshot_path(&state_dir),
        &seal_blob(&ADMIN_KEY, snapshot.as_bytes()).unwrap(),
        0o600,
    )
    .unwrap();
    let wal = format!(
        r#"{{"Mutation":{{"mutation":{{"Issue":{}}},"audit":{{"at":{now},"action":"temporary_key_issue","key_id":{in_wal},"label":"old"}}}}}}"#,
        old_entry(in_wal)
    );
    append_wal(
        &config,
        &ADMIN_KEY,
        &serde_json::from_str::<WalRecord>(&wal).unwrap(),
    )
    .unwrap();

    let (runtime, admin) = start(&state_dir).await;
    assert!(!runtime.status(&admin).await.unwrap().safe_mode);
    for key_id in [in_snapshot, in_wal] {
        let context = authenticate_for_test(&runtime, key_id).unwrap();
        assert_eq!(context.namespace, key_id.as_u64());
        let shown = runtime.show(&admin, key_id, false).await.unwrap();
        assert_eq!(shown.metadata.effective_namespace(), key_id.as_u64());
    }

    // A key issued without a namespace still writes the old form, so a relay
    // rolled back to the previous version reads it without noticing.
    let fresh = runtime.issue(&admin, HOUR, None).await.unwrap();
    assert_eq!(
        fresh.metadata.effective_namespace(),
        fresh.metadata.key_id.as_u64()
    );
    assert!(fresh.metadata.namespace.is_none());
    runtime.gc(&admin).await.unwrap();
    let persisted = try_load_persisted_state(&config, &ADMIN_KEY, instance_id).unwrap();
    let entry = persisted
        .entries
        .iter()
        .find(|entry| entry.key_id == fresh.metadata.key_id)
        .unwrap();
    assert!(!serde_json::to_string(entry).unwrap().contains("namespace"));
    let _ = std::fs::remove_dir_all(state_dir);
}

/// What a client reads to tell whether a relay can do this at all, and how an
/// older relay's answers decode.
#[tokio::test]
async fn the_capability_is_reported_and_an_older_relay_reports_none() {
    let state_dir = temp_state_dir("namespace-capability");
    let (runtime, admin) = start(&state_dir).await;
    let status = runtime.status(&admin).await.unwrap();
    assert!(
        status
            .capabilities
            .iter()
            .any(|capability| capability == CAPABILITY_KEY_NAMESPACES)
    );

    let mut older = serde_json::to_value(&status).unwrap();
    older.as_object_mut().unwrap().remove("capabilities");
    let older: AuthStatus = serde_json::from_value(older).unwrap();
    assert!(older.capabilities.is_empty());

    let older: TemporaryKeyMetadata = serde_json::from_str(
        r#"{"key_id":4294967296,"state":"active","issued_at":1,"expires_at":2,"label":null}"#,
    )
    .unwrap();
    assert_eq!(older.effective_namespace(), 4_294_967_296);
    let _ = std::fs::remove_dir_all(state_dir);
}

/// `renew` is how a retiring key gets its deadline. Until that deadline it stays
/// usable with the same text; at it, the key ends the way any expired key does.
#[tokio::test]
async fn renewing_shorter_ends_the_key_at_the_new_deadline() {
    let state_dir = temp_state_dir("renew-shorter");
    let (runtime, admin) = start(&state_dir).await;
    let issued = runtime.issue(&admin, HOUR, None).await.unwrap();
    let key_id = issued.metadata.key_id;
    let context = authenticate_for_test(&runtime, key_id).unwrap();
    let cancellation = context.cancellation_token().unwrap();

    let shortened = runtime
        .renew(&admin, key_id, MIN_TEMP_KEY_TTL)
        .await
        .unwrap();
    assert!(shortened.metadata.expires_at < issued.metadata.expires_at);
    assert_eq!(shortened.credential, issued.credential);
    context.ensure_active().unwrap();
    authenticate_for_test(&runtime, key_id).unwrap();

    tokio::time::timeout(
        MIN_TEMP_KEY_TTL + Duration::from_secs(10),
        cancellation.cancelled(),
    )
    .await
    .expect("work under a shortened key must be cancelled at the new deadline");
    assert_eq!(
        authenticate_for_test(&runtime, key_id).unwrap_err().code,
        "temporary_key_expired"
    );
    let shown = runtime.show(&admin, key_id, false).await.unwrap();
    assert_eq!(shown.metadata.state, "expired");
    let _ = std::fs::remove_dir_all(state_dir);
}

#[tokio::test]
async fn a_shortened_deadline_holds_across_a_restart() {
    let state_dir = temp_state_dir("renew-shorter-restart");
    let (runtime, admin) = start(&state_dir).await;
    let issued = runtime.issue(&admin, HOUR, None).await.unwrap();
    let key_id = issued.metadata.key_id;
    let shortened = runtime
        .renew(&admin, key_id, MIN_TEMP_KEY_TTL)
        .await
        .unwrap();

    let (runtime, admin) = restart(runtime, &state_dir).await;
    let revealed = runtime.show(&admin, key_id, true).await.unwrap();
    assert_eq!(revealed.metadata.expires_at, shortened.metadata.expires_at);
    assert_eq!(revealed.credential, issued.credential);

    let context = authenticate_for_test(&runtime, key_id).unwrap();
    let cancellation = context.cancellation_token().unwrap();
    tokio::time::timeout(
        MIN_TEMP_KEY_TTL + Duration::from_secs(10),
        cancellation.cancelled(),
    )
    .await
    .expect("a restart must not bring the longer deadline back");
    assert_eq!(
        authenticate_for_test(&runtime, key_id).unwrap_err().code,
        "temporary_key_expired"
    );
    let _ = std::fs::remove_dir_all(state_dir);
}

/// The relay's floor on a TTL is what the design turns into a revoke.
#[tokio::test]
async fn a_shortening_below_the_minimum_is_refused_and_changes_nothing() {
    let state_dir = temp_state_dir("renew-below-minimum");
    let (runtime, admin) = start(&state_dir).await;
    let issued = runtime.issue(&admin, HOUR, None).await.unwrap();
    let key_id = issued.metadata.key_id;
    assert_eq!(
        runtime
            .renew(&admin, key_id, MIN_TEMP_KEY_TTL - Duration::from_secs(1))
            .await
            .unwrap_err()
            .code,
        "temporary_key_ttl_too_short"
    );
    let shown = runtime.show(&admin, key_id, false).await.unwrap();
    assert_eq!(shown.metadata.expires_at, issued.metadata.expires_at);
    let _ = std::fs::remove_dir_all(state_dir);
}
