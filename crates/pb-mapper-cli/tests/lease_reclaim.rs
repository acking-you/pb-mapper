//! The relay reclaims a registration whose lease stopped being renewed.
//!
//! A dedicated test binary, because the cases here shorten
//! `PB_MAPPER_SERVER_LEASE_TIMEOUT` — which every relay in the process reads on
//! every sweep. Setting it inside `regression.rs` would retire the registrations
//! of whatever case happened to be running alongside.

// An integration test: a failed `unwrap` is a failed test, which is the report
// this file exists to produce. `allow-unwrap-in-tests` covers `#[cfg(test)]`
// modules but not a `tests/` target, whose whole body is test code.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::LazyLock;
use std::time::Duration;

use pb_mapper_client::sdk::Client;
use pb_mapper_core::config::{
    PB_MAPPER_SERVER_LEASE_SWEEP_INTERVAL, PB_MAPPER_SERVER_LEASE_TIMEOUT,
};
use pb_mapper_protocol::command::{MessageSerializer, PbServerRequest};
use pb_mapper_protocol::{
    MessageReader, MessageWriter, get_header_msg_reader, get_header_msg_writer,
};
use pb_mapper_testkit::{Relay, V2ControlSpec, admin_credential, register_v2_control};
use tokio::net::TcpStream;
use tokio::time::{Instant, timeout};

/// The lease the relay hands out in this binary, and how often it sweeps.
///
/// Both far under the defaults, so a case finishes in a fraction of a second.
/// The interval stays a small fraction of the lease because retiring one
/// registration takes two sweeps.
const LEASE_TIMEOUT: Duration = Duration::from_millis(150);
const SWEEP_INTERVAL: Duration = Duration::from_millis(25);

/// How long a case waits for the relay to act, generously over two sweeps.
const RECLAIM_TIMEOUT: Duration = Duration::from_secs(5);

/// How often a case writes a control frame while it waits.
const CONTROL_FRAME_INTERVAL: Duration = Duration::from_millis(20);

/// Shorten the lease for this whole test binary.
///
/// Set once and never restored, rather than through a per-case guard: the cases
/// run concurrently, and a guard restoring the default while another case was
/// still waiting on a sweep would leave it waiting the full fifteen seconds.
fn short_lease_env() {
    static ENV: LazyLock<()> = LazyLock::new(|| {
        // SAFETY: mutating the environment is unsafe in edition 2024 because it
        // is process-global. This runs once, before any relay in the binary
        // starts, and nothing here ever writes these names again.
        unsafe {
            std::env::set_var(
                PB_MAPPER_SERVER_LEASE_TIMEOUT,
                format!("{}ms", LEASE_TIMEOUT.as_millis()),
            );
            std::env::set_var(
                PB_MAPPER_SERVER_LEASE_SWEEP_INTERVAL,
                format!("{}ms", SWEEP_INTERVAL.as_millis()),
            );
        }
    });
    *ENV;
}

/// Register a v2 control connection and wait for the relay to publish it.
///
/// The heartbeat is far under the defaults to match this binary's short lease,
/// and the wait is this file's own readiness requirement rather than part of
/// registering — a case here starts timing sweeps from the moment the relay
/// reports the key.
async fn register_and_await_v2_control(
    relay: &Relay,
    reader: &mut impl MessageReader,
    writer: &mut impl MessageWriter,
    key: &str,
) {
    register_v2_control(
        reader,
        writer,
        key,
        V2ControlSpec::new()
            .instance_id("lease-reclaim-test")
            .heartbeat(20, 60),
    )
    .await;
    relay
        .wait_for_registration(key, admin_credential(), None)
        .await;
}

async fn is_registered(relay: &Relay, key: &str) -> bool {
    relay
        .registered_keys(admin_credential(), None)
        .await
        .unwrap()
        .iter()
        .any(|candidate| candidate == key)
}

#[tokio::test]
async fn sweep_reclaims_a_registration_that_reads_but_never_renews() {
    short_lease_env();
    let relay = Relay::start("lease-sweep-stale").await;
    let key = "wedged-service";

    let control = TcpStream::connect(relay.addr()).await.unwrap();
    let (mut read_half, mut write_half) = control.into_split();
    let mut reader = get_header_msg_reader(&mut read_half).unwrap();
    {
        let mut writer = get_header_msg_writer(&mut write_half).unwrap();
        register_and_await_v2_control(&relay, &mut reader, &mut writer, key).await;
    }
    let mut writer = get_header_msg_writer(&mut write_half).unwrap();

    // The wedge this sweep exists for. Frames the relay can read but not decode
    // keep its reader loop returning, so the per-connection idle timeout — which
    // only fires while a read is outstanding — never triggers, while `last_rx_at`
    // is never renewed either. That is the shape of the incident: a control task
    // busy on a socket, holding a slot in a full 16/16 connection quota.
    let deadline = Instant::now() + RECLAIM_TIMEOUT;
    while Instant::now() < deadline {
        // A failed write means the relay has already closed the connection,
        // which on this path only happens because the sweep retired it.
        if writer.write_msg(b"not-a-pb-server-request").await.is_err() {
            break;
        }
        if !is_registered(&relay, key).await {
            return;
        }
        tokio::time::sleep(CONTROL_FRAME_INTERVAL).await;
    }
    assert!(
        !is_registered(&relay, key).await,
        "`{key}` was never reclaimed while its lease went unrenewed"
    );
}

#[tokio::test]
async fn sweep_leaves_a_registration_that_keeps_pinging() {
    short_lease_env();
    let relay = Relay::start("lease-sweep-healthy").await;
    let key = "healthy-service";

    let control = TcpStream::connect(relay.addr()).await.unwrap();
    let (mut read_half, mut write_half) = control.into_split();
    let mut reader = get_header_msg_reader(&mut read_half).unwrap();
    {
        let mut writer = get_header_msg_writer(&mut write_half).unwrap();
        register_and_await_v2_control(&relay, &mut reader, &mut writer, key).await;
    }
    let mut writer = get_header_msg_writer(&mut write_half).unwrap();

    // Well past the two sweeps that retire a registration which has gone quiet,
    // pinging throughout — which is all a healthy client does to renew its lease.
    let deadline = Instant::now() + LEASE_TIMEOUT * 4;
    for seq in 0.. {
        if Instant::now() >= deadline {
            break;
        }
        writer
            .write_msg(&PbServerRequest::PingV2 { seq }.encode().unwrap())
            .await
            .unwrap();
        // Drained so the relay's control writer never blocks on a full socket.
        reader.read_msg().await.unwrap();
        tokio::time::sleep(CONTROL_FRAME_INTERVAL).await;
    }

    assert!(
        is_registered(&relay, key).await,
        "the sweep retired a registration that was renewing its lease"
    );
}

/// `admin connection retire` unwinds the whole socket task, not just the routing
/// entry.
///
/// The incident this guards: a registration the relay could not tell was dead held
/// a slot in a full per-service quota for seventeen hours. Retirement has to reach
/// the socket task, because that task is what holds the connection ID — removing the
/// routing entry alone leaves the ID checked out for as long as the process lives.
///
/// The registration here never drains its socket, so the relay's pongs pile up and
/// nothing this side does can end the connection, and it keeps pinging, so its lease
/// is renewed and the sweep has no reason to act. Retirement is then the only thing
/// left that can close it. Both halves of the chain are asserted: the connection
/// leaving the admin listing is the routing state unwinding, and this side's writes
/// beginning to fail is the socket task itself unwinding — the half
/// `pb-mapper-server`'s unit coverage stops short of, since it ends at the writer
/// returning `Ok(())`.
#[tokio::test]
async fn admin_retire_unwinds_a_registration_that_never_reads() {
    short_lease_env();
    let relay = Relay::start("admin-retire-unwind").await;
    let key = "quota-hog";

    let control = TcpStream::connect(relay.addr()).await.unwrap();
    let (mut read_half, mut write_half) = control.into_split();
    // Read once, for the register response, and never again.
    let mut reader = get_header_msg_reader(&mut read_half).unwrap();
    let conn_id = {
        let mut writer = get_header_msg_writer(&mut write_half).unwrap();
        let (conn_id, _) = register_v2_control(
            &mut reader,
            &mut writer,
            key,
            V2ControlSpec::new()
                .instance_id("admin-retire-unwind")
                .heartbeat(20, 60),
        )
        .await;
        conn_id
    };
    relay
        .wait_for_registration(key, admin_credential(), None)
        .await;

    // Pings from their own task, so the lease keeps being renewed while the
    // administrator's request is in flight: without them the sweep would reclaim the
    // registration on its own and the case would pass without testing retirement.
    // The task returns when a write fails, which is this side observing that the
    // relay dropped the socket.
    let pinger = tokio::spawn(async move {
        let mut writer = get_header_msg_writer(&mut write_half).unwrap();
        for seq in 0.. {
            let request = PbServerRequest::PingV2 { seq }.encode().unwrap();
            if writer.write_msg(&request).await.is_err() {
                return;
            }
            tokio::time::sleep(CONTROL_FRAME_INTERVAL).await;
        }
    });

    let client = Client::from_credential(relay.addr().to_string(), admin_credential(), false, None);
    let admin = client.admin().unwrap();
    assert!(
        admin
            .list_connections_all(None)
            .await
            .unwrap()
            .iter()
            .any(|conn| conn.service_name == key && conn.conn_id == conn_id),
        "the registration should be listed before it is retired"
    );

    let retired = admin
        .retire_connections(None, key.to_string(), Some(conn_id))
        .await
        .unwrap();
    assert_eq!(retired, 1, "the administrator named one connection");

    let deadline = Instant::now() + RECLAIM_TIMEOUT;
    loop {
        let live = admin.list_connections_all(None).await.unwrap();
        if !live
            .iter()
            .any(|conn| conn.service_name == key && conn.conn_id == conn_id)
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "conn {conn_id} was still listed after retirement: {live:?}"
        );
        tokio::time::sleep(CONTROL_FRAME_INTERVAL).await;
    }

    // The socket task, not just the entry. A retirement that unwound only the routing
    // state would leave this task pinging a connection the relay still holds open,
    // and the ID that task is holding checked out for good.
    timeout(RECLAIM_TIMEOUT, pinger)
        .await
        .expect("the retired connection's socket was never closed")
        .unwrap();
}
