//! The relay's own behavior, driven with hand-written frames.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use pb_mapper_client::client::run_client_side_cli_with_callback;
use pb_mapper_client::server::{ServerTunnelOptions, run_server_side_cli_with_pinned_credential};
use pb_mapper_core::checksum::{Credential, parse_credential};
use pb_mapper_protocol::command::{
    AdminRequest, AdminResponse, LocalServer, MessageSerializer, PbConnRequest, PbConnResponse,
    PbConnStatusReq, PbConnStatusResp, PbServerRequest,
};
use pb_mapper_protocol::secure::ClientHeaderSession;
use pb_mapper_protocol::{
    MessageReader, MessageWriter, get_header_msg_reader, get_header_msg_writer,
};
use pb_mapper_server::run_server_with_auth_config;
use pb_mapper_testkit::{
    PROBE, Transport, V2ControlSpec, admin_credential, register_v2_control, reserve_addr, tcp_echo,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use uni_stream::stream::TcpStreamProvider;

use crate::bound_listener::{BoundListener, with_bound_listener};
use crate::credential::{StateDir, auth_config};
use crate::fixtures::{
    InProcessRelay, issue_before_start, read_stream_request, register_control_conn_parts,
    serve_one_stream, subscribe,
};

/// How long a case waits for what the relay does at once: a bound for a
/// loaded machine, not a timing the case asserts.
const READY: Duration = Duration::from_secs(5);

/// How a control connection that never opens a data stream ends.
enum StaleEnd {
    /// It receives a stream request and leaves it unacknowledged.
    IgnoreStream,
    /// As `IgnoreStream`, then waits for the relay to retire it and reports that.
    IgnoreStreamUntilRetired(oneshot::Sender<()>),
    /// It acknowledges its first stream request and opens nothing.
    AckStream,
    /// It acknowledges its first stream request [`LATE_ACK`] late and opens
    /// nothing, then ends as `IgnoreStreamUntilRetired`.
    AckLateThenIgnoreUntilRetired(oneshot::Sender<()>),
}

/// Late for the relay's first 300 ms acknowledgement timeout, and early in the
/// two-second grace that follows it, so the relay readmits the connection and
/// learns to wait 825 ms for its next acknowledgement.
const LATE_ACK: Duration = Duration::from_millis(500);

/// A stale provider: register `key` as a legacy control connection, report it
/// `ready`, and end as `end` says.
async fn stale_control(addr: SocketAddr, key: String, ready: oneshot::Sender<()>, end: StaleEnd) {
    let control = TcpStream::connect(addr).await.unwrap();
    let (mut reader_stream, mut writer_stream) = control.into_split();
    let mut reader = get_header_msg_reader(&mut reader_stream).unwrap();
    {
        let mut writer = get_header_msg_writer(&mut writer_stream).unwrap();
        register_control_conn_parts(&mut reader, &mut writer, &key).await;
    }
    ready.send(()).unwrap();
    let ack = |client_id, server_generation| {
        PbServerRequest::StreamAck {
            client_id,
            server_generation,
        }
        .encode()
        .unwrap()
    };
    match end {
        StaleEnd::IgnoreStream => {
            read_stream_request(&mut reader, "stale control").await;
        }
        StaleEnd::IgnoreStreamUntilRetired(retired) => {
            read_stream_request(&mut reader, "stale control").await;
            until_retired(&mut reader, retired).await;
            return;
        }
        StaleEnd::AckStream => {
            let (client_id, server_generation) =
                read_stream_request(&mut reader, "stale control").await;
            let mut writer = get_header_msg_writer(&mut writer_stream).unwrap();
            writer
                .write_msg(&ack(client_id, server_generation))
                .await
                .unwrap();
        }
        StaleEnd::AckLateThenIgnoreUntilRetired(retired) => {
            let (client_id, server_generation) =
                read_stream_request(&mut reader, "stale control").await;
            tokio::time::sleep(LATE_ACK).await;
            let mut writer = get_header_msg_writer(&mut writer_stream).unwrap();
            writer
                .write_msg(&ack(client_id, server_generation))
                .await
                .unwrap();
            read_stream_request(&mut reader, "stale control").await;
            until_retired(&mut reader, retired).await;
            return;
        }
    }
    std::future::pending::<()>().await;
}

/// Wait for the relay to close the control connection `reader` reads, which
/// a missed acknowledgement earns after a two-second grace, and report it.
async fn until_retired(reader: &mut impl MessageReader, retired: oneshot::Sender<()>) {
    let closed = timeout(Duration::from_secs(5), reader.read_msg())
        .await
        .expect("stale control was not retired by the server");
    if let Ok(bytes) = closed {
        assert!(bytes.is_empty());
    }
    retired.send(()).unwrap();
}

async fn read_status(server_addr: SocketAddr, request: PbConnStatusReq) -> PbConnStatusResp {
    let mut stream = TcpStream::connect(server_addr).await.unwrap();
    let request = PbConnRequest::Status(request).encode().unwrap();
    {
        let mut writer = get_header_msg_writer(&mut stream).unwrap();
        writer.write_msg(&request).await.unwrap();
    }

    let mut reader = get_header_msg_reader(&mut stream).unwrap();
    let response = timeout(READY, reader.read_msg())
        .await
        .expect("status response timed out")
        .unwrap();
    let PbConnResponse::Status(status) = PbConnResponse::decode(response).unwrap() else {
        panic!("unexpected status response");
    };
    status
}

async fn read_status_keys(server_addr: SocketAddr) -> Vec<String> {
    let PbConnStatusResp::Keys(keys) = read_status(server_addr, PbConnStatusReq::Keys).await else {
        panic!("unexpected status keys response");
    };
    keys
}

/// Whether the relay routes to each registration of `key`, by connection id,
/// as its status reports them; none once `key` has none.
async fn read_service_health(server_addr: SocketAddr, key: &str) -> Vec<(u32, bool)> {
    let request = PbConnStatusReq::Service {
        direct_query: None,
        key: key.to_string(),
    };
    let PbConnStatusResp::Service { connections, .. } = read_status(server_addr, request).await
    else {
        panic!("unexpected service status response");
    };
    connections
        .iter()
        .map(|connection| (connection.conn_id, connection.healthy))
        .collect()
}

/// Assert that `response` is the retryable refusal a subscribe gets when no
/// registration can serve it: the client tries again, and counts it neither
/// against the relay nor against the connection it came over.
fn assert_unavailable(response: &[u8]) {
    let response = PbConnResponse::decode(response).unwrap();
    let PbConnResponse::Error(refusal) = response else {
        panic!("expected a refusal, got {response:?}");
    };
    assert_eq!(refusal.code, "service_not_available");
    assert!(refusal.retryable, "the refusal is not retryable");
}

/// Acknowledge a stream request on a control connection's `writer`.
async fn acknowledge(writer: &mut impl MessageWriter, (client_id, server_generation): (u32, u64)) {
    let ack = PbServerRequest::StreamAck {
        client_id,
        server_generation,
    }
    .encode()
    .unwrap();
    writer.write_msg(&ack).await.unwrap();
}

/// Open the data stream a stream request asked `key`'s provider for.
async fn open_stream(
    addr: SocketAddr,
    key: &str,
    (dst_id, server_generation): (u32, u64),
) -> TcpStream {
    let mut data = TcpStream::connect(addr).await.unwrap();
    let request = PbConnRequest::Stream {
        data_protocol: None,
        key: key.to_string(),
        dst_id,
        server_generation,
    }
    .encode()
    .unwrap();
    get_header_msg_writer(&mut data)
        .unwrap()
        .write_msg(&request)
        .await
        .unwrap();
    data
}

async fn send_v2_request(
    server_addr: SocketAddr,
    credential: &Credential,
    request: PbConnRequest,
) -> (TcpStream, ClientHeaderSession, PbConnResponse) {
    let mut stream = TcpStream::connect(server_addr).await.unwrap();
    let session = ClientHeaderSession::new_v2(credential).unwrap();
    session
        .write_initial(&mut stream, &request.encode().unwrap())
        .await
        .unwrap();
    let response = {
        let mut reader = session.response_reader(&mut stream).unwrap();
        let message = timeout(READY, reader.read_msg())
            .await
            .expect("v2 response timed out")
            .unwrap();
        PbConnResponse::decode(message).unwrap()
    };
    (stream, session, response)
}

#[tokio::test]
async fn temporary_credentials_are_isolated_denied_admin_and_revoked_live() {
    let config = auth_config();
    let [first, second] = issue_before_start(&config, ["first", "second"]).await;
    let relay = InProcessRelay::start(config).await;
    let server_addr = relay.addr;

    let first_credential = parse_credential(&first.credential).unwrap();
    let second_credential = parse_credential(&second.credential).unwrap();
    let register_request = |service: &str| PbConnRequest::Register {
        direct: None,
        need_codec: false,
        is_datagram: false,
        key: service.to_string(),
        protocol_version: Some(2),
        client_instance_id: Some("temporary-auth-regression".to_string()),
        heartbeat_interval_ms: Some(50),
        heartbeat_tolerance_ms: Some(150),
    };
    let (mut first_control, _, first_register) = send_v2_request(
        server_addr,
        &first_credential,
        register_request("same-name"),
    )
    .await;
    let (_second_control, _, second_register) = send_v2_request(
        server_addr,
        &second_credential,
        register_request("same-name"),
    )
    .await;
    let first_conn_id = match first_register {
        PbConnResponse::RegisterV2 { conn_id, .. } => conn_id,
        response => panic!("unexpected first register response: {response:?}"),
    };
    let second_conn_id = match second_register {
        PbConnResponse::RegisterV2 { conn_id, .. } => conn_id,
        response => panic!("unexpected second register response: {response:?}"),
    };
    assert_ne!(first_conn_id, second_conn_id);

    let status_request = PbConnRequest::Status(PbConnStatusReq::Service {
        direct_query: None,
        key: "same-name".to_string(),
    });
    for (credential, expected_conn_id) in [
        (&first_credential, first_conn_id),
        (&second_credential, second_conn_id),
    ] {
        let (_, _, response) =
            send_v2_request(server_addr, credential, status_request.clone()).await;
        let PbConnResponse::Status(PbConnStatusResp::Service { connections, .. }) = response else {
            panic!("unexpected scoped status response: {response:?}");
        };
        assert_eq!(connections.len(), 1);
        assert_eq!(connections[0].conn_id, expected_conn_id);
    }

    for admin_request in [
        AdminRequest::AuthStatus,
        AdminRequest::NamespaceAllocate,
        AdminRequest::KeyIssueScoped {
            namespace: 1,
            ttl_seconds: 60,
            label: None,
        },
        AdminRequest::ServiceList {
            key_id: None,
            page: 0,
            page_size: 100,
        },
        AdminRequest::ConnectionList {
            key_id: None,
            page: 0,
            page_size: 100,
        },
    ] {
        let (_, _, denied) = send_v2_request(
            server_addr,
            &first_credential,
            PbConnRequest::Admin(admin_request),
        )
        .await;
        let PbConnResponse::Error(denied) = denied else {
            panic!("temporary credential unexpectedly received an admin response");
        };
        assert_eq!(denied.code, "admin_permission_required");
    }

    let (_, _, revoked) = send_v2_request(
        server_addr,
        &admin_credential(),
        PbConnRequest::Admin(AdminRequest::KeyRevoke {
            key_id: first.metadata.key_id.as_u64(),
        }),
    )
    .await;
    assert!(matches!(
        revoked,
        PbConnResponse::Admin(AdminResponse::KeyRevoked(_))
    ));
    let mut byte = [0_u8; 1];
    let closed = timeout(READY, first_control.read(&mut byte))
        .await
        .expect("revoked control connection was not closed")
        .unwrap();
    assert_eq!(closed, 0);

    let (_, _, second_still_active) =
        send_v2_request(server_addr, &second_credential, status_request).await;
    assert!(matches!(
        second_still_active,
        PbConnResponse::Status(PbConnStatusResp::Service { .. })
    ));

    relay.stop().await;
}

#[tokio::test]
async fn revoking_subscriber_credential_closes_cross_credential_data_stream() {
    let config = auth_config();
    let [issued] = issue_before_start(&config, ["active-stream"]).await;
    let relay = InProcessRelay::start(config).await;
    let server_addr = relay.addr;

    let credential = parse_credential(&issued.credential).unwrap();
    let admin = admin_credential();
    let service = "revoked-stream";
    let mut control = TcpStream::connect(server_addr).await.unwrap();
    let control_session = ClientHeaderSession::new_v2(&admin).unwrap();
    let register = PbConnRequest::RegisterScoped {
        direct: None,
        need_codec: false,
        is_datagram: false,
        key: service.to_string(),
        namespace: issued.metadata.key_id.as_u64(),
        force_namespace: true,
        protocol_version: Some(2),
        client_instance_id: Some("active-stream-test".to_string()),
        heartbeat_interval_ms: Some(5_000),
        heartbeat_tolerance_ms: Some(15_000),
    };
    control_session
        .write_initial(&mut control, &register.encode().unwrap())
        .await
        .unwrap();
    let (mut control_read, mut control_write) = control.into_split();
    let mut control_reader = control_session.response_reader(&mut control_read).unwrap();
    let register_response = timeout(READY, control_reader.read_msg())
        .await
        .expect("register response timed out")
        .unwrap();
    assert!(matches!(
        PbConnResponse::decode(register_response).unwrap(),
        PbConnResponse::RegisterV2 { .. }
    ));

    let mut subscriber = TcpStream::connect(server_addr).await.unwrap();
    let subscriber_session = ClientHeaderSession::new_v2(&credential).unwrap();
    subscriber_session
        .write_initial(
            &mut subscriber,
            &PbConnRequest::Subcribe {
                data_protocol: None,
                key: service.to_string(),
            }
            .encode()
            .unwrap(),
        )
        .await
        .unwrap();

    let stream_request = timeout(READY, control_reader.read_msg())
        .await
        .expect("stream request timed out")
        .unwrap();
    let LocalServer::Stream {
        client_id,
        server_generation,
    } = LocalServer::decode(stream_request).unwrap()
    else {
        panic!("unexpected local server stream request");
    };
    let mut control_writer = control_session
        .continuation_writer(&mut control_write)
        .unwrap();
    control_writer
        .write_msg(
            &PbServerRequest::StreamAck {
                client_id,
                server_generation,
            }
            .encode()
            .unwrap(),
        )
        .await
        .unwrap();

    let mut provider = TcpStream::connect(server_addr).await.unwrap();
    let provider_session = ClientHeaderSession::new_v2(&admin).unwrap();
    provider_session
        .write_initial(
            &mut provider,
            &PbConnRequest::StreamScoped {
                data_protocol: None,
                key: service.to_string(),
                namespace: issued.metadata.key_id.as_u64(),
                dst_id: client_id,
                server_generation,
            }
            .encode()
            .unwrap(),
        )
        .await
        .unwrap();
    {
        let mut subscriber_reader = subscriber_session.response_reader(&mut subscriber).unwrap();
        let response = timeout(READY, subscriber_reader.read_msg())
            .await
            .expect("subscribe response timed out")
            .unwrap();
        assert!(matches!(
            PbConnResponse::decode(response).unwrap(),
            PbConnResponse::Subcribe { .. }
        ));
    }
    {
        let mut provider_reader = provider_session.response_reader(&mut provider).unwrap();
        let response = timeout(READY, provider_reader.read_msg())
            .await
            .expect("provider stream response timed out")
            .unwrap();
        assert!(matches!(
            PbConnResponse::decode(response).unwrap(),
            PbConnResponse::Stream { .. }
        ));
    }

    subscriber.write_all(b"ready").await.unwrap();
    let mut ready = [0_u8; 5];
    timeout(READY, provider.read_exact(&mut ready))
        .await
        .expect("active data stream did not forward")
        .unwrap();
    assert_eq!(&ready, b"ready");

    let (_, _, revoked) = send_v2_request(
        server_addr,
        &admin,
        PbConnRequest::Admin(AdminRequest::KeyRevoke {
            key_id: issued.metadata.key_id.as_u64(),
        }),
    )
    .await;
    assert!(matches!(
        revoked,
        PbConnResponse::Admin(AdminResponse::KeyRevoked(_))
    ));

    let mut byte = [0_u8; 1];
    let read = timeout(READY, subscriber.read(&mut byte))
        .await
        .expect("revoked subscriber data stream was not closed")
        .unwrap();
    assert_eq!(read, 0, "revoked subscriber data stream remained open");

    match timeout(Duration::from_millis(200), control_reader.read_msg()).await {
        Err(_) | Ok(Ok(_)) => {}
        Ok(Err(error)) => panic!("administrator registration was cancelled too: {error}"),
    }

    relay.stop().await;
}

#[tokio::test]
async fn status_service_reports_registered_v2_control_connection() {
    let relay = InProcessRelay::start(auth_config()).await;

    let key = "sf-backend";
    let control = TcpStream::connect(relay.addr).await.unwrap();
    let (mut reader_stream, mut writer_stream) = control.into_split();
    let mut reader = get_header_msg_reader(&mut reader_stream).unwrap();
    let mut writer = get_header_msg_writer(&mut writer_stream).unwrap();
    let (conn_id, generation) = register_v2_control(
        &mut reader,
        &mut writer,
        key,
        V2ControlSpec::new()
            .instance_id("regression-test-client")
            .response_timeout(READY),
    )
    .await;

    let request = PbConnStatusReq::Service {
        direct_query: None,
        key: key.to_string(),
    };
    let PbConnStatusResp::Service {
        direct: _,
        key: status_key,
        connections,
    } = read_status(relay.addr, request).await
    else {
        panic!("unexpected service status response");
    };

    assert_eq!(status_key, key);
    assert_eq!(connections.len(), 1);
    assert_eq!(connections[0].conn_id, conn_id);
    assert_eq!(connections[0].generation, generation);
    assert!(connections[0].healthy);

    relay.stop().await;
}

#[tokio::test]
async fn subscribe_retires_unacked_control_connection() {
    let relay = InProcessRelay::start(auth_config()).await;

    let key = "sf-backend";
    let (stale_ready_tx, stale_ready_rx) = oneshot::channel();
    let stale_task = tokio::spawn(stale_control(
        relay.addr,
        key.to_string(),
        stale_ready_tx,
        StaleEnd::IgnoreStream,
    ));
    stale_ready_rx.await.unwrap();

    let mut client = subscribe(relay.addr, key).await;
    let mut reader = get_header_msg_reader(&mut client).unwrap();
    let response = timeout(Duration::from_secs(4), reader.read_msg())
        .await
        .expect("subscribe did not finish after stale control timed out")
        .expect("the relay closed the subscriber's connection unanswered");
    assert_unavailable(response);

    // Subscribe failure and the manager's suspect sweep complete independently.
    timeout(Duration::from_secs(3), async {
        loop {
            let keys = read_status_keys(relay.addr).await;
            if !keys.iter().any(|candidate| candidate == key) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("unacked stale control connection kept key registered");

    stale_task.abort();
    relay.stop().await;
}

#[tokio::test]
async fn subscribe_waits_for_replacement_after_retiring_stale_control_connection() {
    let relay = InProcessRelay::start(auth_config()).await;

    let key = "sf-backend";
    let (stale_ready_tx, stale_ready_rx) = oneshot::channel();
    let (retired_tx, retired_rx) = oneshot::channel();
    let stale_task = tokio::spawn(stale_control(
        relay.addr,
        key.to_string(),
        stale_ready_tx,
        StaleEnd::IgnoreStreamUntilRetired(retired_tx),
    ));
    stale_ready_rx.await.unwrap();

    let (replacement_stream_tx, replacement_stream_rx) = oneshot::channel();
    let replacement_task = tokio::spawn({
        let addr = relay.addr;
        let key = key.to_string();
        async move {
            retired_rx.await.unwrap();
            serve_one_stream(
                addr,
                key,
                "replacement control",
                None,
                replacement_stream_tx,
            )
            .await;
        }
    });

    let mut client = subscribe(relay.addr, key).await;
    let mut reader = get_header_msg_reader(&mut client).unwrap();
    let response = timeout(Duration::from_secs(3), reader.read_msg())
        .await
        .expect("subscribe did not wait for replacement control connection")
        .unwrap();
    assert!(matches!(
        PbConnResponse::decode(response).unwrap(),
        PbConnResponse::Subcribe { .. }
    ));

    let stream = replacement_stream_rx.await.unwrap();
    drop(stream);
    stale_task.abort();
    replacement_task.abort();
    relay.stop().await;
}

/// As above, for a provider whose acknowledgements the relay has learned to
/// wait longer for: the subscriber's wait for a replacement runs from when
/// the relay gave up on the stale connection, which is later.
#[tokio::test]
async fn subscribe_waits_for_replacement_of_a_slow_provider() {
    let relay = InProcessRelay::start(auth_config()).await;

    let key = "slow-backend";
    let (stale_ready_tx, stale_ready_rx) = oneshot::channel();
    let (retired_tx, retired_rx) = oneshot::channel();
    let stale_task = tokio::spawn(stale_control(
        relay.addr,
        key.to_string(),
        stale_ready_tx,
        StaleEnd::AckLateThenIgnoreUntilRetired(retired_tx),
    ));
    stale_ready_rx.await.unwrap();

    // The first subscriber's acknowledgement comes late: the relay passes
    // over the provider for it, and it fails with nothing else to try. The
    // provider is readmitted, and acknowledgements from it are now awaited
    // for 825 ms.
    let mut first = subscribe(relay.addr, key).await;
    let mut reader = get_header_msg_reader(&mut first).unwrap();
    let refused = timeout(Duration::from_secs(5), reader.read_msg())
        .await
        .expect("the first subscriber was neither served nor refused");
    assert!(
        !refused.is_ok_and(|response| matches!(
            PbConnResponse::decode(response),
            Ok(PbConnResponse::Subcribe { .. })
        )),
        "a provider passed over for an acknowledgement served the stream anyway"
    );

    let (replacement_stream_tx, replacement_stream_rx) = oneshot::channel();
    let replacement_task = tokio::spawn({
        let addr = relay.addr;
        let key = key.to_string();
        async move {
            retired_rx.await.unwrap();
            serve_one_stream(
                addr,
                key,
                "replacement control",
                None,
                replacement_stream_tx,
            )
            .await;
        }
    });

    // The second is never acknowledged: 825 ms, then the two-second grace,
    // and the provider is retired, after which the replacement registers.
    let mut client = subscribe(relay.addr, key).await;
    let mut reader = get_header_msg_reader(&mut client).unwrap();
    let response = timeout(Duration::from_secs(6), reader.read_msg())
        .await
        .expect("subscribe did not wait for the slow provider's replacement")
        .unwrap();
    assert!(matches!(
        PbConnResponse::decode(response).unwrap(),
        PbConnResponse::Subcribe { .. }
    ));

    let stream = replacement_stream_rx.await.unwrap();
    drop(stream);
    stale_task.abort();
    replacement_task.abort();
    relay.stop().await;
}

/// A subscribe waiting for a replacement is served by any registration it
/// has not passed over that becomes usable inside its window, not only by a
/// new one: here an older registration the relay suspected for another
/// subscriber, which then answers that subscriber late.
#[tokio::test]
async fn a_waiting_subscribe_is_served_by_a_registration_that_recovers() {
    let relay = InProcessRelay::start(auth_config()).await;
    let key = "recovering-backend";

    // The older registration, asked for the first subscriber's stream, which
    // it acknowledges only later.
    let older = TcpStream::connect(relay.addr).await.unwrap();
    let (mut older_read, mut older_write) = older.into_split();
    let mut older_reader = get_header_msg_reader(&mut older_read).unwrap();
    {
        let mut writer = get_header_msg_writer(&mut older_write).unwrap();
        register_control_conn_parts(&mut older_reader, &mut writer, key).await;
    }
    let mut older_writer = get_header_msg_writer(&mut older_write).unwrap();
    let mut first = subscribe(relay.addr, key).await;
    let first_request = read_stream_request(&mut older_reader, "older").await;

    // A newer one, which answers nothing.
    let (newer_ready_tx, newer_ready_rx) = oneshot::channel();
    let newer = tokio::spawn(stale_control(
        relay.addr,
        key.to_string(),
        newer_ready_tx,
        StaleEnd::IgnoreStream,
    ));
    newer_ready_rx.await.unwrap();

    // The second subscriber is asked of the newer, passes it over at its
    // acknowledgement budget and, the older being suspected for the first,
    // waits for a replacement.
    let mut second = subscribe(relay.addr, key).await;
    timeout(READY, async {
        while read_service_health(relay.addr, key)
            .await
            .iter()
            .any(|&(_, healthy)| healthy)
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the relay did not suspect both registrations");

    // The older answers the first subscriber, late: it is back in rotation,
    // and serves the second.
    acknowledge(&mut older_writer, first_request).await;
    let second_request = read_stream_request(&mut older_reader, "older").await;
    acknowledge(&mut older_writer, second_request).await;
    let _stream = open_stream(relay.addr, key, second_request).await;
    let mut reader = get_header_msg_reader(&mut second).unwrap();
    let response = timeout(READY, reader.read_msg())
        .await
        .expect("the waiting subscriber was neither served nor refused")
        .expect("the relay closed the waiting subscriber's connection unanswered");
    assert!(matches!(
        PbConnResponse::decode(response).unwrap(),
        PbConnResponse::Subcribe { .. }
    ));

    first.shutdown().await.unwrap();
    newer.abort();
    relay.stop().await;
}

/// A real `connect` tunnel through its publisher's retire-and-replace: the
/// first provider leaves its stream request unanswered, the relay retires it,
/// and a real `register` started then carries the tunnel's connection. Within
/// the client's first attempt as a rule; past it, on the attempt after, as
/// the relay no longer serves one it has given up.
#[tokio::test]
async fn a_connect_tunnel_is_served_by_the_replacement_of_a_retired_provider() {
    let relay = InProcessRelay::start(auth_config()).await;
    let key = "replaced-backend";

    let (stale_ready_tx, stale_ready_rx) = oneshot::channel();
    let (retired_tx, retired_rx) = oneshot::channel();
    let stale_task = tokio::spawn(stale_control(
        relay.addr,
        key.to_string(),
        stale_ready_tx,
        StaleEnd::IgnoreStreamUntilRetired(retired_tx),
    ));
    stale_ready_rx.await.unwrap();

    let echo = tcp_echo(None).await;
    let replacement = tokio::spawn({
        let (backend, relay_addr) = (echo.addr(), relay.addr);
        async move {
            retired_rx.await.unwrap();
            run_server_side_cli_with_pinned_credential::<TcpStreamProvider, _>(
                backend,
                relay_addr,
                Arc::from(key),
                ServerTunnelOptions {
                    need_codec: false,
                    is_datagram: false,
                    keep_alive: false,
                    namespace: None,
                    force_namespace: false,
                },
                None,
                admin_credential(),
            )
            .await;
        }
    });

    let local = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let local_addr = local.local_addr().unwrap();
    let tunnel = tokio::spawn(with_bound_listener(
        local,
        run_client_side_cli_with_callback::<BoundListener, _>(
            local_addr,
            relay.addr,
            Arc::from(key),
            false,
            None,
        ),
    ));

    let mut app = TcpStream::connect(local_addr).await.unwrap();
    app.write_all(PROBE).await.unwrap();
    let mut echoed = vec![0; PROBE.len()];
    // A hang bound: the client gives a stream's setup five seconds, and the
    // relay retires the first provider about 2.3 s into it.
    timeout(Duration::from_secs(10), app.read_exact(&mut echoed))
        .await
        .expect("the tunnel's connection was not served")
        .expect("the tunnel closed its connection instead of serving it");
    assert_eq!(echoed, PROBE);

    tunnel.abort();
    replacement.abort();
    stale_task.abort();
    relay.stop().await;
}

/// A subscriber that gives up, as a client does once its attempt runs out,
/// closes its connection while the relay still waits for a stream on its
/// behalf. The relay stops waiting then: a stream the provider opens for it
/// afterwards is dropped, not set up for a subscriber that has gone.
#[tokio::test]
async fn a_stream_is_not_set_up_for_a_subscriber_that_gave_up() {
    let relay = InProcessRelay::start(auth_config()).await;
    let key = "late-backend";

    // A provider that acknowledges at once and opens its stream only later.
    let control = TcpStream::connect(relay.addr).await.unwrap();
    let (mut reader_stream, mut writer_stream) = control.into_split();
    let mut reader = get_header_msg_reader(&mut reader_stream).unwrap();
    {
        let mut writer = get_header_msg_writer(&mut writer_stream).unwrap();
        register_control_conn_parts(&mut reader, &mut writer, key).await;
    }
    let mut writer = get_header_msg_writer(&mut writer_stream).unwrap();

    let subscriber = subscribe(relay.addr, key).await;
    let (client_id, server_generation) = read_stream_request(&mut reader, "provider").await;
    let ack = PbServerRequest::StreamAck {
        client_id,
        server_generation,
    }
    .encode()
    .unwrap();
    writer.write_msg(&ack).await.unwrap();

    drop(subscriber);
    let mut data = TcpStream::connect(relay.addr).await.unwrap();
    let request = PbConnRequest::Stream {
        data_protocol: None,
        key: key.to_string(),
        dst_id: client_id,
        server_generation,
    }
    .encode()
    .unwrap();
    get_header_msg_writer(&mut data)
        .unwrap()
        .write_msg(&request)
        .await
        .unwrap();
    let mut data_reader = get_header_msg_reader(&mut data).unwrap();
    let answer = timeout(READY, data_reader.read_msg())
        .await
        .expect("the relay neither set the stream up nor dropped it");
    assert!(
        !answer.is_ok_and(|bytes| !bytes.is_empty()),
        "the relay set a stream up for a subscriber that had gone"
    );

    relay.stop().await;
}

/// A subscriber that leaves while the relay waits for its provider to
/// acknowledge the stream request does not take the relay's verdict on that
/// acknowledgement with it: one that comes after the subscriber left, inside
/// the provider's budget, keeps the registration in rotation (its stream,
/// which no one takes, is dropped), and one that never comes has it suspected
/// at the budget, so that later subscribes pass it over, as if the subscriber
/// had stayed.
#[tokio::test]
async fn a_subscriber_that_leaves_does_not_spare_a_silent_provider() {
    let relay = InProcessRelay::start(auth_config()).await;
    let key = "silent-backend";

    let control = TcpStream::connect(relay.addr).await.unwrap();
    let (mut reader_stream, mut writer_stream) = control.into_split();
    let mut reader = get_header_msg_reader(&mut reader_stream).unwrap();
    {
        let mut writer = get_header_msg_writer(&mut writer_stream).unwrap();
        register_control_conn_parts(&mut reader, &mut writer, key).await;
    }
    let mut writer = get_header_msg_writer(&mut writer_stream).unwrap();

    // Each subscriber leaves as soon as the provider is asked, well inside
    // the relay's first acknowledgement budget of 300 ms.
    let mut subscriber = subscribe(relay.addr, key).await;
    let request = read_stream_request(&mut reader, "provider").await;
    subscriber.shutdown().await.unwrap();
    acknowledge(&mut writer, request).await;
    let _stream = open_stream(relay.addr, key, request).await;
    closed_by_relay(&mut subscriber).await;
    assert!(
        read_service_health(relay.addr, key)
            .await
            .iter()
            .all(|&(_, healthy)| healthy),
        "a provider that acknowledged in time was suspected because its subscriber had left"
    );

    let mut subscriber = subscribe(relay.addr, key).await;
    read_stream_request(&mut reader, "provider").await;
    subscriber.shutdown().await.unwrap();
    closed_by_relay(&mut subscriber).await;
    // Suspected, or, two seconds on, retired for its silence.
    assert!(
        !read_service_health(relay.addr, key)
            .await
            .iter()
            .any(|&(_, healthy)| healthy),
        "a provider that never acknowledged was not suspected because its subscriber had left"
    );

    relay.stop().await;
}

/// A subscriber that leaves after its provider acknowledged still has that
/// provider judged on the stream it owes: one that opens none is suspected
/// once the stream is due, as if the subscriber had stayed. No other
/// registration is asked for a stream on behalf of a subscriber that has gone.
#[tokio::test]
async fn a_subscriber_that_leaves_does_not_spare_a_provider_that_opens_no_stream() {
    let relay = InProcessRelay::start(auth_config()).await;
    let key = "streamless-backend";

    // An older registration, which the relay would ask next.
    let sibling = TcpStream::connect(relay.addr).await.unwrap();
    let (mut sibling_read, mut sibling_write) = sibling.into_split();
    let mut sibling_reader = get_header_msg_reader(&mut sibling_read).unwrap();
    {
        let mut writer = get_header_msg_writer(&mut sibling_write).unwrap();
        register_control_conn_parts(&mut sibling_reader, &mut writer, key).await;
    }
    // The newest, asked first: it acknowledges, and opens nothing.
    let control = TcpStream::connect(relay.addr).await.unwrap();
    let (mut reader_stream, mut writer_stream) = control.into_split();
    let mut reader = get_header_msg_reader(&mut reader_stream).unwrap();
    let provider = {
        let mut writer = get_header_msg_writer(&mut writer_stream).unwrap();
        register_control_conn_parts(&mut reader, &mut writer, key).await
    };
    let mut writer = get_header_msg_writer(&mut writer_stream).unwrap();

    let mut subscriber = subscribe(relay.addr, key).await;
    let request = read_stream_request(&mut reader, "provider").await;
    acknowledge(&mut writer, request).await;
    subscriber.shutdown().await.unwrap();
    closed_by_relay(&mut subscriber).await;
    // Suspected, or, two seconds on, retired for its silence.
    assert!(
        !read_service_health(relay.addr, key)
            .await
            .contains(&(provider, true)),
        "a provider that opened no stream was not suspected because its subscriber had left"
    );
    assert!(
        timeout(Duration::from_millis(500), sibling_reader.read_msg())
            .await
            .is_err(),
        "another registration was asked for a stream after its subscriber had left"
    );

    relay.stop().await;
}

/// Late for the relay's first acknowledgement budget of 300 ms, which it then
/// doubles to 600 ms; what the relay learns from it, a quarter of three
/// times this on top of three quarters of that, is about 1.6 s.
const SLOW_ACK: Duration = Duration::from_millis(1_500);

/// A subscriber that leaves before a slow provider's acknowledgement does not
/// take what that acknowledgement teaches the relay with it: one that comes
/// past its budget still lengthens the relay's wait for the next, as if the
/// subscriber had stayed, so the next subscribe that provider answers as
/// slowly is served.
#[tokio::test]
async fn a_subscriber_that_leaves_does_not_take_a_late_acknowledgement_with_it() {
    let relay = InProcessRelay::start(auth_config()).await;
    let key = "late-ack-backend";

    let control = TcpStream::connect(relay.addr).await.unwrap();
    let (mut reader_stream, mut writer_stream) = control.into_split();
    let mut reader = get_header_msg_reader(&mut reader_stream).unwrap();
    {
        let mut writer = get_header_msg_writer(&mut writer_stream).unwrap();
        register_control_conn_parts(&mut reader, &mut writer, key).await;
    }
    let mut writer = get_header_msg_writer(&mut writer_stream).unwrap();

    let mut first = subscribe(relay.addr, key).await;
    let request = read_stream_request(&mut reader, "provider").await;
    first.shutdown().await.unwrap();
    tokio::time::sleep(SLOW_ACK).await;
    acknowledge(&mut writer, request).await;
    closed_by_relay(&mut first).await;

    // A second past the stream request: inside what the relay learned, and
    // 400 ms past the 600 ms it would wait without it.
    let mut second = subscribe(relay.addr, key).await;
    let request = read_stream_request(&mut reader, "provider").await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    acknowledge(&mut writer, request).await;
    let _stream = open_stream(relay.addr, key, request).await;
    let mut second_reader = get_header_msg_reader(&mut second).unwrap();
    let response = timeout(READY, second_reader.read_msg())
        .await
        .expect("the second subscriber was neither served nor refused")
        .expect("the relay closed the second subscriber's connection unanswered");
    assert!(
        matches!(
            PbConnResponse::decode(response).unwrap(),
            PbConnResponse::Subcribe { .. }
        ),
        "the relay did not learn from an acknowledgement that came after its subscriber had left"
    );

    relay.stop().await;
}

/// A subscriber the relay has not served when its five-second setup bound
/// runs out gets the retryable refusal it would get from a relay with no
/// registration to serve it, not a closed connection, which its client counts
/// as a failed transport. Here the only provider is one the relay has learned
/// to wait about two seconds for, and it answers nothing: the wait for its
/// replacement would run past the bound.
#[tokio::test]
async fn a_subscribe_that_runs_out_its_setup_bound_is_refused() {
    let relay = InProcessRelay::start(auth_config()).await;
    let key = "slowest-backend";

    let control = TcpStream::connect(relay.addr).await.unwrap();
    let (mut reader_stream, mut writer_stream) = control.into_split();
    let mut reader = get_header_msg_reader(&mut reader_stream).unwrap();
    {
        let mut writer = get_header_msg_writer(&mut writer_stream).unwrap();
        register_control_conn_parts(&mut reader, &mut writer, key).await;
    }
    let mut writer = get_header_msg_writer(&mut writer_stream).unwrap();

    // Acknowledged two seconds late, for a subscriber that has left: the
    // relay learns to wait 1.95 s, and then for the suspected provider's
    // replacement until twice that past it.
    let mut first = subscribe(relay.addr, key).await;
    let request = read_stream_request(&mut reader, "provider").await;
    first.shutdown().await.unwrap();
    tokio::time::sleep(Duration::from_secs(2)).await;
    acknowledge(&mut writer, request).await;
    closed_by_relay(&mut first).await;

    let mut second = subscribe(relay.addr, key).await;
    let asked = tokio::time::Instant::now();
    read_stream_request(&mut reader, "provider").await;
    let mut second_reader = get_header_msg_reader(&mut second).unwrap();
    let response = timeout(Duration::from_secs(8), second_reader.read_msg())
        .await
        .expect("the subscriber was neither served nor refused")
        .expect("the relay closed the subscriber's connection unanswered");
    assert_unavailable(response);
    // Not the refusal that ends a shorter wait for a replacement, at about
    // 2.8 s, which a relay that had not learned the budget would send.
    assert!(
        asked.elapsed() >= Duration::from_secs(4),
        "refused after {:?}, before the setup bound",
        asked.elapsed()
    );

    relay.stop().await;
}

/// Wait for the relay to close `subscriber`'s connection, which it does once
/// it is done with the subscribe, verdicts included.
async fn closed_by_relay(subscriber: &mut TcpStream) {
    let mut byte = [0_u8; 1];
    let read = timeout(READY, subscriber.read(&mut byte))
        .await
        .expect("the relay kept the connection of a subscriber that had left");
    assert!(
        !read.is_ok_and(|read| read > 0),
        "the relay answered a subscriber that had left"
    );
}

#[tokio::test]
async fn subscribe_missing_key_closes_without_hanging() {
    let relay = InProcessRelay::start(auth_config()).await;

    let mut stream = subscribe(relay.addr, "missing-key").await;
    let mut reader = get_header_msg_reader(&mut stream).unwrap();
    // Well inside the 2.5 s a subscribe would wait for a provider to appear.
    let result = timeout(Duration::from_secs(1), reader.read_msg())
        .await
        .expect("missing-key subscribe hung instead of closing");
    let response = PbConnResponse::decode(result.unwrap()).unwrap();
    let PbConnResponse::Error(error) = response else {
        panic!("expected structured missing-service error");
    };
    assert_eq!(error.code, "service_not_available");

    relay.stop().await;
}

#[tokio::test]
async fn subscribe_bypasses_acked_control_connection_without_stream() {
    let relay = InProcessRelay::start(auth_config()).await;

    let key = "sf-backend";
    let (healthy_ready_tx, healthy_ready_rx) = oneshot::channel();
    let (stream_tx, stream_rx) = oneshot::channel();
    let healthy_task = tokio::spawn(serve_one_stream(
        relay.addr,
        key.to_string(),
        "healthy control",
        Some(healthy_ready_tx),
        stream_tx,
    ));
    healthy_ready_rx.await.unwrap();

    let (stale_ready_tx, stale_ready_rx) = oneshot::channel();
    let stale_task = tokio::spawn(stale_control(
        relay.addr,
        key.to_string(),
        stale_ready_tx,
        StaleEnd::AckStream,
    ));
    stale_ready_rx.await.unwrap();

    let mut client = subscribe(relay.addr, key).await;
    let mut reader = get_header_msg_reader(&mut client).unwrap();
    let response = timeout(Duration::from_millis(2_000), reader.read_msg())
        .await
        .expect("subscribe did not bypass acked control without stream quickly")
        .unwrap();
    assert!(matches!(
        PbConnResponse::decode(response).unwrap(),
        PbConnResponse::Subcribe { .. }
    ));

    let stream = stream_rx.await.unwrap();
    drop(stream);
    healthy_task.abort();
    stale_task.abort();
    relay.stop().await;
}

/// `run_server_with_auth_config`, the published entry point that binds for
/// its caller and that `InProcessRelay` inlines: it loads the process
/// credential, binds, serves, and returns `Ok` once cancelled.
#[tokio::test]
async fn run_server_with_auth_config_serves_until_cancelled() {
    let config = auth_config();
    let _state = StateDir(config.state_dir.clone());
    // It binds the address itself and says where only in its log, so it is
    // handed a reserved port; this binary starts no process that could hold
    // the reservation's socket.
    let addr = reserve_addr(Transport::Tcp).await;
    let shutdown = CancellationToken::new();
    let server = tokio::spawn(run_server_with_auth_config(
        addr,
        shutdown.clone(),
        None,
        false,
        config,
    ));

    // Connect once it listens, some time after it is spawned.
    timeout(Duration::from_secs(5), async {
        while TcpStream::connect(addr).await.is_err() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the relay did not start listening");
    assert!(read_status_keys(addr).await.is_empty());

    shutdown.cancel();
    timeout(Duration::from_secs(5), server)
        .await
        .expect("the relay did not stop once cancelled")
        .unwrap()
        .unwrap();
}
