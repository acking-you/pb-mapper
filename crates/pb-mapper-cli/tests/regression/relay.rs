//! The relay's own behavior, driven with hand-written frames.

use std::net::SocketAddr;
use std::time::Duration;

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
    Transport, V2ControlSpec, admin_credential, register_v2_control, reserve_addr,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::oneshot;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

use crate::credential::{StateDir, auth_config};
use crate::fixtures::{
    InProcessRelay, issue_before_start, read_stream_request, register_control_conn_parts,
    serve_one_stream, subscribe, wait_for_server,
};

/// How a control connection that never opens a data stream ends.
enum StaleEnd {
    /// It receives a stream request and leaves it unacknowledged.
    IgnoreStream,
    /// As `IgnoreStream`, then waits for the relay to retire it and reports that.
    IgnoreStreamUntilRetired(oneshot::Sender<()>),
    /// It acknowledges its first stream request and opens nothing.
    AckStream,
}

/// A stale provider: register `key` as a legacy control connection, report it
/// `ready`, and end as `end` says.
async fn stale_control(addr: SocketAddr, key: String, ready: oneshot::Sender<()>, end: StaleEnd) {
    let control = wait_for_server(addr).await;
    let (mut reader_stream, mut writer_stream) = control.into_split();
    let mut reader = get_header_msg_reader(&mut reader_stream).unwrap();
    {
        let mut writer = get_header_msg_writer(&mut writer_stream).unwrap();
        register_control_conn_parts(&mut reader, &mut writer, &key).await;
    }
    ready.send(()).unwrap();
    match end {
        StaleEnd::IgnoreStream => {
            read_stream_request(&mut reader, "stale control").await;
        }
        StaleEnd::IgnoreStreamUntilRetired(retired) => {
            read_stream_request(&mut reader, "stale control").await;
            // A missed stream ACK first receives a two-second confirmation grace.
            let closed = timeout(Duration::from_secs(3), reader.read_msg())
                .await
                .expect("stale control was not retired by the server");
            if let Ok(bytes) = closed {
                assert!(bytes.is_empty());
            }
            retired.send(()).unwrap();
            return;
        }
        StaleEnd::AckStream => {
            let (client_id, server_generation) =
                read_stream_request(&mut reader, "stale control").await;
            let mut writer = get_header_msg_writer(&mut writer_stream).unwrap();
            let ack = PbServerRequest::StreamAck {
                client_id,
                server_generation,
            }
            .encode()
            .unwrap();
            writer.write_msg(&ack).await.unwrap();
        }
    }
    std::future::pending::<()>().await;
}

async fn read_status_keys(server_addr: SocketAddr) -> Vec<String> {
    let mut stream = TcpStream::connect(server_addr).await.unwrap();
    let request = PbConnRequest::Status(PbConnStatusReq::Keys)
        .encode()
        .unwrap();
    {
        let mut writer = get_header_msg_writer(&mut stream).unwrap();
        writer.write_msg(&request).await.unwrap();
    }

    let mut reader = get_header_msg_reader(&mut stream).unwrap();
    let response = timeout(Duration::from_secs(1), reader.read_msg())
        .await
        .expect("status keys response timed out")
        .unwrap();
    let PbConnResponse::Status(PbConnStatusResp::Keys(keys)) =
        PbConnResponse::decode(response).unwrap()
    else {
        panic!("unexpected status keys response");
    };
    keys
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
        let message = timeout(Duration::from_secs(1), reader.read_msg())
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
    let closed = timeout(Duration::from_secs(1), first_control.read(&mut byte))
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
    let register_response = timeout(Duration::from_secs(1), control_reader.read_msg())
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

    let stream_request = timeout(Duration::from_secs(1), control_reader.read_msg())
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
        let response = timeout(Duration::from_secs(1), subscriber_reader.read_msg())
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
        let response = timeout(Duration::from_secs(1), provider_reader.read_msg())
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
    timeout(Duration::from_secs(1), provider.read_exact(&mut ready))
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
    let read = timeout(Duration::from_secs(1), subscriber.read(&mut byte))
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
            .response_timeout(Duration::from_secs(1)),
    )
    .await;

    let mut status = TcpStream::connect(relay.addr).await.unwrap();
    let request = PbConnRequest::Status(PbConnStatusReq::Service {
        direct_query: None,
        key: key.to_string(),
    })
    .encode()
    .unwrap();
    {
        let mut writer = get_header_msg_writer(&mut status).unwrap();
        writer.write_msg(&request).await.unwrap();
    }

    let mut reader = get_header_msg_reader(&mut status).unwrap();
    let response = timeout(Duration::from_secs(1), reader.read_msg())
        .await
        .expect("service status response timed out")
        .unwrap();
    let PbConnResponse::Status(PbConnStatusResp::Service {
        direct: _,
        key: status_key,
        connections,
    }) = PbConnResponse::decode(response).unwrap()
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
    let result = timeout(Duration::from_secs(4), reader.read_msg())
        .await
        .expect("subscribe did not finish after stale control timed out");
    assert!(result.is_err());

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

#[tokio::test]
async fn subscribe_missing_key_closes_without_hanging() {
    let relay = InProcessRelay::start(auth_config()).await;

    let mut stream = subscribe(relay.addr, "missing-key").await;
    let mut reader = get_header_msg_reader(&mut stream).unwrap();
    let result = timeout(Duration::from_millis(200), reader.read_msg())
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
    // It binds the address itself, so it cannot be handed a bound socket.
    let addr = reserve_addr(Transport::Tcp).await;
    let shutdown = CancellationToken::new();
    let server = tokio::spawn(run_server_with_auth_config(
        addr,
        shutdown.clone(),
        None,
        false,
        config,
    ));

    wait_for_server(addr).await;
    assert!(read_status_keys(addr).await.is_empty());

    shutdown.cancel();
    timeout(Duration::from_secs(5), server)
        .await
        .expect("the relay did not stop once cancelled")
        .unwrap()
        .unwrap();
}
