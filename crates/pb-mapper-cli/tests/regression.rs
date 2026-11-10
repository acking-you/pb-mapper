// An integration test: a failed `unwrap` is a failed test, which is the report
// this file exists to produce. `allow-unwrap-in-tests` covers `#[cfg(test)]`
// modules but not a `tests/` target, whose whole body is test code.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use pb_mapper_auth::{
    ADMIN_KEY_ID, AuthConfig, AuthRuntime, IssuedTemporaryKey, write_admin_key_file,
};
use pb_mapper_client::client::run_client_side_cli_with_callback;
use pb_mapper_client::server::{
    ServerTunnelOptions, StatusCallback, run_server_side_cli_with_callback,
};
use pb_mapper_core::checksum::{Credential, parse_credential, set_process_msg_header_key};
use pb_mapper_protocol::command::{
    AdminRequest, AdminResponse, LocalServer, MessageSerializer, PbConnRequest, PbConnResponse,
    PbConnStatusReq, PbConnStatusResp, PbServerRequest, PbServiceConnStatus,
};
use pb_mapper_protocol::secure::{ClientHeaderSession, ServerHeaderSession, ServerSecurity};
use pb_mapper_protocol::{
    MessageReader, MessageWriter, get_header_msg_reader, get_header_msg_writer,
};
use pb_mapper_server::run_server_on_listener;
// Nothing that runs `init_test_env` (`Relay`, `Tunnel`): that would also
// install the testkit's tracing here. This binary writes the process
// credential itself; see `init_process_credential`.
use pb_mapper_testkit::{
    TEST_ADMIN_KEY, Transport, V2ControlSpec, admin_credential, admin_key_bytes,
    register_v2_control, reserve_addr,
};
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use tokio_util::task::AbortOnDropHandle;
use uni_stream::stream::{TcpListenerProvider, TcpStreamProvider};

/// Serialises tests that override or require default process-global configuration.
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

/// Connect once something listens on `server_addr`: a relay in a child process
/// binds its port some time after it is spawned.
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

/// The process credential, written once per binary before any relay or client
/// here reads it: the hand-written frames in these cases checksum with it.
///
/// `AuthRuntime::from_process` and `AuthRuntime::start` write it again at every
/// relay start, which this binary cannot avoid; every write is the same key.
fn init_process_credential() {
    static CREDENTIAL: LazyLock<()> =
        LazyLock::new(|| set_process_msg_header_key(Some(TEST_ADMIN_KEY)).unwrap());
    *CREDENTIAL
}

/// A fresh authentication state directory, with the process credential in place.
fn auth_config() -> AuthConfig {
    init_process_credential();
    pb_mapper_testkit::auth_config("regression")
}

/// An authentication state directory, removed when the case ends, passed or not.
struct StateDir(PathBuf);

impl Drop for StateDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A relay in this process, on a socket it bound before it started, so nothing
/// else can take the port in between.
///
/// The server under test is `run_server_with_auth_config`'s: the same
/// `AuthRuntime::from_process` and `run_server_on_listener`, with the bind
/// moved ahead of them.
struct InProcessRelay {
    addr: SocketAddr,
    auth: AuthRuntime,
    shutdown: CancellationToken,
    task: JoinHandle<()>,
    // Last, so the directory goes only after the relay is told to stop.
    _state: StateDir,
}

impl InProcessRelay {
    async fn start(config: AuthConfig) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let state = StateDir(config.state_dir.clone());
        let auth = AuthRuntime::from_process(config).await.unwrap();
        let shutdown = CancellationToken::new();
        let task = tokio::spawn({
            let shutdown = shutdown.clone();
            let auth = auth.clone();
            async move {
                run_server_on_listener(listener, shutdown, None, false, auth)
                    .await
                    .unwrap();
            }
        });
        Self {
            addr,
            auth,
            shutdown,
            task,
            _state: state,
        }
    }

    /// Stop the server, which must not have failed, then the auth actor that
    /// holds the state directory's lock, so the directory can go.
    async fn stop(mut self) {
        self.shutdown.cancel();
        (&mut self.task).await.unwrap();
        self.auth.shutdown_actor().await;
    }
}

impl Drop for InProcessRelay {
    fn drop(&mut self) {
        self.shutdown.cancel();
        self.task.abort();
    }
}

/// Issue temporary keys into `config`'s state before a relay starts on it, one
/// per label, so the relay has to load them from disk.
async fn issue_before_start<const N: usize>(
    config: &AuthConfig,
    labels: [&str; N],
) -> [IssuedTemporaryKey; N] {
    let _ = std::fs::remove_dir_all(&config.state_dir);
    write_admin_key_file(&config.state_dir.join("admin.key"), TEST_ADMIN_KEY, true).unwrap();
    let runtime = AuthRuntime::start(admin_key_bytes(), config.clone())
        .await
        .unwrap();
    let admin = runtime
        .authenticate_presented(ADMIN_KEY_ID, &admin_key_bytes())
        .unwrap();
    let mut issued = Vec::with_capacity(N);
    for label in labels {
        issued.push(
            runtime
                .issue(&admin, Duration::from_secs(120), Some(label.to_string()))
                .await
                .unwrap(),
        );
    }
    // The actor holds `auth.lock` until it ends; the relay takes it next.
    runtime.shutdown_actor().await;
    issued.try_into().unwrap()
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
        // The assertion reads the error log; an inherited filter must not hide it.
        .env("RUST_LOG", "error")
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
    assert!(
        logs.contains("administrator_key_invalid"),
        "status: {}, logs: {logs}",
        output.status
    );
    assert!(!state_dir.join("admin.key").exists());
    let _ = std::fs::remove_dir_all(state_dir);
}

#[tokio::test]
async fn admin_all_preserves_json_output_mode() {
    let config = auth_config();
    issue_before_start(&config, ["json-output"]).await;
    let relay = InProcessRelay::start(config).await;

    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_pb-mapper"));
    command
        .arg("admin")
        .arg("--server")
        .arg(relay.addr.to_string())
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

    relay.stop().await;
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

async fn register_control_conn_parts(
    reader: &mut impl MessageReader,
    writer: &mut impl MessageWriter,
    key: &str,
) -> u32 {
    let request = PbConnRequest::Register {
        direct: None,
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

/// Wait up to two seconds for a stream request on `who`'s control connection.
async fn read_stream_request(reader: &mut impl MessageReader, who: &str) -> (u32, u64) {
    let request = timeout(Duration::from_secs(2), reader.read_msg())
        .await
        .unwrap_or_else(|_| panic!("{who} did not receive a stream request"))
        .unwrap();
    let LocalServer::Stream {
        client_id,
        server_generation,
    } = LocalServer::decode(request).unwrap()
    else {
        panic!("{who} received another local server control message");
    };
    (client_id, server_generation)
}

/// A healthy provider: register `key` as a legacy control connection, report it
/// `ready`, then serve one stream request — acknowledge it and open its data
/// connection, which goes to `stream` — and park.
async fn serve_one_stream(
    addr: SocketAddr,
    key: String,
    who: &'static str,
    ready: Option<oneshot::Sender<()>>,
    stream: oneshot::Sender<TcpStream>,
) {
    let control = wait_for_server(addr).await;
    let (mut reader_stream, mut writer_stream) = control.into_split();
    let mut reader = get_header_msg_reader(&mut reader_stream).unwrap();
    {
        let mut writer = get_header_msg_writer(&mut writer_stream).unwrap();
        register_control_conn_parts(&mut reader, &mut writer, &key).await;
    }
    // Frames after the registration go through a writer of their own.
    let mut writer = get_header_msg_writer(&mut writer_stream).unwrap();
    if let Some(ready) = ready {
        ready.send(()).unwrap();
    }
    let (client_id, server_generation) = read_stream_request(&mut reader, who).await;
    let ack = PbServerRequest::StreamAck {
        client_id,
        server_generation,
    }
    .encode()
    .unwrap();
    writer.write_msg(&ack).await.unwrap();

    let mut data = TcpStream::connect(addr).await.unwrap();
    let request = PbConnRequest::Stream {
        data_protocol: None,
        key,
        dst_id: client_id,
        server_generation,
    }
    .encode()
    .unwrap();
    let mut data_writer = get_header_msg_writer(&mut data).unwrap();
    data_writer.write_msg(&request).await.unwrap();
    stream.send(data).unwrap();
    std::future::pending::<()>().await;
}

/// How a control connection that never opens a data stream ends.
enum StaleEnd {
    /// It reads nothing more.
    Park,
    /// It receives a stream request and leaves it unacknowledged.
    IgnoreStream,
    /// As `IgnoreStream`, then waits for the relay to retire it and reports that.
    IgnoreStreamUntilRetired(oneshot::Sender<()>),
    /// It acknowledges its first stream request and opens nothing.
    AckStream,
}

/// A stale provider: register `key` as a legacy control connection, report it
/// `ready`, and end as `end` says.
async fn stale_control(addr: SocketAddr, key: String, ready: oneshot::Sender<()>, end: StaleEnd) {
    let control = wait_for_server(addr).await;
    let (mut reader_stream, mut writer_stream) = control.into_split();
    let mut reader = get_header_msg_reader(&mut reader_stream).unwrap();
    {
        let mut writer = get_header_msg_writer(&mut writer_stream).unwrap();
        register_control_conn_parts(&mut reader, &mut writer, &key).await;
    }
    ready.send(()).unwrap();
    match end {
        StaleEnd::Park => {}
        StaleEnd::IgnoreStream => {
            read_stream_request(&mut reader, "stale control").await;
        }
        StaleEnd::IgnoreStreamUntilRetired(retired) => {
            read_stream_request(&mut reader, "stale control").await;
            // A missed stream ACK first receives a two-second confirmation grace.
            let closed = timeout(Duration::from_secs(3), reader.read_msg())
                .await
                .expect("stale control was not retired by the server");
            if let Ok(bytes) = closed {
                assert!(bytes.is_empty());
            }
            retired.send(()).unwrap();
            return;
        }
        StaleEnd::AckStream => {
            let (client_id, server_generation) =
                read_stream_request(&mut reader, "stale control").await;
            let mut writer = get_header_msg_writer(&mut writer_stream).unwrap();
            let ack = PbServerRequest::StreamAck {
                client_id,
                server_generation,
            }
            .encode()
            .unwrap();
            writer.write_msg(&ack).await.unwrap();
        }
    }
    std::future::pending::<()>().await;
}

/// Subscribe to `key` on the relay at `addr` with plain frames.
async fn subscribe(addr: SocketAddr, key: &str) -> TcpStream {
    let mut client = TcpStream::connect(addr).await.unwrap();
    let request = PbConnRequest::Subcribe {
        data_protocol: None,
        key: key.to_string(),
    }
    .encode()
    .unwrap();
    let mut writer = get_header_msg_writer(&mut client).unwrap();
    writer.write_msg(&request).await.unwrap();
    client
}

async fn read_status_keys(server_addr: SocketAddr) -> Vec<String> {
    let mut stream = TcpStream::connect(server_addr).await.unwrap();
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
    let mut stream = TcpStream::connect(server_addr).await.unwrap();
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

/// What a [`FakeRelay`] does with one authenticated request.
enum Reply {
    /// Answer, then close the connection.
    Respond(PbConnResponse),
    /// Answer, then hold the connection open.
    RespondAndHold(PbConnResponse),
    /// Hold the connection open without answering.
    Hold,
    /// Close the connection without answering.
    Close,
}

/// The authentication a relay over a fresh state directory runs, and that directory.
async fn relay_security() -> (ServerSecurity, StateDir) {
    let config = auth_config();
    let state = StateDir(config.state_dir.clone());
    let auth = AuthRuntime::from_process(config).await.unwrap();
    (ServerSecurity::new(auth), state)
}

/// A relay that authenticates the first request on each connection as the real
/// one does, and answers it as `reply` says. Dropping it stops it.
struct FakeRelay {
    _task: AbortOnDropHandle<()>,
    // Last, so the directory goes only after the relay is told to stop.
    _state: StateDir,
}

impl FakeRelay {
    async fn start<F>(listener: TcpListener, reply: F) -> Self
    where
        F: Fn(PbConnRequest) -> Reply + Send + Sync + 'static,
    {
        let (security, state) = relay_security().await;
        let reply = Arc::new(reply);
        let task = tokio::spawn(async move {
            let mut connections = JoinSet::new();
            loop {
                tokio::select! {
                    Some(_) = connections.join_next(), if !connections.is_empty() => {}
                    accepted = listener.accept() => {
                        let (mut stream, _) = accepted.unwrap();
                        let security = security.clone();
                        let reply = reply.clone();
                        connections.spawn(async move {
                            let (request, session) = read_secure_request(&security, &mut stream).await;
                            let (response, hold) = match reply(request) {
                                Reply::Respond(response) => (Some(response), false),
                                Reply::RespondAndHold(response) => (Some(response), true),
                                Reply::Hold => (None, true),
                                Reply::Close => (None, false),
                            };
                            if let Some(response) = response {
                                let mut writer = session.response_writer(&mut stream).unwrap();
                                writer.write_msg(&response.encode().unwrap()).await.unwrap();
                            }
                            if hold {
                                std::future::pending::<()>().await;
                            }
                        });
                    }
                }
            }
        });
        Self {
            _task: AbortOnDropHandle::new(task),
            _state: state,
        }
    }
}

/// `register`'s options for a plain TCP service in the credential's own namespace.
fn plain_tcp_options() -> ServerTunnelOptions {
    ServerTunnelOptions {
        need_codec: false,
        is_datagram: false,
        keep_alive: false,
        namespace: None,
        force_namespace: false,
    }
}

/// Every status a tunnel reports, in order, and the callback that records them,
/// for `register` and `connect` alike: both take this callback type.
fn status_recorder() -> (Arc<Mutex<Vec<String>>>, StatusCallback) {
    let statuses = Arc::new(Mutex::new(Vec::new()));
    let recorder = statuses.clone();
    let callback = Box::new(move |status: &str| recorder.lock().unwrap().push(status.to_string()));
    (statuses, callback)
}

#[tokio::test]
async fn temporary_credentials_are_isolated_denied_admin_and_revoked_live() {
    let config = auth_config();
    let [first, second] = issue_before_start(&config, ["first", "second"]).await;
    let relay = InProcessRelay::start(config).await;
    let server_addr = relay.addr;

    let first_credential = parse_credential(&first.credential).unwrap();
    let second_credential = parse_credential(&second.credential).unwrap();
    let register_request = |service: &str| PbConnRequest::Register {
        direct: None,
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
        direct_query: None,
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
        AdminRequest::NamespaceAllocate,
        AdminRequest::KeyIssueScoped {
            namespace: 1,
            ttl_seconds: 60,
            label: None,
        },
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

    let (_, _, revoked) = send_v2_request(
        server_addr,
        &admin_credential(),
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

    relay.stop().await;
}

#[tokio::test]
async fn revoking_subscriber_credential_closes_cross_credential_data_stream() {
    let config = auth_config();
    let [issued] = issue_before_start(&config, ["active-stream"]).await;
    let relay = InProcessRelay::start(config).await;
    let server_addr = relay.addr;

    let credential = parse_credential(&issued.credential).unwrap();
    let admin = admin_credential();
    let service = "revoked-stream";
    let mut control = TcpStream::connect(server_addr).await.unwrap();
    let control_session = ClientHeaderSession::new_v2(&admin).unwrap();
    let register = PbConnRequest::RegisterScoped {
        direct: None,
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

    let mut subscriber = TcpStream::connect(server_addr).await.unwrap();
    let subscriber_session = ClientHeaderSession::new_v2(&credential).unwrap();
    subscriber_session
        .write_initial(
            &mut subscriber,
            &PbConnRequest::Subcribe {
                data_protocol: None,
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

    let mut provider = TcpStream::connect(server_addr).await.unwrap();
    let provider_session = ClientHeaderSession::new_v2(&admin).unwrap();
    provider_session
        .write_initial(
            &mut provider,
            &PbConnRequest::StreamScoped {
                data_protocol: None,
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
        &admin,
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

    relay.stop().await;
}

#[tokio::test]
async fn status_service_reports_registered_v2_control_connection() {
    let relay = InProcessRelay::start(auth_config()).await;

    let key = "sf-backend";
    let control = TcpStream::connect(relay.addr).await.unwrap();
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

    let mut status = TcpStream::connect(relay.addr).await.unwrap();
    let request = PbConnRequest::Status(PbConnStatusReq::Service {
        direct_query: None,
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
        direct: _,
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

    relay.stop().await;
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
    let register_count = AtomicUsize::new(0);
    let (second_register_tx, second_register_rx) = oneshot::channel();
    let second_register_tx = Mutex::new(Some(second_register_tx));
    // Registers are accepted and held; every status says the service has no
    // connection, which is what has to make the local server register again.
    let _relay = FakeRelay::start(remote_listener, move |request| match request {
        PbConnRequest::Register { key, .. } => {
            let count = register_count.fetch_add(1, Ordering::SeqCst) + 1;
            // Reported as the second register arrives, ahead of its answer:
            // its arrival is all this case waits for.
            if count == 2
                && let Some(tx) = second_register_tx.lock().unwrap().take()
            {
                tx.send(()).unwrap();
            }
            tracing::debug!(key, count, "fake server accepted register");
            Reply::RespondAndHold(PbConnResponse::RegisterV2 {
                direct: None,
                conn_id: count as u32,
                generation: count as u64,
                lease_ttl_ms: 150,
            })
        }
        PbConnRequest::Status(PbConnStatusReq::Service {
            direct_query: _,
            key,
        }) => Reply::Respond(PbConnResponse::Status(PbConnStatusResp::Service {
            direct: None,
            key,
            connections: Vec::new(),
        })),
        PbConnRequest::Status(PbConnStatusReq::Keys) => {
            Reply::Respond(PbConnResponse::Status(PbConnStatusResp::Keys(Vec::new())))
        }
        _ => Reply::Close,
    })
    .await;

    let local_addr: SocketAddr = "127.0.0.1:9".parse().unwrap();
    let local_server = tokio::spawn(run_server_side_cli_with_callback::<TcpStreamProvider, _>(
        local_addr,
        remote_addr,
        Arc::from("sf-backend"),
        plain_tcp_options(),
        None,
    ));

    timeout(Duration::from_secs(2), second_register_rx)
        .await
        .expect("local server did not reconnect after remote status lost its registration")
        .unwrap();

    local_server.abort();
}

#[tokio::test]
async fn client_closes_initial_status_probe_after_key_check() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let remote_addr = listener.local_addr().unwrap();
    let (security, _state) = relay_security().await;

    let fake_server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let (request, session) = read_secure_request(&security, &mut stream).await;
        let PbConnRequest::Status(PbConnStatusReq::Service {
            direct_query: _,
            key,
        }) = request
        else {
            panic!("client did not use service status for initial key check");
        };

        let response = PbConnResponse::Status(PbConnStatusResp::Service {
            direct: None,
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

    let local_addr = reserve_addr(Transport::Tcp).await;

    let remote_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let remote_addr = remote_listener.local_addr().unwrap();
    let failed_status_responses = Arc::new(AtomicUsize::new(0));
    let status_count = Arc::new(AtomicUsize::new(0));

    // Every status succeeds unless the case has asked for failures; the
    // subscription itself is held open, unanswered.
    let relay_failures = failed_status_responses.clone();
    let relay_status_count = status_count.clone();
    let _relay = FakeRelay::start(remote_listener, move |request| {
        let should_fail = || {
            relay_status_count.fetch_add(1, Ordering::SeqCst);
            relay_failures
                .try_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                    remaining.checked_sub(1)
                })
                .is_ok()
        };
        match request {
            PbConnRequest::Status(PbConnStatusReq::Service {
                direct_query: _,
                key,
            }) => {
                let connections = if should_fail() {
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
                Reply::Respond(PbConnResponse::Status(PbConnStatusResp::Service {
                    direct: None,
                    key,
                    connections,
                }))
            }
            PbConnRequest::Status(PbConnStatusReq::Keys) => {
                let keys = if should_fail() {
                    Vec::new()
                } else {
                    vec!["sf-backend".to_string()]
                };
                Reply::Respond(PbConnResponse::Status(PbConnStatusResp::Keys(keys)))
            }
            PbConnRequest::Subcribe { .. } => Reply::Hold,
            _ => Reply::Close,
        }
    })
    .await;

    let (status_changes, record_status) = status_recorder();
    let client = tokio::spawn(run_client_side_cli_with_callback::<TcpListenerProvider, _>(
        local_addr,
        remote_addr,
        Arc::from("sf-backend"),
        false,
        Some(record_status),
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
}

#[tokio::test]
async fn subscribe_retires_unacked_control_connection() {
    let _config_guard = ENV_OVERRIDE_LOCK.lock().await;
    let relay = InProcessRelay::start(auth_config()).await;

    let key = "sf-backend";
    let (stale_ready_tx, stale_ready_rx) = oneshot::channel();
    let stale_task = tokio::spawn(stale_control(
        relay.addr,
        key.to_string(),
        stale_ready_tx,
        StaleEnd::IgnoreStream,
    ));
    stale_ready_rx.await.unwrap();

    let mut client = subscribe(relay.addr, key).await;
    let mut reader = get_header_msg_reader(&mut client).unwrap();
    let result = timeout(Duration::from_secs(4), reader.read_msg())
        .await
        .expect("subscribe did not finish after stale control timed out");
    assert!(result.is_err());

    // Subscribe failure and the manager's suspect sweep complete independently.
    timeout(Duration::from_secs(3), async {
        loop {
            let keys = read_status_keys(relay.addr).await;
            if !keys.iter().any(|candidate| candidate == key) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("unacked stale control connection kept key registered");

    stale_task.abort();
    relay.stop().await;
}

#[tokio::test]
async fn subscribe_waits_for_replacement_after_retiring_stale_control_connection() {
    let _config_guard = ENV_OVERRIDE_LOCK.lock().await;
    let relay = InProcessRelay::start(auth_config()).await;

    let key = "sf-backend";
    let (stale_ready_tx, stale_ready_rx) = oneshot::channel();
    let (retired_tx, retired_rx) = oneshot::channel();
    let stale_task = tokio::spawn(stale_control(
        relay.addr,
        key.to_string(),
        stale_ready_tx,
        StaleEnd::IgnoreStreamUntilRetired(retired_tx),
    ));
    stale_ready_rx.await.unwrap();

    let (replacement_stream_tx, replacement_stream_rx) = oneshot::channel();
    let replacement_task = tokio::spawn({
        let addr = relay.addr;
        let key = key.to_string();
        async move {
            retired_rx.await.unwrap();
            serve_one_stream(
                addr,
                key,
                "replacement control",
                None,
                replacement_stream_tx,
            )
            .await;
        }
    });

    let mut client = subscribe(relay.addr, key).await;
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
    relay.stop().await;
}

#[tokio::test]
async fn subscribe_missing_key_closes_without_hanging() {
    let relay = InProcessRelay::start(auth_config()).await;

    let mut stream = subscribe(relay.addr, "missing-key").await;
    let mut reader = get_header_msg_reader(&mut stream).unwrap();
    let result = timeout(Duration::from_millis(200), reader.read_msg())
        .await
        .expect("missing-key subscribe hung instead of closing");
    let response = PbConnResponse::decode(result.unwrap()).unwrap();
    let PbConnResponse::Error(error) = response else {
        panic!("expected structured missing-service error");
    };
    assert_eq!(error.code, "service_not_available");

    relay.stop().await;
}

#[tokio::test]
async fn subscribe_bypasses_unacked_stale_control_connection() {
    bypass_unacked_control(false).await;
}

#[tokio::test]
async fn subscribe_failover_reuses_its_namespace_slot_and_rate_token() {
    bypass_unacked_control(true).await;
}

async fn bypass_unacked_control(at_capacity: bool) {
    // Bound by the child relay, so only an address no `:0` bind can be handed.
    let server_addr = reserve_addr(Transport::Tcp).await;
    let config = auth_config();
    let _state = StateDir(config.state_dir.clone());
    // Limits belong only to this relay process, not the parallel test runner.
    let mut server = tokio::process::Command::new(env!("CARGO_BIN_EXE_pb-mapper"))
        .args([
            "server",
            "--port",
            &server_addr.port().to_string(),
            "--legacy-protocol",
            "allow",
        ])
        .arg("--auth-state-dir")
        .arg(&config.state_dir)
        .env("MSG_HEADER_KEY", TEST_ADMIN_KEY)
        .env(
            "PB_MAPPER_MAX_STREAMS_PER_NAMESPACE",
            if at_capacity { "1" } else { "1024" },
        )
        .env(
            "PB_MAPPER_NEW_STREAMS_BURST",
            if at_capacity { "1" } else { "200" },
        )
        .env("PB_MAPPER_NEW_STREAMS_PER_SECOND", "1")
        .env("RUST_LOG", "error")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();

    let key = "sf-backend";
    let (healthy_ready_tx, healthy_ready_rx) = oneshot::channel();
    let (stream_tx, stream_rx) = oneshot::channel();
    let healthy_task = tokio::spawn(serve_one_stream(
        server_addr,
        key.to_string(),
        "healthy control",
        Some(healthy_ready_tx),
        stream_tx,
    ));
    healthy_ready_rx.await.unwrap();

    let (stale_ready_tx, stale_ready_rx) = oneshot::channel();
    let stale_task = tokio::spawn(stale_control(
        server_addr,
        key.to_string(),
        stale_ready_tx,
        StaleEnd::Park,
    ));
    stale_ready_rx.await.unwrap();

    let mut client = subscribe(server_addr, key).await;
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
    server.kill().await.unwrap();
    server.wait().await.unwrap();
}

#[tokio::test]
async fn subscribe_bypasses_acked_control_connection_without_stream() {
    let relay = InProcessRelay::start(auth_config()).await;

    let key = "sf-backend";
    let (healthy_ready_tx, healthy_ready_rx) = oneshot::channel();
    let (stream_tx, stream_rx) = oneshot::channel();
    let healthy_task = tokio::spawn(serve_one_stream(
        relay.addr,
        key.to_string(),
        "healthy control",
        Some(healthy_ready_tx),
        stream_tx,
    ));
    healthy_ready_rx.await.unwrap();

    let (stale_ready_tx, stale_ready_rx) = oneshot::channel();
    let stale_task = tokio::spawn(stale_control(
        relay.addr,
        key.to_string(),
        stale_ready_tx,
        StaleEnd::AckStream,
    ));
    stale_ready_rx.await.unwrap();

    let mut client = subscribe(relay.addr, key).await;
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
    relay.stop().await;
}

/// A fake relay that refuses every registration with one error.
///
/// Returns the timestamps of the register attempts it saw, which is what the two
/// cases below actually assert on: how long the client waited before trying
/// again, and whether it tried again at all.
async fn rejecting_relay(
    listener: TcpListener,
    code: &'static str,
    retryable: bool,
) -> (FakeRelay, Arc<Mutex<Vec<tokio::time::Instant>>>) {
    let attempts = Arc::new(Mutex::new(Vec::new()));
    let relay_attempts = attempts.clone();
    let relay = FakeRelay::start(listener, move |request| match request {
        PbConnRequest::Register { .. } | PbConnRequest::RegisterScoped { .. } => {
            relay_attempts
                .lock()
                .unwrap()
                .push(tokio::time::Instant::now());
            Reply::RespondAndHold(PbConnResponse::error(
                code,
                "refused by the test relay",
                retryable,
            ))
        }
        // The registration probe, which the client runs against a
        // relay it has not registered with yet.
        PbConnRequest::Status(PbConnStatusReq::Service {
            direct_query: _,
            key,
        }) => Reply::RespondAndHold(PbConnResponse::Status(PbConnStatusResp::Service {
            direct: None,
            key,
            connections: Vec::new(),
        })),
        PbConnRequest::Status(PbConnStatusReq::Keys) => {
            Reply::RespondAndHold(PbConnResponse::Status(PbConnStatusResp::Keys(Vec::new())))
        }
        _ => Reply::Close,
    })
    .await;
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
    let (_relay, attempts) =
        rejecting_relay(listener, "service_connection_limit_exceeded", true).await;

    let (status_changes, record_status) = status_recorder();
    let local_addr: SocketAddr = "127.0.0.1:9".parse().unwrap();
    let local_server = tokio::spawn(run_server_side_cli_with_callback::<TcpStreamProvider, _>(
        local_addr,
        remote_addr,
        Arc::from("sf-backend"),
        plain_tcp_options(),
        Some(record_status),
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
}

#[tokio::test]
async fn terminal_registration_rejection_stops_the_worker() {
    let _env = EnvOverrides::set(&[("PB_MAPPER_CONTROL_CONN_POOL_SIZE", "1")]).await;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let remote_addr = listener.local_addr().unwrap();
    let (_relay, attempts) = rejecting_relay(listener, "service_transport_mismatch", false).await;

    let (status_changes, record_status) = status_recorder();
    let local_addr: SocketAddr = "127.0.0.1:9".parse().unwrap();
    let local_server = tokio::spawn(run_server_side_cli_with_callback::<TcpStreamProvider, _>(
        local_addr,
        remote_addr,
        Arc::from("sf-backend"),
        plain_tcp_options(),
        Some(record_status),
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
}
