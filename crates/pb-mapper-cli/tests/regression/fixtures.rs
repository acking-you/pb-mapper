//! Relays and scripted peers that `regression` and `regression_cli` share:
//! a relay in this process, and control connections and subscribers that
//! speak the legacy framing by hand. `regression_cli` includes this file by
//! path.
//!
//! Every relay here is listening by the time a case knows its address, so
//! peers connect without waiting for one to start.

use std::net::SocketAddr;
use std::time::Duration;

use pb_mapper_auth::{
    ADMIN_KEY_ID, AuthConfig, AuthRuntime, IssuedTemporaryKey, write_admin_key_file,
};
use pb_mapper_protocol::command::{
    LocalServer, MessageSerializer, PbConnRequest, PbConnResponse, PbServerRequest,
};
use pb_mapper_protocol::{
    MessageReader, MessageWriter, get_header_msg_reader, get_header_msg_writer,
};
use pb_mapper_server::run_server_on_listener;
use pb_mapper_testkit::{TEST_ADMIN_KEY, admin_key_bytes};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

use crate::credential::StateDir;

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

/// Register `key` as a legacy control connection over `reader` and `writer`,
/// and return the relay's connection id for it.
pub(crate) async fn register_control_conn_parts(
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
pub(crate) async fn read_stream_request(reader: &mut impl MessageReader, who: &str) -> (u32, u64) {
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
    let control = TcpStream::connect(addr).await.unwrap();
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
