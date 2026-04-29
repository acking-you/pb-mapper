use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use pb_mapper::common::message::command::{
    LocalServer, MessageSerializer, PbConnRequest, PbConnResponse, PbConnStatusReq,
    PbConnStatusResp, PbServerRequest, PbServiceConnStatus,
};
use pb_mapper::common::message::{
    get_header_msg_reader, get_header_msg_writer, MessageReader, MessageWriter,
};
use pb_mapper::local::client::run_client_side_cli_with_callback;
use pb_mapper::local::server::run_server_side_cli_with_callback;
use pb_mapper::pb_server::{get_init_request, run_server_with_shutdown};
use tokio::io::AsyncReadExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use uni_stream::stream::{TcpListenerProvider, TcpStreamProvider};

struct EnvVarGuard {
    key: &'static str,
    old_value: Option<String>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: &'static str) -> Self {
        let old_value = std::env::var(key).ok();
        std::env::set_var(key, value);
        Self { key, old_value }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        if let Some(value) = self.old_value.take() {
            std::env::set_var(self.key, value);
        } else {
            std::env::remove_var(self.key);
        }
    }
}

async fn wait_for_server(server_addr: SocketAddr) -> TcpStream {
    timeout(Duration::from_secs(2), async {
        loop {
            match TcpStream::connect(server_addr).await {
                Ok(stream) => break stream,
                Err(_) => tokio::time::sleep(Duration::from_millis(10)).await,
            }
        }
    })
    .await
    .expect("server did not start")
}

async fn register_control_conn_parts(
    reader: &mut impl MessageReader,
    writer: &mut impl MessageWriter,
    key: &str,
) -> u32 {
    let request = PbConnRequest::Register {
        need_codec: false,
        is_datagram: false,
        key: key.to_string(),
        protocol_version: None,
        client_instance_id: None,
        heartbeat_interval_ms: None,
        heartbeat_tolerance_ms: None,
    }
    .encode()
    .unwrap();
    writer.write_msg(&request).await.unwrap();

    let response = timeout(Duration::from_secs(1), reader.read_msg())
        .await
        .expect("register response timed out")
        .unwrap();
    let PbConnResponse::Register(conn_id) = PbConnResponse::decode(response).unwrap() else {
        panic!("unexpected register response");
    };
    conn_id
}

async fn register_v2_control_conn_parts(
    reader: &mut impl MessageReader,
    writer: &mut impl MessageWriter,
    key: &str,
) -> (u32, u64) {
    let request = PbConnRequest::Register {
        need_codec: false,
        is_datagram: false,
        key: key.to_string(),
        protocol_version: Some(2),
        client_instance_id: Some("regression-test-client".to_string()),
        heartbeat_interval_ms: Some(50),
        heartbeat_tolerance_ms: Some(150),
    }
    .encode()
    .unwrap();
    writer.write_msg(&request).await.unwrap();

    let response = timeout(Duration::from_secs(1), reader.read_msg())
        .await
        .expect("register v2 response timed out")
        .unwrap();
    let PbConnResponse::RegisterV2 {
        conn_id,
        generation,
        ..
    } = PbConnResponse::decode(response).unwrap()
    else {
        panic!("unexpected register v2 response");
    };
    (conn_id, generation)
}

async fn read_status_keys(server_addr: SocketAddr) -> Vec<String> {
    let mut stream = wait_for_server(server_addr).await;
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

#[tokio::test]
async fn status_service_reports_registered_v2_control_connection() {
    let probe_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let server_addr = probe_listener.local_addr().unwrap();
    drop(probe_listener);

    let shutdown_token = CancellationToken::new();
    let server_shutdown = shutdown_token.clone();
    let server = tokio::spawn(async move {
        run_server_with_shutdown(server_addr, server_shutdown, None)
            .await
            .unwrap();
    });

    let key = "sf-backend";
    let control = wait_for_server(server_addr).await;
    let (mut reader_stream, mut writer_stream) = control.into_split();
    let mut reader = get_header_msg_reader(&mut reader_stream).unwrap();
    let mut writer = get_header_msg_writer(&mut writer_stream).unwrap();
    let (conn_id, generation) = register_v2_control_conn_parts(&mut reader, &mut writer, key).await;

    let mut status = wait_for_server(server_addr).await;
    let request = PbConnRequest::Status(PbConnStatusReq::Service {
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

    shutdown_token.cancel();
    server.await.unwrap();
}

#[tokio::test]
async fn local_server_reconnects_when_registered_conn_is_missing_from_remote_status() {
    let _pool_size = EnvVarGuard::set("PB_MAPPER_CONTROL_CONN_POOL_SIZE", "1");
    let _heartbeat = EnvVarGuard::set("PB_MAPPER_CONTROL_HEARTBEAT_INTERVAL", "20ms");
    let _tolerance = EnvVarGuard::set("PB_MAPPER_CONTROL_HEARTBEAT_TOLERANCE", "50ms");
    let _grace = EnvVarGuard::set("PB_MAPPER_CONTROL_SUSPECT_GRACE", "20ms");
    let _probe_timeout = EnvVarGuard::set("PB_MAPPER_REGISTRATION_PROBE_TIMEOUT", "50ms");

    let remote_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let remote_addr = remote_listener.local_addr().unwrap();
    let register_count = Arc::new(AtomicUsize::new(0));
    let (second_register_tx, second_register_rx) = tokio::sync::oneshot::channel();
    let second_register_tx = Arc::new(tokio::sync::Mutex::new(Some(second_register_tx)));

    let fake_register_count = register_count.clone();
    let fake_second_register_tx = second_register_tx.clone();
    let fake_server = tokio::spawn(async move {
        loop {
            let (mut stream, _) = remote_listener.accept().await.unwrap();
            let register_count = fake_register_count.clone();
            let second_register_tx = fake_second_register_tx.clone();
            tokio::spawn(async move {
                let Ok(request) = get_init_request(&mut stream, 0.into()).await else {
                    return;
                };
                match request {
                    PbConnRequest::Register { key, .. } => {
                        let count = register_count.fetch_add(1, Ordering::SeqCst) + 1;
                        let response = PbConnResponse::RegisterV2 {
                            conn_id: count as u32,
                            generation: count as u64,
                            lease_ttl_ms: 150,
                        }
                        .encode()
                        .unwrap();
                        let mut writer = get_header_msg_writer(&mut stream).unwrap();
                        writer.write_msg(&response).await.unwrap();
                        if count == 2 {
                            if let Some(tx) = second_register_tx.lock().await.take() {
                                tx.send(()).unwrap();
                            }
                        }
                        tracing::debug!(key, count, "fake server accepted register");
                        std::future::pending::<()>().await;
                    }
                    PbConnRequest::Status(PbConnStatusReq::Service { key }) => {
                        let response = PbConnResponse::Status(PbConnStatusResp::Service {
                            key,
                            connections: Vec::new(),
                        })
                        .encode()
                        .unwrap();
                        let mut writer = get_header_msg_writer(&mut stream).unwrap();
                        writer.write_msg(&response).await.unwrap();
                    }
                    PbConnRequest::Status(PbConnStatusReq::Keys) => {
                        let response = PbConnResponse::Status(PbConnStatusResp::Keys(Vec::new()))
                            .encode()
                            .unwrap();
                        let mut writer = get_header_msg_writer(&mut stream).unwrap();
                        writer.write_msg(&response).await.unwrap();
                    }
                    _ => {}
                }
            });
        }
    });

    let local_addr: SocketAddr = "127.0.0.1:9".parse().unwrap();
    let local_server = tokio::spawn(run_server_side_cli_with_callback::<TcpStreamProvider, _>(
        local_addr,
        remote_addr,
        Arc::from("sf-backend"),
        false,
        false,
        None,
    ));

    timeout(Duration::from_secs(2), second_register_rx)
        .await
        .expect("local server did not reconnect after remote status lost its registration")
        .unwrap();

    local_server.abort();
    fake_server.abort();
}

#[tokio::test]
async fn client_closes_initial_status_probe_after_key_check() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let remote_addr = listener.local_addr().unwrap();

    let fake_server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let request = get_init_request(&mut stream, 0.into()).await.unwrap();
        let PbConnRequest::Status(PbConnStatusReq::Service { key }) = request else {
            panic!("client did not use service status for initial key check");
        };

        let response = PbConnResponse::Status(PbConnStatusResp::Service {
            key,
            connections: vec![PbServiceConnStatus {
                conn_id: 1,
                generation: 1,
                protocol_version: 2,
                healthy: true,
                last_rx_age_ms: 0,
            }],
        })
        .encode()
        .unwrap();
        {
            let mut writer = get_header_msg_writer(&mut stream).unwrap();
            writer.write_msg(&response).await.unwrap();
        }

        let mut buf = [0u8; 1];
        timeout(Duration::from_secs(1), stream.read(&mut buf))
            .await
            .expect("client kept the one-shot status probe connection open")
            .unwrap()
    });

    let local_addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let client = tokio::spawn(run_client_side_cli_with_callback::<TcpListenerProvider, _>(
        local_addr,
        remote_addr,
        Arc::from("sf-backend"),
        None,
    ));

    assert_eq!(fake_server.await.unwrap(), 0);
    client.abort();
}

#[tokio::test]
async fn client_rechecks_remote_key_while_listener_is_active() {
    let _health_interval = EnvVarGuard::set("PB_MAPPER_CLIENT_HEALTH_CHECK_INTERVAL", "20ms");
    let _health_timeout = EnvVarGuard::set("PB_MAPPER_CLIENT_HEALTH_CHECK_TIMEOUT", "200ms");

    let local_probe = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let local_addr = local_probe.local_addr().unwrap();
    drop(local_probe);

    let remote_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let remote_addr = remote_listener.local_addr().unwrap();
    let key_available = Arc::new(AtomicBool::new(true));
    let status_count = Arc::new(AtomicUsize::new(0));

    let fake_key_available = key_available.clone();
    let fake_status_count = status_count.clone();
    let fake_server = tokio::spawn(async move {
        loop {
            let (mut stream, _) = remote_listener.accept().await.unwrap();
            let key_available = fake_key_available.clone();
            let status_count = fake_status_count.clone();
            tokio::spawn(async move {
                let Ok(request) = get_init_request(&mut stream, 0.into()).await else {
                    return;
                };
                match request {
                    PbConnRequest::Status(PbConnStatusReq::Service { key }) => {
                        status_count.fetch_add(1, Ordering::SeqCst);
                        let connections = if key_available.load(Ordering::SeqCst) {
                            vec![PbServiceConnStatus {
                                conn_id: 1,
                                generation: 1,
                                protocol_version: 2,
                                healthy: true,
                                last_rx_age_ms: 0,
                            }]
                        } else {
                            Vec::new()
                        };
                        let response =
                            PbConnResponse::Status(PbConnStatusResp::Service { key, connections })
                                .encode()
                                .unwrap();
                        let mut writer = get_header_msg_writer(&mut stream).unwrap();
                        writer.write_msg(&response).await.unwrap();
                    }
                    PbConnRequest::Status(PbConnStatusReq::Keys) => {
                        status_count.fetch_add(1, Ordering::SeqCst);
                        let keys = if key_available.load(Ordering::SeqCst) {
                            vec!["sf-backend".to_string()]
                        } else {
                            Vec::new()
                        };
                        let response = PbConnResponse::Status(PbConnStatusResp::Keys(keys))
                            .encode()
                            .unwrap();
                        let mut writer = get_header_msg_writer(&mut stream).unwrap();
                        writer.write_msg(&response).await.unwrap();
                    }
                    PbConnRequest::Subcribe { .. } => {}
                    _ => {}
                }
            });
        }
    });

    let client = tokio::spawn(run_client_side_cli_with_callback::<TcpListenerProvider, _>(
        local_addr,
        remote_addr,
        Arc::from("sf-backend"),
        None,
    ));

    timeout(Duration::from_secs(1), async {
        loop {
            if status_count.load(Ordering::SeqCst) >= 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("client did not run initial key probe");

    timeout(Duration::from_secs(1), async {
        loop {
            if TcpStream::connect(local_addr).await.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("client listener did not bind after key probe");

    let baseline = status_count.load(Ordering::SeqCst);
    key_available.store(false, Ordering::SeqCst);

    timeout(Duration::from_secs(1), async {
        loop {
            if status_count.load(Ordering::SeqCst) > baseline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("client did not recheck remote key while listener was active");

    client.abort();
    fake_server.abort();
}

#[tokio::test]
async fn subscribe_retires_unacked_control_connection() {
    let probe_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let server_addr = probe_listener.local_addr().unwrap();
    drop(probe_listener);

    let shutdown_token = CancellationToken::new();
    let server_shutdown = shutdown_token.clone();
    let server = tokio::spawn(async move {
        run_server_with_shutdown(server_addr, server_shutdown, None)
            .await
            .unwrap();
    });

    let key = "sf-backend";
    let (stale_ready_tx, stale_ready_rx) = tokio::sync::oneshot::channel();
    let stale_key = key.to_string();
    let stale_task = tokio::spawn(async move {
        let stale_control = wait_for_server(server_addr).await;
        let (mut reader_stream, mut writer_stream) = stale_control.into_split();
        let mut reader = get_header_msg_reader(&mut reader_stream).unwrap();
        let mut writer = get_header_msg_writer(&mut writer_stream).unwrap();
        register_control_conn_parts(&mut reader, &mut writer, &stale_key).await;
        stale_ready_tx.send(()).unwrap();
        let request = timeout(Duration::from_secs(2), reader.read_msg())
            .await
            .expect("stale control did not receive stream request")
            .unwrap();
        assert!(matches!(
            LocalServer::decode(request).unwrap(),
            LocalServer::Stream { .. }
        ));
        std::future::pending::<()>().await;
    });
    stale_ready_rx.await.unwrap();

    let mut client = wait_for_server(server_addr).await;
    let request = PbConnRequest::Subcribe {
        key: key.to_string(),
    }
    .encode()
    .unwrap();
    {
        let mut writer = get_header_msg_writer(&mut client).unwrap();
        writer.write_msg(&request).await.unwrap();
    }

    let mut reader = get_header_msg_reader(&mut client).unwrap();
    let result = timeout(Duration::from_secs(4), reader.read_msg())
        .await
        .expect("subscribe did not finish after stale control timed out");
    assert!(result.is_err());

    let keys = read_status_keys(server_addr).await;
    assert!(
        !keys.iter().any(|candidate| candidate == key),
        "unacked stale control connection kept key registered: {keys:?}"
    );

    stale_task.abort();
    shutdown_token.cancel();
    server.await.unwrap();
}

#[tokio::test]
async fn subscribe_waits_for_replacement_after_retiring_stale_control_connection() {
    let probe_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let server_addr = probe_listener.local_addr().unwrap();
    drop(probe_listener);

    let shutdown_token = CancellationToken::new();
    let server_shutdown = shutdown_token.clone();
    let server = tokio::spawn(async move {
        run_server_with_shutdown(server_addr, server_shutdown, None)
            .await
            .unwrap();
    });

    let key = "sf-backend";
    let (stale_ready_tx, stale_ready_rx) = tokio::sync::oneshot::channel();
    let (retired_tx, retired_rx) = tokio::sync::oneshot::channel();
    let stale_key = key.to_string();
    let stale_task = tokio::spawn(async move {
        let stale_control = wait_for_server(server_addr).await;
        let (mut reader_stream, mut writer_stream) = stale_control.into_split();
        let mut reader = get_header_msg_reader(&mut reader_stream).unwrap();
        let mut writer = get_header_msg_writer(&mut writer_stream).unwrap();
        register_control_conn_parts(&mut reader, &mut writer, &stale_key).await;
        stale_ready_tx.send(()).unwrap();
        let request = timeout(Duration::from_secs(2), reader.read_msg())
            .await
            .expect("stale control did not receive stream request")
            .unwrap();
        assert!(matches!(
            LocalServer::decode(request).unwrap(),
            LocalServer::Stream { .. }
        ));

        let retired = timeout(Duration::from_secs(2), reader.read_msg())
            .await
            .expect("stale control was not retired by the server");
        if let Ok(bytes) = retired {
            assert!(bytes.is_empty());
        }
        retired_tx.send(()).unwrap();
    });
    stale_ready_rx.await.unwrap();

    let (replacement_stream_tx, replacement_stream_rx) = tokio::sync::oneshot::channel();
    let replacement_key = key.to_string();
    let replacement_task = tokio::spawn(async move {
        retired_rx.await.unwrap();
        let replacement_control = wait_for_server(server_addr).await;
        let (mut reader_stream, mut writer_stream) = replacement_control.into_split();
        let mut reader = get_header_msg_reader(&mut reader_stream).unwrap();
        {
            let mut writer = get_header_msg_writer(&mut writer_stream).unwrap();
            register_control_conn_parts(&mut reader, &mut writer, &replacement_key).await;
        }
        let mut writer = get_header_msg_writer(&mut writer_stream).unwrap();
        let request = timeout(Duration::from_secs(2), reader.read_msg())
            .await
            .expect("replacement control did not receive stream request")
            .unwrap();
        let LocalServer::Stream {
            client_id,
            server_generation,
        } = LocalServer::decode(request).unwrap()
        else {
            panic!("unexpected local server control message");
        };

        let ack = PbServerRequest::StreamAck {
            client_id,
            server_generation,
        }
        .encode()
        .unwrap();
        writer.write_msg(&ack).await.unwrap();

        let mut stream = TcpStream::connect(server_addr).await.unwrap();
        let request = PbConnRequest::Stream {
            key: replacement_key,
            dst_id: client_id,
            server_generation,
        }
        .encode()
        .unwrap();
        let mut stream_writer = get_header_msg_writer(&mut stream).unwrap();
        stream_writer.write_msg(&request).await.unwrap();
        replacement_stream_tx.send(stream).unwrap();
        std::future::pending::<()>().await;
    });

    let mut client = wait_for_server(server_addr).await;
    let request = PbConnRequest::Subcribe {
        key: key.to_string(),
    }
    .encode()
    .unwrap();
    {
        let mut writer = get_header_msg_writer(&mut client).unwrap();
        writer.write_msg(&request).await.unwrap();
    }

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
    shutdown_token.cancel();
    server.await.unwrap();
}

#[tokio::test]
async fn subscribe_missing_key_closes_without_hanging() {
    let probe_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let server_addr = probe_listener.local_addr().unwrap();
    drop(probe_listener);

    let shutdown_token = CancellationToken::new();
    let server_shutdown = shutdown_token.clone();
    let server = tokio::spawn(async move {
        run_server_with_shutdown(server_addr, server_shutdown, None)
            .await
            .unwrap();
    });

    let mut stream = timeout(Duration::from_secs(2), async {
        loop {
            match TcpStream::connect(server_addr).await {
                Ok(stream) => break stream,
                Err(_) => tokio::time::sleep(Duration::from_millis(10)).await,
            }
        }
    })
    .await
    .expect("server did not start");

    let request = PbConnRequest::Subcribe {
        key: "missing-key".to_string(),
    }
    .encode()
    .unwrap();
    {
        let mut writer = get_header_msg_writer(&mut stream).unwrap();
        writer.write_msg(&request).await.unwrap();
    }

    let mut reader = get_header_msg_reader(&mut stream).unwrap();
    let result = timeout(Duration::from_millis(200), reader.read_msg())
        .await
        .expect("missing-key subscribe hung instead of closing");
    assert!(result.is_err());

    shutdown_token.cancel();
    server.await.unwrap();
}

#[tokio::test]
async fn subscribe_bypasses_unacked_stale_control_connection() {
    let probe_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let server_addr = probe_listener.local_addr().unwrap();
    drop(probe_listener);

    let shutdown_token = CancellationToken::new();
    let server_shutdown = shutdown_token.clone();
    let server = tokio::spawn(async move {
        run_server_with_shutdown(server_addr, server_shutdown, None)
            .await
            .unwrap();
    });

    let key = "sf-backend";
    let (healthy_ready_tx, healthy_ready_rx) = tokio::sync::oneshot::channel();
    let (stream_tx, stream_rx) = tokio::sync::oneshot::channel();
    let healthy_key = key.to_string();
    let healthy_task = tokio::spawn(async move {
        let healthy_control = wait_for_server(server_addr).await;
        let (mut reader_stream, mut writer_stream) = healthy_control.into_split();
        let mut reader = get_header_msg_reader(&mut reader_stream).unwrap();
        {
            let mut writer = get_header_msg_writer(&mut writer_stream).unwrap();
            register_control_conn_parts(&mut reader, &mut writer, &healthy_key).await;
        }
        let mut writer = get_header_msg_writer(&mut writer_stream).unwrap();
        healthy_ready_tx.send(()).unwrap();
        let request = timeout(Duration::from_secs(2), reader.read_msg())
            .await
            .expect("healthy control did not receive stream request")
            .unwrap();
        let LocalServer::Stream {
            client_id,
            server_generation,
        } = LocalServer::decode(request).unwrap()
        else {
            panic!("unexpected local server control message");
        };

        let ack = PbServerRequest::StreamAck {
            client_id,
            server_generation,
        }
        .encode()
        .unwrap();
        writer.write_msg(&ack).await.unwrap();

        let mut stream = TcpStream::connect(server_addr).await.unwrap();
        let request = PbConnRequest::Stream {
            key: healthy_key,
            dst_id: client_id,
            server_generation,
        }
        .encode()
        .unwrap();
        let mut stream_writer = get_header_msg_writer(&mut stream).unwrap();
        stream_writer.write_msg(&request).await.unwrap();
        stream_tx.send(stream).unwrap();
        std::future::pending::<()>().await;
    });
    healthy_ready_rx.await.unwrap();

    let (stale_ready_tx, stale_ready_rx) = tokio::sync::oneshot::channel();
    let stale_key = key.to_string();
    let stale_task = tokio::spawn(async move {
        let stale_control = wait_for_server(server_addr).await;
        let (mut reader_stream, mut writer_stream) = stale_control.into_split();
        let mut reader = get_header_msg_reader(&mut reader_stream).unwrap();
        let mut writer = get_header_msg_writer(&mut writer_stream).unwrap();
        register_control_conn_parts(&mut reader, &mut writer, &stale_key).await;
        stale_ready_tx.send(()).unwrap();
        std::future::pending::<()>().await;
    });
    stale_ready_rx.await.unwrap();

    let mut client = wait_for_server(server_addr).await;
    let request = PbConnRequest::Subcribe {
        key: key.to_string(),
    }
    .encode()
    .unwrap();
    {
        let mut writer = get_header_msg_writer(&mut client).unwrap();
        writer.write_msg(&request).await.unwrap();
    }

    let mut reader = get_header_msg_reader(&mut client).unwrap();
    let response = timeout(Duration::from_millis(1_000), reader.read_msg())
        .await
        .expect("subscribe did not bypass stale control connection quickly")
        .unwrap();
    assert!(matches!(
        PbConnResponse::decode(response).unwrap(),
        PbConnResponse::Subcribe { .. }
    ));

    let stream = stream_rx.await.unwrap();
    drop(stream);
    healthy_task.abort();
    stale_task.abort();
    shutdown_token.cancel();
    server.await.unwrap();
}

#[tokio::test]
async fn subscribe_bypasses_acked_control_connection_without_stream() {
    let probe_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let server_addr = probe_listener.local_addr().unwrap();
    drop(probe_listener);

    let shutdown_token = CancellationToken::new();
    let server_shutdown = shutdown_token.clone();
    let server = tokio::spawn(async move {
        run_server_with_shutdown(server_addr, server_shutdown, None)
            .await
            .unwrap();
    });

    let key = "sf-backend";
    let (healthy_ready_tx, healthy_ready_rx) = tokio::sync::oneshot::channel();
    let (stream_tx, stream_rx) = tokio::sync::oneshot::channel();
    let healthy_key = key.to_string();
    let healthy_task = tokio::spawn(async move {
        let healthy_control = wait_for_server(server_addr).await;
        let (mut reader_stream, mut writer_stream) = healthy_control.into_split();
        let mut reader = get_header_msg_reader(&mut reader_stream).unwrap();
        {
            let mut writer = get_header_msg_writer(&mut writer_stream).unwrap();
            register_control_conn_parts(&mut reader, &mut writer, &healthy_key).await;
        }
        let mut writer = get_header_msg_writer(&mut writer_stream).unwrap();
        healthy_ready_tx.send(()).unwrap();
        let request = timeout(Duration::from_secs(2), reader.read_msg())
            .await
            .expect("healthy control did not receive fallback stream request")
            .unwrap();
        let LocalServer::Stream {
            client_id,
            server_generation,
        } = LocalServer::decode(request).unwrap()
        else {
            panic!("unexpected local server control message");
        };

        let ack = PbServerRequest::StreamAck {
            client_id,
            server_generation,
        }
        .encode()
        .unwrap();
        writer.write_msg(&ack).await.unwrap();

        let mut stream = TcpStream::connect(server_addr).await.unwrap();
        let request = PbConnRequest::Stream {
            key: healthy_key,
            dst_id: client_id,
            server_generation,
        }
        .encode()
        .unwrap();
        let mut stream_writer = get_header_msg_writer(&mut stream).unwrap();
        stream_writer.write_msg(&request).await.unwrap();
        stream_tx.send(stream).unwrap();
        std::future::pending::<()>().await;
    });
    healthy_ready_rx.await.unwrap();

    let (stale_ready_tx, stale_ready_rx) = tokio::sync::oneshot::channel();
    let stale_key = key.to_string();
    let stale_task = tokio::spawn(async move {
        let stale_control = wait_for_server(server_addr).await;
        let (mut reader_stream, mut writer_stream) = stale_control.into_split();
        let mut reader = get_header_msg_reader(&mut reader_stream).unwrap();
        {
            let mut writer = get_header_msg_writer(&mut writer_stream).unwrap();
            register_control_conn_parts(&mut reader, &mut writer, &stale_key).await;
        }
        let mut writer = get_header_msg_writer(&mut writer_stream).unwrap();
        stale_ready_tx.send(()).unwrap();
        let request = timeout(Duration::from_secs(2), reader.read_msg())
            .await
            .expect("stale control did not receive first stream request")
            .unwrap();
        let LocalServer::Stream {
            client_id,
            server_generation,
        } = LocalServer::decode(request).unwrap()
        else {
            panic!("unexpected local server control message");
        };
        let ack = PbServerRequest::StreamAck {
            client_id,
            server_generation,
        }
        .encode()
        .unwrap();
        writer.write_msg(&ack).await.unwrap();
        std::future::pending::<()>().await;
    });
    stale_ready_rx.await.unwrap();

    let mut client = wait_for_server(server_addr).await;
    let request = PbConnRequest::Subcribe {
        key: key.to_string(),
    }
    .encode()
    .unwrap();
    {
        let mut writer = get_header_msg_writer(&mut client).unwrap();
        writer.write_msg(&request).await.unwrap();
    }

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
    shutdown_token.cancel();
    server.await.unwrap();
}
