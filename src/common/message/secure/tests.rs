//! End-to-end protocol-v2 framing and admission invariants.
//!
//! ```text
//! client session -> duplex transport -> ServerSecurity -> authenticated context
//! captured frame -- concurrent replay -----------------> exactly one admission
//! oversized first header ------------------------------> reject before body read
//! ```
//!
//! These tests intentionally exercise both administrator and derived temporary
//! credentials, while lifecycle persistence remains covered by `common::auth::tests`.

use super::*;
use crate::common::auth::{AuthConfig, LegacyProtocolPolicy, PROCESS_CREDENTIAL_TEST_LOCK};
use crate::common::checksum::{
    encode_temporary_credential, parse_credential, set_process_msg_header_key,
};

fn temp_config() -> AuthConfig {
    let mut random = [0_u8; 8];
    let mut rng = rand::rng();
    for byte in &mut random {
        *byte = rng.random();
    }
    AuthConfig {
        state_dir: std::env::temp_dir()
            .join(format!("pb-mapper-v2-{}", u64::from_be_bytes(random))),
        max_temporary_keys: 8,
        max_temporary_key_ttl: std::time::Duration::from_secs(3600),
        legacy_protocol: LegacyProtocolPolicy::Allow,
    }
}

#[tokio::test]
async fn v2_round_trip_uses_directional_counters() {
    let credential = Credential::Admin(*b"0123456789abcdefghijklmnopqrstuv");
    let client = ClientHeaderSession::new_v2(&credential).unwrap();
    let config = temp_config();
    let auth = AuthRuntime::start(*credential.key(), config.clone())
        .await
        .unwrap();
    let security = ServerSecurity::new(auth);
    let (mut client_io, mut server_io) = tokio::io::duplex(4096);

    let client_task = async {
        client
            .write_initial(&mut client_io, b"request")
            .await
            .unwrap();
        let mut reader = client.response_reader(&mut client_io).unwrap();
        assert_eq!(reader.read_msg().await.unwrap(), b"response");
    };
    let server_task = async {
        let initial = security.read_initial(&mut server_io).await.unwrap();
        assert_eq!(initial.payload, b"request");
        let mut writer = initial.session.response_writer(&mut server_io).unwrap();
        writer.write_msg(b"response").await.unwrap();
    };
    tokio::join!(client_task, server_task);
    let _ = std::fs::remove_dir_all(config.state_dir);
}

#[tokio::test]
async fn temporary_credential_authenticates_without_storing_secret() {
    let admin = *b"0123456789abcdefghijklmnopqrstuv";
    let config = temp_config();
    let auth = AuthRuntime::start(admin, config.clone()).await.unwrap();
    let admin_context = auth.authenticate_presented(0, &admin).unwrap();
    let issued = auth
        .issue(&admin_context, std::time::Duration::from_secs(60), None)
        .await
        .unwrap();
    let Credential::Temporary { key_id, key } =
        crate::common::checksum::parse_credential(&issued.credential).unwrap()
    else {
        panic!("expected temporary credential")
    };
    assert_eq!(issued.credential, encode_temporary_credential(key_id, &key));
    let client = ClientHeaderSession::new_v2(&Credential::Temporary { key_id, key }).unwrap();
    let security = ServerSecurity::new(auth);
    let (mut client_io, mut server_io) = tokio::io::duplex(4096);
    let client_task = client.write_initial(&mut client_io, b"temporary");
    let server_task = security.read_initial(&mut server_io);
    let (client_result, server_result) = tokio::join!(client_task, server_task);
    client_result.unwrap();
    let initial = server_result.unwrap();
    assert_eq!(initial.payload, b"temporary");
    assert_eq!(initial.session.context().unwrap().namespace, key_id);
    let _ = std::fs::remove_dir_all(config.state_dir);
}

async fn encode_initial(session: &ClientHeaderSession, payload: &[u8]) -> Vec<u8> {
    let (mut writer, mut reader) = tokio::io::duplex(128 * 1024);
    let write = async {
        session.write_initial(&mut writer, payload).await.unwrap();
        drop(writer);
    };
    let read = async {
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).await.unwrap();
        bytes
    };
    let (_, bytes) = tokio::join!(write, read);
    bytes
}

#[tokio::test]
async fn identical_initial_frames_are_admitted_only_once() {
    let credential = Credential::Admin(*b"0123456789abcdefghijklmnopqrstuv");
    let session = ClientHeaderSession::new_v2(&credential).unwrap();
    let bytes = encode_initial(&session, b"same-request").await;
    let config = temp_config();
    let auth = AuthRuntime::start(*credential.key(), config.clone())
        .await
        .unwrap();
    let security = ServerSecurity::new(auth);
    let mut first = std::io::Cursor::new(bytes.clone());
    let mut second = std::io::Cursor::new(bytes);

    let (first, second) = tokio::join!(
        security.read_initial(&mut first),
        security.read_initial(&mut second)
    );
    let results = [first, second];
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter_map(|result| result.as_ref().err())
            .next()
            .unwrap()
            .failure
            .code,
        "connection_salt_replayed"
    );

    let _ = std::fs::remove_dir_all(config.state_dir);
}

#[tokio::test]
async fn revoked_first_flights_do_not_consume_the_replay_filter() {
    let admin = *b"0123456789abcdefghijklmnopqrstuv";
    let config = temp_config();
    let auth = AuthRuntime::start(admin, config.clone()).await.unwrap();
    let admin_context = auth.authenticate_presented(0, &admin).unwrap();
    let issued = auth
        .issue(&admin_context, std::time::Duration::from_secs(60), None)
        .await
        .unwrap();
    let Credential::Temporary { key_id, key } = parse_credential(&issued.credential).unwrap()
    else {
        panic!("expected temporary credential");
    };
    let client = ClientHeaderSession::new_v2(&Credential::Temporary { key_id, key }).unwrap();
    let bytes = encode_initial(&client, b"revoked").await;
    auth.revoke(&admin_context, key_id).await.unwrap();
    let security = ServerSecurity::new(auth);

    let first = match security
        .read_initial(&mut std::io::Cursor::new(bytes.clone()))
        .await
    {
        Ok(_) => panic!("revoked credential should fail"),
        Err(error) => error,
    };
    let second = match security
        .read_initial(&mut std::io::Cursor::new(bytes))
        .await
    {
        Ok(_) => panic!("revoked credential should fail again"),
        Err(error) => error,
    };
    assert_eq!(first.failure.code, "temporary_key_revoked");
    assert_eq!(second.failure.code, "temporary_key_revoked");

    let _ = std::fs::remove_dir_all(config.state_dir);
}

#[tokio::test]
async fn rotated_temporary_first_flight_returns_a_readable_rotated_error() {
    let _process_credential_guard = PROCESS_CREDENTIAL_TEST_LOCK.lock().await;
    let admin = *b"0123456789abcdefghijklmnopqrstuv";
    let new_admin = *b"abcdefghijklmnopqrstuvwxyz012345";
    let config = temp_config();
    let auth = AuthRuntime::start(admin, config.clone()).await.unwrap();
    let admin_context = auth.authenticate_presented(0, &admin).unwrap();
    let issued = auth
        .issue(&admin_context, std::time::Duration::from_secs(60), None)
        .await
        .unwrap();
    let Credential::Temporary { key_id, key } = parse_credential(&issued.credential).unwrap()
    else {
        panic!("expected temporary credential");
    };
    let client = ClientHeaderSession::new_v2(&Credential::Temporary { key_id, key }).unwrap();
    auth.rotate_root(&admin_context, new_admin).await.unwrap();

    let security = ServerSecurity::new(auth);
    let (mut client_io, mut server_io) = tokio::io::duplex(4096);
    let client_task = async {
        client
            .write_initial(&mut client_io, b"stale-after-rotate")
            .await
            .unwrap();
        let mut reader = client.response_reader(&mut client_io).unwrap();
        reader.read_msg().await.unwrap().to_vec()
    };
    let server_task = async {
        let error = match security.read_initial(&mut server_io).await {
            Ok(_) => panic!("rotated credential should fail"),
            Err(error) => error,
        };
        assert_eq!(error.failure.code, "temporary_key_rotated");
        let session = error.response_session.expect("readable error session");
        let mut writer = session.response_writer(&mut server_io).unwrap();
        writer.write_msg(b"temporary_key_rotated").await.unwrap();
        error.failure.code
    };
    let (plaintext, code) = tokio::join!(client_task, server_task);
    assert_eq!(code, "temporary_key_rotated");
    assert_eq!(plaintext, b"temporary_key_rotated");

    let _ = std::fs::remove_dir_all(config.state_dir);
}

#[tokio::test]
async fn reset_temporary_first_flight_returns_a_readable_rotated_error() {
    let _process_credential_guard = PROCESS_CREDENTIAL_TEST_LOCK.lock().await;
    let admin = *b"0123456789abcdefghijklmnopqrstuv";
    let config = temp_config();
    let auth = AuthRuntime::start(admin, config.clone()).await.unwrap();
    let admin_context = auth.authenticate_presented(0, &admin).unwrap();
    let issued = auth
        .issue(&admin_context, std::time::Duration::from_secs(60), None)
        .await
        .unwrap();
    let Credential::Temporary { key_id, key } = parse_credential(&issued.credential).unwrap()
    else {
        panic!("expected temporary credential");
    };
    let client = ClientHeaderSession::new_v2(&Credential::Temporary { key_id, key }).unwrap();
    auth.reset(&admin_context).await.unwrap();

    let security = ServerSecurity::new(auth);
    let (mut client_io, mut server_io) = tokio::io::duplex(4096);
    let client_task = async {
        client
            .write_initial(&mut client_io, b"stale-after-reset")
            .await
            .unwrap();
        let mut reader = client.response_reader(&mut client_io).unwrap();
        reader.read_msg().await.unwrap().to_vec()
    };
    let server_task = async {
        let error = match security.read_initial(&mut server_io).await {
            Ok(_) => panic!("reset credential should fail"),
            Err(error) => error,
        };
        assert_eq!(error.failure.code, "temporary_key_rotated");
        let session = error.response_session.expect("readable error session");
        let mut writer = session.response_writer(&mut server_io).unwrap();
        writer.write_msg(b"temporary_key_rotated").await.unwrap();
        error.failure.code
    };
    let (plaintext, code) = tokio::join!(client_task, server_task);
    assert_eq!(code, "temporary_key_rotated");
    assert_eq!(plaintext, b"temporary_key_rotated");

    let _ = std::fs::remove_dir_all(config.state_dir);
}

#[tokio::test]
async fn oversized_initial_frame_is_rejected_before_reading_its_body() {
    let credential = Credential::Admin(*b"0123456789abcdefghijklmnopqrstuv");
    let session = ClientHeaderSession::new_v2(&credential).unwrap();
    let material = session.v2.as_ref().unwrap();
    let mut bytes = first_prefix(material);
    bytes.extend_from_slice(&0_u64.to_be_bytes());
    bytes.extend_from_slice(&(MAX_INITIAL_PLAINTEXT_LEN + 17).to_be_bytes());
    let config = temp_config();
    let auth = AuthRuntime::start(*credential.key(), config.clone())
        .await
        .unwrap();
    let security = ServerSecurity::new(auth);
    let mut input = std::io::Cursor::new(bytes);

    let error = match security.read_initial(&mut input).await {
        Ok(_) => panic!("oversized initial frame was accepted"),
        Err(error) => error,
    };
    assert_eq!(error.failure.code, "protocol_v2_decrypt_failed");
    assert!(error.failure.message.contains("65536-byte limit"));

    let _ = std::fs::remove_dir_all(config.state_dir);
}

#[tokio::test]
async fn oversized_legacy_initial_frame_is_rejected_before_reading_its_body() {
    use crate::common::checksum::get_checksum_for_key;

    let admin = *b"0123456789abcdefghijklmnopqrstuv";
    let config = temp_config();
    let auth = AuthRuntime::start(admin, config.clone()).await.unwrap();
    let datalen = MAX_INITIAL_CIPHERTEXT_LEN + 1;
    let checksum = get_checksum_for_key(datalen, &admin);
    let mut bytes = checksum.to_be_bytes().to_vec();
    bytes.extend_from_slice(&datalen.to_be_bytes());
    let security = ServerSecurity::new(auth);
    let error = match security
        .read_initial(&mut std::io::Cursor::new(bytes))
        .await
    {
        Ok(_) => panic!("oversized legacy frame was accepted"),
        Err(error) => error,
    };
    assert_eq!(error.failure.code, "legacy_frame_invalid");

    let _ = std::fs::remove_dir_all(config.state_dir);
}

#[test]
fn rotating_bloom_covers_current_and_previous_window() {
    let mut bloom = RotatingBloom::new(1024, DEFAULT_REPLAY_WINDOW_SECONDS);
    let value = [7_u8; 32];
    let start = bloom.current_started_at;
    assert!(!bloom.contains(&value, start));
    bloom.insert(&value, start);
    assert!(bloom.contains(&value, start + DEFAULT_REPLAY_WINDOW_SECONDS));
    assert!(!bloom.contains(
        &value,
        start + DEFAULT_REPLAY_WINDOW_SECONDS.saturating_mul(2) + 1
    ));
}

#[test]
fn rotating_bloom_retains_fingerprints_for_the_clock_skew_window() {
    let mut bloom = RotatingBloom::new(1024, DEFAULT_REPLAY_WINDOW_SECONDS);
    let value = [9_u8; 32];
    let start = bloom.current_started_at;
    bloom.insert(&value, start);
    assert!(bloom.contains(&value, start + 120));
    assert!(bloom.contains(&value, start + MAX_CONNECTION_CLOCK_SKEW_SECONDS));
}

#[test]
fn rotating_bloom_retains_a_max_future_timestamp_past_the_next_rotation() {
    let mut bloom = RotatingBloom::new(1024, DEFAULT_REPLAY_WINDOW_SECONDS);
    let value = [11_u8; 32];
    let start = bloom.current_started_at;
    let insert_at = start + DEFAULT_REPLAY_WINDOW_SECONDS - 1;
    bloom.insert(&value, insert_at);
    assert!(bloom.contains(
        &value,
        insert_at + MAX_CONNECTION_CLOCK_SKEW_SECONDS.saturating_mul(2) - 1
    ));
}

#[test]
fn per_credential_admission_limit_does_not_consume_other_keys() {
    let now = unix_seconds();
    let mut guard =
        ReplayGuard::open(None, 1024, DEFAULT_REPLAY_WINDOW_SECONDS).with_max_per_key(2);
    assert_eq!(guard.admit(1, &[1_u8; 32], now), FirstFlightAdmit::Fresh);
    assert_eq!(guard.admit(1, &[2_u8; 32], now), FirstFlightAdmit::Fresh);
    assert_eq!(guard.admit(1, &[3_u8; 32], now), FirstFlightAdmit::Limited);
    assert_eq!(guard.admit(2, &[3_u8; 32], now), FirstFlightAdmit::Fresh);
    assert_eq!(guard.admit(1, &[1_u8; 32], now), FirstFlightAdmit::Replayed);
}

#[test]
fn persisted_first_flights_survive_a_torn_trailing_record() {
    let mut random = [0_u8; 8];
    let mut rng = rand::rng();
    for byte in &mut random {
        *byte = rng.random();
    }
    let path = std::env::temp_dir().join(format!(
        "pb-mapper-replay-torn-{}",
        u64::from_be_bytes(random)
    ));
    let now = unix_seconds();
    let fingerprint = [17_u8; 32];
    {
        let mut guard = ReplayGuard::open(Some(path.clone()), 1024, DEFAULT_REPLAY_WINDOW_SECONDS);
        assert_eq!(guard.admit(7, &fingerprint, now), FirstFlightAdmit::Fresh);
    }
    let mut torn = std::fs::read(&path).unwrap();
    torn.extend_from_slice(&[0_u8; 10]);
    std::fs::write(&path, torn).unwrap();
    let mut restored = ReplayGuard::open(Some(path.clone()), 1024, DEFAULT_REPLAY_WINDOW_SECONDS);
    assert_eq!(
        restored.admit(7, &fingerprint, now),
        FirstFlightAdmit::Replayed
    );
    let _ = std::fs::remove_file(path);
}

#[test]
fn replay_rewrite_succeeds_when_a_pid_temporary_file_already_exists() {
    let mut random = [0_u8; 8];
    let mut rng = rand::rng();
    for byte in &mut random {
        *byte = rng.random();
    }
    let path = std::env::temp_dir().join(format!(
        "pb-mapper-replay-tmp-{}",
        u64::from_be_bytes(random)
    ));
    let now = unix_seconds();
    let fingerprint = [19_u8; 32];
    {
        let mut guard = ReplayGuard::open(Some(path.clone()), 1024, DEFAULT_REPLAY_WINDOW_SECONDS);
        assert_eq!(guard.admit(9, &fingerprint, now), FirstFlightAdmit::Fresh);
    }
    let leftover = path.with_file_name(format!(
        ".{}.tmp-{}",
        path.file_name().unwrap().to_str().unwrap(),
        std::process::id()
    ));
    std::fs::write(&leftover, b"stale").unwrap();
    let mut restored = ReplayGuard::open(Some(path.clone()), 1024, DEFAULT_REPLAY_WINDOW_SECONDS);
    assert_eq!(
        restored.admit(9, &fingerprint, now),
        FirstFlightAdmit::Replayed
    );
    let _ = std::fs::remove_file(leftover);
    let _ = std::fs::remove_file(path);
}

#[test]
fn persisted_first_flights_survive_replay_guard_restart() {
    let mut random = [0_u8; 8];
    let mut rng = rand::rng();
    for byte in &mut random {
        *byte = rng.random();
    }
    let path =
        std::env::temp_dir().join(format!("pb-mapper-replay-{}", u64::from_be_bytes(random)));
    let now = unix_seconds();
    let fingerprint = [13_u8; 32];
    {
        let mut guard = ReplayGuard::open(Some(path.clone()), 1024, DEFAULT_REPLAY_WINDOW_SECONDS);
        assert_eq!(guard.admit(7, &fingerprint, now), FirstFlightAdmit::Fresh);
    }
    let mut restored = ReplayGuard::open(Some(path.clone()), 1024, DEFAULT_REPLAY_WINDOW_SECONDS);
    assert_eq!(
        restored.admit(7, &fingerprint, now),
        FirstFlightAdmit::Replayed
    );
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn legacy_initial_frame_validates_against_isolated_relay_key() {
    let _process_credential_guard = PROCESS_CREDENTIAL_TEST_LOCK.lock().await;
    let config = temp_config();
    let isolated_admin = *b"isolated-admin-key-0123456789abc";
    std::fs::create_dir_all(&config.state_dir).unwrap();
    std::fs::write(config.state_dir.join("admin.key"), isolated_admin).unwrap();
    let auth = AuthRuntime::from_isolated_state(config.clone())
        .await
        .unwrap();

    let temporary_key_id = 1;
    let temporary_key = *b"temporary-remote-key-0123456789a";
    set_process_msg_header_key(Some(&encode_temporary_credential(
        temporary_key_id,
        &temporary_key,
    )))
    .unwrap();

    let security = ServerSecurity::new(auth);
    let (mut client_io, mut server_io) = tokio::io::duplex(4096);
    let client = ClientHeaderSession::new_legacy(isolated_admin);
    let client_task = async {
        client
            .write_initial(&mut client_io, b"legacy-isolated")
            .await
            .unwrap();
        let mut reader = client.response_reader(&mut client_io).unwrap();
        reader.read_msg().await.unwrap().to_vec()
    };
    let server_task = async {
        let initial = security.read_initial(&mut server_io).await.unwrap();
        assert_eq!(initial.payload, b"legacy-isolated");
        assert_eq!(initial.session.protocol(), HeaderProtocol::Legacy);
        let mut writer = initial.session.response_writer(&mut server_io).unwrap();
        writer.write_msg(b"legacy-response").await.unwrap();
    };
    let (response, _) = tokio::join!(client_task, server_task);
    assert_eq!(response, b"legacy-response");

    set_process_msg_header_key(None).unwrap();
    let _ = std::fs::remove_dir_all(config.state_dir);
}

#[test]
fn failure_log_limiter_has_a_hard_cardinality_bound() {
    let mut limiter = FailureLogLimiter::default();
    let peer = "127.0.0.1".parse().unwrap();
    for key_id in 0..10_000 {
        limiter.record(peer, key_id, "invalid", 1_000);
    }
    assert_eq!(limiter.entries.len(), 4096);
    assert!(limiter.overflow.is_some());
}
