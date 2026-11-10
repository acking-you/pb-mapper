//! Fake relays: they authenticate a connection's first request as the real
//! one does and answer it as the case scripts.

use std::sync::{Arc, Mutex};

use pb_mapper_auth::AuthRuntime;
use pb_mapper_client::server::{ServerTunnelOptions, StatusCallback};
use pb_mapper_protocol::MessageWriter;
use pb_mapper_protocol::command::{MessageSerializer, PbConnRequest, PbConnResponse};
use pb_mapper_protocol::secure::{ServerHeaderSession, ServerSecurity};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinSet;
use tokio_util::task::AbortOnDropHandle;

use crate::credential::{StateDir, auth_config};

/// The authentication a relay over a fresh state directory runs, and that directory.
pub(crate) async fn relay_security() -> (ServerSecurity, StateDir) {
    let config = auth_config();
    let state = StateDir(config.state_dir.clone());
    let auth = AuthRuntime::from_process(config).await.unwrap();
    (ServerSecurity::new(auth), state)
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
