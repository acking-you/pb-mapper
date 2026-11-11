//! A provider that never acknowledges a stream request, as every provider
//! before 0.2.12 did, is served through the stream it opens: the relay takes
//! that stream in place of the acknowledgement. A newer publisher whose stream
//! overtakes its queued acknowledgement takes the same branch, but only when
//! it loses that race, so this is the case that runs it every time.
//!
//! A binary of its own, because the stream has to reach the relay within its
//! wait for an acknowledgement, `PB_MAPPER_STREAM_ACK_TIMEOUT` (300 ms by
//! default), and a loaded machine can take longer than that to open a
//! connection. This binary sets it to the five seconds the relay gives a whole
//! subscribe, once, before its relay starts; every relay in the process reads
//! it.

// An integration test: a failed `unwrap` is a failed test, which is the report
// this target exists to produce. `allow-unwrap-in-tests` covers `#[cfg(test)]`
// modules but not a `tests/` target, whose whole body is test code.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::LazyLock;
use std::time::Duration;

use pb_mapper_core::config::PB_MAPPER_STREAM_ACK_TIMEOUT;
use pb_mapper_protocol::command::{LocalServer, MessageSerializer, PbConnRequest, PbConnResponse};
use pb_mapper_protocol::{
    MessageReader, MessageWriter, get_header_msg_reader, get_header_msg_writer,
};
use pb_mapper_testkit::Relay;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;

/// How long the case waits for what the relay does at once: a bound for a
/// loaded machine, not a timing the case asserts.
const READY: Duration = Duration::from_secs(5);

/// Let the relay wait [`READY`] for an acknowledgement, for this whole binary.
fn long_ack_wait_env() {
    static ENV: LazyLock<()> = LazyLock::new(|| {
        // SAFETY: mutating the environment is unsafe in edition 2024 because
        // it is process-global. This runs once, before the binary's relay
        // starts, and nothing here writes the name again.
        unsafe {
            std::env::set_var(
                PB_MAPPER_STREAM_ACK_TIMEOUT,
                format!("{}ms", READY.as_millis()),
            );
        }
    });
    *ENV;
}

/// Write `frame` as the first, legacy-framed message on `stream`, and read the
/// relay's answer to it.
async fn exchange(stream: &mut TcpStream, frame: &[u8]) -> PbConnResponse {
    get_header_msg_writer(&mut *stream)
        .unwrap()
        .write_msg(frame)
        .await
        .unwrap();
    let mut reader = get_header_msg_reader(stream).unwrap();
    let answer = timeout(READY, reader.read_msg())
        .await
        .expect("the relay did not answer")
        .unwrap();
    PbConnResponse::decode(answer).unwrap()
}

#[tokio::test]
async fn a_stream_that_comes_without_its_acknowledgement_is_served() {
    long_ack_wait_env();
    let relay = Relay::start("stream-before-ack").await;
    let key = "unacknowledged";

    let control = TcpStream::connect(relay.addr()).await.unwrap();
    let (mut control_read, mut control_write) = control.into_split();
    let mut control_reader = get_header_msg_reader(&mut control_read).unwrap();
    let register = PbConnRequest::Register {
        direct: None,
        need_codec: false,
        is_datagram: false,
        key: key.into(),
        protocol_version: None,
        client_instance_id: None,
        heartbeat_interval_ms: None,
        heartbeat_tolerance_ms: None,
    };
    get_header_msg_writer(&mut control_write)
        .unwrap()
        .write_msg(&register.encode().unwrap())
        .await
        .unwrap();
    let registered = timeout(READY, control_reader.read_msg())
        .await
        .expect("the relay did not answer the registration")
        .unwrap();
    assert!(matches!(
        PbConnResponse::decode(registered).unwrap(),
        PbConnResponse::Register(_)
    ));

    let mut subscriber = TcpStream::connect(relay.addr()).await.unwrap();
    let subscribe = PbConnRequest::Subcribe {
        data_protocol: None,
        key: key.into(),
    }
    .encode()
    .unwrap();
    let subscribed = tokio::spawn(async move {
        let answer = exchange(&mut subscriber, &subscribe).await;
        (subscriber, answer)
    });

    let request = timeout(READY, control_reader.read_msg())
        .await
        .expect("the relay did not ask the provider for a stream")
        .unwrap();
    let LocalServer::Stream { client_id, .. } = LocalServer::decode(request).unwrap() else {
        panic!("expected a stream request");
    };
    // No acknowledgement, and the stream in the shape such a provider sent,
    // without a generation.
    let mut publisher = TcpStream::connect(relay.addr()).await.unwrap();
    let stream = serde_json::json!({"Stream": {"key": key, "dst_id": client_id}});
    let published = exchange(&mut publisher, &serde_json::to_vec(&stream).unwrap()).await;
    assert!(
        matches!(published, PbConnResponse::Stream { .. }),
        "{published:?}"
    );
    let (mut subscriber, subscribed) = subscribed.await.unwrap();
    assert!(
        matches!(subscribed, PbConnResponse::Subcribe { .. }),
        "{subscribed:?}"
    );

    // The two legs are joined, unframed as neither asked for a codec.
    let mut buf = [0u8; 4];
    subscriber.write_all(b"ping").await.unwrap();
    timeout(READY, publisher.read_exact(&mut buf))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(&buf, b"ping");
    publisher.write_all(b"pong").await.unwrap();
    timeout(READY, subscriber.read_exact(&mut buf))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(&buf, b"pong");
}
