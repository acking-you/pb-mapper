use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use pb_mapper::common::message::command::{
    LocalServer, MessageSerializer, PbConnRequest, PbConnResponse, PbConnStatusReq,
    PbConnStatusResp, PbServerRequest,
};
use pb_mapper::common::message::{
    get_header_msg_reader, get_header_msg_writer, MessageReader, MessageWriter,
};
use pb_mapper::local::client::run_client_side_cli_with_callback;
use pb_mapper::pb_server::{get_init_request, run_server_with_shutdown};
use tokio::io::AsyncReadExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use uni_stream::stream::TcpListenerProvider;

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

#[tokio::test]
async fn client_closes_initial_status_probe_after_key_check() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let remote_addr = listener.local_addr().unwrap();

    let fake_server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let request = get_init_request(&mut stream, 0.into()).await.unwrap();
        assert!(matches!(
            request,
            PbConnRequest::Status(PbConnStatusReq::Keys)
        ));

        let response =
            PbConnResponse::Status(PbConnStatusResp::Keys(vec!["sf-backend".to_string()]))
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
