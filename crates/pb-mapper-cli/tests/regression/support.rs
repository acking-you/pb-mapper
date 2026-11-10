//! The fixtures every regression case shares: the environment and process
//! credential this binary owns, in-process and fake relays, and scripted
//! control connections.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use pb_mapper_auth::{
    ADMIN_KEY_ID, AuthConfig, AuthRuntime, IssuedTemporaryKey, write_admin_key_file,
};
use pb_mapper_client::server::{ServerTunnelOptions, StatusCallback};
use pb_mapper_core::checksum::{Credential, set_process_msg_header_key};
use pb_mapper_protocol::command::{
    LocalServer, MessageSerializer, PbConnRequest, PbConnResponse, PbConnStatusReq,
    PbConnStatusResp, PbServerRequest,
};
use pb_mapper_protocol::secure::{ClientHeaderSession, ServerHeaderSession, ServerSecurity};
use pb_mapper_protocol::{
    MessageReader, MessageWriter, get_header_msg_reader, get_header_msg_writer,
};
use pb_mapper_server::run_server_on_listener;
use pb_mapper_testkit::{TEST_ADMIN_KEY, admin_key_bytes};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use tokio_util::task::AbortOnDropHandle;

/// Serialises tests that override or require default process-global configuration.
///
/// The environment is one namespace shared by every test in this binary, and
/// several of these tests override the *same* variable —
/// `PB_MAPPER_CONTROL_CONN_POOL_SIZE`, so that one control worker serves the
/// registration and its retries are countable. Run concurrently, one test's
/// guard restores the variable while another test's tunnel is still reading it,
/// and that tunnel silently gets the default two-worker pool: two register
/// attempts where the test asserts one.
pub(crate) static ENV_OVERRIDE_LOCK: std::sync::LazyLock<tokio::sync::Mutex<()>> =
    std::sync::LazyLock::new(|| tokio::sync::Mutex::new(()));

/// A set of environment overrides held for the duration of one test.
///
/// Restores every previous value on drop, and releases the lock only then, so no
/// two overriding tests overlap.
pub(crate) struct EnvOverrides {
    _lock: tokio::sync::MutexGuard<'static, ()>,
    vars: Vec<EnvVarGuard>,
}

impl EnvOverrides {
    pub(crate) async fn set(overrides: &[(&'static str, &'static str)]) -> Self {
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

/// Connect once something listens on `server_addr`: a relay that binds its own
/// port, in a child process or a task, does so some time after it is spawned.
pub(crate) async fn wait_for_server(server_addr: SocketAddr) -> TcpStream {
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
pub(crate) fn auth_config() -> AuthConfig {
    init_process_credential();
    pb_mapper_testkit::auth_config("regression")
}

/// An authentication state directory, removed when the case ends, passed or not.
pub(crate) struct StateDir(pub(crate) PathBuf);

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
pub(crate) struct InProcessRelay {
    pub(crate) addr: SocketAddr,
    auth: AuthRuntime,
    shutdown: CancellationToken,
    task: JoinHandle<()>,
    // Last, so the directory goes only after the relay is told to stop.
    _state: StateDir,
}

impl InProcessRelay {
    pub(crate) async fn start(config: AuthConfig) -> Self {
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
    pub(crate) async fn stop(mut self) {
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
pub(crate) async fn issue_before_start<const N: usize>(
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

pub(crate) async fn read_secure_request(
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
pub(crate) async fn serve_one_stream(
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
pub(crate) enum StaleEnd {
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
pub(crate) async fn stale_control(
    addr: SocketAddr,
    key: String,
    ready: oneshot::Sender<()>,
    end: StaleEnd,
) {
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
pub(crate) async fn subscribe(addr: SocketAddr, key: &str) -> TcpStream {
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

pub(crate) async fn read_status_keys(server_addr: SocketAddr) -> Vec<String> {
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

pub(crate) async fn send_v2_request(
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
pub(crate) enum Reply {
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
pub(crate) async fn relay_security() -> (ServerSecurity, StateDir) {
    let config = auth_config();
    let state = StateDir(config.state_dir.clone());
    let auth = AuthRuntime::from_process(config).await.unwrap();
    (ServerSecurity::new(auth), state)
}

/// A relay that authenticates the first request on each connection as the real
/// one does, and answers it as `reply` says. Dropping it stops it.
pub(crate) struct FakeRelay {
    _task: AbortOnDropHandle<()>,
    // Last, so the directory goes only after the relay is told to stop.
    _state: StateDir,
}

impl FakeRelay {
    pub(crate) async fn start<F>(listener: TcpListener, reply: F) -> Self
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
pub(crate) fn plain_tcp_options() -> ServerTunnelOptions {
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
pub(crate) fn status_recorder() -> (Arc<Mutex<Vec<String>>>, StatusCallback) {
    let statuses = Arc::new(Mutex::new(Vec::new()));
    let recorder = statuses.clone();
    let callback = Box::new(move |status: &str| recorder.lock().unwrap().push(status.to_string()));
    (statuses, callback)
}
