#![allow(clippy::unwrap_used, clippy::expect_used)]
use super::*;
use crate::sdk::{Client, RegisterOptions, RegisterRequest, Transport};
use pb_mapper_auth::{AuthConfig, AuthRuntime};
use pb_mapper_core::config::ResolvedAddrs;
use pb_mapper_server::DirectServerConfig;
use tokio::{io::AsyncWriteExt, net::TcpListener};
use tokio_util::{sync::CancellationToken, task::AbortOnDropHandle};
use uni_stream::stream::{StreamProvider, TcpStreamProvider};

#[tokio::test]
async fn blocked_quic_selects_relay_and_dials_backend_once() {
    let credential = Credential::Admin(pb_mapper_core::checksum::gen_random_key());
    let state_dir = std::env::temp_dir().join(format!(
        "pb-direct-fallback-{}-{}",
        std::process::id(),
        rand::random::<u64>()
    ));
    let auth = AuthRuntime::start(
        *credential.key(),
        AuthConfig {
            state_dir: state_dir.clone(),
            ..AuthConfig::default()
        },
    )
    .await
    .unwrap();
    let relay = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = relay.local_addr().unwrap();
    let _relay = AbortOnDropHandle::new(tokio::spawn(
        pb_mapper_server::run_server_on_listener_with_direct(
            relay,
            CancellationToken::new(),
            None,
            false,
            auth,
            Some(DirectServerConfig {
                listen: "127.0.0.1:0".parse().unwrap(),
                advertise: "127.0.0.1:0".parse().unwrap(),
            }),
        ),
    ));
    let backend = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let backend_addr = backend.local_addr().unwrap();
    let client = Client::from_credential(addr.to_string(), credential, false, None);
    let registration = client
        .register_with_options(
            RegisterRequest {
                key: "fallback".into(),
                local_addr: backend_addr.to_string(),
                transport: Transport::Tcp,
                codec: true,
                force_namespace: false,
            },
            RegisterOptions {
                allow_direct: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    registration
        .wait_ready_timeout(Duration::from_secs(5))
        .await
        .unwrap();
    let diagnostics = Diagnostics::default();
    let consumer = Consumer::new(
        RelayEndpoint::fixed(ResolvedAddrs::from(addr)),
        credential,
        None,
        false,
        diagnostics,
    );
    let network = Network::new(false).unwrap();
    network.block_quic();
    consumer
        .network
        .set(network)
        .unwrap_or_else(|_| panic!("network already initialized"));
    let prepared = tokio::time::timeout(
        Duration::from_secs(8),
        consumer.prepare("fallback", Instant::now() + Duration::from_secs(8)),
    )
    .await
    .unwrap()
    .unwrap()
    .unwrap();
    assert!(matches!(prepared.path, DataPath::Relay { .. }));
    let (mut service, _) = tokio::time::timeout(Duration::from_secs(1), backend.accept())
        .await
        .unwrap()
        .unwrap();
    let downstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let local = TcpStreamProvider::from_addr(downstream.local_addr().unwrap())
        .await
        .unwrap();
    let _forward = AbortOnDropHandle::new(tokio::spawn(prepared.forward(local, credential)));
    let (mut application, _) = downstream.accept().await.unwrap();
    application.write_all(b"exactly once").await.unwrap();
    let mut bytes = [0; 12];
    tokio::time::timeout(Duration::from_secs(2), service.read_exact(&mut bytes))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(&bytes, b"exactly once");
    service.write_all(&bytes).await.unwrap();
    application.read_exact(&mut bytes).await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(100), backend.accept())
            .await
            .is_err()
    );
    registration.stop().await.unwrap();
    drop(_relay);
    let _ = std::fs::remove_dir_all(state_dir);
}

#[tokio::test]
async fn peer_certificate_and_session_binding_are_both_required() {
    let provider = Network::new(true).unwrap();
    let consumer = Network::new(false).unwrap();
    // No STUN server is needed for the TLS boundary test; loopback candidates
    // come from the real bound sockets through discovery.
    let stun: std::net::SocketAddr = "127.0.0.1:9".parse().unwrap();
    let provider_peer = provider.peer(stun).await;
    let consumer_peer = consumer.peer(stun).await;
    let grant = SessionGrant {
        id: [1; 16],
        server_instance: [2; 16],
        generation: 1,
        namespace: 0,
        service: "test".into(),
        is_datagram: false,
        consumer_key: 0,
        provider_key: 0,
        consumer: consumer_peer,
        provider: provider_peer,
        secret: SessionSecret([3; 32]),
        ttl_ms: 8000,
    };
    let deadline = Instant::now() + Duration::from_secs(3);
    provider.authorize(&grant, true, deadline).unwrap();
    consumer.authorize(&grant, false, deadline).unwrap();
    let (outgoing, incoming) = tokio::join!(
        consumer.connect(&grant, deadline),
        tokio::time::timeout_at(deadline, provider.accept())
    );
    let outgoing = outgoing.unwrap();
    let incoming = incoming.unwrap().unwrap();
    let tag = endpoint::binding(&outgoing, &grant).unwrap();
    endpoint::verify_binding(&incoming, &grant, &tag).unwrap();
    let mut wrong = grant.clone();
    wrong.service = "another-service".into();
    assert!(endpoint::verify_binding(&incoming, &wrong, &tag).is_err());
    let stranger = Network::new(false).unwrap();
    stranger
        .authorize(&grant, false, Instant::now() + Duration::from_secs(3))
        .unwrap();
    if let Ok(connection) = stranger
        .connect(&grant, Instant::now() + Duration::from_millis(500))
        .await
    {
        // TLS 1.3 client completion can precede the server's client-cert decision.
        tokio::time::timeout(Duration::from_secs(1), connection.closed())
            .await
            .unwrap();
    }
    while let Ok(Some(accepted)) =
        tokio::time::timeout(Duration::from_millis(50), provider.accept()).await
    {
        let identity = accepted
            .peer_identity()
            .unwrap()
            .downcast::<Vec<quinn::rustls::pki_types::CertificateDer<'static>>>()
            .unwrap();
        assert_eq!(
            identity[0].as_ref(),
            grant.consumer.certificate,
            "only the granted identity may reach application accept"
        );
    }
    let other_provider = Network::new(true).unwrap();
    wrong.provider.certificate = other_provider.identity.certificate.clone();
    assert!(
        consumer
            .connect(&wrong, Instant::now() + Duration::from_millis(500))
            .await
            .is_err(),
        "server certificate must match grant"
    );
}
