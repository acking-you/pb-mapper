//! Cases that run the `pb-mapper` binary.

use std::process::Stdio;
use std::time::Duration;

use pb_mapper_core::test_support::TempPath;
use pb_mapper_protocol::command::{MessageSerializer, PbConnResponse};
use pb_mapper_protocol::{MessageReader, get_header_msg_reader};
use pb_mapper_testkit::{TEST_ADMIN_KEY, Transport, reserve_addr};
use tokio::sync::oneshot;
use tokio::time::timeout;

use crate::support::{
    InProcessRelay, StaleEnd, StateDir, auth_config, issue_before_start, serve_one_stream,
    stale_control, subscribe,
};

#[tokio::test]
async fn explicit_invalid_msg_header_key_fails_server_startup() {
    let state_dir = TempPath::new("invalid-env");
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_pb-mapper"));
    command
        .arg("server")
        .arg("--port")
        .arg("0")
        .arg("--auth-state-dir")
        .arg(state_dir.path())
        .env("MSG_HEADER_KEY", "invalid")
        // The assertion reads the error log; an inherited filter must not hide it.
        .env("RUST_LOG", "error")
        .kill_on_drop(true);

    let output = timeout(Duration::from_secs(3), command.output())
        .await
        .expect("invalid explicit key must fail instead of starting the server")
        .unwrap();
    assert!(!output.status.success());
    let logs = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        logs.contains("administrator_key_invalid"),
        "status: {}, logs: {logs}",
        output.status
    );
    assert!(!state_dir.join("admin.key").exists());
}

#[tokio::test]
async fn admin_all_preserves_json_output_mode() {
    let config = auth_config();
    issue_before_start(&config, ["json-output"]).await;
    let relay = InProcessRelay::start(config).await;

    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_pb-mapper"));
    command
        .arg("admin")
        .arg("--server")
        .arg(relay.addr.to_string())
        .arg("--output")
        .arg("json")
        .arg("key")
        .arg("list")
        .arg("--all")
        .env("MSG_HEADER_KEY", TEST_ADMIN_KEY)
        .env("RUST_LOG", "off")
        .kill_on_drop(true);
    let output = timeout(Duration::from_secs(3), command.output())
        .await
        .expect("admin JSON request timed out")
        .unwrap();
    assert!(output.status.success());
    let document: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(document["schema_version"], 1);
    assert_eq!(
        document["data"]["KeyList"]["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    relay.stop().await;
}

#[tokio::test]
async fn subscribe_bypasses_unacked_stale_control_connection() {
    bypass_unacked_control(false).await;
}

#[tokio::test]
async fn subscribe_failover_reuses_its_namespace_slot_and_rate_token() {
    bypass_unacked_control(true).await;
}

async fn bypass_unacked_control(at_capacity: bool) {
    // Bound by the child relay, so only an address no `:0` bind can be handed.
    let server_addr = reserve_addr(Transport::Tcp).await;
    let config = auth_config();
    let _state = StateDir(config.state_dir.clone());
    // Limits belong only to this relay process, not the parallel test runner.
    let mut server = tokio::process::Command::new(env!("CARGO_BIN_EXE_pb-mapper"))
        .args([
            "server",
            "--port",
            &server_addr.port().to_string(),
            "--legacy-protocol",
            "allow",
        ])
        .arg("--auth-state-dir")
        .arg(&config.state_dir)
        .env("MSG_HEADER_KEY", TEST_ADMIN_KEY)
        .env(
            "PB_MAPPER_MAX_STREAMS_PER_NAMESPACE",
            if at_capacity { "1" } else { "1024" },
        )
        .env(
            "PB_MAPPER_NEW_STREAMS_BURST",
            if at_capacity { "1" } else { "200" },
        )
        .env("PB_MAPPER_NEW_STREAMS_PER_SECOND", "1")
        .env("RUST_LOG", "error")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();

    let key = "sf-backend";
    let (healthy_ready_tx, healthy_ready_rx) = oneshot::channel();
    let (stream_tx, stream_rx) = oneshot::channel();
    let healthy_task = tokio::spawn(serve_one_stream(
        server_addr,
        key.to_string(),
        "healthy control",
        Some(healthy_ready_tx),
        stream_tx,
    ));
    healthy_ready_rx.await.unwrap();

    let (stale_ready_tx, stale_ready_rx) = oneshot::channel();
    let stale_task = tokio::spawn(stale_control(
        server_addr,
        key.to_string(),
        stale_ready_tx,
        StaleEnd::Park,
    ));
    stale_ready_rx.await.unwrap();

    let mut client = subscribe(server_addr, key).await;
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
    server.kill().await.unwrap();
    server.wait().await.unwrap();
}
