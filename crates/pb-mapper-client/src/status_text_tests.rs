//! The text the public status callbacks receive: `connected`, `retrying`,
//! `failed`, and `failed: <reason>`. The testkit, the CLI's tests and callers
//! outside the workspace read it, so it is pinned here through the public
//! entry points of both tunnel directions, against a real relay.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pb_mapper_core::checksum::parse_credential;
use pb_mapper_core::config::ResolvedAddrs;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use uni_stream::stream::{TcpListenerProvider, TcpStreamProvider, UdpStreamProvider};

use crate::client::{run_client_side_cli_with_callback_scoped, run_client_side_cli_with_shutdown};
use crate::sdk::{
    Client, ConnectOptions, ConnectRequest, Error, PathPolicy, RegisterRequest, Transport,
};
use crate::server::{ServerTunnelOptions, StatusCallback, run_server_side_cli_with_shutdown};
use crate::test_relay::TestRelay;

type Seen = Arc<Mutex<Vec<String>>>;

/// A callback that keeps every status it is given, in order.
fn recorder() -> (Seen, StatusCallback) {
    let seen = Seen::default();
    let sink = seen.clone();
    let callback: StatusCallback = Box::new(move |status: &str| {
        sink.lock().unwrap().push(status.to_owned());
    });
    (seen, callback)
}

/// The latest status once it satisfies `done`, checking that only
/// `retrying` came before it.
async fn settled(seen: &Seen, done: impl Fn(&str) -> bool) -> String {
    loop {
        let statuses = seen.lock().unwrap().clone();
        if let Some((last, earlier)) = statuses.split_last()
            && done(last)
        {
            assert!(earlier.iter().all(|s| s == "retrying"), "{statuses:?}");
            return last.clone();
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn options(is_datagram: bool) -> ServerTunnelOptions {
    ServerTunnelOptions {
        need_codec: false,
        is_datagram,
        keep_alive: false,
        namespace: None,
        force_namespace: false,
    }
}

fn addrs(addr: &str) -> ResolvedAddrs {
    ResolvedAddrs::from(addr.parse::<SocketAddr>().unwrap())
}

/// The reason an SDK tunnel gave up with.
fn failure(ready: crate::sdk::Result<()>) -> String {
    match ready {
        Err(Error::TunnelFailed { reason }) => reason,
        other => panic!("expected a failed tunnel, got {other:?}"),
    }
}

#[tokio::test]
async fn public_status_callbacks_keep_their_text() {
    tokio::time::timeout(Duration::from_secs(20), async {
        let relay = TestRelay::start("status-text", None).await;
        let relay_addr = ResolvedAddrs::from(relay.addr);
        let key: Arc<str> = Arc::from("status-text");
        let stop = CancellationToken::new();

        // A connect tunnel to a key nobody publishes yet retries.
        let (connect, callback) = recorder();
        let connecting = tokio::spawn(run_client_side_cli_with_shutdown::<TcpListenerProvider>(
            addrs("127.0.0.1:0"),
            relay_addr.clone(),
            key.clone(),
            false,
            None,
            Some(callback),
            Some(relay.credential),
            stop.clone(),
        ));
        assert_eq!(settled(&connect, |_| true).await, "retrying");

        // Once a publisher is up, both ends are connected.
        let backend = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (publish, callback) = recorder();
        let publishing = tokio::spawn(run_server_side_cli_with_shutdown::<TcpStreamProvider>(
            ResolvedAddrs::from(backend.local_addr().unwrap()),
            relay_addr.clone(),
            key.clone(),
            options(false),
            Some(callback),
            relay.credential,
            stop.clone(),
        ));
        settled(&publish, |last| last == "connected").await;
        settled(&connect, |last| last == "connected").await;

        // The same key over UDP is refused for good: the publisher's pool
        // gives up, with the relay's reason.
        let (mismatch, callback) = recorder();
        run_server_side_cli_with_shutdown::<UdpStreamProvider>(
            addrs("127.0.0.1:9"),
            relay_addr.clone(),
            key.clone(),
            options(true),
            Some(callback),
            relay.credential,
            stop.clone(),
        )
        .await;
        let failed = settled(&mismatch, |_| true).await;
        assert!(
            failed.starts_with("failed: service_transport_mismatch: "),
            "{failed}"
        );
        // The SDK reports the same reason.
        let admin = Client::from_credential(relay.addr.to_string(), relay.credential, false, None);
        let registration = admin
            .register(RegisterRequest {
                key: key.to_string(),
                local_addr: "127.0.0.1:9".into(),
                transport: Transport::Udp,
                codec: false,
                force_namespace: false,
            })
            .await
            .unwrap();
        let reason = failure(registration.wait_ready().await);
        assert_eq!(format!("failed: {reason}"), failed);

        // A temporary credential asking after another namespace is refused
        // for good too: the connect tunnel gives up, with the relay's reason.
        let issued = admin
            .admin()
            .unwrap()
            .issue_key(Duration::from_secs(60), None)
            .await
            .unwrap();
        let temporary = parse_credential(&issued.credential).unwrap();
        let (refused, callback) = recorder();
        run_client_side_cli_with_shutdown::<TcpListenerProvider>(
            addrs("127.0.0.1:0"),
            relay_addr.clone(),
            key.clone(),
            false,
            Some(0),
            Some(callback),
            Some(temporary),
            stop.clone(),
        )
        .await;
        let failed = settled(&refused, |_| true).await;
        assert!(
            failed.starts_with(
                "failed: get status failed: relay refused the status request: \
                 namespace_access_denied: "
            ),
            "{failed}"
        );
        // The reason ends in a line break, which the SDK trims.
        let connection = Client::from_credential(relay.addr.to_string(), temporary, false, Some(0))
            .connect_with_options(
                ConnectRequest {
                    key: key.to_string(),
                    local_addr: "127.0.0.1:0".into(),
                    transport: Transport::Tcp,
                },
                ConnectOptions {
                    path: PathPolicy::RelayOnly,
                    ..ConnectOptions::default()
                },
            )
            .await
            .unwrap();
        let reason = failure(connection.wait_ready().await);
        assert_eq!(format!("failed: {reason}\n"), failed);

        // An address that does not parse fails at once, with no reason.
        let (unparsed, callback) = recorder();
        run_client_side_cli_with_callback_scoped::<TcpListenerProvider, _>(
            "not an address",
            "not an address",
            key.clone(),
            false,
            None,
            Some(callback),
            None,
        )
        .await;
        assert_eq!(*unparsed.lock().unwrap(), ["failed"]);

        stop.cancel();
        connecting.await.unwrap();
        publishing.await.unwrap();
    })
    .await
    .unwrap();
}
