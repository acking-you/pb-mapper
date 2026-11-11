//! Cases that run the `pb-mapper` binary.
//!
//! A binary of its own, because they start processes. On Windows a child
//! inherits every inheritable socket open in its parent when it starts and
//! keeps it until it exits, so a connection another case dropped stays open,
//! then resets, and a listener it dropped keeps its port (Tokio's sockets
//! were inheritable until mio 1.2.1). No case here waits for a close, and no
//! case here reserves a port: a child relay binds port 0 and says which port
//! it took.

// An integration test: a failed `unwrap` is a failed test, which is the report
// this target exists to produce. `allow-unwrap-in-tests` covers `#[cfg(test)]`
// modules but not a `tests/` target, whose whole body is test code.
#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "../regression/credential.rs"]
mod credential;
#[path = "../regression/fixtures.rs"]
mod fixtures;

use std::net::{Ipv4Addr, SocketAddr};
use std::process::Stdio;
use std::time::Duration;

use pb_mapper_core::test_support::TempPath;
use pb_mapper_protocol::command::{MessageSerializer, PbConnResponse};
use pb_mapper_protocol::{MessageReader, get_header_msg_reader, get_header_msg_writer};
use pb_mapper_testkit::TEST_ADMIN_KEY;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::TcpStream;
use tokio::process::Child;
use tokio::sync::oneshot;
use tokio::time::timeout;
use tokio_util::task::AbortOnDropHandle;

use credential::{StateDir, auth_config};
use fixtures::{
    InProcessRelay, issue_before_start, register_control_conn_parts, serve_one_stream, subscribe,
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

/// Start `command`, a `pb-mapper server` on port 0, and return the loopback
/// address of the port it took: it says so in its `pb_server_listening`
/// event, which it writes to stdout once it listens. The rest of its stdout
/// is drained for as long as the returned task lives, so the child never
/// blocks on a full pipe.
async fn spawn_relay(
    command: &mut tokio::process::Command,
) -> (Child, SocketAddr, AbortOnDropHandle<()>) {
    let mut child = command
        .env("PB_MAPPER_LOG_FORMAT", "json")
        .env("RUST_LOG", "error,pb_mapper_server::runtime=info")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let port = timeout(Duration::from_secs(10), async {
        loop {
            let line = lines
                .next_line()
                .await
                .unwrap()
                .expect("the relay exited before it listened");
            let Ok(event) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };
            if event["event"] == "pb_server_listening" {
                let listen_addr: SocketAddr =
                    event["listen_addr"].as_str().unwrap().parse().unwrap();
                break listen_addr.port();
            }
        }
    })
    .await
    .expect("the relay did not start listening");
    let drain = AbortOnDropHandle::new(tokio::spawn(async move {
        while let Ok(Some(_)) = lines.next_line().await {}
    }));
    (child, SocketAddr::from((Ipv4Addr::LOCALHOST, port)), drain)
}

/// A provider that registers `key` as a legacy control connection, reports
/// it `ready`, and reads nothing more.
async fn parked_control(addr: SocketAddr, key: String, ready: oneshot::Sender<()>) {
    let control = TcpStream::connect(addr).await.unwrap();
    let (mut reader_stream, mut writer_stream) = control.into_split();
    let mut reader = get_header_msg_reader(&mut reader_stream).unwrap();
    let mut writer = get_header_msg_writer(&mut writer_stream).unwrap();
    register_control_conn_parts(&mut reader, &mut writer, &key).await;
    ready.send(()).unwrap();
    std::future::pending::<()>().await;
}

async fn bypass_unacked_control(at_capacity: bool) {
    let config = auth_config();
    let _state = StateDir(config.state_dir.clone());
    // Limits belong only to this relay process, not the parallel test runner.
    let (mut server, server_addr, _drain) = spawn_relay(
        tokio::process::Command::new(env!("CARGO_BIN_EXE_pb-mapper"))
            .args(["server", "--port", "0", "--legacy-protocol", "allow"])
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
            .env("PB_MAPPER_NEW_STREAMS_PER_SECOND", "1"),
    )
    .await;

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
    let stale_task = tokio::spawn(parked_control(server_addr, key.to_string(), stale_ready_tx));
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
