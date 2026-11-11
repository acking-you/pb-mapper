//! Relay orchestration and the serialized routing-manager event loop.
//!
//! ```text
//! listener task ---- Accept -------+
//! control tasks ---- Register -----+-> ManagerTask loop -> routing maps / quotas
//! subscriber ------- Subcribe -----+                    -> ConnTask responses
//! provider stream -- Stream/Ack ---+
//! ```
//!
//! The manager loop is the single writer for connection IDs, registrations, pending
//! streams, per-namespace counts, and rate limits. Socket I/O runs in spawned connection
//! tasks and communicates with this state only through typed tasks.

use std::sync::Arc;
use std::time::Instant;

use pb_mapper_auth::{AuthConfig, AuthRuntime};
use pb_mapper_core::config::{
    control_io_timeout, keep_alive_from_env, server_lease_sweep_interval,
};
use pb_mapper_protocol::secure::ServerSecurity;
use tokio::net::{TcpListener, ToSocketAddrs};
use tokio_util::sync::CancellationToken;

use crate::connection::handle_listener;
use crate::manager::{TaskManager, TrySendError};
#[cfg(feature = "relay-quic")]
use crate::quic;
use crate::routing::{
    NamespaceRateLimits, PendingStreams, ReplacementWaiters, RoutingConfig, ServerConnMap,
};
use crate::task::ManagerTask;
use crate::{DirectServerConfig, RelayServerOptions, ServerStatusInfo, direct};

mod inventory;
mod lease;
mod router;
mod subscribe;

/// Run a server that takes its keep-alive setting from the environment.
///
/// Callers that own the setting — the binary, and the UI, which has a toggle for
/// it — should use [`run_server_with_shutdown`] and pass it explicitly.
pub async fn run_server<A: ToSocketAddrs>(addr: A) -> std::io::Result<()> {
    run_server_with_shutdown(addr, CancellationToken::new(), None, keep_alive_from_env()).await
}

pub async fn run_server_with_shutdown<A: ToSocketAddrs>(
    addr: A,
    shutdown_token: CancellationToken,
    status_channel: Option<
        tokio::sync::mpsc::UnboundedReceiver<tokio::sync::oneshot::Sender<ServerStatusInfo>>,
    >,
    keep_alive: bool,
) -> std::io::Result<()> {
    run_server_with_auth_config(
        addr,
        shutdown_token,
        status_channel,
        keep_alive,
        AuthConfig::default(),
    )
    .await
}

pub async fn run_server_with_auth_config<A: ToSocketAddrs>(
    addr: A,
    shutdown_token: CancellationToken,
    status_channel: Option<
        tokio::sync::mpsc::UnboundedReceiver<tokio::sync::oneshot::Sender<ServerStatusInfo>>,
    >,
    keep_alive: bool,
    auth_config: AuthConfig,
) -> std::io::Result<()> {
    let auth = AuthRuntime::from_process(auth_config)
        .await
        .map_err(std::io::Error::other)?;
    let listener = pb_mapper_core::listener::bind_first(addr).await?;
    run_server_on_listener(listener, shutdown_token, status_channel, keep_alive, auth).await
}

pub async fn run_server_on_listener(
    listener: TcpListener,
    shutdown_token: CancellationToken,
    status_channel: Option<
        tokio::sync::mpsc::UnboundedReceiver<tokio::sync::oneshot::Sender<ServerStatusInfo>>,
    >,
    keep_alive: bool,
    auth: AuthRuntime,
) -> std::io::Result<()> {
    run_server_on_listener_with_direct(
        listener,
        shutdown_token,
        status_channel,
        keep_alive,
        auth,
        None,
    )
    .await
}

pub async fn run_server_on_listener_with_direct(
    listener: TcpListener,
    shutdown_token: CancellationToken,
    status_channel: Option<
        tokio::sync::mpsc::UnboundedReceiver<tokio::sync::oneshot::Sender<ServerStatusInfo>>,
    >,
    keep_alive: bool,
    auth: AuthRuntime,
    direct_config: Option<DirectServerConfig>,
) -> std::io::Result<()> {
    run_server_on_listener_with_options(
        listener,
        shutdown_token,
        status_channel,
        keep_alive,
        auth,
        RelayServerOptions {
            direct: direct_config,
            quic: None,
        },
    )
    .await
}

pub async fn run_server_on_listener_with_options(
    listener: TcpListener,
    shutdown_token: CancellationToken,
    status_channel: Option<
        tokio::sync::mpsc::UnboundedReceiver<tokio::sync::oneshot::Sender<ServerStatusInfo>>,
    >,
    keep_alive: bool,
    auth: AuthRuntime,
    options: RelayServerOptions,
) -> std::io::Result<()> {
    let (capability, stun_task) = direct::bind(options.direct)
        .await?
        .map(|stun| (stun.capability, stun.task))
        .unzip();
    let direct_state = direct::DirectState::new(capability);
    let security = ServerSecurity::new(auth);
    let manager = TaskManager::new();
    let config = RoutingConfig::from_env();

    let listen_addr = listener.local_addr()?;
    tracing::info!(
        event = "pb_server_listening",
        listen_addr = %listen_addr,
        control_timeout = ?control_io_timeout(),
        "pb-mapper server is listening"
    );

    // Clients reach QUIC at the IP they reached TCP at, so a listener on the
    // other address family is one some of them cannot use.
    if let Some(quic) = &options.quic
        && quic.listen.is_ipv6() != listen_addr.is_ipv6()
    {
        tracing::warn!(
            event = "relay_quic_family_mismatch",
            tcp = %listen_addr,
            quic = %quic.listen,
            "QUIC and TCP listen on different address families; clients on TCP's cannot use QUIC"
        );
    }
    let quic_stop = CancellationToken::new();
    #[cfg(feature = "relay-quic")]
    let (quic_task, quic_capability) = quic::start(
        options.quic,
        security.clone(),
        manager.get_task_sender(),
        quic_stop.clone(),
    )
    .await?
    .map(|quic| (quic.task, quic.endpoint))
    .unzip();
    #[cfg(not(feature = "relay-quic"))]
    let (quic_task, quic_capability): (
        Option<tokio_util::task::AbortOnDropHandle<()>>,
        Option<Arc<pb_mapper_protocol::command::QuicEndpoint>>,
    ) = {
        if options.quic.is_some() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "binary built without relay-quic",
            ));
        }
        (None, None)
    };
    let task_sender = manager.get_task_sender();
    let shutdown_token_clone = shutdown_token.clone();

    let listener_handle = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
        tokio::select! {
            biased;
            _ = shutdown_token_clone.cancelled() => {
                tracing::info!("Listener shutdown requested");
            }
            result = handle_listener(task_sender, listener, keep_alive) => {
                if let Err(e) = result {
                    tracing::error!("Listener error: {}", snafu::Report::from_error(e));
                }
            }
        }
    }));

    let start_time = std::time::Instant::now();

    let status_forward_handle = status_channel.map(|mut receiver| {
        let status_sender = manager.get_task_sender();
        tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
            while let Some(response_sender) = receiver.recv().await {
                if status_sender
                    .send(ManagerTask::StatusQuery { response_sender })
                    .await
                    .is_err()
                {
                    break;
                }
            }
        }))
    });

    let shutdown_handle = {
        let shutdown_sender = manager.get_task_sender();
        tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
            shutdown_token.cancelled().await;
            let _ = shutdown_sender.send(ManagerTask::Shutdown).await;
        }))
    };

    // Drives lease expiry. A tick is a request to sweep, not a deadline: it is
    // dropped rather than queued behind a busy manager, since the next tick asks
    // for the same thing and a backlog of sweeps would only pile up while the loop
    // was already too busy to serve them.
    let sweep_handle = {
        let sweep_sender = manager.get_task_sender();
        let sweep_interval = server_lease_sweep_interval();
        tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
            let mut ticker = tokio::time::interval(sweep_interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                ticker.tick().await;
                // A tick that finds the queue full is dropped, not retried: the
                // next tick asks for the same sweep, and a backlog of them would
                // only grow while the manager was already behind.
                if let Err(TrySendError::Closed(_)) =
                    sweep_sender.try_send(ManagerTask::SweepServerLeases)
                {
                    break;
                }
            }
        }))
    };

    let mut router = Router::new(
        manager,
        direct_state,
        security,
        quic_capability,
        config,
        start_time,
    );
    loop {
        // `None` cannot happen while the manager holds its own sender.
        let Some(task) = router.manager.wait_for_task().await else {
            break;
        };

        router.reap_finished_tasks();
        if stun_task.as_ref().is_some_and(|task| task.is_finished()) {
            router.direct_state.capability = None;
        }
        if router.handle(task).is_break() {
            break;
        }
    }

    // Abort first, then wait. Dropping a JoinHandle after abort() does not
    // wait for the task to drop its AuthRuntime clone, so a UI restart can
    // still see auth.lock held.
    router.connection_tasks.abort_all();
    while router.connection_tasks.join_next().await.is_some() {}
    // The QUIC listener drains its own connection tasks before it ends.
    quic_stop.cancel();
    if let Some(task) = quic_task {
        let _ = task.await;
    }
    // The STUN task too: its UDP port must be free again by the time this
    // returns, for a relay restarted in the same process.
    abort_and_wait(
        [listener_handle, shutdown_handle, sweep_handle]
            .into_iter()
            .chain(status_forward_handle)
            .chain(stun_task),
    )
    .await;
    router.security.auth().shutdown_actor().await;
    tracing::info!("Server shutdown completed");
    Ok(())
}

/// The routing manager's state, and the one place that changes it.
///
/// Connection IDs, registrations, pending streams, per-namespace counts and rate
/// limits have a single writer: the relay's routing loop, which hands each task to
/// [`Router::handle`]. Nothing here awaits. The loop's only await is the receive
/// that produced the task, and a handler that waited on a connection's mailbox
/// would stall every other service, heartbeat and retirement behind it.
struct Router {
    manager: TaskManager,
    /// Registered control connections, by service key.
    server_conn_map: ServerConnMap,
    pending_streams: PendingStreams,
    replacement_waiters: ReplacementWaiters,
    namespace_rate_limits: NamespaceRateLimits,
    config: RoutingConfig,
    next_server_generation: u64,
    direct_state: direct::DirectState,
    /// Connection tasks and suspicion timers, aborted when the relay stops.
    connection_tasks: tokio::task::JoinSet<()>,
    security: ServerSecurity,
    quic_capability: Option<Arc<pb_mapper_protocol::command::QuicEndpoint>>,
    start_time: Instant,
}

async fn abort_and_wait(
    handles: impl IntoIterator<Item = tokio_util::task::AbortOnDropHandle<()>>,
) {
    let handles: Vec<_> = handles.into_iter().collect();
    for handle in &handles {
        handle.abort();
    }
    for handle in handles {
        let _ = handle.await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routing::{ImutableKey, ServerConnInfo, service_conn_count};
    use pb_mapper_auth::{AuthConfig, AuthRuntime, LegacyProtocolPolicy};
    use pb_mapper_core::conn_id::RemoteConnId;
    use pb_mapper_core::test_support::{
        PROCESS_CREDENTIAL_TEST_LOCK, TEST_ADMIN_KEY_BYTES, TempPath,
    };
    use pb_mapper_protocol::command::CONTROL_PROTOCOL_V2;
    use std::time::Duration;

    /// A retired connection's ID must not be handed out again until its own socket
    /// task has deregistered.
    ///
    /// The race this pins: retirement drops the routing entry while the socket task
    /// is still unwinding, and that task still owns a guard that will send
    /// `DeRegisterServerConn` for the same ID. If the ID were recycled at
    /// retirement, the next `Accept` would take it, a replacement could register
    /// under it, and the old guard's deregistration — which matches on ID alone —
    /// would remove the replacement's sender and its service-map entry instead.
    #[tokio::test]
    async fn a_retired_conn_id_is_withheld_until_its_socket_task_deregisters() {
        let state_dir = TempPath::new("retired-id");
        let auth = AuthRuntime::from_isolated_state(AuthConfig {
            state_dir: state_dir.to_path_buf(),
            ..AuthConfig::default()
        })
        .await
        .unwrap();
        let mut router = Router::new(
            TaskManager::new(),
            direct::DirectState::new(None),
            ServerSecurity::new(auth.clone()),
            None,
            RoutingConfig::from_env(),
            Instant::now(),
        );
        let key: ImutableKey = Arc::from("echo");

        // One registered control connection, holding the first ID the manager hands out.
        let retired = router.manager.get_conn_id(std::iter::empty()).unwrap();
        let (conn_sender, _conn_receiver) = kanal::bounded_async(MAILBOX_CAPACITY);
        router.manager.sign_up_conn_sender(retired, conn_sender);
        router.server_conn_map.insert(
            key.clone(),
            vec![ServerConnInfo::for_test(
                retired.into(),
                1,
                CONTROL_PROTOCOL_V2,
                None,
            )],
        );

        assert!(
            router
                .handle(ManagerTask::RetireServerConn {
                    key: key.clone(),
                    conn_id: retired,
                    reason: "retired by administrator".to_string(),
                })
                .is_continue()
        );
        assert!(
            !router.server_conn_map.contains_key(&key),
            "retirement unwinds the routing entry immediately"
        );
        assert!(
            router.manager.get_conn_sender_chan(&retired).is_none(),
            "the sender goes at retirement"
        );

        // The retired socket task has not unwound yet, so its ID is still spoken for.
        let replacement = router
            .manager
            .get_conn_id(
                router
                    .server_conn_map
                    .iter()
                    .flat_map(|(_, ids)| ids.iter().map(|info| info.conn_id)),
            )
            .unwrap();
        assert_ne!(
            replacement, retired,
            "the retired id was handed to a new connection while its guard was still live"
        );

        // The replacement registers, and only then does the old guard's
        // deregistration arrive. It must not disturb the replacement.
        let (replacement_sender, _replacement_receiver) = kanal::bounded_async(MAILBOX_CAPACITY);
        router
            .manager
            .sign_up_conn_sender(replacement, replacement_sender);
        router.server_conn_map.insert(
            key.clone(),
            vec![ServerConnInfo::for_test(
                replacement.into(),
                2,
                CONTROL_PROTOCOL_V2,
                None,
            )],
        );

        assert!(
            router
                .handle(ManagerTask::DeRegisterServerConn {
                    key: key.clone(),
                    conn_id: retired,
                })
                .is_continue()
        );

        assert!(
            router.manager.get_conn_sender_chan(&replacement).is_some(),
            "the replacement kept its sender"
        );
        assert_eq!(
            service_conn_count(&router.server_conn_map, &key),
            1,
            "the replacement kept its service-map entry"
        );

        // And the ID serves again now that its task is gone, once it has
        // rested.
        assert_eq!(
            router.manager.take_conn_id(
                std::iter::empty(),
                Instant::now() + crate::manager::REGISTRATION_ID_REST
            ),
            Some(retired)
        );

        drop(router);
        auth.shutdown_actor().await;
        drop(auth);
    }

    /// An administrator retires a connection by the ID a listing showed. When
    /// the relay has retired that connection meanwhile and its publisher has
    /// replaced it, which takes the publisher a backoff of two seconds at
    /// most, the ID must not name the replacement: the retire, aimed at the
    /// connection that is gone, retires nothing.
    #[tokio::test]
    async fn a_retire_by_id_does_not_reach_the_replacement_of_the_connection_it_named() {
        let state_dir = TempPath::new("retire-replaced");
        let auth = AuthRuntime::from_isolated_state(AuthConfig {
            state_dir: state_dir.to_path_buf(),
            ..AuthConfig::default()
        })
        .await
        .unwrap();
        let mut router = Router::new(
            TaskManager::new(),
            direct::DirectState::new(None),
            ServerSecurity::new(auth.clone()),
            None,
            RoutingConfig::from_env(),
            Instant::now(),
        );
        let key: ImutableKey = Arc::from("echo");
        let register = |router: &mut Router| {
            let conn_id = router
                .manager
                .get_conn_id(
                    router
                        .server_conn_map
                        .iter()
                        .flat_map(|(_, infos)| infos.iter().map(|info| info.conn_id)),
                )
                .unwrap();
            let (conn_sender, mailbox) = kanal::bounded_async(MAILBOX_CAPACITY);
            let _ = router.handle(ManagerTask::Register {
                key: key.clone(),
                conn_id,
                need_codec: false,
                is_datagram: false,
                protocol_version: CONTROL_PROTOCOL_V2,
                conn_sender,
                retire_token: CancellationToken::new(),
                direct: None,
            });
            (conn_id, mailbox)
        };

        // What an administrator's listing showed, then the relay's own
        // retirement of that connection, its socket's end, and the publisher's
        // replacement.
        let (listed, _listed_mailbox) = register(&mut router);
        let _ = router.handle(ManagerTask::RetireServerConn {
            key: key.clone(),
            conn_id: listed,
            reason: "lease expired".to_string(),
        });
        let _ = router.handle(ManagerTask::DeRegisterServerConn {
            key: key.clone(),
            conn_id: listed,
        });
        let (replacement, _replacement_mailbox) = register(&mut router);

        let (response_sender, retired) = tokio::sync::oneshot::channel();
        let _ = router.handle(ManagerTask::AdminConnectionRetire {
            key: key.clone(),
            conn_id: Some(listed),
            response_sender,
        });
        assert_eq!(
            retired.await.unwrap(),
            0,
            "a retire by the ID of a connection that was gone reached its replacement"
        );
        assert_eq!(
            router.server_conn_map[&key]
                .iter()
                .map(|info| info.conn_id)
                .collect::<Vec<_>>(),
            [replacement]
        );

        drop(router);
        auth.shutdown_actor().await;
        drop(auth);
    }

    /// A connection that ends before it authenticates, as a port scan's does,
    /// still returns the ID its accept took. Each one that did not was an ID
    /// gone for good, and the counter behind fresh ones only climbed.
    #[tokio::test]
    async fn a_connection_that_never_authenticates_returns_its_id() {
        let state_dir = TempPath::new("probe-id");
        let auth = AuthRuntime::from_isolated_state(AuthConfig {
            state_dir: state_dir.to_path_buf(),
            ..AuthConfig::default()
        })
        .await
        .unwrap();
        let mut router = Router::new(
            TaskManager::new(),
            direct::DirectState::new(None),
            ServerSecurity::new(auth.clone()),
            None,
            RoutingConfig::from_env(),
            Instant::now(),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let probe = tokio::net::TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (accepted, peer_addr) = listener.accept().await.unwrap();
        drop(probe);

        assert!(
            router
                .handle(ManagerTask::Accept {
                    stream: accepted.into(),
                    peer_addr,
                })
                .is_continue()
        );
        // Bounds a hang, not a speed: the probe's socket is already closed.
        let returned =
            tokio::time::timeout(Duration::from_secs(10), router.manager.wait_for_task())
                .await
                .expect("the probe's connection ended without returning its id")
                .unwrap();
        assert!(router.handle(returned).is_continue());
        assert_eq!(
            router.manager.get_conn_id(std::iter::empty()),
            Some(RemoteConnId::from(0)),
            "the probe's id serves the next connection"
        );

        drop(router);
        auth.shutdown_actor().await;
        drop(auth);
    }

    /// A direct setup reaches its provider by connection ID. Once the
    /// provider's registration has ended, that ID can serve anyone's next
    /// connection, another tenant's included, so the setup must stop routing
    /// to it: the consumer's cancel reaches no one, not the subscriber of
    /// another namespace that now holds the ID.
    #[tokio::test]
    async fn a_direct_setup_does_not_follow_its_providers_id_to_another_connection() {
        use crate::ConnTask;
        use pb_mapper_auth::ADMIN_KEY_ID;
        use pb_mapper_protocol::direct::{
            DIRECT_VERSION, DirectCapability, DirectClientMessage, DirectEvent, PeerInfo,
        };

        let admin_key = [23; 32];
        let state_dir = TempPath::new("direct-provider-gone");
        let auth = AuthRuntime::start(
            admin_key,
            AuthConfig {
                state_dir: state_dir.to_path_buf(),
                ..AuthConfig::default()
            },
        )
        .await
        .unwrap();
        let admin = auth
            .authenticate_presented(ADMIN_KEY_ID, &admin_key)
            .unwrap();
        let mut router = Router::new(
            TaskManager::new(),
            direct::DirectState::new(Some(DirectCapability {
                version: DIRECT_VERSION,
                stun_addr: "127.0.0.1:10811".parse().unwrap(),
            })),
            ServerSecurity::new(auth.clone()),
            None,
            RoutingConfig::from_env(),
            Instant::now(),
        );
        let register = |router: &mut Router, key: &ImutableKey, direct| {
            let conn_id = router.manager.get_conn_id(std::iter::empty()).unwrap();
            let (conn_sender, mailbox) = kanal::bounded_async(MAILBOX_CAPACITY);
            assert!(
                router
                    .handle(ManagerTask::Register {
                        key: key.clone(),
                        conn_id,
                        need_codec: false,
                        is_datagram: false,
                        protocol_version: CONTROL_PROTOCOL_V2,
                        conn_sender,
                        retire_token: CancellationToken::new(),
                        direct,
                    })
                    .is_continue()
            );
            assert!(matches!(
                mailbox.try_recv(),
                Ok(Some(ConnTask::RegisterResp { .. }))
            ));
            (conn_id, mailbox)
        };

        // One tenant's direct-capable provider, and a consumer's setup with it.
        let echo: ImutableKey = crate::compose_service_key(0, "echo");
        let (provider, provider_mailbox) = register(&mut router, &echo, Some(admin.clone()));
        let consumer = router.manager.get_conn_id(std::iter::empty()).unwrap();
        let (consumer_sender, mut consumer_events) = tokio::sync::mpsc::channel(8);
        let _ = router.handle(ManagerTask::Direct(direct::DirectTask::Connect {
            connection: consumer,
            namespace: 0,
            auth: admin.clone(),
            sender: consumer_sender,
        }));
        assert!(matches!(
            consumer_events.try_recv(),
            Ok(DirectEvent::Ready(_))
        ));
        let _ = router.handle(ManagerTask::Direct(direct::DirectTask::Client {
            connection: consumer,
            message: DirectClientMessage::Open {
                request: 1,
                key: "echo".into(),
                is_datagram: false,
                peer: PeerInfo {
                    certificate: vec![1],
                    candidates: vec![],
                    lan_tcp: None,
                },
            },
        }));
        let Ok(Some(ConnTask::Direct(offer))) = provider_mailbox.try_recv() else {
            panic!("the provider was not offered the setup");
        };
        let DirectEvent::Offer { grant, .. } = *offer else {
            panic!("expected an offer");
        };

        // The provider's registration ends, and its ID serves a subscriber of
        // another namespace, waiting on that namespace's provider.
        let other: ImutableKey = crate::compose_service_key(7, "echo");
        let (_other_provider, _other_mailbox) = register(&mut router, &other, None);
        let _ = router.handle(ManagerTask::DeRegisterServerConn {
            key: echo,
            conn_id: provider,
        });
        let (subscriber_sender, subscriber_mailbox) = kanal::bounded_async(MAILBOX_CAPACITY);
        let _ = router.handle(ManagerTask::Subcribe {
            key: other,
            conn_id: provider,
            conn_sender: subscriber_sender,
            excluded_server_conns: Vec::new(),
            selected: None,
        });
        assert!(matches!(
            subscriber_mailbox.try_recv(),
            Ok(Some(ConnTask::SubcribeResp { .. }))
        ));

        // The consumer gives the setup up.
        let _ = router.handle(ManagerTask::Direct(direct::DirectTask::Client {
            connection: consumer,
            message: DirectClientMessage::Cancel { id: grant.id },
        }));
        while let Ok(Some(task)) = subscriber_mailbox.try_recv() {
            assert!(
                !matches!(task, ConnTask::Direct(_)),
                "another namespace's subscriber received a direct setup's message"
            );
        }

        drop(router);
        auth.shutdown_actor().await;
        drop(auth);
    }

    /// A direct setup's fallback to the relay is pinned to the registration
    /// and generation its grant named, so no replacement can serve it. Once
    /// its subscribe has given that registration up, or the registration is
    /// gone, it is refused as a subscribe pinned to one that never existed
    /// is, not told to wait for a replacement until its recovery deadline.
    #[tokio::test]
    async fn a_pinned_subscribe_is_refused_once_its_provider_cannot_serve_it() {
        use crate::ConnTask;

        let state_dir = TempPath::new("pinned-subscribe");
        let auth = AuthRuntime::from_isolated_state(AuthConfig {
            state_dir: state_dir.to_path_buf(),
            ..AuthConfig::default()
        })
        .await
        .unwrap();
        let mut router = Router::new(
            TaskManager::new(),
            direct::DirectState::new(None),
            ServerSecurity::new(auth.clone()),
            None,
            RoutingConfig::from_env(),
            Instant::now(),
        );
        let key: ImutableKey = crate::compose_service_key(0, "echo");
        let provider = router.manager.get_conn_id(std::iter::empty()).unwrap();
        let (provider_sender, provider_mailbox) = kanal::bounded_async(MAILBOX_CAPACITY);
        let _ = router.handle(ManagerTask::Register {
            key: key.clone(),
            conn_id: provider,
            need_codec: false,
            is_datagram: false,
            protocol_version: CONTROL_PROTOCOL_V2,
            conn_sender: provider_sender,
            retire_token: CancellationToken::new(),
            direct: None,
        });
        let Ok(Some(ConnTask::RegisterResp { generation, .. })) = provider_mailbox.try_recv()
        else {
            panic!("the provider was not registered");
        };
        let pin = (provider, generation);

        let subscriber = router.manager.get_conn_id(std::iter::empty()).unwrap();
        let (subscriber_sender, subscriber_mailbox) = kanal::bounded_async(MAILBOX_CAPACITY);
        let subscribe = |router: &mut Router, selected, excluded_server_conns| {
            let _ = router.handle(ManagerTask::Subcribe {
                key: key.clone(),
                conn_id: subscriber,
                conn_sender: subscriber_sender.clone(),
                excluded_server_conns,
                selected: Some(selected),
            });
            match subscriber_mailbox.try_recv() {
                Ok(Some(answer)) => answer,
                other => panic!("the subscribe was not answered: {other:?}"),
            }
        };

        // What a subscribe pinned to a registration that does not exist hears.
        let ConnTask::SubcribeFailed {
            code: missing,
            retryable: true,
            ..
        } = subscribe(&mut router, (provider, generation + 1), Vec::new())
        else {
            panic!("a subscribe pinned to no registration was not refused");
        };

        // The pinned registration takes the stream request, and never
        // acknowledges it: the subscribe suspects it and asks again without it.
        assert!(matches!(
            subscribe(&mut router, pin, Vec::new()),
            ConnTask::SubcribeResp { .. }
        ));
        let _ = router.handle(ManagerTask::SuspectServerConn {
            key: key.clone(),
            conn_id: provider,
            generation,
        });
        let answer = subscribe(&mut router, pin, vec![pin]);
        assert!(
            matches!(&answer, ConnTask::SubcribeFailed { code, retryable: true, .. } if *code == missing),
            "a pinned subscribe whose provider was suspected heard {answer:?}"
        );

        // Its registration ends.
        let _ = router.handle(ManagerTask::DeRegisterServerConn {
            key: key.clone(),
            conn_id: provider,
        });
        let answer = subscribe(&mut router, pin, vec![pin]);
        assert!(
            matches!(&answer, ConnTask::SubcribeFailed { code, retryable: true, .. } if *code == missing),
            "a pinned subscribe whose provider is gone heard {answer:?}"
        );

        drop(router);
        auth.shutdown_actor().await;
        drop(auth);
    }

    /// The capacity of a connection's `ConnTask` mailbox; the value does not
    /// matter here.
    const MAILBOX_CAPACITY: usize = 8;

    #[tokio::test]
    async fn shutdown_releases_auth_lock_while_a_connection_is_open() {
        let _process_credential_guard = PROCESS_CREDENTIAL_TEST_LOCK.lock().await;
        let state_dir = TempPath::new("shutdown-lock");
        let admin_key = TEST_ADMIN_KEY_BYTES;
        let config = AuthConfig {
            state_dir: state_dir.to_path_buf(),
            max_temporary_keys: 4,
            max_temporary_key_ttl: Duration::from_secs(3600),
            legacy_protocol: LegacyProtocolPolicy::Allow,
        };
        let auth = AuthRuntime::start(admin_key, config.clone()).await.unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let shutdown_token = CancellationToken::new();
        let server = tokio::spawn({
            let shutdown_token = shutdown_token.clone();
            async move { run_server_on_listener(listener, shutdown_token, None, false, auth).await }
        });
        let _client = tokio::net::TcpStream::connect(addr).await.unwrap();
        // Nothing signals that the relay has accepted an unauthenticated socket and
        // spawned its connection task, the state this test shuts down from.
        tokio::time::sleep(Duration::from_millis(80)).await;
        shutdown_token.cancel();
        tokio::time::timeout(Duration::from_secs(2), server)
            .await
            .expect("server shutdown should finish after aborting connections")
            .unwrap()
            .unwrap();
        let restarted = AuthRuntime::start(admin_key, config).await.unwrap();
        restarted.shutdown_actor().await;
        drop(restarted);
    }

    /// A relay restarted in the same process, as the app does, must find its
    /// direct UDP port free once the old one has returned.
    #[cfg(feature = "p2p")]
    #[tokio::test]
    async fn shutdown_releases_the_direct_udp_port() {
        let state_dir = TempPath::new("shutdown-udp");
        let auth = AuthRuntime::from_isolated_state(AuthConfig {
            state_dir: state_dir.to_path_buf(),
            ..AuthConfig::default()
        })
        .await
        .unwrap();
        let udp = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let direct = DirectServerConfig::on_relay_port(
            std::net::Ipv4Addr::LOCALHOST.into(),
            udp.local_addr().unwrap().port(),
        );
        drop(udp);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let shutdown = CancellationToken::new();
        // Stops as soon as it has started.
        shutdown.cancel();
        run_server_on_listener_with_direct(
            listener,
            shutdown,
            None,
            false,
            auth,
            Some(direct.clone()),
        )
        .await
        .unwrap();
        std::net::UdpSocket::bind(direct.listen)
            .expect("the stopped relay still holds its UDP port");
    }
}

#[cfg(test)]
mod cancellation_audit {
    use super::*;
    use std::{
        future::{Future, poll_fn},
        task::Poll,
    };
    #[tokio::test]
    async fn dropping_relay_future_must_release_its_listener() {
        let state_dir = pb_mapper_core::test_support::TempPath::new("audit-abort");
        let config = AuthConfig {
            state_dir: state_dir.to_path_buf(),
            ..AuthConfig::default()
        };
        let auth = AuthRuntime::from_isolated_state(config).await.unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let shutdown = CancellationToken::new();
        let mut relay = Box::pin(run_server_on_listener(
            listener,
            shutdown.clone(),
            None,
            false,
            auth.clone(),
        ));
        poll_fn(|cx| {
            assert!(relay.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        drop(relay);
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
        let rebound = TcpListener::bind(addr).await;
        let leaked = rebound.is_err();
        shutdown.cancel();
        auth.shutdown_actor().await;
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
        drop(auth);
        assert!(
            !leaked,
            "relay future dropped, but detached listener task still owns {addr}"
        );
    }
}
