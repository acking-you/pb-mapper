#![allow(clippy::unwrap_used, clippy::expect_used)]
use super::*;
use crate::{
    diagnostics::{ByteCount, InterfaceTier, StunOutcome, TunnelEventKind},
    relay::RelayTransport,
    sdk::{Client, PortMapping, RegisterOptions, RegisterRequest, Registration, Transport},
};
use endpoint::InterfaceSource;
use lan::{Interface, Interfaces};
use pb_mapper_auth::{AuthConfig, AuthRuntime};
use pb_mapper_core::config::ResolvedAddrs;
use pb_mapper_server::DirectServerConfig;
use tokio::{io::AsyncWriteExt, net::TcpListener};
use tokio_util::{sync::CancellationToken, task::AbortOnDropHandle};
use uni_stream::stream::{StreamProvider, TcpStreamProvider};

#[test]
fn fallback_cooldown_doubles_to_its_cap_and_clears() {
    let start = Instant::now();
    let mut cooldown = Cooldown::default();
    assert!(!cooldown.active(start));

    cooldown.back_off(start);
    assert!(cooldown.active(start + COOLDOWN_INITIAL - Duration::from_millis(1)));
    assert!(!cooldown.active(start + COOLDOWN_INITIAL));

    // Fallbacks within a running pause belong to the same failure.
    cooldown.back_off(start + Duration::from_secs(1));
    assert!(!cooldown.active(start + COOLDOWN_INITIAL));

    // Each fallback after a pause ends pauses longer, never past the cap.
    let mut now = start + COOLDOWN_INITIAL;
    let mut pause = COOLDOWN_INITIAL;
    for _ in 0..16 {
        cooldown.back_off(now);
        pause = (pause * 2).min(COOLDOWN_MAX);
        assert!(cooldown.active(now + pause - Duration::from_millis(1)));
        assert!(!cooldown.active(now + pause));
        now += pause;
    }
    assert_eq!(pause, COOLDOWN_MAX);

    // A success or a network change starts the schedule over.
    cooldown.clear();
    assert!(!cooldown.active(now));
    cooldown.back_off(now);
    assert!(!cooldown.active(now + COOLDOWN_INITIAL));
}

/// A loopback relay that offers direct, with a share of a local TCP backend
/// on it.
struct Share {
    credential: Credential,
    relay: std::net::SocketAddr,
    client: Client,
    registration: Registration,
    backend: TcpListener,
    state_dir: std::path::PathBuf,
    _relay: AbortOnDropHandle<()>,
}

impl Share {
    async fn start(key: &str) -> Self {
        let credential = Credential::Admin(pb_mapper_core::checksum::gen_random_key());
        let state_dir = std::env::temp_dir().join(format!(
            "pb-direct-{key}-{}-{}",
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
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let relay = listener.local_addr().unwrap();
        let server = pb_mapper_server::run_server_on_listener_with_direct(
            listener,
            CancellationToken::new(),
            None,
            false,
            auth,
            Some(DirectServerConfig {
                listen: "127.0.0.1:0".parse().unwrap(),
                advertise: "127.0.0.1:0".parse().unwrap(),
            }),
        );
        let _relay = AbortOnDropHandle::new(tokio::spawn(async move {
            let _ = server.await;
        }));
        let backend = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = Client::from_credential(relay.to_string(), credential, false, None);
        let registration = client
            .register_with_options(
                RegisterRequest {
                    key: key.into(),
                    local_addr: backend.local_addr().unwrap().to_string(),
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
        Self {
            credential,
            relay,
            client,
            registration,
            backend,
            state_dir,
            _relay,
        }
    }

    /// A consumer of the share over `network`.
    fn consumer(&self, network: Arc<Network>, diagnostics: Diagnostics) -> Arc<Consumer> {
        let consumer = Consumer::new(
            RelayEndpoint::fixed(ResolvedAddrs::from(self.relay)),
            self.credential,
            None,
            false,
            true,
            diagnostics,
            crate::sdk::DirectPolicy {
                port_mapping: PortMapping::Off,
                lan_first: true,
            },
        );
        consumer
            .network
            .set(network)
            .unwrap_or_else(|_| panic!("network already initialized"));
        consumer
    }

    /// Forward a prepared session from a local application, and check that
    /// bytes reach the backend; returns the forwarding task.
    async fn forward(&self, prepared: Prepared) -> AbortOnDropHandle<io::Result<()>> {
        let (mut service, _) = tokio::time::timeout(Duration::from_secs(1), self.backend.accept())
            .await
            .unwrap()
            .unwrap();
        let downstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let local = TcpStreamProvider::from_addr(downstream.local_addr().unwrap())
            .await
            .unwrap();
        let forward =
            AbortOnDropHandle::new(tokio::spawn(prepared.forward(local, self.credential)));
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
            tokio::time::timeout(Duration::from_millis(100), self.backend.accept())
                .await
                .is_err(),
            "exactly one backend connection"
        );
        forward
    }

    async fn stop(self) {
        self.registration.stop().await.unwrap();
        drop(self._relay);
        let _ = std::fs::remove_dir_all(self.state_dir);
    }
}

/// No handshake gets across: the session takes the relay, which carries and
/// counts its bytes. The share is on a LAN of this end, by strong evidence
/// (see the next test), so the miss is reported with where it went instead.
#[tokio::test]
async fn blocked_quic_selects_relay_and_dials_backend_once() {
    let share = Share::start("fallback").await;
    let mut network = Network::new(false, PortMapping::Off, &Diagnostics::default()).unwrap();
    Arc::get_mut(&mut network).unwrap().interfaces =
        InterfaceSource::Fixed(Interfaces::new(vec![Interface::new(
            "lan0",
            InterfaceTier::Lan,
            [("127.0.0.2".parse().unwrap(), 8)],
        )]));
    network.block_quic();
    let diagnostics = Diagnostics::default();
    let mut events = diagnostics.subscribe();
    let consumer = share.consumer(network, diagnostics.clone());
    let prepared = tokio::time::timeout(
        Duration::from_secs(8),
        consumer.prepare("fallback", Instant::now() + Duration::from_secs(8)),
    )
    .await
    .unwrap()
    .unwrap()
    .unwrap();
    assert!(matches!(prepared.path, DataPath::Relay { .. }));
    // The fallback pauses direct: the next session goes to the relay at once.
    let next = consumer
        .prepare("fallback", Instant::now() + Duration::from_secs(8))
        .await
        .unwrap();
    assert!(next.is_none());
    let paused = diagnostics.metrics(RelayTransport::Tcp);
    assert!(paused.direct_cooldown_ms.is_some());
    assert_eq!(paused.cooldown_skips, 1);
    // A reconnect snapshot can include more retained reports than the control
    // channel can hold. Every authenticated observation must eventually arrive.
    let mut report_ids = std::collections::HashSet::new();
    for _ in 0..128 {
        let mut summary = SessionSummary::from(&prepared.grant);
        summary.id = random_session_id();
        report_ids.insert(summary.id);
        consumer.reports.put(SessionUpdate {
            report: SessionReport {
                id: summary.id,
                seq: 1,
                path: SessionPath::Direct,
                state: SessionState::Active,
                reason: None,
                link: None,
            },
            session: summary,
            secret: SessionSecret::random(),
        });
    }
    consumer.report_wake.notify_one();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            for observation in share.client.sessions().await.unwrap() {
                report_ids.remove(&observation.id);
            }
            if report_ids.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("snapshot must drain beyond the control channel capacity");
    let _forward = share.forward(prepared).await;
    let missed = loop {
        let event = tokio::time::timeout(Duration::from_secs(1), events.recv())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        if let TunnelEventKind::LanMissed { evidence, took } = event.kind {
            break (evidence, took);
        }
    };
    assert_eq!(missed, (LanEvidence::PublicIpSubnet, None));
    let metrics = diagnostics.metrics(RelayTransport::Tcp);
    assert_eq!((metrics.lan.detected, metrics.lan.missed), (1, 1));
    assert_eq!(
        metrics.traffic.relay,
        ByteCount {
            sent: 12,
            received: 12
        }
    );
    share.stop().await;
}

/// Strong evidence of one LAN: the LAN path is dialed on its first answer,
/// the paths off it wait, and the session says where it runs while it is
/// open. This machine's loopback stands in for the LAN: injected as one, its
/// subnet holds the share's 127.0.0.1, which is also what the relay's STUN
/// sees of this end.
#[tokio::test]
async fn a_lan_is_dialed_on_its_first_answer_and_reported_while_open() {
    let share = Share::start("lan").await;
    let mut network = Network::new(false, PortMapping::Off, &Diagnostics::default()).unwrap();
    Arc::get_mut(&mut network).unwrap().interfaces =
        InterfaceSource::Fixed(Interfaces::new(vec![Interface::new(
            "lan0",
            InterfaceTier::Lan,
            [("127.0.0.2".parse().unwrap(), 8)],
        )]));
    let diagnostics = Diagnostics::default();
    let consumer = share.consumer(network, diagnostics.clone());
    let prepared = consumer
        .prepare("lan", Instant::now() + Duration::from_secs(8))
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(prepared.path, DataPath::Quic { .. }));
    let forward = share.forward(prepared).await;
    let read = || {
        diagnostics
            .snapshot(&RelayEndpoint::shared("127.0.0.1:1"))
            .direct
            .unwrap()
    };
    let direct = read();
    let link = direct
        .link
        .clone()
        .expect("an open direct session has a link");
    assert_eq!(
        (link.kind, link.family, link.same_lan),
        (
            LinkKind::Lan,
            AddressFamily::Ipv4,
            Some(LanEvidence::PublicIpSubnet)
        )
    );
    // Every other address of the share answered too, and was held back.
    assert_eq!(link.candidates.handshakes, 1);
    assert!(link.candidates.answered >= 1);
    assert!(link.stats.rtt_us > 0, "QUIC's round trip, read live");
    let timings = link.timings;
    assert_eq!(timings.stun, StunOutcome::Ok);
    for stage in [
        timings.negotiate_ms,
        timings.discover_ms,
        timings.grant_ms,
        timings.lan_first_pong_ms,
        timings.handshake_ms,
        timings.commit_ms,
    ] {
        assert!(stage.is_some(), "{timings:?}");
    }
    assert_eq!(
        (direct.active_lan, direct.lan_sessions, direct.lan_detected),
        (1, 1, 1)
    );
    assert_eq!(
        (direct.lan_missed, direct.last_kind),
        (0, Some(LinkKind::Lan))
    );
    // Its bytes count on the LAN, which is direct, and only there.
    let traffic = diagnostics.metrics(RelayTransport::Tcp).traffic;
    let echoed = ByteCount {
        sent: 12,
        received: 12,
    };
    assert_eq!((traffic.lan, traffic.direct), (echoed, echoed));
    assert_eq!(traffic.relay, ByteCount::default());
    // The session's end lets its connection go.
    drop(forward);
    tokio::time::timeout(Duration::from_secs(2), async {
        while read().link.is_some() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(read().active_lan, 0);
    share.stop().await;
}

#[tokio::test]
async fn peer_certificate_and_session_binding_are_both_required() {
    let provider = Network::new(true, PortMapping::Off, &Diagnostics::default()).unwrap();
    let consumer = Network::new(false, PortMapping::Off, &Diagnostics::default()).unwrap();
    // Use one discovered loopback path for the TLS boundary test. With multiple
    // candidates, the client's winning connection and the first server accept
    // can be different TLS sessions, which correctly have different bindings.
    let stun: std::net::SocketAddr = "127.0.0.1:9".parse().unwrap();
    let mut provider_peer = provider.peer(stun).await.peer;
    let mut consumer_peer = consumer.peer(stun).await.peer;
    for peer in [&mut provider_peer, &mut consumer_peer] {
        peer.candidates
            .retain(|addr| addr.is_ipv4() && addr.ip().is_loopback());
        peer.candidates.truncate(1);
        assert_eq!(
            peer.candidates.len(),
            1,
            "an IPv4 loopback path is required"
        );
    }
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
    let no_lan = Assessment::default();
    let (outgoing, incoming) = tokio::join!(
        consumer.connect(&grant, deadline, &no_lan),
        tokio::time::timeout_at(deadline, provider.accept())
    );
    let (outgoing, _) = outgoing.unwrap();
    let incoming = incoming.unwrap().unwrap();
    let tag = endpoint::binding(&outgoing, &grant).unwrap();
    endpoint::verify_binding(&incoming, &grant, &tag).unwrap();
    let mut wrong = grant.clone();
    wrong.service = "another-service".into();
    assert!(endpoint::verify_binding(&incoming, &wrong, &tag).is_err());
    let stranger = Network::new(false, PortMapping::Off, &Diagnostics::default()).unwrap();
    stranger
        .authorize(&grant, false, Instant::now() + Duration::from_secs(3))
        .unwrap();
    if let Ok((connection, _)) = stranger
        .connect(&grant, Instant::now() + Duration::from_millis(500), &no_lan)
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
    let other_provider = Network::new(true, PortMapping::Off, &Diagnostics::default()).unwrap();
    wrong.provider.certificate = other_provider.identity.certificate.clone();
    assert!(
        consumer
            .connect(&wrong, Instant::now() + Duration::from_millis(500), &no_lan)
            .await
            .is_err(),
        "server certificate must match grant"
    );
}
