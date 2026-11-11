//! Several keys in one allocated namespace, over everything that authenticates:
//! the TCP and QUIC relay carriers, and direct setup. Driven through the SDK's
//! administrator API, the way a backend holding the administrator key would.

use std::net::SocketAddr;
use std::time::Duration;

use pb_mapper_client::sdk::{
    Client, ConnectOptions, ConnectRequest, Error, IssuedKey, PathPolicy, RegisterOptions,
    RegisterRequest, RelayTransport, Transport,
};
use pb_mapper_core::checksum::parse_credential;
use pb_mapper_server::RelayServerOptions;
use pb_mapper_testkit::{
    READY_TIMEOUT, Relay, admin_client, loopback_direct, loopback_quic, publish, subscribe,
    tcp_echo,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const LONG_TTL: Duration = Duration::from_secs(600);

async fn relay_with_everything(label: &str) -> Relay {
    Relay::start_with_options(
        label,
        RelayServerOptions {
            direct: Some(loopback_direct()),
            quic: Some(loopback_quic()),
        },
    )
    .await
}

fn client_for(relay: &Relay, key: &IssuedKey, carrier: RelayTransport) -> Client {
    Client::from_credential(
        relay.addr().to_string(),
        parse_credential(&key.credential).unwrap(),
        false,
        None,
    )
    .with_relay_transport(carrier)
    .unwrap()
}

fn register_options(allow_direct: bool) -> RegisterOptions {
    RegisterOptions {
        allow_direct,
        ..Default::default()
    }
}

fn connect_options(path: PathPolicy) -> ConnectOptions {
    ConnectOptions {
        path,
        ..Default::default()
    }
}

/// A session through `addr`, checked to end at the backend tagged `expected`.
///
/// The backends name themselves with a tag as they accept, so a test can tell
/// whose service it reached: a leak between namespaces would otherwise echo
/// byte for byte.
async fn session_with(addr: SocketAddr, expected: u8) -> TcpStream {
    let mut socket = TcpStream::connect(addr).await.unwrap();
    let tag = tokio::time::timeout(READY_TIMEOUT, socket.read_u8())
        .await
        .expect("the backend should answer through the tunnel")
        .unwrap();
    assert_eq!(tag, expected, "reached another tenant's service");
    roundtrip(&mut socket).await;
    socket
}

async fn roundtrip(socket: &mut TcpStream) {
    tokio::time::timeout(READY_TIMEOUT, async {
        let payload = [0x5a; 2048];
        socket.write_all(&payload).await.unwrap();
        let mut echoed = [0; 2048];
        socket.read_exact(&mut echoed).await.unwrap();
        assert_eq!(echoed, payload);
    })
    .await
    .expect("traffic should keep flowing");
}

/// What a revoked key must get from any new attempt: a terminal failure, not a
/// tunnel that keeps retrying.
async fn assert_refused<F, Fut>(what: &str, wait_ready: F)
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<(), Error>>,
{
    let result = tokio::time::timeout(READY_TIMEOUT, wait_ready())
        .await
        .unwrap_or_else(|_| panic!("{what} under a revoked key must fail, not keep retrying"));
    assert!(
        matches!(result, Err(Error::TunnelFailed { .. })),
        "{what} under a revoked key: {result:?}"
    );
}

async fn assert_new_register_and_subscribe_refused(client: &Client, backend: SocketAddr) {
    let registration = client
        .register_with_options(
            RegisterRequest {
                key: "refused".into(),
                local_addr: backend.to_string(),
                transport: Transport::Tcp,
                codec: false,
                force_namespace: false,
            },
            RegisterOptions::default(),
        )
        .await
        .unwrap();
    assert_refused("a registration", || registration.wait_ready()).await;
    let connection = client
        .connect_with_options(
            ConnectRequest {
                key: "shared".into(),
                local_addr: "127.0.0.1:0".into(),
                transport: Transport::Tcp,
            },
            connect_options(PathPolicy::Auto),
        )
        .await
        .unwrap();
    assert_refused("a subscription", || connection.wait_ready()).await;
}

async fn namespace_boundary_over(label: &str, carrier: RelayTransport) {
    let relay = relay_with_everything(label).await;
    let administrator = admin_client(relay.addr());
    let admin = administrator.admin().unwrap();
    assert!(admin.supports_key_namespaces().await.unwrap());

    let namespace = admin.allocate_namespace().await.unwrap();
    let issue = |label: &'static str| {
        let admin = admin.clone();
        async move {
            admin
                .issue_key_in_namespace(namespace, LONG_TTL, Some(label.into()))
                .await
                .unwrap()
        }
    };
    let publisher_key = issue("publisher").await;
    let consumer_key = issue("consumer").await;
    let bystander_key = issue("bystander").await;
    assert_eq!(publisher_key.namespace, namespace);
    assert_ne!(publisher_key.key_id, namespace);
    let publisher = client_for(&relay, &publisher_key, carrier);
    let consumer = client_for(&relay, &consumer_key, carrier);
    let bystander = client_for(&relay, &bystander_key, carrier);

    // Another allocated namespace and a key owning its own: neither is a neighbour.
    let other_namespace = admin.allocate_namespace().await.unwrap();
    let other_key = admin
        .issue_key_in_namespace(other_namespace, LONG_TTL, None)
        .await
        .unwrap();
    let legacy_key = admin.issue_key(LONG_TTL, None).await.unwrap();
    assert_eq!(legacy_key.namespace, legacy_key.key_id);
    let other = client_for(&relay, &other_key, carrier);
    let legacy = client_for(&relay, &legacy_key, carrier);
    let relay_only = connect_options(PathPolicy::RelayOnly);

    // One key publishes, another subscribes: that is what sharing a namespace is.
    let backend = tcp_echo(Some(b'A')).await;
    let _registration = publish(
        &publisher,
        "shared",
        backend.addr(),
        Transport::Tcp,
        false,
        register_options(false),
    )
    .await;
    let (_connection, local) = subscribe(&consumer, "shared", Transport::Tcp, relay_only).await;
    let mut session = session_with(local, b'A').await;
    assert_eq!(consumer.list_keys().await.unwrap(), ["shared"]);
    let listed = admin.list_services_all(Some(namespace)).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].namespace, namespace);

    // The same name elsewhere is a different service, and neither sees the other.
    assert!(other.list_keys().await.unwrap().is_empty());
    assert!(legacy.list_keys().await.unwrap().is_empty());
    let other_backend = tcp_echo(Some(b'B')).await;
    let _other_registration = publish(
        &other,
        "shared",
        other_backend.addr(),
        Transport::Tcp,
        false,
        register_options(false),
    )
    .await;
    let (_other_connection, other_local) =
        subscribe(&other, "shared", Transport::Tcp, relay_only).await;
    let _other_session = session_with(other_local, b'B').await;
    assert_eq!(consumer.list_keys().await.unwrap(), ["shared"]);
    roundtrip(&mut session).await;

    // Revoking one key refuses it at every new door and costs its peers nothing.
    let unused_backend = tcp_echo(Some(b'X')).await;
    admin.revoke_key(bystander_key.key_id).await.unwrap();
    assert_new_register_and_subscribe_refused(&bystander, unused_backend.addr()).await;
    roundtrip(&mut session).await;
    let (_second, second_local) = subscribe(&consumer, "shared", Transport::Tcp, relay_only).await;
    let _second_session = session_with(second_local, b'A').await;

    // A key issued later lands in the same namespace and meets the same services.
    let late_key = issue("late").await;
    let late = client_for(&relay, &late_key, carrier);
    let (_late_connection, late_local) =
        subscribe(&late, "shared", Transport::Tcp, relay_only).await;
    let _late_session = session_with(late_local, b'A').await;
}

#[tokio::test]
async fn namespace_boundary_holds_over_tcp() {
    namespace_boundary_over("namespace-tcp", RelayTransport::Tcp).await;
}

#[tokio::test]
async fn namespace_boundary_holds_over_quic() {
    namespace_boundary_over("namespace-quic", RelayTransport::Quic).await;
}

async fn direct_setup_over(label: &str, carrier: RelayTransport) {
    let relay = relay_with_everything(label).await;
    let administrator = admin_client(relay.addr());
    let admin = administrator.admin().unwrap();
    let namespace = admin.allocate_namespace().await.unwrap();
    let issue = || {
        let admin = admin.clone();
        async move {
            admin
                .issue_key_in_namespace(namespace, LONG_TTL, None)
                .await
                .unwrap()
        }
    };
    let publisher_key = issue().await;
    let consumer_key = issue().await;
    let peer_key = issue().await;
    let publisher = client_for(&relay, &publisher_key, carrier);
    let consumer = client_for(&relay, &consumer_key, carrier);
    let peer = client_for(&relay, &peer_key, carrier);
    let auto = connect_options(PathPolicy::Auto);

    // A grant names the namespace, which no longer equals either key's id.
    let backend = tcp_echo(Some(b'A')).await;
    let registration = publish(
        &publisher,
        "shared",
        backend.addr(),
        Transport::Tcp,
        false,
        register_options(true),
    )
    .await;
    let (connection, local) = subscribe(&consumer, "shared", Transport::Tcp, auto).await;
    let mut session = session_with(local, b'A').await;
    assert_eq!(connection.diagnostics().direct.unwrap().active_direct, 1);

    // Revoking the consumer refuses its new setups and leaves the established
    // session to its endpoints, as direct sessions always are.
    let unused_backend = tcp_echo(Some(b'X')).await;
    admin.revoke_key(consumer_key.key_id).await.unwrap();
    assert_new_register_and_subscribe_refused(&consumer, unused_backend.addr()).await;
    roundtrip(&mut session).await;

    // The publisher never noticed: a peer still gets a direct session from it.
    let (peer_connection, peer_local) = subscribe(&peer, "shared", Transport::Tcp, auto).await;
    let _peer_session = session_with(peer_local, b'A').await;
    assert_eq!(
        peer_connection.diagnostics().direct.unwrap().active_direct,
        1
    );
    assert!(registration.diagnostics().direct.unwrap().active_direct >= 1);

    // Revoking the publisher ends its registration, so no new setup can reach it.
    admin.revoke_key(publisher_key.key_id).await.unwrap();
    tokio::time::timeout(READY_TIMEOUT, async {
        while !admin
            .list_services_all(Some(namespace))
            .await
            .unwrap()
            .is_empty()
        {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("the revoked publisher's registration should leave the relay");
    // Not `subscribe`: what is asserted here is before, and instead of, ready.
    let late = peer
        .connect_with_options(
            ConnectRequest {
                key: "shared".into(),
                local_addr: "127.0.0.1:0".into(),
                transport: Transport::Tcp,
            },
            auto,
        )
        .await
        .unwrap();
    assert!(
        late.diagnostics()
            .direct
            .is_none_or(|direct| direct.active_direct == 0),
        "no direct session can start from a revoked publisher"
    );
    if late
        .wait_ready_timeout(Duration::from_secs(2))
        .await
        .is_ok()
        && let Ok(Ok(mut socket)) = tokio::time::timeout(
            Duration::from_secs(2),
            TcpStream::connect(late.local_addr().unwrap()),
        )
        .await
    {
        let answered = tokio::time::timeout(Duration::from_secs(2), socket.read_u8()).await;
        assert!(
            !matches!(answered, Ok(Ok(_))),
            "a revoked publisher's service answered a new session"
        );
    }
}

#[tokio::test]
async fn direct_setup_works_in_a_namespace_and_stops_at_revocation_over_tcp() {
    direct_setup_over("namespace-direct-tcp", RelayTransport::Tcp).await;
}

#[tokio::test]
async fn direct_setup_works_in_a_namespace_and_stops_at_revocation_over_quic() {
    direct_setup_over("namespace-direct-quic", RelayTransport::Quic).await;
}

/// The wire API refuses what the auth state refuses.
#[tokio::test]
async fn the_wire_api_refuses_a_namespace_that_was_not_allocated() {
    let relay = Relay::start("namespace-wire").await;
    let administrator = admin_client(relay.addr());
    let admin = administrator.admin().unwrap();
    let own = admin.issue_key(LONG_TTL, None).await.unwrap();

    for refused in [0, own.key_id, 1] {
        let error = admin
            .issue_key_in_namespace(refused, LONG_TTL, None)
            .await
            .unwrap_err();
        assert!(
            matches!(&error, Error::Remote { code, .. } if code == "namespace_not_allocated"),
            "namespace {refused}: {error}"
        );
    }

    let tenant = client_for(&relay, &own, RelayTransport::Tcp);
    assert!(matches!(tenant.admin(), Err(Error::NotAdministrator)));
}
