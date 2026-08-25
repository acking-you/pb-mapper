// An integration test: a failed `unwrap` is a failed test, which is the report
// this file exists to produce. `allow-unwrap-in-tests` covers `#[cfg(test)]`
// modules but not a `tests/` target, whose whole body is test code.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pb_mapper_auth::{
    ADMIN_KEY_ID, AuthConfig, AuthRuntime, LegacyProtocolPolicy, write_admin_key_file,
};
use pb_mapper_client::client::run_client_side_cli_with_callback;
use pb_mapper_client::server::{ServerTunnelOptions, run_server_side_cli_with_callback};
use pb_mapper_core::checksum::{Credential, parse_credential, set_process_msg_header_key};
use pb_mapper_protocol::command::{
    AdminRequest, AdminResponse, LocalServer, MessageSerializer, PbConnRequest, PbConnResponse,
    PbConnStatusReq, PbConnStatusResp, PbServerRequest, PbServiceConnStatus,
};
use pb_mapper_protocol::secure::{ClientHeaderSession, ServerHeaderSession, ServerSecurity};
use pb_mapper_protocol::{
    MessageReader, MessageWriter, get_header_msg_reader, get_header_msg_writer,
};
use pb_mapper_server::run_server_with_auth_config;
// Only the hand-rolled v2 registration helper: this file sets the process
// credential itself, so it must not touch anything in the testkit that runs
// `init_test_env` — `Relay` and `Tunnel` — which would write it a second time.
use pb_mapper_testkit::{V2ControlSpec, register_v2_control};
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use uni_stream::stream::{TcpListenerProvider, TcpStreamProvider};

/// Serialises the tests that override process-global configuration.
///
/// The environment is one namespace shared by every test in this binary, and
/// several of these tests override the *same* variable —
/// `PB_MAPPER_CONTROL_CONN_POOL_SIZE`, so that one control worker serves the
/// registration and its retries are countable. Run concurrently, one test's
/// guard restores the variable while another test's tunnel is still reading it,
/// and that tunnel silently gets the default two-worker pool: two register
/// attempts where the test asserts one.
static ENV_OVERRIDE_LOCK: std::sync::LazyLock<tokio::sync::Mutex<()>> =
    std::sync::LazyLock::new(|| tokio::sync::Mutex::new(()));

/// A set of environment overrides held for the duration of one test.
///
/// Restores every previous value on drop, and releases the lock only then, so no
/// two overriding tests overlap.
struct EnvOverrides {
    _lock: tokio::sync::MutexGuard<'static, ()>,
    vars: Vec<EnvVarGuard>,
}

impl EnvOverrides {
    async fn set(overrides: &[(&'static str, &'static str)]) -> Self {
        let lock = ENV_OVERRIDE_LOCK.lock().await;
        let vars = overrides
            .iter()
            .map(|(key, value)| EnvVarGuard::set(key, value))
            .collect();
        Self { _lock: lock, vars }
    }
}

impl Drop for EnvOverrides {
    fn drop(&mut self) {
        // Explicit, so the ordering against the lock guard is stated rather than
        // left to field order: every variable is restored before the next test
        // that overrides one can start.
        self.vars.clear();
    }
}

struct EnvVarGuard {
    key: &'static str,
    old_value: Option<String>,
}

impl EnvVarGuard {
    /// # Safety note
    ///
    /// Mutating the environment is unsafe in edition 2024 because it races
    /// concurrent readers. Callers reach this through [`EnvOverrides`], which
    /// holds [`ENV_OVERRIDE_LOCK`] for as long as the values are in place, and
    /// the guard restores the previous value on drop.
    fn set(key: &'static str, value: &'static str) -> Self {
        let old_value = std::env::var(key).ok();
        unsafe { std::env::set_var(key, value) };
        Self { key, old_value }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        // SAFETY: as in `set`.
        unsafe {
            if let Some(value) = self.old_value.take() {
                std::env::set_var(self.key, value);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }
}

async fn wait_for_server(server_addr: SocketAddr) -> TcpStream {
    timeout(Duration::from_secs(2), async {
        loop {
            match TcpStream::connect(server_addr).await {
                Ok(stream) => break stream,
                Err(_) => tokio::time::sleep(Duration::from_millis(10)).await,
            }
        }
    })
    .await
    .expect("server did not start")
}

#[tokio::test]
async fn explicit_invalid_msg_header_key_fails_server_startup() {
    let state_dir =
        std::env::temp_dir().join(format!("pb-mapper-invalid-env-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&state_dir);
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_pb-mapper"));
    command
        .arg("server")
        .arg("--port")
        .arg("0")
        .arg("--auth-state-dir")
        .arg(&state_dir)
        .env("MSG_HEADER_KEY", "invalid")
        .kill_on_drop(true);

    let output = timeout(Duration::from_secs(3), command.output())
        .await
        .expect("invalid explicit key must fail instead of starting the server")
        .unwrap();
    assert!(!output.status.success());
    let logs = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(logs.contains("administrator_key_invalid"), "logs: {logs}");
    assert!(!state_dir.join("admin.key").exists());
    let _ = std::fs::remove_dir_all(state_dir);
}

#[tokio::test]
async fn admin_all_preserves_json_output_mode() {
    let probe_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let server_addr = probe_listener.local_addr().unwrap();
    drop(probe_listener);
    let config = auth_config(server_addr);
    let _ = std::fs::remove_dir_all(&config.state_dir);
    write_admin_key_file(&config.state_dir.join("admin.key"), TEST_ADMIN_KEY, true).unwrap();
    let runtime = AuthRuntime::start(
        *TEST_ADMIN_KEY.as_bytes().first_chunk::<32>().unwrap(),
        config.clone(),
    )
    .await
    .unwrap();
    let admin = runtime
        .authenticate_presented(
            ADMIN_KEY_ID,
            TEST_ADMIN_KEY.as_bytes().first_chunk::<32>().unwrap(),
        )
        .unwrap();
    runtime
        .issue(
            &admin,
            Duration::from_secs(120),
            Some("json-output".to_string()),
        )
        .await
        .unwrap();
    drop(runtime);
    tokio::time::sleep(Duration::from_millis(20)).await;

    let shutdown = CancellationToken::new();
    let server_shutdown = shutdown.clone();
    let server_config = config.clone();
    let server = tokio::spawn(async move {
        run_server_with_auth_config(server_addr, server_shutdown, None, false, server_config)
            .await
            .unwrap();
    });
    drop(wait_for_server(server_addr).await);

    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_pb-mapper"));
    command
        .arg("admin")
        .arg("--server")
        .arg(server_addr.to_string())
        .arg("--output")
        .arg("json")
        .arg("key")
        .arg("list")
        .arg("--all")
        .env("MSG_HEADER_KEY", TEST_ADMIN_KEY)
        .env("RUST_LOG", "off")
        .kill_on_drop(true);
    let output = timeout(Duration::from_secs(3), command.output())
        .await
        .expect("admin JSON request timed out")
        .unwrap();
    assert!(output.status.success());
    let document: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(document["schema_version"], 1);
    assert_eq!(
        document["data"]["KeyList"]["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    shutdown.cancel();
    server.await.unwrap();
    let _ = std::fs::remove_dir_all(config.state_dir);
}

async fn read_secure_request(
    security: &ServerSecurity,
    stream: &mut TcpStream,
) -> (PbConnRequest, ServerHeaderSession) {
    let initial = security.read_initial(stream).await.unwrap();
    (
        PbConnRequest::decode(&initial.payload).unwrap(),
        initial.session,
    )
}

fn auth_config(server_addr: SocketAddr) -> AuthConfig {
    static CONFIG_SEQUENCE: AtomicUsize = AtomicUsize::new(0);

    set_process_msg_header_key(Some(TEST_ADMIN_KEY)).unwrap();
    let sequence = CONFIG_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    AuthConfig {
        state_dir: std::env::temp_dir().join(format!(
            "pb-mapper-regression-{}-{}-{sequence}",
            std::process::id(),
            server_addr.port()
        )),
        max_temporary_keys: 64,
        max_temporary_key_ttl: Duration::from_secs(3600),
        legacy_protocol: LegacyProtocolPolicy::Allow,
    }
}

const TEST_ADMIN_KEY: &str = "0123456789abcdefghijklmnopqrstuv";

async fn register_control_conn_parts(
    reader: &mut impl MessageReader,
    writer: &mut impl MessageWriter,
    key: &str,
) -> u32 {
    let request = PbConnRequest::Register {
        need_codec: false,
        is_datagram: false,
        key: key.to_string(),
        protocol_version: None,
        client_instance_id: None,
        heartbeat_interval_ms: None,
        heartbeat_tolerance_ms: None,
    }
    .encode()
    .unwrap();
    writer.write_msg(&request).await.unwrap();

    let response = timeout(Duration::from_secs(1), reader.read_msg())
        .await
        .expect("register response timed out")
        .unwrap();
    let PbConnResponse::Register(conn_id) = PbConnResponse::decode(response).unwrap() else {
        panic!("unexpected register response");
    };
    conn_id
}

async fn read_status_keys(server_addr: SocketAddr) -> Vec<String> {
    let mut stream = wait_for_server(server_addr).await;
    let request = PbConnRequest::Status(PbConnStatusReq::Keys)
        .encode()
        .unwrap();
    {
        let mut writer = get_header_msg_writer(&mut stream).unwrap();
        writer.write_msg(&request).await.unwrap();
    }

    let mut reader = get_header_msg_reader(&mut stream).unwrap();
    let response = timeout(Duration::from_secs(1), reader.read_msg())
        .await
        .expect("status keys response timed out")
        .unwrap();
    let PbConnResponse::Status(PbConnStatusResp::Keys(keys)) =
        PbConnResponse::decode(response).unwrap()
    else {
        panic!("unexpected status keys response");
    };
    keys
}

async fn send_v2_request(
    server_addr: SocketAddr,
    credential: &Credential,
    request: PbConnRequest,
) -> (TcpStream, ClientHeaderSession, PbConnResponse) {
    let mut stream = wait_for_server(server_addr).await;
    let session = ClientHeaderSession::new_v2(credential).unwrap();
    session
        .write_initial(&mut stream, &request.encode().unwrap())
        .await
        .unwrap();
    let response = {
        let mut reader = session.response_reader(&mut stream).unwrap();
        let message = timeout(Duration::from_secs(1), reader.read_msg())
            .await
            .expect("v2 response timed out")
            .unwrap();
        PbConnResponse::decode(message).unwrap()
    };
    (stream, session, response)
}

#[tokio::test]
async fn temporary_credentials_are_isolated_denied_admin_and_revoked_live() {
    let probe_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let server_addr = probe_listener.local_addr().unwrap();
    drop(probe_listener);

    let config = auth_config(server_addr);
    let _ = std::fs::remove_dir_all(&config.state_dir);
    write_admin_key_file(&config.state_dir.join("admin.key"), TEST_ADMIN_KEY, true).unwrap();
    let runtime = AuthRuntime::start(
        *TEST_ADMIN_KEY.as_bytes().first_chunk::<32>().unwrap(),
        config.clone(),
    )
    .await
    .unwrap();
    let admin = runtime
        .authenticate_presented(
            ADMIN_KEY_ID,
            TEST_ADMIN_KEY.as_bytes().first_chunk::<32>().unwrap(),
        )
        .unwrap();
    let first = runtime
        .issue(&admin, Duration::from_secs(120), Some("first".to_string()))
        .await
        .unwrap();
    let second = runtime
        .issue(&admin, Duration::from_secs(120), Some("second".to_string()))
        .await
        .unwrap();
    drop(runtime);
    tokio::time::sleep(Duration::from_millis(20)).await;

    let shutdown_token = CancellationToken::new();
    let server_shutdown = shutdown_token.clone();
    let server_config = config.clone();
    let server = tokio::spawn(async move {
        run_server_with_auth_config(server_addr, server_shutdown, None, false, server_config)
            .await
            .unwrap();
    });

    let first_credential = parse_credential(&first.credential).unwrap();
    let second_credential = parse_credential(&second.credential).unwrap();
    let register_request = |service: &str| PbConnRequest::Register {
        need_codec: false,
        is_datagram: false,
        key: service.to_string(),
        protocol_version: Some(2),
        client_instance_id: Some("temporary-auth-regression".to_string()),
        heartbeat_interval_ms: Some(50),
        heartbeat_tolerance_ms: Some(150),
    };
    let (mut first_control, _, first_register) = send_v2_request(
        server_addr,
        &first_credential,
        register_request("same-name"),
    )
    .await;
    let (_second_control, _, second_register) = send_v2_request(
        server_addr,
        &second_credential,
        register_request("same-name"),
    )
    .await;
    let first_conn_id = match first_register {
        PbConnResponse::RegisterV2 { conn_id, .. } => conn_id,
        response => panic!("unexpected first register response: {response:?}"),
    };
    let second_conn_id = match second_register {
        PbConnResponse::RegisterV2 { conn_id, .. } => conn_id,
        response => panic!("unexpected second register response: {response:?}"),
    };
    assert_ne!(first_conn_id, second_conn_id);

    let status_request = PbConnRequest::Status(PbConnStatusReq::Service {
        key: "same-name".to_string(),
    });
    for (credential, expected_conn_id) in [
        (&first_credential, first_conn_id),
        (&second_credential, second_conn_id),
    ] {
        let (_, _, response) =
            send_v2_request(server_addr, credential, status_request.clone()).await;
        let PbConnResponse::Status(PbConnStatusResp::Service { connections, .. }) = response else {
            panic!("unexpected scoped status response: {response:?}");
        };
        assert_eq!(connections.len(), 1);
        assert_eq!(connections[0].conn_id, expected_conn_id);
    }

    for admin_request in [
        AdminRequest::AuthStatus,
        AdminRequest::ServiceList {
            key_id: None,
            page: 0,
            page_size: 100,
        },
        AdminRequest::ConnectionList {
            key_id: None,
            page: 0,
            page_size: 100,
        },
    ] {
        let (_, _, denied) = send_v2_request(
            server_addr,
            &first_credential,
            PbConnRequest::Admin(admin_request),
        )
        .await;
        let PbConnResponse::Error(denied) = denied else {
            panic!("temporary credential unexpectedly received an admin response");
        };
        assert_eq!(denied.code, "admin_permission_required");
    }

    let admin_credential =
        Credential::Admin(*TEST_ADMIN_KEY.as_bytes().first_chunk::<32>().unwrap());
    let (_, _, revoked) = send_v2_request(
        server_addr,
        &admin_credential,
        PbConnRequest::Admin(AdminRequest::KeyRevoke {
            key_id: first.metadata.key_id.as_u64(),
        }),
    )
    .await;
    assert!(matches!(
        revoked,
        PbConnResponse::Admin(AdminResponse::KeyRevoked(_))
    ));
    let mut byte = [0_u8; 1];
    let closed = timeout(Duration::from_secs(1), first_control.read(&mut byte))
        .await
        .expect("revoked control connection was not closed")
        .unwrap();
    assert_eq!(closed, 0);

    let (_, _, second_still_active) =
        send_v2_request(server_addr, &second_credential, status_request).await;
    assert!(matches!(
        second_still_active,
        PbConnResponse::Status(PbConnStatusResp::Service { .. })
    ));

    shutdown_token.cancel();
    server.await.unwrap();
    let _ = std::fs::remove_dir_all(config.state_dir);
}

#[tokio::test]
async fn revoking_subscriber_credential_closes_cross_credential_data_stream() {
    let probe_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let server_addr = probe_listener.local_addr().unwrap();
    drop(probe_listener);

    let config = auth_config(server_addr);
    let _ = std::fs::remove_dir_all(&config.state_dir);
    write_admin_key_file(&config.state_dir.join("admin.key"), TEST_ADMIN_KEY, true).unwrap();
    let runtime = AuthRuntime::start(
        *TEST_ADMIN_KEY.as_bytes().first_chunk::<32>().unwrap(),
        config.clone(),
    )
    .await
    .unwrap();
    let admin = runtime
        .authenticate_presented(
            ADMIN_KEY_ID,
            TEST_ADMIN_KEY.as_bytes().first_chunk::<32>().unwrap(),
        )
        .unwrap();
    let issued = runtime
        .issue(
            &admin,
            Duration::from_secs(120),
            Some("active-stream".to_string()),
        )
        .await
        .unwrap();
    drop(runtime);
    tokio::time::sleep(Duration::from_millis(20)).await;

    let shutdown_token = CancellationToken::new();
    let server_shutdown = shutdown_token.clone();
    let server_config = config.clone();
    let server = tokio::spawn(async move {
        run_server_with_auth_config(server_addr, server_shutdown, None, false, server_config)
            .await
            .unwrap();
    });

    let credential = parse_credential(&issued.credential).unwrap();
    let admin_credential =
        Credential::Admin(*TEST_ADMIN_KEY.as_bytes().first_chunk::<32>().unwrap());
    let service = "revoked-stream";
    let mut control = wait_for_server(server_addr).await;
    let control_session = ClientHeaderSession::new_v2(&admin_credential).unwrap();
    let register = PbConnRequest::RegisterScoped {
        need_codec: false,
        is_datagram: false,
        key: service.to_string(),
        namespace: issued.metadata.key_id.as_u64(),
        force_namespace: true,
        protocol_version: Some(2),
        client_instance_id: Some("active-stream-test".to_string()),
        heartbeat_interval_ms: Some(5_000),
        heartbeat_tolerance_ms: Some(15_000),
    };
    control_session
        .write_initial(&mut control, &register.encode().unwrap())
        .await
        .unwrap();
    let (mut control_read, mut control_write) = control.into_split();
    let mut control_reader = control_session.response_reader(&mut control_read).unwrap();
    let register_response = timeout(Duration::from_secs(1), control_reader.read_msg())
        .await
        .expect("register response timed out")
        .unwrap();
    assert!(matches!(
        PbConnResponse::decode(register_response).unwrap(),
        PbConnResponse::RegisterV2 { .. }
    ));

    let mut subscriber = wait_for_server(server_addr).await;
    let subscriber_session = ClientHeaderSession::new_v2(&credential).unwrap();
    subscriber_session
        .write_initial(
            &mut subscriber,
            &PbConnRequest::Subcribe {
                key: service.to_string(),
            }
            .encode()
            .unwrap(),
        )
        .await
        .unwrap();

    let stream_request = timeout(Duration::from_secs(1), control_reader.read_msg())
        .await
        .expect("stream request timed out")
        .unwrap();
    let LocalServer::Stream {
        client_id,
        server_generation,
    } = LocalServer::decode(stream_request).unwrap()
    else {
        panic!("unexpected local server stream request");
    };
    let mut control_writer = control_session
        .continuation_writer(&mut control_write)
        .unwrap();
    control_writer
        .write_msg(
            &PbServerRequest::StreamAck {
                client_id,
                server_generation,
            }
            .encode()
            .unwrap(),
        )
        .await
        .unwrap();

    let mut provider = wait_for_server(server_addr).await;
    let provider_session = ClientHeaderSession::new_v2(&admin_credential).unwrap();
    provider_session
        .write_initial(
            &mut provider,
            &PbConnRequest::StreamScoped {
                key: service.to_string(),
                namespace: issued.metadata.key_id.as_u64(),
                dst_id: client_id,
                server_generation,
            }
            .encode()
            .unwrap(),
        )
        .await
        .unwrap();
    {
        let mut subscriber_reader = subscriber_session.response_reader(&mut subscriber).unwrap();
        let response = timeout(Duration::from_secs(1), subscriber_reader.read_msg())
            .await
            .expect("subscribe response timed out")
            .unwrap();
        assert!(matches!(
            PbConnResponse::decode(response).unwrap(),
            PbConnResponse::Subcribe { .. }
        ));
    }
    {
        let mut provider_reader = provider_session.response_reader(&mut provider).unwrap();
        let response = timeout(Duration::from_secs(1), provider_reader.read_msg())
            .await
            .expect("provider stream response timed out")
            .unwrap();
        assert!(matches!(
            PbConnResponse::decode(response).unwrap(),
            PbConnResponse::Stream { .. }
        ));
    }

    subscriber.write_all(b"ready").await.unwrap();
    let mut ready = [0_u8; 5];
    timeout(Duration::from_secs(1), provider.read_exact(&mut ready))
        .await
        .expect("active data stream did not forward")
        .unwrap();
    assert_eq!(&ready, b"ready");

    let (_, _, revoked) = send_v2_request(
        server_addr,
        &admin_credential,
        PbConnRequest::Admin(AdminRequest::KeyRevoke {
            key_id: issued.metadata.key_id.as_u64(),
        }),
    )
    .await;
    assert!(matches!(
        revoked,
        PbConnResponse::Admin(AdminResponse::KeyRevoked(_))
    ));

    let mut byte = [0_u8; 1];
    let read = timeout(Duration::from_secs(1), subscriber.read(&mut byte))
        .await
        .expect("revoked subscriber data stream was not closed")
        .unwrap();
    assert_eq!(read, 0, "revoked subscriber data stream remained open");

    match timeout(Duration::from_millis(200), control_reader.read_msg()).await {
        Err(_) | Ok(Ok(_)) => {}
        Ok(Err(error)) => panic!("administrator registration was cancelled too: {error}"),
    }

    shutdown_token.cancel();
    server.await.unwrap();
    let _ = std::fs::remove_dir_all(config.state_dir);
}

#[tokio::test]
async fn status_service_reports_registered_v2_control_connection() {
    let probe_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let server_addr = probe_listener.local_addr().unwrap();
    drop(probe_listener);

    let shutdown_token = CancellationToken::new();
    let server_shutdown = shutdown_token.clone();
    let server = tokio::spawn(async move {
        run_server_with_auth_config(
            server_addr,
            server_shutdown,
            None,
            false,
            auth_config(server_addr),
        )
        .await
        .unwrap();
    });

    let key = "sf-backend";
    let control = wait_for_server(server_addr).await;
    let (mut reader_stream, mut writer_stream) = control.into_split();
    let mut reader = get_header_msg_reader(&mut reader_stream).unwrap();
    let mut writer = get_header_msg_writer(&mut writer_stream).unwrap();
    let (conn_id, generation) = register_v2_control(
        &mut reader,
        &mut writer,
        key,
        V2ControlSpec::new()
            .instance_id("regression-test-client")
            .response_timeout(Duration::from_secs(1)),
    )
    .await;

    let mut status = wait_for_server(server_addr).await;
    let request = PbConnRequest::Status(PbConnStatusReq::Service {
        key: key.to_string(),
    })
    .encode()
    .unwrap();
    {
        let mut writer = get_header_msg_writer(&mut status).unwrap();
        writer.write_msg(&request).await.unwrap();
    }

    let mut reader = get_header_msg_reader(&mut status).unwrap();
    let response = timeout(Duration::from_secs(1), reader.read_msg())
        .await
        .expect("service status response timed out")
        .unwrap();
    let PbConnResponse::Status(PbConnStatusResp::Service {
        key: status_key,
        connections,
    }) = PbConnResponse::decode(response).unwrap()
    else {
        panic!("unexpected service status response");
    };

    assert_eq!(status_key, key);
    assert_eq!(connections.len(), 1);
    assert_eq!(connections[0].conn_id, conn_id);
    assert_eq!(connections[0].generation, generation);
    assert!(connections[0].healthy);

    shutdown_token.cancel();
    server.await.unwrap();
}

#[tokio::test]
async fn local_server_reconnects_when_registered_conn_is_missing_from_remote_status() {
    let _env = EnvOverrides::set(&[
        ("PB_MAPPER_CONTROL_CONN_POOL_SIZE", "1"),
        ("PB_MAPPER_CONTROL_HEARTBEAT_INTERVAL", "20ms"),
        ("PB_MAPPER_CONTROL_HEARTBEAT_TOLERANCE", "50ms"),
        ("PB_MAPPER_CONTROL_SUSPECT_GRACE", "20ms"),
        ("PB_MAPPER_REGISTRATION_PROBE_TIMEOUT", "50ms"),
    ])
    .await;

    let remote_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let remote_addr = remote_listener.local_addr().unwrap();
    let register_count = Arc::new(AtomicUsize::new(0));
    let (second_register_tx, second_register_rx) = tokio::sync::oneshot::channel();
    let second_register_tx = Arc::new(tokio::sync::Mutex::new(Some(second_register_tx)));

    let fake_register_count = register_count.clone();
    let fake_second_register_tx = second_register_tx.clone();
    let fake_security = ServerSecurity::new(
        AuthRuntime::from_process(auth_config(remote_addr))
            .await
            .unwrap(),
    );
    let fake_server = tokio::spawn(async move {
        loop {
            let (mut stream, _) = remote_listener.accept().await.unwrap();
            let register_count = fake_register_count.clone();
            let second_register_tx = fake_second_register_tx.clone();
            let security = fake_security.clone();
            tokio::spawn(async move {
                let (request, session) = read_secure_request(&security, &mut stream).await;
                match request {
                    PbConnRequest::Register { key, .. } => {
                        let count = register_count.fetch_add(1, Ordering::SeqCst) + 1;
                        let response = PbConnResponse::RegisterV2 {
                            conn_id: count as u32,
                            generation: count as u64,
                            lease_ttl_ms: 150,
                        }
                        .encode()
                        .unwrap();
                        let mut writer = session.response_writer(&mut stream).unwrap();
                        writer.write_msg(&response).await.unwrap();
                        if count == 2
                            && let Some(tx) = second_register_tx.lock().await.take()
                        {
                            tx.send(()).unwrap();
                        }
                        tracing::debug!(key, count, "fake server accepted register");
                        std::future::pending::<()>().await;
                    }
                    PbConnRequest::Status(PbConnStatusReq::Service { key }) => {
                        let response = PbConnResponse::Status(PbConnStatusResp::Service {
                            key,
                            connections: Vec::new(),
                        })
                        .encode()
                        .unwrap();
                        let mut writer = session.response_writer(&mut stream).unwrap();
                        writer.write_msg(&response).await.unwrap();
                    }
                    PbConnRequest::Status(PbConnStatusReq::Keys) => {
                        let response = PbConnResponse::Status(PbConnStatusResp::Keys(Vec::new()))
                            .encode()
                            .unwrap();
                        let mut writer = session.response_writer(&mut stream).unwrap();
                        writer.write_msg(&response).await.unwrap();
                    }
                    _ => {}
                }
            });
        }
    });

    let local_addr: SocketAddr = "127.0.0.1:9".parse().unwrap();
    let local_server = tokio::spawn(run_server_side_cli_with_callback::<TcpStreamProvider, _>(
        local_addr,
        remote_addr,
        Arc::from("sf-backend"),
        ServerTunnelOptions {
            need_codec: false,
            is_datagram: false,
            keep_alive: false,
            namespace: None,
            force_namespace: false,
        },
        None,
    ));

    timeout(Duration::from_secs(2), second_register_rx)
        .await
        .expect("local server did not reconnect after remote status lost its registration")
        .unwrap();

    local_server.abort();
    fake_server.abort();
}

#[tokio::test]
async fn client_closes_initial_status_probe_after_key_check() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let remote_addr = listener.local_addr().unwrap();
    let security = ServerSecurity::new(
        AuthRuntime::from_process(auth_config(remote_addr))
            .await
            .unwrap(),
    );

    let fake_server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let (request, session) = read_secure_request(&security, &mut stream).await;
        let PbConnRequest::Status(PbConnStatusReq::Service { key }) = request else {
            panic!("client did not use service status for initial key check");
        };

        let response = PbConnResponse::Status(PbConnStatusResp::Service {
            key,
            connections: vec![PbServiceConnStatus {
                conn_id: 1,
                generation: 1,
                protocol_version: 2,
                healthy: true,
                last_rx_age_ms: 0,
            }],
        })
        .encode()
        .unwrap();
        {
            let mut writer = session.response_writer(&mut stream).unwrap();
            writer.write_msg(&response).await.unwrap();
        }

        let mut buf = [0u8; 1];
        timeout(Duration::from_secs(1), stream.read(&mut buf))
            .await
            .expect("client kept the one-shot status probe connection open")
            .unwrap()
    });

    let local_addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let client = tokio::spawn(run_client_side_cli_with_callback::<TcpListenerProvider, _>(
        local_addr,
        remote_addr,
        Arc::from("sf-backend"),
        false,
        None,
    ));

    assert_eq!(fake_server.await.unwrap(), 0);
    client.abort();
}

#[tokio::test]
async fn client_tolerates_one_failed_health_check_while_listener_is_active() {
    let _env = EnvOverrides::set(&[
        ("PB_MAPPER_CLIENT_HEALTH_CHECK_INTERVAL", "20ms"),
        ("PB_MAPPER_CLIENT_HEALTH_CHECK_TIMEOUT", "200ms"),
        ("PB_MAPPER_CLIENT_HEALTH_FAILURE_THRESHOLD", "3"),
    ])
    .await;

    let local_probe = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let local_addr = local_probe.local_addr().unwrap();
    drop(local_probe);

    let remote_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let remote_addr = remote_listener.local_addr().unwrap();
    let failed_status_responses = Arc::new(AtomicUsize::new(0));
    let status_count = Arc::new(AtomicUsize::new(0));
    let status_changes = Arc::new(Mutex::new(Vec::<String>::new()));

    let fake_failed_status_responses = failed_status_responses.clone();
    let fake_status_count = status_count.clone();
    let fake_security = ServerSecurity::new(
        AuthRuntime::from_process(auth_config(remote_addr))
            .await
            .unwrap(),
    );
    let fake_server = tokio::spawn(async move {
        loop {
            let (mut stream, _) = remote_listener.accept().await.unwrap();
            let failed_status_responses = fake_failed_status_responses.clone();
            let status_count = fake_status_count.clone();
            let security = fake_security.clone();
            tokio::spawn(async move {
                let (request, session) = read_secure_request(&security, &mut stream).await;
                match request {
                    PbConnRequest::Status(PbConnStatusReq::Service { key }) => {
                        status_count.fetch_add(1, Ordering::SeqCst);
                        let should_fail = failed_status_responses
                            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                                remaining.checked_sub(1)
                            })
                            .is_ok();
                        let connections = if should_fail {
                            Vec::new()
                        } else {
                            vec![PbServiceConnStatus {
                                conn_id: 1,
                                generation: 1,
                                protocol_version: 2,
                                healthy: true,
                                last_rx_age_ms: 0,
                            }]
                        };
                        let response =
                            PbConnResponse::Status(PbConnStatusResp::Service { key, connections })
                                .encode()
                                .unwrap();
                        let mut writer = session.response_writer(&mut stream).unwrap();
                        writer.write_msg(&response).await.unwrap();
                    }
                    PbConnRequest::Status(PbConnStatusReq::Keys) => {
                        status_count.fetch_add(1, Ordering::SeqCst);
                        let should_fail = failed_status_responses
                            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                                remaining.checked_sub(1)
                            })
                            .is_ok();
                        let keys = if should_fail {
                            Vec::new()
                        } else {
                            vec!["sf-backend".to_string()]
                        };
                        let response = PbConnResponse::Status(PbConnStatusResp::Keys(keys))
                            .encode()
                            .unwrap();
                        let mut writer = session.response_writer(&mut stream).unwrap();
                        writer.write_msg(&response).await.unwrap();
                    }
                    PbConnRequest::Subcribe { .. } => std::future::pending().await,
                    _ => {}
                }
            });
        }
    });

    let callback_status_changes = status_changes.clone();
    let client = tokio::spawn(run_client_side_cli_with_callback::<TcpListenerProvider, _>(
        local_addr,
        remote_addr,
        Arc::from("sf-backend"),
        false,
        Some(Box::new(move |status| {
            callback_status_changes
                .lock()
                .unwrap()
                .push(status.to_string());
        })),
    ));

    timeout(Duration::from_secs(1), async {
        loop {
            if status_count.load(Ordering::SeqCst) >= 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("client did not run initial key probe");

    timeout(Duration::from_secs(1), async {
        loop {
            if TcpStream::connect(local_addr).await.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("client listener did not bind after key probe");

    let baseline = status_count.load(Ordering::SeqCst);
    failed_status_responses.store(2, Ordering::SeqCst);

    timeout(Duration::from_secs(1), async {
        loop {
            if status_count.load(Ordering::SeqCst) >= baseline + 3 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("client did not recover after one failed health check");

    assert!(
        TcpStream::connect(local_addr).await.is_ok(),
        "client listener stopped after one transient health-check failure"
    );
    assert_eq!(
        status_changes.lock().unwrap().as_slice(),
        ["connected"],
        "a single failed health check must not restart the listener"
    );

    client.abort();
    fake_server.abort();
}

#[tokio::test]
async fn subscribe_retires_unacked_control_connection() {
    let probe_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let server_addr = probe_listener.local_addr().unwrap();
    drop(probe_listener);

    let shutdown_token = CancellationToken::new();
    let server_shutdown = shutdown_token.clone();
    let server = tokio::spawn(async move {
        run_server_with_auth_config(
            server_addr,
            server_shutdown,
            None,
            false,
            auth_config(server_addr),
        )
        .await
        .unwrap();
    });

    let key = "sf-backend";
    let (stale_ready_tx, stale_ready_rx) = tokio::sync::oneshot::channel();
    let stale_key = key.to_string();
    let stale_task = tokio::spawn(async move {
        let stale_control = wait_for_server(server_addr).await;
        let (mut reader_stream, mut writer_stream) = stale_control.into_split();
        let mut reader = get_header_msg_reader(&mut reader_stream).unwrap();
        let mut writer = get_header_msg_writer(&mut writer_stream).unwrap();
        register_control_conn_parts(&mut reader, &mut writer, &stale_key).await;
        stale_ready_tx.send(()).unwrap();
        let request = timeout(Duration::from_secs(2), reader.read_msg())
            .await
            .expect("stale control did not receive stream request")
            .unwrap();
        assert!(matches!(
            LocalServer::decode(request).unwrap(),
            LocalServer::Stream { .. }
        ));
        std::future::pending::<()>().await;
    });
    stale_ready_rx.await.unwrap();

    let mut client = wait_for_server(server_addr).await;
    let request = PbConnRequest::Subcribe {
        key: key.to_string(),
    }
    .encode()
    .unwrap();
    {
        let mut writer = get_header_msg_writer(&mut client).unwrap();
        writer.write_msg(&request).await.unwrap();
    }

    let mut reader = get_header_msg_reader(&mut client).unwrap();
    let result = timeout(Duration::from_secs(4), reader.read_msg())
        .await
        .expect("subscribe did not finish after stale control timed out");
    assert!(result.is_err());

    let keys = read_status_keys(server_addr).await;
    assert!(
        !keys.iter().any(|candidate| candidate == key),
        "unacked stale control connection kept key registered: {keys:?}"
    );

    stale_task.abort();
    shutdown_token.cancel();
    server.await.unwrap();
}

#[tokio::test]
async fn subscribe_waits_for_replacement_after_retiring_stale_control_connection() {
    let probe_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let server_addr = probe_listener.local_addr().unwrap();
    drop(probe_listener);

    let shutdown_token = CancellationToken::new();
    let server_shutdown = shutdown_token.clone();
    let server = tokio::spawn(async move {
        run_server_with_auth_config(
            server_addr,
            server_shutdown,
            None,
            false,
            auth_config(server_addr),
        )
        .await
        .unwrap();
    });

    let key = "sf-backend";
    let (stale_ready_tx, stale_ready_rx) = tokio::sync::oneshot::channel();
    let (retired_tx, retired_rx) = tokio::sync::oneshot::channel();
    let stale_key = key.to_string();
    let stale_task = tokio::spawn(async move {
        let stale_control = wait_for_server(server_addr).await;
        let (mut reader_stream, mut writer_stream) = stale_control.into_split();
        let mut reader = get_header_msg_reader(&mut reader_stream).unwrap();
        let mut writer = get_header_msg_writer(&mut writer_stream).unwrap();
        register_control_conn_parts(&mut reader, &mut writer, &stale_key).await;
        stale_ready_tx.send(()).unwrap();
        let request = timeout(Duration::from_secs(2), reader.read_msg())
            .await
            .expect("stale control did not receive stream request")
            .unwrap();
        assert!(matches!(
            LocalServer::decode(request).unwrap(),
            LocalServer::Stream { .. }
        ));

        let retired = timeout(Duration::from_secs(2), reader.read_msg())
            .await
            .expect("stale control was not retired by the server");
        if let Ok(bytes) = retired {
            assert!(bytes.is_empty());
        }
        retired_tx.send(()).unwrap();
    });
    stale_ready_rx.await.unwrap();

    let (replacement_stream_tx, replacement_stream_rx) = tokio::sync::oneshot::channel();
    let replacement_key = key.to_string();
    let replacement_task = tokio::spawn(async move {
        retired_rx.await.unwrap();
        let replacement_control = wait_for_server(server_addr).await;
        let (mut reader_stream, mut writer_stream) = replacement_control.into_split();
        let mut reader = get_header_msg_reader(&mut reader_stream).unwrap();
        {
            let mut writer = get_header_msg_writer(&mut writer_stream).unwrap();
            register_control_conn_parts(&mut reader, &mut writer, &replacement_key).await;
        }
        let mut writer = get_header_msg_writer(&mut writer_stream).unwrap();
        let request = timeout(Duration::from_secs(2), reader.read_msg())
            .await
            .expect("replacement control did not receive stream request")
            .unwrap();
        let LocalServer::Stream {
            client_id,
            server_generation,
        } = LocalServer::decode(request).unwrap()
        else {
            panic!("unexpected local server control message");
        };

        let ack = PbServerRequest::StreamAck {
            client_id,
            server_generation,
        }
        .encode()
        .unwrap();
        writer.write_msg(&ack).await.unwrap();

        let mut stream = TcpStream::connect(server_addr).await.unwrap();
        let request = PbConnRequest::Stream {
            key: replacement_key,
            dst_id: client_id,
            server_generation,
        }
        .encode()
        .unwrap();
        let mut stream_writer = get_header_msg_writer(&mut stream).unwrap();
        stream_writer.write_msg(&request).await.unwrap();
        replacement_stream_tx.send(stream).unwrap();
        std::future::pending::<()>().await;
    });

    let mut client = wait_for_server(server_addr).await;
    let request = PbConnRequest::Subcribe {
        key: key.to_string(),
    }
    .encode()
    .unwrap();
    {
        let mut writer = get_header_msg_writer(&mut client).unwrap();
        writer.write_msg(&request).await.unwrap();
    }

    let mut reader = get_header_msg_reader(&mut client).unwrap();
    let response = timeout(Duration::from_secs(3), reader.read_msg())
        .await
        .expect("subscribe did not wait for replacement control connection")
        .unwrap();
    assert!(matches!(
        PbConnResponse::decode(response).unwrap(),
        PbConnResponse::Subcribe { .. }
    ));

    let stream = replacement_stream_rx.await.unwrap();
    drop(stream);
    stale_task.abort();
    replacement_task.abort();
    shutdown_token.cancel();
    server.await.unwrap();
}

#[tokio::test]
async fn subscribe_missing_key_closes_without_hanging() {
    let probe_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let server_addr = probe_listener.local_addr().unwrap();
    drop(probe_listener);

    let shutdown_token = CancellationToken::new();
    let server_shutdown = shutdown_token.clone();
    let server = tokio::spawn(async move {
        run_server_with_auth_config(
            server_addr,
            server_shutdown,
            None,
            false,
            auth_config(server_addr),
        )
        .await
        .unwrap();
    });

    let mut stream = timeout(Duration::from_secs(2), async {
        loop {
            match TcpStream::connect(server_addr).await {
                Ok(stream) => break stream,
                Err(_) => tokio::time::sleep(Duration::from_millis(10)).await,
            }
        }
    })
    .await
    .expect("server did not start");

    let request = PbConnRequest::Subcribe {
        key: "missing-key".to_string(),
    }
    .encode()
    .unwrap();
    {
        let mut writer = get_header_msg_writer(&mut stream).unwrap();
        writer.write_msg(&request).await.unwrap();
    }

    let mut reader = get_header_msg_reader(&mut stream).unwrap();
    let result = timeout(Duration::from_millis(200), reader.read_msg())
        .await
        .expect("missing-key subscribe hung instead of closing");
    let response = PbConnResponse::decode(result.unwrap()).unwrap();
    let PbConnResponse::Error(error) = response else {
        panic!("expected structured missing-service error");
    };
    assert_eq!(error.code, "service_not_available");

    shutdown_token.cancel();
    server.await.unwrap();
}

#[tokio::test]
async fn subscribe_bypasses_unacked_stale_control_connection() {
    let probe_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let server_addr = probe_listener.local_addr().unwrap();
    drop(probe_listener);

    let shutdown_token = CancellationToken::new();
    let server_shutdown = shutdown_token.clone();
    let server = tokio::spawn(async move {
        run_server_with_auth_config(
            server_addr,
            server_shutdown,
            None,
            false,
            auth_config(server_addr),
        )
        .await
        .unwrap();
    });

    let key = "sf-backend";
    let (healthy_ready_tx, healthy_ready_rx) = tokio::sync::oneshot::channel();
    let (stream_tx, stream_rx) = tokio::sync::oneshot::channel();
    let healthy_key = key.to_string();
    let healthy_task = tokio::spawn(async move {
        let healthy_control = wait_for_server(server_addr).await;
        let (mut reader_stream, mut writer_stream) = healthy_control.into_split();
        let mut reader = get_header_msg_reader(&mut reader_stream).unwrap();
        {
            let mut writer = get_header_msg_writer(&mut writer_stream).unwrap();
            register_control_conn_parts(&mut reader, &mut writer, &healthy_key).await;
        }
        let mut writer = get_header_msg_writer(&mut writer_stream).unwrap();
        healthy_ready_tx.send(()).unwrap();
        let request = timeout(Duration::from_secs(2), reader.read_msg())
            .await
            .expect("healthy control did not receive stream request")
            .unwrap();
        let LocalServer::Stream {
            client_id,
            server_generation,
        } = LocalServer::decode(request).unwrap()
        else {
            panic!("unexpected local server control message");
        };

        let ack = PbServerRequest::StreamAck {
            client_id,
            server_generation,
        }
        .encode()
        .unwrap();
        writer.write_msg(&ack).await.unwrap();

        let mut stream = TcpStream::connect(server_addr).await.unwrap();
        let request = PbConnRequest::Stream {
            key: healthy_key,
            dst_id: client_id,
            server_generation,
        }
        .encode()
        .unwrap();
        let mut stream_writer = get_header_msg_writer(&mut stream).unwrap();
        stream_writer.write_msg(&request).await.unwrap();
        stream_tx.send(stream).unwrap();
        std::future::pending::<()>().await;
    });
    healthy_ready_rx.await.unwrap();

    let (stale_ready_tx, stale_ready_rx) = tokio::sync::oneshot::channel();
    let stale_key = key.to_string();
    let stale_task = tokio::spawn(async move {
        let stale_control = wait_for_server(server_addr).await;
        let (mut reader_stream, mut writer_stream) = stale_control.into_split();
        let mut reader = get_header_msg_reader(&mut reader_stream).unwrap();
        let mut writer = get_header_msg_writer(&mut writer_stream).unwrap();
        register_control_conn_parts(&mut reader, &mut writer, &stale_key).await;
        stale_ready_tx.send(()).unwrap();
        std::future::pending::<()>().await;
    });
    stale_ready_rx.await.unwrap();

    let mut client = wait_for_server(server_addr).await;
    let request = PbConnRequest::Subcribe {
        key: key.to_string(),
    }
    .encode()
    .unwrap();
    {
        let mut writer = get_header_msg_writer(&mut client).unwrap();
        writer.write_msg(&request).await.unwrap();
    }

    let mut reader = get_header_msg_reader(&mut client).unwrap();
    let response = timeout(Duration::from_millis(1_000), reader.read_msg())
        .await
        .expect("subscribe did not bypass stale control connection quickly")
        .unwrap();
    assert!(matches!(
        PbConnResponse::decode(response).unwrap(),
        PbConnResponse::Subcribe { .. }
    ));

    let stream = stream_rx.await.unwrap();
    drop(stream);
    healthy_task.abort();
    stale_task.abort();
    shutdown_token.cancel();
    server.await.unwrap();
}

#[tokio::test]
async fn subscribe_bypasses_acked_control_connection_without_stream() {
    let probe_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let server_addr = probe_listener.local_addr().unwrap();
    drop(probe_listener);

    let shutdown_token = CancellationToken::new();
    let server_shutdown = shutdown_token.clone();
    let server = tokio::spawn(async move {
        run_server_with_auth_config(
            server_addr,
            server_shutdown,
            None,
            false,
            auth_config(server_addr),
        )
        .await
        .unwrap();
    });

    let key = "sf-backend";
    let (healthy_ready_tx, healthy_ready_rx) = tokio::sync::oneshot::channel();
    let (stream_tx, stream_rx) = tokio::sync::oneshot::channel();
    let healthy_key = key.to_string();
    let healthy_task = tokio::spawn(async move {
        let healthy_control = wait_for_server(server_addr).await;
        let (mut reader_stream, mut writer_stream) = healthy_control.into_split();
        let mut reader = get_header_msg_reader(&mut reader_stream).unwrap();
        {
            let mut writer = get_header_msg_writer(&mut writer_stream).unwrap();
            register_control_conn_parts(&mut reader, &mut writer, &healthy_key).await;
        }
        let mut writer = get_header_msg_writer(&mut writer_stream).unwrap();
        healthy_ready_tx.send(()).unwrap();
        let request = timeout(Duration::from_secs(2), reader.read_msg())
            .await
            .expect("healthy control did not receive fallback stream request")
            .unwrap();
        let LocalServer::Stream {
            client_id,
            server_generation,
        } = LocalServer::decode(request).unwrap()
        else {
            panic!("unexpected local server control message");
        };

        let ack = PbServerRequest::StreamAck {
            client_id,
            server_generation,
        }
        .encode()
        .unwrap();
        writer.write_msg(&ack).await.unwrap();

        let mut stream = TcpStream::connect(server_addr).await.unwrap();
        let request = PbConnRequest::Stream {
            key: healthy_key,
            dst_id: client_id,
            server_generation,
        }
        .encode()
        .unwrap();
        let mut stream_writer = get_header_msg_writer(&mut stream).unwrap();
        stream_writer.write_msg(&request).await.unwrap();
        stream_tx.send(stream).unwrap();
        std::future::pending::<()>().await;
    });
    healthy_ready_rx.await.unwrap();

    let (stale_ready_tx, stale_ready_rx) = tokio::sync::oneshot::channel();
    let stale_key = key.to_string();
    let stale_task = tokio::spawn(async move {
        let stale_control = wait_for_server(server_addr).await;
        let (mut reader_stream, mut writer_stream) = stale_control.into_split();
        let mut reader = get_header_msg_reader(&mut reader_stream).unwrap();
        {
            let mut writer = get_header_msg_writer(&mut writer_stream).unwrap();
            register_control_conn_parts(&mut reader, &mut writer, &stale_key).await;
        }
        let mut writer = get_header_msg_writer(&mut writer_stream).unwrap();
        stale_ready_tx.send(()).unwrap();
        let request = timeout(Duration::from_secs(2), reader.read_msg())
            .await
            .expect("stale control did not receive first stream request")
            .unwrap();
        let LocalServer::Stream {
            client_id,
            server_generation,
        } = LocalServer::decode(request).unwrap()
        else {
            panic!("unexpected local server control message");
        };
        let ack = PbServerRequest::StreamAck {
            client_id,
            server_generation,
        }
        .encode()
        .unwrap();
        writer.write_msg(&ack).await.unwrap();
        std::future::pending::<()>().await;
    });
    stale_ready_rx.await.unwrap();

    let mut client = wait_for_server(server_addr).await;
    let request = PbConnRequest::Subcribe {
        key: key.to_string(),
    }
    .encode()
    .unwrap();
    {
        let mut writer = get_header_msg_writer(&mut client).unwrap();
        writer.write_msg(&request).await.unwrap();
    }

    let mut reader = get_header_msg_reader(&mut client).unwrap();
    let response = timeout(Duration::from_millis(2_000), reader.read_msg())
        .await
        .expect("subscribe did not bypass acked control without stream quickly")
        .unwrap();
    assert!(matches!(
        PbConnResponse::decode(response).unwrap(),
        PbConnResponse::Subcribe { .. }
    ));

    let stream = stream_rx.await.unwrap();
    drop(stream);
    healthy_task.abort();
    stale_task.abort();
    shutdown_token.cancel();
    server.await.unwrap();
}

/// A fake relay that refuses every registration with one error.
///
/// Returns the timestamps of the register attempts it saw, which is what the two
/// cases below actually assert on: how long the client waited before trying
/// again, and whether it tried again at all.
fn spawn_rejecting_relay(
    listener: TcpListener,
    remote_addr: SocketAddr,
    code: &'static str,
    retryable: bool,
) -> (
    tokio::task::JoinHandle<()>,
    Arc<Mutex<Vec<tokio::time::Instant>>>,
) {
    let attempts = Arc::new(Mutex::new(Vec::new()));
    let relay_attempts = attempts.clone();
    let relay = tokio::spawn(async move {
        let security = ServerSecurity::new(
            AuthRuntime::from_process(auth_config(remote_addr))
                .await
                .unwrap(),
        );
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let attempts = relay_attempts.clone();
            let security = security.clone();
            tokio::spawn(async move {
                let (request, session) = read_secure_request(&security, &mut stream).await;
                let response = match request {
                    PbConnRequest::Register { .. } | PbConnRequest::RegisterScoped { .. } => {
                        attempts.lock().unwrap().push(tokio::time::Instant::now());
                        PbConnResponse::error(code, "refused by the test relay", retryable)
                    }
                    // The registration probe, which the client runs against a
                    // relay it has not registered with yet.
                    PbConnRequest::Status(PbConnStatusReq::Service { key }) => {
                        PbConnResponse::Status(PbConnStatusResp::Service {
                            key,
                            connections: Vec::new(),
                        })
                    }
                    PbConnRequest::Status(PbConnStatusReq::Keys) => {
                        PbConnResponse::Status(PbConnStatusResp::Keys(Vec::new()))
                    }
                    _ => return,
                };
                let mut writer = session.response_writer(&mut stream).unwrap();
                writer.write_msg(&response.encode().unwrap()).await.unwrap();
                std::future::pending::<()>().await;
            });
        }
    });
    (relay, attempts)
}

/// Wait until the fake relay has seen `count` register attempts.
async fn wait_for_register_attempts(
    attempts: &Arc<Mutex<Vec<tokio::time::Instant>>>,
    count: usize,
    within: Duration,
) -> Vec<tokio::time::Instant> {
    timeout(within, async {
        loop {
            let seen = attempts.lock().unwrap().clone();
            if seen.len() >= count {
                return seen;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("relay saw fewer than {count} register attempts"))
}

#[tokio::test]
async fn retryable_registration_rejection_waits_on_the_slow_ladder() {
    let _env = EnvOverrides::set(&[
        ("PB_MAPPER_CONTROL_CONN_POOL_SIZE", "1"),
        ("PB_MAPPER_REGISTRATION_REJECT_BACKOFF_MIN", "400ms"),
        ("PB_MAPPER_REGISTRATION_REJECT_BACKOFF_MAX", "800ms"),
    ])
    .await;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let remote_addr = listener.local_addr().unwrap();
    let (relay, attempts) = spawn_rejecting_relay(
        listener,
        remote_addr,
        "service_connection_limit_exceeded",
        true,
    );

    let status_changes = Arc::new(Mutex::new(Vec::<String>::new()));
    let callback_status_changes = status_changes.clone();
    let local_addr: SocketAddr = "127.0.0.1:9".parse().unwrap();
    let local_server = tokio::spawn(run_server_side_cli_with_callback::<TcpStreamProvider, _>(
        local_addr,
        remote_addr,
        Arc::from("sf-backend"),
        ServerTunnelOptions {
            need_codec: false,
            is_datagram: false,
            keep_alive: false,
            namespace: None,
            force_namespace: false,
        },
        Some(Box::new(move |status| {
            callback_status_changes
                .lock()
                .unwrap()
                .push(status.to_string());
        })),
    ));

    let seen = wait_for_register_attempts(&attempts, 2, Duration::from_secs(5)).await;
    let gap = seen[1].duration_since(seen[0]);
    // The transport ladder's first delay is 100ms, so anything under the reject
    // ladder's 400ms minimum means the rejection was retried as a transport
    // failure — which is what turned one refusal into thousands a minute.
    assert!(
        gap >= Duration::from_millis(300),
        "a retryable rejection retried after {gap:?}, far quicker than the reject ladder"
    );
    assert!(
        !status_changes
            .lock()
            .unwrap()
            .iter()
            .any(|s| s == "connected"),
        "a refused registration reported itself connected: {:?}",
        status_changes.lock().unwrap()
    );

    local_server.abort();
    relay.abort();
}

#[tokio::test]
async fn terminal_registration_rejection_stops_the_worker() {
    let _env = EnvOverrides::set(&[("PB_MAPPER_CONTROL_CONN_POOL_SIZE", "1")]).await;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let remote_addr = listener.local_addr().unwrap();
    let (relay, attempts) =
        spawn_rejecting_relay(listener, remote_addr, "service_transport_mismatch", false);

    let status_changes = Arc::new(Mutex::new(Vec::<String>::new()));
    let callback_status_changes = status_changes.clone();
    let local_addr: SocketAddr = "127.0.0.1:9".parse().unwrap();
    let local_server = tokio::spawn(run_server_side_cli_with_callback::<TcpStreamProvider, _>(
        local_addr,
        remote_addr,
        Arc::from("sf-backend"),
        ServerTunnelOptions {
            need_codec: false,
            is_datagram: false,
            keep_alive: false,
            namespace: None,
            force_namespace: false,
        },
        Some(Box::new(move |status| {
            callback_status_changes
                .lock()
                .unwrap()
                .push(status.to_string());
        })),
    ));

    // The pool returns once its only worker gives up, which is the whole point:
    // a permanent rejection has to end the tunnel rather than retry it forever.
    timeout(Duration::from_secs(5), local_server)
        .await
        .expect("a permanently rejected registration kept retrying")
        .unwrap();
    assert_eq!(attempts.lock().unwrap().len(), 1);
    let statuses = status_changes.lock().unwrap().clone();
    assert!(
        statuses
            .last()
            .is_some_and(|status| status.starts_with("failed: service_transport_mismatch")),
        "a permanent rejection was not reported to the caller: {statuses:?}"
    );

    relay.abort();
}
