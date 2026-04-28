use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use pb_mapper::common::message::command::{
    MessageSerializer, PbConnRequest, PbConnResponse, PbConnStatusReq, PbConnStatusResp,
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
