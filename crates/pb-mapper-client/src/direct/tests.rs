use super::{
    binding,
    conn::DirectConn,
    consumer::{COOLDOWN_INITIAL, COOLDOWN_MAX, Consumer, Cooldown, DataPath, Prepared},
    endpoint::{Arrival, InterfaceSource, Network},
    lan::{Assessment, Interface, Interfaces},
    tcp,
};
use crate::{
    diagnostics::{
        ByteCount, Diagnostics, InterfaceTier, LanEvidence, LanTcpMiss, LinkKind, StunOutcome,
        TunnelEventKind,
    },
    endpoint::RelayEndpoint,
    relay::RelayTransport,
    sdk::{
        Client, LanTransport, PortMapping, RegisterOptions, RegisterRequest, Registration,
        Transport,
    },
};
use pb_mapper_auth::{AuthConfig, AuthRuntime};
use pb_mapper_core::{checksum::Credential, config::ResolvedAddrs};
use pb_mapper_protocol::direct::*;
use pb_mapper_server::DirectServerConfig;
use std::{io, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    time::Instant,
};
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
    async fn start(key: &str, lan_tcp: bool) -> Self {
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
                    lan_tcp,
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

    /// A consumer of the share over `network`, asking for `lan_transport`.
    fn consumer(
        &self,
        network: Arc<Network>,
        diagnostics: Diagnostics,
        lan_transport: LanTransport,
    ) -> Arc<Consumer> {
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
                lan_transport,
                lan_tcp: false,
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

/// A consumer's endpoint on which this machine's loopback stands in for a
/// LAN: injected as one, its subnet holds the share's 127.0.0.1, which is
/// also what the relay's STUN sees of this end.
fn lan_network() -> Arc<Network> {
    let mut network = Network::new(false, PortMapping::Off, &Diagnostics::default()).unwrap();
    Arc::get_mut(&mut network).unwrap().interfaces =
        InterfaceSource::Fixed(Interfaces::new(vec![Interface::new(
            "lan0",
            InterfaceTier::Lan,
            [("127.0.0.2".parse().unwrap(), 8)],
        )]));
    network
}

/// Prepare one session of `share` through `consumer`.
async fn prepare(consumer: &Arc<Consumer>, key: &str) -> Prepared {
    tokio::time::timeout(
        Duration::from_secs(8),
        consumer.prepare(key, Instant::now() + Duration::from_secs(8)),
    )
    .await
    .unwrap()
    .unwrap()
    .unwrap()
}

/// The next event of `kind` that `events` delivers.
async fn next<T>(
    events: &mut crate::diagnostics::TunnelEvents,
    kind: impl Fn(TunnelEventKind) -> Option<T>,
) -> T {
    loop {
        let event = tokio::time::timeout(Duration::from_secs(1), events.recv())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        if let Some(found) = kind(event.kind) {
            return found;
        }
    }
}

/// No handshake gets across: the session takes the relay, which carries and
/// counts its bytes. The share is on a LAN of this end, by strong evidence
/// (see the next test), so the miss is reported with where it went instead.
#[tokio::test]
async fn blocked_quic_selects_relay_and_dials_backend_once() {
    let share = Share::start("fallback", true).await;
    let network = lan_network();
    network.block_quic();
    let diagnostics = Diagnostics::default();
    let mut events = diagnostics.subscribe();
    let consumer = share.consumer(network, diagnostics.clone(), LanTransport::Quic);
    let prepared = prepare(&consumer, "fallback").await;
    assert!(matches!(prepared.path, DataPath::Relay { .. }));
    // The fallback pauses direct: the next session goes to the relay at once.
    let paused = consumer
        .prepare("fallback", Instant::now() + Duration::from_secs(8))
        .await
        .unwrap();
    assert!(paused.is_none());
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
    let missed = next(&mut events, |kind| match kind {
        TunnelEventKind::LanMissed { evidence, took } => Some((evidence, took)),
        _ => None,
    })
    .await;
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
/// open. A publisher that serves TCP opens nothing for a consumer that does
/// not ask for it.
#[tokio::test]
async fn a_lan_is_dialed_on_its_first_answer_and_reported_while_open() {
    let share = Share::start("lan", true).await;
    let network = lan_network();
    let diagnostics = Diagnostics::default();
    let consumer = share.consumer(network, diagnostics.clone(), LanTransport::Quic);
    let prepared = prepare(&consumer, "lan").await;
    assert!(matches!(
        prepared.path,
        DataPath::Direct {
            conn: DirectConn::Quic { .. },
            ..
        }
    ));
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
    // The session's end lets its connection go: closing it in diagnostics
    // and letting its link go are one step.
    let mut events = diagnostics.subscribe();
    drop(forward);
    next(&mut events, |kind| match kind {
        TunnelEventKind::SessionClosed { .. } => Some(()),
        _ => None,
    })
    .await;
    assert!(read().link.is_none());
    assert_eq!(read().active_lan, 0);
    share.stop().await;
}

/// Asked for, TCP carries a session within the LAN: it is dialed first, the
/// QUIC paths wait, the bytes go over it with the backend dialed once, and
/// the session says so.
#[tokio::test]
async fn a_lan_asked_for_tcp_runs_over_it() {
    let share = Share::start("tcp", true).await;
    let diagnostics = Diagnostics::default();
    let mut events = diagnostics.subscribe();
    let consumer = share.consumer(lan_network(), diagnostics.clone(), LanTransport::Tcp);
    let prepared = prepare(&consumer, "tcp").await;
    assert!(matches!(
        prepared.path,
        DataPath::Direct {
            conn: DirectConn::Tcp { .. },
            ..
        }
    ));
    let _forward = share.forward(prepared).await;
    let selected = next(&mut events, |kind| match kind {
        TunnelEventKind::PathSelected {
            link, transport, ..
        } => Some((link, transport)),
        _ => None,
    })
    .await;
    assert_eq!(selected, (LinkKind::Lan, DirectTransport::Tcp));
    let direct = diagnostics
        .snapshot(&RelayEndpoint::shared("127.0.0.1:1"))
        .direct
        .unwrap();
    let link = direct.link.unwrap();
    assert_eq!(
        (link.kind, link.transport),
        (LinkKind::Lan, DirectTransport::Tcp)
    );
    assert!(link.stats.rtt_us > 0, "the handshake's round trip");
    assert_eq!((direct.active_lan_tcp, direct.lan_tcp_sessions), (1, 1));
    let traffic = diagnostics.metrics(RelayTransport::Tcp).traffic;
    assert_eq!(traffic.lan.sent, 12);
    share.stop().await;
}

/// A publisher that does not serve TCP answers as one that predates it: the
/// consumer that asked takes QUIC on the LAN and says why; nothing pauses.
#[tokio::test]
async fn a_publisher_without_tcp_leaves_the_lan_on_quic() {
    let share = Share::start("no-tcp", false).await;
    let diagnostics = Diagnostics::default();
    let mut events = diagnostics.subscribe();
    let consumer = share.consumer(lan_network(), diagnostics.clone(), LanTransport::Tcp);
    let prepared = prepare(&consumer, "no-tcp").await;
    assert!(matches!(
        prepared.path,
        DataPath::Direct {
            conn: DirectConn::Quic { .. },
            ..
        }
    ));
    let missed = next(&mut events, |kind| match kind {
        TunnelEventKind::LanTcpMissed { reason, took } => Some((reason, took)),
        _ => None,
    })
    .await;
    assert_eq!(
        missed,
        (LanTcpMiss::NotOffered, Some(DirectTransport::Quic))
    );
    assert!(!consumer.tcp_cooldown().active(Instant::now()));
    let _forward = share.forward(prepared).await;
    share.stop().await;
}

/// TCP refused: QUIC on the LAN carries the session within the head start.
/// The relay is not paused, as direct worked; TCP is, so later sessions do
/// not pay the head start again.
#[tokio::test]
async fn refused_tcp_falls_back_to_quic_on_the_lan_and_pauses_tcp_alone() {
    let share = Share::start("refused", true).await;
    let closed = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let network = lan_network();
    network
        .tcp_redirect
        .set(closed.local_addr().unwrap())
        .unwrap();
    drop(closed);
    let diagnostics = Diagnostics::default();
    let mut events = diagnostics.subscribe();
    let consumer = share.consumer(network, diagnostics.clone(), LanTransport::Tcp);
    let prepared = prepare(&consumer, "refused").await;
    assert!(matches!(
        prepared.path,
        DataPath::Direct {
            conn: DirectConn::Quic { .. },
            ..
        }
    ));
    let (reason, took) = next(&mut events, |kind| match kind {
        TunnelEventKind::LanTcpMissed { reason, took } => Some((reason, took)),
        _ => None,
    })
    .await;
    // Windows tries a refused connect again for a second or two, so the
    // head start can run out before it says so.
    assert!(
        matches!(reason, LanTcpMiss::Refused | LanTcpMiss::TimedOut),
        "{reason:?}"
    );
    assert_eq!(took, Some(DirectTransport::Quic));
    assert!(consumer.tcp_cooldown().active(Instant::now()));
    let metrics = diagnostics.metrics(RelayTransport::Tcp);
    assert_eq!(metrics.direct_cooldown_ms, None);
    let _forward = share.forward(prepared).await;
    share.stop().await;
}

#[tokio::test]
async fn relay_recovery_preserves_direct_cooldown_until_network_changes() {
    use crate::sdk::ConnectRequest;
    use tokio::net::TcpStream;

    tokio::time::timeout(Duration::from_secs(8), async {
        let credential = Credential::Admin(pb_mapper_core::checksum::gen_random_key());
        let state_dir = std::env::temp_dir().join(format!(
            "pb-cooldown-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let auth = AuthRuntime::start(
            *credential.key(),
            AuthConfig {
                state_dir: state_dir.clone(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let relay_addr = listener.local_addr().unwrap();
        let relay = AbortOnDropHandle::new(tokio::spawn(
            pb_mapper_server::run_server_on_listener_with_direct(
                listener,
                CancellationToken::new(),
                None,
                false,
                auth,
                None,
            ),
        ));
        let backend = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let backend_addr = backend.local_addr().unwrap();
        let echo = AbortOnDropHandle::new(tokio::spawn(async move {
            loop {
                let (mut stream, _) = backend.accept().await.unwrap();
                let mut byte = [0; 1];
                stream.read_exact(&mut byte).await.unwrap();
                stream.write_all(&byte).await.unwrap();
            }
        }));
        let mut client = Client::from_credential(relay_addr.to_string(), credential, false, None);
        Arc::get_mut(&mut client.inner).unwrap().endpoint =
            RelayEndpoint::isolated(relay_addr.into());
        let registration = client
            .register_with_options(
                RegisterRequest {
                    key: "cooldown".into(),
                    local_addr: backend_addr.to_string(),
                    transport: Transport::Tcp,
                    codec: true,
                    force_namespace: false,
                },
                RegisterOptions {
                    allow_direct: false,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        registration
            .wait_ready_timeout(Duration::from_secs(3))
            .await
            .unwrap();
        // The SDK owns its listener and cannot accept a bound socket.
        let reservation = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let local = reservation.local_addr().unwrap();
        drop(reservation);
        let tunnel = client
            .connect(ConnectRequest {
                key: "cooldown".into(),
                local_addr: local.to_string(),
                transport: Transport::Tcp,
            })
            .await
            .unwrap();
        tunnel
            .wait_ready_timeout(Duration::from_secs(3))
            .await
            .unwrap();
        let exchange = async || {
            let mut stream = TcpStream::connect(local).await.unwrap();
            stream.write_all(b"x").await.unwrap();
            let mut response = [0; 1];
            stream.read_exact(&mut response).await.unwrap();
            assert_eq!(&response, b"x");
        };
        exchange().await;
        assert_eq!(tunnel.diagnostics().direct.unwrap().fallbacks, 1);
        exchange().await;
        let before = tunnel.diagnostics();
        assert_eq!(before.direct.unwrap().fallbacks, 1);

        // A shared endpoint's recovery must wake the worker's health probe,
        // without claiming that this tunnel's direct path became reachable.
        let endpoint = &client.inner.endpoint;
        endpoint.transport_failed(None);
        endpoint.protocol_succeeded();
        while tunnel.diagnostics().attempts == before.attempts {
            tokio::task::yield_now().await;
        }
        exchange().await;
        let recovered = tunnel.diagnostics();
        assert_eq!(recovered.network_generation, before.network_generation);
        assert_eq!(recovered.direct.unwrap().fallbacks, 1);

        endpoint.notify_network_change();
        while !tunnel
            .diagnostics()
            .events
            .iter()
            .any(|event| matches!(event.kind, TunnelEventKind::NetworkChanged))
        {
            tokio::task::yield_now().await;
        }
        exchange().await;
        let changed = tunnel.diagnostics();
        assert_ne!(changed.network_generation, before.network_generation);
        assert_eq!(changed.direct.unwrap().fallbacks, 2);
        tunnel.stop().await.unwrap();
        registration.stop().await.unwrap();
        drop(echo);
        drop(relay);
        let _ = std::fs::remove_dir_all(state_dir);
    })
    .await
    .unwrap();
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
    let quic = tcp::Race::idle;
    let mut race = quic();
    let (outgoing, incoming) = tokio::join!(
        consumer.connect(&grant, deadline, &no_lan, &mut race),
        tokio::time::timeout_at(deadline, provider.accept())
    );
    let (outgoing, _) = outgoing.unwrap();
    let Some(Arrival::Quic(incoming)) = incoming.unwrap() else {
        panic!("expected a QUIC arrival");
    };
    let tag = outgoing.binding(&grant).unwrap();
    binding::verify_binding(&incoming, &grant, &tag).unwrap();
    let mut wrong = grant.clone();
    wrong.service = "another-service".into();
    assert!(binding::verify_binding(&incoming, &wrong, &tag).is_err());
    let stranger = Network::new(false, PortMapping::Off, &Diagnostics::default()).unwrap();
    stranger
        .authorize(&grant, false, Instant::now() + Duration::from_secs(3))
        .unwrap();
    if let Ok((DirectConn::Quic { connection, .. }, _)) = stranger
        .connect(
            &grant,
            Instant::now() + Duration::from_millis(500),
            &no_lan,
            &mut quic(),
        )
        .await
    {
        // TLS 1.3 client completion can precede the server's client-cert decision.
        tokio::time::timeout(Duration::from_secs(1), connection.0.closed())
            .await
            .unwrap();
    }
    // Only the granted identity may reach application accept.
    let only_granted = || async {
        while let Ok(Some(accepted)) =
            tokio::time::timeout(Duration::from_millis(100), provider.accept()).await
        {
            assert_eq!(
                presented(&accepted).as_deref(),
                Some(grant.consumer.certificate.as_slice())
            );
        }
    };
    only_granted().await;
    let other_provider = Network::new(true, PortMapping::Off, &Diagnostics::default()).unwrap();
    wrong.provider.certificate = other_provider.identity.certificate.clone();
    assert!(
        consumer
            .connect(
                &wrong,
                Instant::now() + Duration::from_millis(500),
                &no_lan,
                &mut quic()
            )
            .await
            .is_err(),
        "server certificate must match grant"
    );

    // Over TCP: the same identity and binding, under TCP's own label.
    let deadline = Instant::now() + Duration::from_secs(3);
    provider
        .identity
        .permissions
        .allow(grant.consumer.certificate.clone(), deadline)
        .unwrap();
    let (_offered, addrs) = provider
        .lan_tcp(true)
        .unwrap()
        .offer(&Interfaces::default());
    let client = |network: &Network| {
        network
            .identity
            .tcp_client(&grant.provider.certificate)
            .unwrap()
    };
    race = tcp::Race::start(&addrs[..1], client(&consumer));
    let (outgoing, incoming) = tokio::join!(
        race.next(),
        tokio::time::timeout_at(deadline, provider.accept())
    );
    let outgoing = outgoing.unwrap().stream;
    let Some(Arrival::Tcp {
        stream: incoming, ..
    }) = incoming.unwrap()
    else {
        panic!("expected a TCP arrival");
    };
    let tag = binding::binding(&*outgoing, &grant).unwrap();
    binding::verify_binding(&*incoming, &grant, &tag).unwrap();
    let quic_label = binding::tag(&*outgoing, b"pb-mapper/direct/1", &grant).unwrap();
    assert!(
        binding::verify_binding(&*incoming, &grant, &quic_label).is_err(),
        "a binding made for QUIC is worthless over TCP"
    );
    let _ = tcp::Race::start(&addrs[..1], client(&stranger))
        .next()
        .await;
    only_granted().await;
}

/// The one certificate an accepted connection presented.
fn presented(arrival: &Arrival) -> Option<Vec<u8>> {
    match arrival {
        Arrival::Quic(connection) => connection
            .peer_identity()?
            .downcast::<Vec<quinn::rustls::pki_types::CertificateDer<'static>>>()
            .ok()?
            .first()
            .map(|certificate| certificate.to_vec()),
        Arrival::Tcp { stream, .. } => tcp::peer_certificate(stream).map(<[u8]>::to_vec),
    }
}
