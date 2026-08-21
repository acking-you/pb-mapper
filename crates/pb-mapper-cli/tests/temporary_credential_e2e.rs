// See the note in `regression.rs`: the whole file is test code.
#![allow(clippy::unwrap_used, clippy::expect_used)]

//! End-to-end tunnels authenticated by a temporary credential.
//!
//! `regression.rs` already covers the credential lifecycle at the hand-rolled
//! frame level. These cases run the real `register` and `connect` entry points
//! instead, so they also exercise connection pooling, the heartbeat window,
//! control-connection reconnect, the stream-establishment handshake, and
//! `connect`'s startup status probe — none of which a hand-written frame touches.
//!
//! Every case builds its own relay through `pb-mapper-testkit`, so they run
//! concurrently and never collide with a relay already on the machine.

use std::time::Duration;

use pb_mapper_auth::MIN_TEMP_KEY_TTL;
use pb_mapper_testkit::{
    Relay, Transport, TunnelSpec, admin_credential, run_echo_delay, run_raw_tcp_echo,
    run_udp_datagram_echo,
};
use uni_stream::stream::TcpStreamProvider;

/// A TTL long enough that nothing expires mid-test.
const LONG_TTL: Duration = Duration::from_secs(600);

/// Drive traffic through a tunnel a temporary credential registered and subscribed.
async fn assert_temporary_credential_echoes(transport: Transport, need_codec: bool) {
    let label = format!(
        "temp-{}-{}",
        transport.name(),
        if need_codec { "codec" } else { "plain" }
    );
    let relay = Relay::start(&label).await;
    let (_key_id, credential) = relay.issue_credential(LONG_TTL, &label).await;

    // `namespace: None` resolves to the credential's own namespace, which is its
    // key ID — so the temporary tunnel needs no explicit namespace anywhere.
    let tunnel = relay
        .start_tunnel(
            TunnelSpec::new(transport)
                .codec(need_codec)
                .credential(credential),
        )
        .await;

    match transport {
        Transport::Tcp => run_echo_delay::<TcpStreamProvider, _>(tunnel.addr(), 10).await,
        Transport::Udp => run_udp_datagram_echo(tunnel.addr(), 10, 8, None).await,
    }
}

#[tokio::test]
async fn temporary_credential_tcp_tunnel_echoes_without_codec() {
    assert_temporary_credential_echoes(Transport::Tcp, false).await;
}

#[tokio::test]
async fn temporary_credential_tcp_tunnel_echoes_with_codec() {
    assert_temporary_credential_echoes(Transport::Tcp, true).await;
}

#[tokio::test]
async fn temporary_credential_udp_tunnel_echoes_without_codec() {
    assert_temporary_credential_echoes(Transport::Udp, false).await;
}

#[tokio::test]
async fn temporary_credential_udp_tunnel_echoes_with_codec() {
    assert_temporary_credential_echoes(Transport::Udp, true).await;
}

/// Two credentials may register the same service name; neither sees the other's.
///
/// The echo servers are tagged, because without a tag a leak between namespaces
/// would still echo the payload byte for byte and the assertion would pass.
#[tokio::test]
async fn two_temporary_credentials_share_a_service_name() {
    let relay = Relay::start("temp-shared-name").await;
    let (first_id, first) = relay.issue_credential(LONG_TTL, "first").await;
    let (second_id, second) = relay.issue_credential(LONG_TTL, "second").await;
    assert_ne!(first_id, second_id);

    const SHARED: &str = "shared-service";
    let first_tunnel = relay
        .start_tunnel(
            TunnelSpec::new(Transport::Tcp)
                .credential(first)
                .service_key(SHARED)
                .echo_tag(b'1'),
        )
        .await;
    let second_tunnel = relay
        .start_tunnel(
            TunnelSpec::new(Transport::Tcp)
                .credential(second)
                .service_key(SHARED)
                .echo_tag(b'2'),
        )
        .await;

    // Each credential's `Keys` view holds the shared name exactly once.
    for credential in [first, second] {
        let keys = relay.registered_keys(credential, None).await.unwrap();
        assert_eq!(
            keys.iter().filter(|key| *key == SHARED).count(),
            1,
            "a credential should see the shared name once, saw {keys:?}"
        );
    }

    // Both tunnels forward concurrently, each to its own echo server.
    run_raw_tcp_echo(first_tunnel.addr(), 5, Some(b'1')).await;
    run_raw_tcp_echo(second_tunnel.addr(), 5, Some(b'2')).await;
}

/// Renewing keeps a live tunnel forwarding, and does not change the credential.
#[tokio::test]
async fn renew_keeps_a_live_tunnel_forwarding() {
    let relay = Relay::start("temp-renew").await;
    let issued = relay.issue(MIN_TEMP_KEY_TTL, "renew").await;
    let key_id = issued.metadata.key_id;
    let credential = pb_mapper_core::checksum::parse_credential(&issued.credential).unwrap();

    let tunnel = relay
        .start_tunnel(TunnelSpec::new(Transport::Tcp).credential(credential))
        .await;

    let renewed = relay.renew(key_id, LONG_TTL).await;
    assert_eq!(renewed.metadata.key_id, key_id);
    // Renewal moves the expiry without reissuing; a renewed credential that
    // changed text would silently break every process already holding it.
    assert_eq!(renewed.credential, issued.credential);
    assert!(renewed.metadata.expires_at > issued.metadata.expires_at);

    // Past the original TTL, the renewed tunnel still carries traffic.
    tokio::time::sleep(MIN_TEMP_KEY_TTL + Duration::from_secs(2)).await;
    tunnel.wait_until_forwarding().await;
    run_echo_delay::<TcpStreamProvider, _>(tunnel.addr(), 3).await;
}

/// Expiry — not revocation — tears a live tunnel down.
///
/// The lifecycle actor drives expiry from a one-second tick, and the shortest
/// accepted TTL is [`MIN_TEMP_KEY_TTL`], so this case costs that plus a tick.
#[tokio::test]
async fn expiry_closes_a_live_tunnel() {
    let relay = Relay::start("temp-expiry").await;
    let (_key_id, credential) = relay.issue_credential(MIN_TEMP_KEY_TTL, "expiry").await;

    let tunnel = relay
        .start_tunnel(TunnelSpec::new(Transport::Tcp).credential(credential))
        .await;

    tunnel
        .wait_until_not_forwarding(MIN_TEMP_KEY_TTL + Duration::from_secs(10))
        .await;
}

/// Revoking a credential closes the tunnel it authenticated.
#[tokio::test]
async fn revoke_closes_a_live_tunnel() {
    let relay = Relay::start("temp-revoke").await;
    let (key_id, credential) = relay.issue_credential(LONG_TTL, "revoke").await;

    let tunnel = relay
        .start_tunnel(TunnelSpec::new(Transport::Tcp).credential(credential))
        .await;

    relay.revoke(key_id).await;
    tunnel
        .wait_until_not_forwarding(Duration::from_secs(10))
        .await;
}

/// The administrator can register into a temporary namespace, with `--force`.
///
/// The subscriber here is the temporary credential itself, so the case also shows
/// the service landing where the tenant can reach it.
#[tokio::test]
async fn admin_registers_into_a_temporary_namespace() {
    let relay = Relay::start("temp-admin-namespace").await;
    let (key_id, credential) = relay.issue_credential(LONG_TTL, "admin-force").await;

    let tunnel = relay
        .start_tunnel(
            TunnelSpec::new(Transport::Tcp)
                .credential(admin_credential())
                .connect_credential(credential)
                .namespace(key_id.as_u64())
                .force_namespace(true)
                .service_key("admin-placed"),
        )
        .await;

    // The tenant sees it as an ordinary service in its own namespace.
    let keys = relay.registered_keys(credential, None).await.unwrap();
    assert!(
        keys.iter().any(|key| key == "admin-placed"),
        "the tenant should see the service the administrator placed, saw {keys:?}"
    );
    // And the administrator's own namespace 0 stays empty.
    let admin_keys = relay
        .registered_keys(admin_credential(), None)
        .await
        .unwrap();
    assert!(
        !admin_keys.iter().any(|key| key == "admin-placed"),
        "namespace 0 should not hold the service, saw {admin_keys:?}"
    );

    run_echo_delay::<TcpStreamProvider, _>(tunnel.addr(), 3).await;
}
