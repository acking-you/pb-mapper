use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use pb_mapper_client::sdk::{
    AddressFamily, Client, DirectStage, DirectTransport, LanEvidence, LanTransport, LinkKind,
    LinkVia, PortMapping, RegisterOptions, RegisterRequest, RelayTransport, StunOutcome, Transport,
};
use pb_mapper_core::checksum::{parse_credential, set_process_msg_header_key};
use pb_mapper_core::config::ResolvedAddrs;
use pb_mapper_core::test_support::PROCESS_CREDENTIAL_TEST_LOCK;
use pb_mapper_server::{DirectServerConfig, QuicServerConfig};
use tokio::sync::Mutex;

use super::tunnel::{PinnedTunnel, TunnelHandle, TunnelRuntime};
use super::views::{PathLink, PathOutcome};
use super::{
    AppConfig, DirectOptions, PathInfo, PbMapperState, ServiceConfigData, connect_service,
    force_refresh_server_status, get_server_status_detail, register_service,
};
use crate::error::ErrorCode;

type ProcessCredentialGuard = tokio::sync::MutexGuard<'static, ()>;

/// A state rooted in a temporary directory, so a test never reads or writes
/// the real user config.
///
/// Building a state applies its stored `MSG_HEADER_KEY` to the process, and
/// a fresh temporary directory has no stored key — so this *clears* the
/// process credential. That is process-global, so the returned guard is what
/// keeps one test's state from wiping the credential another test just set.
/// Hold it for the whole test, not just the construction.
async fn temp_state(name: &str) -> (ProcessCredentialGuard, Arc<Mutex<PbMapperState>>, PathBuf) {
    let guard = PROCESS_CREDENTIAL_TEST_LOCK.lock().await;
    let root = std::env::temp_dir().join(format!(
        "pb-mapper-ffi-test-{name}-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default()
    ));
    let state = PbMapperState::new(Some(root.to_string_lossy().into_owned()));
    (guard, Arc::new(Mutex::new(state)), root)
}

/// The window reads a LAN link's kind, address and figures, and hears
/// when a tunnel's path turns to a LAN, but not when its round trip moves.
#[test]
fn a_lan_link_reaches_the_window_and_changes_its_shape() {
    let lan = PathInfo {
        active_direct: 1,
        active_lan: 1,
        direct_sessions: 1,
        lan_sessions: 1,
        last: Some(PathOutcome::Direct {
            setup_ms: 140,
            kind: Some(LinkKind::Lan),
        }),
        link: Some(PathLink {
            kind: LinkKind::Lan,
            family: AddressFamily::Ipv4,
            via: LinkVia::Host,
            transport: DirectTransport::Quic,
            peer: "192.168.21.100:53000".into(),
            local_ip: Some("192.168.21.5".into()),
            interface: Some("WLAN".into()),
            rtt_ms: 4,
            min_rtt_ms: 3,
            setup_ms: 140,
            same_lan: Some(LanEvidence::PublicIpSubnet),
            negotiate_ms: Some(12),
            discover_ms: None,
            stun_ms: None,
            stun: StunOutcome::Cached,
            grant_ms: Some(40),
            first_pong_ms: Some(3),
            lan_first_pong_ms: Some(3),
            handshake_ms: Some(5),
            commit_ms: Some(2),
        }),
        ..PathInfo::default()
    };
    let json = serde_json::to_value(&lan).unwrap();
    assert_eq!(
        (&json["activeLan"], &json["lanSessions"]),
        (&1.into(), &1.into())
    );
    assert_eq!(
        json["last"],
        serde_json::json!({ "path": "direct", "setupMs": 140, "kind": "lan" })
    );
    let link = &json["link"];
    assert_eq!(
        (
            &link["kind"],
            &link["localIp"],
            &link["sameLan"],
            &link["rttMs"]
        ),
        (
            &"lan".into(),
            &"192.168.21.5".into(),
            &"public_ip_subnet".into(),
            &4.into()
        )
    );
    assert!(link.get("discoverMs").is_none());

    let mut internet = lan.clone();
    internet.active_lan = 0;
    internet.last = Some(PathOutcome::Direct {
        setup_ms: 140,
        kind: Some(LinkKind::Internet),
    });
    assert_ne!(lan.shape(), internet.shape());
    let mut slower = lan.clone();
    if let Some(link) = &mut slower.link {
        link.rtt_ms = 40;
    }
    assert_eq!(lan.shape(), slower.shape());
}

#[tokio::test]
async fn ui_server_uses_its_writable_config_directory_and_reports_readiness() {
    let (_process_credential_guard, state, root) = temp_state("server-auth-path").await;
    let quic_port = std::net::UdpSocket::bind("0.0.0.0:0")
        .and_then(|socket| socket.local_addr())
        .unwrap()
        .port();
    let auth_dir = {
        let mut state = state.lock().await;
        let auth_dir = state.config_dir.join("auth");
        state
            .start_server(0, false, Some(quic_port))
            .await
            .expect("UI server should bind and initialize authentication");
        assert!(state.embedded_relay.is_some());
        let status = state.get_local_server_status();
        assert!(status.is_running);
        assert_eq!(status.quic_port, Some(quic_port));
        auth_dir
    };

    assert!(auth_dir.join("admin.key").is_file());
    let isolated = state
        .lock()
        .await
        .isolated_admin_key()
        .expect("embedded relay should expose its administrator key");
    assert_eq!(isolated.len(), 32);
    state
        .lock()
        .await
        .stop_server()
        .await
        .expect("UI server should stop cleanly");
    let _ = std::fs::remove_dir_all(&root);
}

/// After a rotation only the rotated relay's tunnels on the retired key
/// start again. One on another credential would land in the
/// administrator's namespace under the new key, so it is only named; one
/// on another relay is left alone. The relay is known by its address as
/// well as by how it was written. A change of carrier or LAN-first
/// restarts the same ones, and LAN-first only those that may go direct.
#[tokio::test]
async fn a_change_restarts_only_the_relays_tunnels_on_its_key() {
    let (_process_credential_guard, state, root) = temp_state("rotation-tunnels").await;
    let retired = pb_mapper_core::checksum::parse_credential(&"a".repeat(32)).unwrap();
    let other = pb_mapper_core::checksum::parse_credential(&"b".repeat(32)).unwrap();
    let relay: std::net::SocketAddr = "127.0.0.1:7666".parse().unwrap();
    let elsewhere: std::net::SocketAddr = "127.0.0.2:7666".parse().unwrap();
    // A real handle, on a relay that is not there: only the pin matters.
    let idle = Client::from_credential("127.0.0.1:1", retired, false, None)
        .register_with_options(
            RegisterRequest {
                key: "idle".into(),
                local_addr: "127.0.0.1:1".into(),
                transport: Transport::Tcp,
                codec: false,
                force_namespace: false,
            },
            RegisterOptions::default(),
        )
        .await
        .unwrap();
    let handle = Arc::new(TunnelHandle::Share(idle));
    let runtime = |credential, name: &str, addr| TunnelRuntime {
        handle: handle.clone(),
        pin: PinnedTunnel {
            credential,
            endpoint: ResolvedAddrs::from(addr),
            relay: name.to_owned(),
            namespace: None,
            allow_direct: true,
        },
    };
    let mut state = state.lock().await;
    for (key, tunnel) in [
        ("web", runtime(retired, "relay.example:7666", relay)),
        ("cam", runtime(other, "relay.example:7666", relay)),
    ] {
        state.service_runtime.insert(key.to_owned(), tunnel);
    }
    for (key, tunnel) in [
        ("db", runtime(retired, "127.0.0.1:7666", relay)),
        ("far", runtime(retired, "other.example:7666", elsewhere)),
    ] {
        state.client_runtime.insert(key.to_owned(), tunnel);
    }

    let found = state.tunnels_on("relay.example:7666", Some(&relay.into()), retired);
    assert_eq!(found.shares, ["web"]);
    assert_eq!(found.connections, ["db"]);
    assert_eq!(found.stranded, ["cam"]);

    // The handle runs on TCP, trying the LAN first, so leaving either
    // reaches every tunnel on the relay and key; the others stay put.
    let endpoint = ResolvedAddrs::from(relay);
    let to_quic = state.on_other_carrier(
        "relay.example:7666",
        Some(&endpoint),
        retired,
        RelayTransport::Quic,
    );
    assert_eq!(to_quic.shares, ["web"]);
    assert_eq!(to_quic.connections, ["db"]);
    let lan_off = state.on_other_lan_first("relay.example:7666", Some(&endpoint), retired, false);
    assert_eq!(lan_off.shares, ["web"]);
    assert_eq!(lan_off.connections, ["db"]);
    // Those already running with the setting are not offered.
    let lan_on = state.on_other_lan_first("relay.example:7666", Some(&endpoint), retired, true);
    assert!(lan_on.shares.is_empty() && lan_on.connections.is_empty());
    // One kept on the relay is not offered: no start would change it.
    state
        .service_runtime
        .get_mut("web")
        .unwrap()
        .pin
        .allow_direct = false;
    let lan_off = state.on_other_lan_first("relay.example:7666", Some(&endpoint), retired, false);
    assert!(lan_off.shares.is_empty());
    drop(state);
    let _ = std::fs::remove_dir_all(&root);
}

/// Each session of a tunnel goes direct when both ends allow it, and the
/// list says so; a share that keeps to the relay sends its connections
/// there, and the list says at which stage direct gave way.
#[tokio::test]
async fn the_list_says_which_path_each_tunnel_takes() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let (_process_credential_guard, state, root) = temp_state("paths").await;
    let relay = pb_mapper_testkit::Relay::start_with_direct(
        "ffi-paths",
        Some(DirectServerConfig {
            listen: "127.0.0.1:0".parse().unwrap(),
            advertise: "127.0.0.1:0".parse().unwrap(),
        }),
    )
    .await;
    set_process_msg_header_key(Some(pb_mapper_testkit::TEST_ADMIN_KEY)).unwrap();
    state.lock().await.config.server_address = relay.addr().to_string();
    let echo = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let backend = echo.local_addr().unwrap().to_string();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = echo.accept().await {
            tokio::spawn(async move {
                let (mut read, mut write) = socket.split();
                let _ = tokio::io::copy(&mut read, &mut write).await;
            });
        }
    });

    let mut ports = Vec::new();
    for (key, direct) in [("direct", true), ("relayed", false)] {
        register_service(
            &state,
            key.into(),
            backend.clone(),
            "TCP".into(),
            true,
            false,
            DirectOptions {
                allow_direct: direct,
                port_mapping: PortMapping::Off,
            },
        )
        .await
        .unwrap();
        let port = pb_mapper_testkit::reserve_addr(pb_mapper_testkit::Transport::Tcp).await;
        connect_service(
            &state,
            key.into(),
            port.to_string(),
            "TCP".into(),
            false,
            None,
            DirectOptions {
                allow_direct: true,
                port_mapping: PortMapping::Off,
            },
        )
        .await
        .unwrap();
        ports.push(port);
    }
    let running = || async {
        let state = state.lock().await;
        let clients = state.get_client_configs();
        clients.len() == 2 && clients.iter().all(|client| client.status == "running")
    };
    tokio::time::timeout(pb_mapper_testkit::READY_TIMEOUT, async {
        while !running().await {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("both connections come up");

    // One session through each, held open while the list is read.
    let mut sessions = Vec::new();
    for port in ports {
        let mut socket = tokio::net::TcpStream::connect(port).await.unwrap();
        socket.write_all(b"path").await.unwrap();
        let mut echoed = [0; 4];
        tokio::time::timeout(
            pb_mapper_testkit::READY_TIMEOUT,
            socket.read_exact(&mut echoed),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(&echoed, b"path");
        sessions.push(socket);
    }

    let state = state.lock().await;
    let path = |key: &str, infos: &[(String, Option<PathInfo>)]| {
        infos
            .iter()
            .find(|(name, _)| name == key)
            .and_then(|(_, path)| path.clone())
            .unwrap_or_else(|| panic!("{key} reports no path"))
    };
    let clients: Vec<_> = state
        .get_client_configs()
        .into_iter()
        .map(|client| (client.service_key, client.path))
        .collect();
    let shares: Vec<_> = state
        .get_service_configs()
        .into_iter()
        .map(|share| (share.service_key, share.path))
        .collect();

    let direct = path("direct", &clients);
    assert_eq!((direct.active_direct, direct.active_relay), (1, 0));
    assert!(matches!(
        direct.last,
        Some(PathOutcome::Direct { kind: Some(_), .. })
    ));
    assert!(
        direct.link.is_some(),
        "an open direct session reports its link"
    );
    assert_eq!(path("direct", &shares).active_direct, 1);

    // The relay asked the share, which declined: no grant came back.
    let relayed = path("relayed", &clients);
    assert_eq!((relayed.active_direct, relayed.active_relay), (0, 1));
    assert_eq!(
        relayed.last,
        Some(PathOutcome::Relay {
            stage: DirectStage::Discovering
        })
    );
    // As the window reads it; `store_test.dart` parses this same shape.
    assert_eq!(
        serde_json::to_value(&relayed).unwrap()["last"],
        serde_json::json!({ "path": "relay", "stage": "discovering" })
    );
    assert_eq!(relayed.traffic.relay.sent, 4);
    // The share counts the session the relay brought it, too.
    let reached = path("relayed", &shares);
    assert_eq!((reached.active_direct, reached.active_relay), (0, 1));

    drop(sessions);
    drop(state);
    drop(relay);
    let _ = set_process_msg_header_key(None);
    let _ = std::fs::remove_dir_all(&root);
}

/// With QUIC chosen, tunnels and the window's relay check go over it and
/// the list says so; switching back names the tunnels to move. A relay
/// that offers no QUIC is refused before a tunnel starts, not retried.
#[tokio::test]
async fn tunnels_and_checks_follow_the_chosen_carrier() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let (_process_credential_guard, state, root) = temp_state("carrier").await;
    let relay = pb_mapper_testkit::Relay::start_with_quic("ffi-carrier").await;
    set_process_msg_header_key(Some(pb_mapper_testkit::TEST_ADMIN_KEY)).unwrap();
    {
        let mut state = state.lock().await;
        state.config.server_address = relay.addr().to_string();
        state.config.relay_transport = RelayTransport::Quic;
    }
    let echo = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let backend = echo.local_addr().unwrap().to_string();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = echo.accept().await {
            tokio::spawn(async move {
                let (mut read, mut write) = socket.split();
                let _ = tokio::io::copy(&mut read, &mut write).await;
            });
        }
    });

    // Kept on the relay, so the bytes cross the QUIC carrier.
    register_service(
        &state,
        "web".into(),
        backend.clone(),
        "TCP".into(),
        true,
        false,
        DirectOptions {
            allow_direct: false,
            port_mapping: PortMapping::Always,
        },
    )
    .await
    .unwrap();
    let port = pb_mapper_testkit::reserve_addr(pb_mapper_testkit::Transport::Tcp).await;
    connect_service(
        &state,
        "web".into(),
        port.to_string(),
        "TCP".into(),
        false,
        None,
        DirectOptions {
            allow_direct: false,
            port_mapping: PortMapping::Always,
        },
    )
    .await
    .unwrap();
    tokio::time::timeout(pb_mapper_testkit::READY_TIMEOUT, async {
        loop {
            let clients = state.lock().await.get_client_configs();
            if clients.iter().all(|client| client.status == "running") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the connection comes up over QUIC");
    let mut socket = tokio::net::TcpStream::connect(port).await.unwrap();
    socket.write_all(b"quic").await.unwrap();
    let mut echoed = [0; 4];
    tokio::time::timeout(
        pb_mapper_testkit::READY_TIMEOUT,
        socket.read_exact(&mut echoed),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(&echoed, b"quic");

    let detail = force_refresh_server_status(&state).await.unwrap();
    assert!(detail.server_available, "{:?}", detail.error);
    assert!(detail.quic_port.is_some());
    {
        let state = state.lock().await;
        let share = &state.get_service_configs()[0];
        let client = &state.get_client_configs()[0];
        assert_eq!(share.relay_transport, Some(RelayTransport::Quic));
        assert_eq!(client.relay_transport, Some(RelayTransport::Quic));
        // Saved with each tunnel, but one kept on the relay asks no router.
        assert_eq!(share.port_mapping, PortMapping::Always);
        assert_eq!(client.port_mapping, PortMapping::Always);
        let tunnel = state.running_tunnel(false, "web").unwrap();
        assert!(tunnel.diagnostics().port_mapping.is_none());
        // A tunnel saved before the choice maps when needed; one saved
        // with the first draft's switch keeps what it meant.
        let saved = serde_json::to_value(&state.load_service_configs().services["web"]).unwrap();
        let read = |port_mapping: Option<serde_json::Value>| {
            let mut saved = saved.clone();
            match port_mapping {
                Some(value) => saved["port_mapping"] = value,
                None => drop(saved.as_object_mut().unwrap().remove("port_mapping")),
            }
            serde_json::from_value::<ServiceConfigData>(saved)
                .unwrap()
                .port_mapping
        };
        assert_eq!(read(None), PortMapping::Auto);
        assert_eq!(read(Some(true.into())), PortMapping::Always);
        assert_eq!(read(Some(false.into())), PortMapping::Off);
        assert_eq!(read(Some("off".into())), PortMapping::Off);
        let admin = parse_credential(pb_mapper_testkit::TEST_ADMIN_KEY).unwrap();
        let moving =
            state.on_other_carrier(&relay.addr().to_string(), None, admin, RelayTransport::Tcp);
        assert_eq!(moving.shares, ["web"]);
        assert_eq!(moving.connections, ["web"]);
    }

    let tcp_only = pb_mapper_testkit::Relay::start("ffi-carrier-tcp-only").await;
    set_process_msg_header_key(Some(pb_mapper_testkit::TEST_ADMIN_KEY)).unwrap();
    state.lock().await.config.server_address = tcp_only.addr().to_string();
    let refused = register_service(
        &state,
        "cam".into(),
        backend,
        "TCP".into(),
        false,
        false,
        DirectOptions {
            allow_direct: true,
            port_mapping: PortMapping::Off,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(refused.code(), ErrorCode::ServerUnreachable, "{refused}");
    assert!(refused.to_string().contains("QUIC"), "{refused}");

    drop(socket);
    drop((relay, tcp_only));
    let _ = set_process_msg_header_key(None);
    let _ = std::fs::remove_dir_all(&root);
}

/// A relay that offers QUIC on a UDP port nothing gets through to — one
/// its firewall never opened — answers over TCP: the check says that it
/// is UDP, and which port, rather than that the relay is silent.
#[tokio::test]
async fn a_relay_whose_quic_port_is_blocked_is_named_as_such() {
    let (_process_credential_guard, state, root) = temp_state("quic-blocked").await;
    // Holds the port and reads nothing: every QUIC packet is dropped.
    let blackhole = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let blocked = blackhole.local_addr().unwrap().port();
    let relay = pb_mapper_testkit::Relay::start_with_options(
        "ffi-quic-blocked",
        pb_mapper_server::RelayServerOptions {
            direct: None,
            quic: Some(QuicServerConfig {
                listen: "127.0.0.1:0".parse().unwrap(),
                advertise_port: blocked,
            }),
        },
    )
    .await;
    set_process_msg_header_key(Some(pb_mapper_testkit::TEST_ADMIN_KEY)).unwrap();
    {
        let mut state = state.lock().await;
        state.config.server_address = relay.addr().to_string();
        state.config.relay_transport = RelayTransport::Quic;
    }

    let detail = get_server_status_detail(&state).await.unwrap();
    assert!(!detail.server_available);
    assert_eq!(detail.quic_port, Some(blocked));
    let error = detail.error.unwrap();
    assert!(error.contains(&format!("UDP {blocked}")), "{error}");

    drop((relay, blackhole));
    let _ = set_process_msg_header_key(None);
    let _ = std::fs::remove_dir_all(&root);
}

/// The claim is what stands in for the lock that registration no longer
/// holds across its slow phase. Without it, two callers — the window and a
/// terminal, say — could both finish the preflight and both insert, and the
/// second would replace the first's handle, stopping a tunnel the first
/// caller was told had started.
#[tokio::test]
async fn a_second_registration_of_the_same_key_is_refused() {
    let (_process_credential_guard, state, root) = temp_state("claim").await;

    let held = {
        let guard = state.lock().await;
        guard
            .claim_registering("home")
            .expect("first claim should succeed")
    };

    let err = register_service(
        &state,
        "home".to_string(),
        "127.0.0.1:8080".to_string(),
        "TCP".to_string(),
        false,
        false,
        DirectOptions {
            allow_direct: true,
            port_mapping: PortMapping::Off,
        },
    )
    .await
    .expect_err("a claimed key must be refused");
    assert_eq!(
        err.code(),
        ErrorCode::AlreadyInProgress,
        "expected a claim error, got: {err}"
    );

    // A different key is unaffected: the claim is per key, not a global gate.
    {
        let guard = state.lock().await;
        guard
            .claim_registering("other")
            .expect("an unrelated key should still be claimable");
    }

    drop(held);
    let _ = std::fs::remove_dir_all(&root);
}

/// The claim has to survive every way a setup can end, including the ones
/// that return early. It is released by `Drop` precisely so that no error
/// path can forget it — this pins that.
#[tokio::test]
async fn a_failed_registration_releases_its_claim() {
    let (_process_credential_guard, state, root) = temp_state("release").await;
    struct RestoreProcessKey;
    impl Drop for RestoreProcessKey {
        fn drop(&mut self) {
            let _ = set_process_msg_header_key(None);
        }
    }
    set_process_msg_header_key(Some("0123456789abcdefghijklmnopqrstuv")).expect("test credential");
    let _restore_process_key = RestoreProcessKey;

    // Fails in phase 2, while the claim is held.
    let first = register_service(
        &state,
        "home".to_string(),
        "not a socket address".to_string(),
        "TCP".to_string(),
        false,
        false,
        DirectOptions {
            allow_direct: true,
            port_mapping: PortMapping::Off,
        },
    )
    .await
    .expect_err("an unparseable local address should fail");
    assert_eq!(
        first.code(),
        ErrorCode::InvalidAddress,
        "expected an address error, got: {first}"
    );

    // If the claim had leaked, this would be refused rather than reaching
    // the same address parsing it failed on before.
    let second = register_service(
        &state,
        "home".to_string(),
        "not a socket address".to_string(),
        "TCP".to_string(),
        false,
        false,
        DirectOptions {
            allow_direct: true,
            port_mapping: PortMapping::Off,
        },
    )
    .await
    .expect_err("still an unparseable address");
    assert_ne!(
        second.code(),
        ErrorCode::AlreadyInProgress,
        "the claim leaked after a failed registration: {second}"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// Registering and connecting share a key space in the UI but not a claim:
/// exposing `home` and subscribing to `home` are different operations and
/// must not block each other.
#[tokio::test]
async fn registering_and_connecting_claim_separately() {
    let (_process_credential_guard, state, root) = temp_state("separate").await;

    let guard = state.lock().await;
    let _registering = guard
        .claim_registering("home")
        .expect("register claim should succeed");
    guard
        .claim_connecting("home")
        .expect("connecting the same key must not be blocked by registering it");

    drop(guard);
    let _ = std::fs::remove_dir_all(&root);
}

/// A config.json saved before LAN-first could be switched off, or the
/// LAN transport chosen, loads with LAN-first on over QUIC; and a change
/// of either alone leaves the relay settings the same.
#[test]
fn a_config_saved_before_the_switch_tries_the_lan_first() {
    let saved: AppConfig = serde_json::from_str(
        r#"{"server_address":"relay.example:7666","keep_alive_enabled":true,
                "msg_header_key":"","relay_transport":"quic"}"#,
    )
    .unwrap();
    assert!(saved.lan_first);
    assert_eq!(saved.lan_transport, LanTransport::Quic);
    let off = AppConfig {
        lan_first: false,
        lan_transport: LanTransport::Tcp,
        ..saved.clone()
    };
    assert!(off.same_relay(&saved));
    let written = serde_json::to_string(&off).unwrap();
    assert!(written.contains(r#""lan_transport":"tcp""#));
    assert!(serde_json::from_str::<AppConfig>(&written).unwrap() == off);
    assert!(!off.same_relay(&AppConfig {
        relay_transport: RelayTransport::Tcp,
        ..saved
    }));
}

/// A new handle's state holds the defaults and no credential until it is
/// told its directory, and then takes that directory's configuration.
#[tokio::test]
async fn a_state_reads_nothing_until_its_directory_arrives() {
    let _guard = PROCESS_CREDENTIAL_TEST_LOCK.lock().await;
    set_process_msg_header_key(None).unwrap();
    let root = std::env::temp_dir().join(format!(
        "pb-mapper-ffi-test-unloaded-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default()
    ));
    std::fs::create_dir_all(root.join("pb-mapper-ui")).unwrap();
    let saved = AppConfig {
        server_address: "relay.example:7666".into(),
        msg_header_key: pb_mapper_testkit::TEST_ADMIN_KEY.into(),
        ..AppConfig::default()
    };
    std::fs::write(
        root.join("pb-mapper-ui").join("config.json"),
        serde_json::to_vec(&saved).unwrap(),
    )
    .unwrap();

    let mut state = PbMapperState::unloaded();
    assert!(state.config == AppConfig::default());
    assert!(!pb_mapper_core::checksum::process_credential_configured());

    state
        .set_app_directory_path(Some(root.to_string_lossy().into_owned()))
        .unwrap();
    assert!(state.config == saved);
    assert!(pb_mapper_core::checksum::process_credential_configured());

    set_process_msg_header_key(None).unwrap();
    let _ = std::fs::remove_dir_all(&root);
}
