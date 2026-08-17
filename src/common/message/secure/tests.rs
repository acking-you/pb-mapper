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
use crate::common::auth::{AuthConfig, LegacyProtocolPolicy};
use crate::common::checksum::encode_temporary_credential;

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

#[test]
fn rotating_bloom_covers_current_and_previous_window() {
    let mut bloom = RotatingBloom::new(1024, 60);
    let value = [7_u8; 32];
    let start = bloom.current_started_at;
    assert!(!bloom.contains(&value, start));
    bloom.insert(&value, start);
    assert!(bloom.contains(&value, start + 60));
    assert!(!bloom.contains(&value, start + 121));
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
