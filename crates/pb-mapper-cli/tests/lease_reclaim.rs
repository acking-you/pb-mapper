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

use pb_mapper_core::config::{
    PB_MAPPER_SERVER_LEASE_SWEEP_INTERVAL, PB_MAPPER_SERVER_LEASE_TIMEOUT,
};
use pb_mapper_protocol::command::{
    MessageSerializer, PbConnRequest, PbConnResponse, PbServerRequest,
};
use pb_mapper_protocol::{
    MessageReader, MessageWriter, get_header_msg_reader, get_header_msg_writer,
};
use pb_mapper_testkit::{Relay, admin_credential};
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

/// Register a protocol-v2 control connection over the given framing pair.
///
/// Plain framing carrying `protocol_version: 2`, which is what makes the relay
/// record the registration as v2 — and so put it on the lease timeout rather than
/// the eleven-minute legacy grace — without this file having to drive a secure
/// first flight.
///
/// The reader is the caller's and must outlive every frame on the connection: the
/// header codec is a counter nonce sequence, so a second reader would restart at
/// zero and fail to decrypt. The writer is the opposite — the relay reads the
/// initial frame and its continuation frames through two separate decoders, each
/// starting at zero, so the caller has to build a **fresh** writer after this
/// returns before sending anything else.
async fn register_v2_control(
    relay: &Relay,
    reader: &mut impl MessageReader,
    writer: &mut impl MessageWriter,
    key: &str,
) {
    let request = PbConnRequest::Register {
        need_codec: false,
        is_datagram: false,
        key: key.to_string(),
        protocol_version: Some(2),
        client_instance_id: Some("lease-reclaim-test".to_string()),
        heartbeat_interval_ms: Some(20),
        heartbeat_tolerance_ms: Some(60),
    }
    .encode()
    .unwrap();
    writer.write_msg(&request).await.unwrap();

    let response = timeout(Duration::from_secs(2), reader.read_msg())
        .await
        .expect("register response timed out")
        .unwrap();
    assert!(matches!(
        PbConnResponse::decode(response).unwrap(),
        PbConnResponse::RegisterV2 { .. }
    ));
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
        register_v2_control(&relay, &mut reader, &mut writer, key).await;
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
        register_v2_control(&relay, &mut reader, &mut writer, key).await;
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
