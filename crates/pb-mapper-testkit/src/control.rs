//! Registering a protocol-v2 control connection by hand.
//!
//! For cases that need the relay to treat a registration as v2 — the lease
//! timeout rather than the eleven-minute legacy grace — without driving a secure
//! first flight, and without the `register` client's reconnect behaviour in the
//! way.

use std::time::Duration;

use pb_mapper_protocol::command::{MessageSerializer, PbConnRequest, PbConnResponse};
use pb_mapper_protocol::{MessageReader, MessageWriter};
use tokio::time::timeout;

/// What a case wants its hand-rolled v2 registration to look like.
///
/// Everything has a default that suits a case with no opinion; override only the
/// parts the case is actually about.
pub struct V2ControlSpec {
    instance_id: String,
    heartbeat_interval_ms: u64,
    heartbeat_tolerance_ms: u64,
    response_timeout: Duration,
}

impl Default for V2ControlSpec {
    fn default() -> Self {
        Self {
            instance_id: "pb-mapper-testkit".to_string(),
            heartbeat_interval_ms: 50,
            heartbeat_tolerance_ms: 150,
            response_timeout: Duration::from_secs(2),
        }
    }
}

impl V2ControlSpec {
    pub fn new() -> Self {
        Self::default()
    }

    /// The instance ID the relay records for the registration.
    pub fn instance_id(mut self, instance_id: &str) -> Self {
        self.instance_id = instance_id.to_string();
        self
    }

    /// The heartbeat the registration advertises, which is what the relay derives
    /// the lease from.
    pub fn heartbeat(mut self, interval_ms: u64, tolerance_ms: u64) -> Self {
        self.heartbeat_interval_ms = interval_ms;
        self.heartbeat_tolerance_ms = tolerance_ms;
        self
    }

    /// How long to wait for the relay's `RegisterV2` response.
    pub fn response_timeout(mut self, response_timeout: Duration) -> Self {
        self.response_timeout = response_timeout;
        self
    }
}

/// Register a protocol-v2 control connection over the given framing pair, and
/// return the connection ID and generation the relay assigned.
///
/// Plain framing carrying `protocol_version: 2`, which is what makes the relay
/// record the registration as v2 without a secure first flight.
///
/// The reader is the caller's and must outlive every frame on the connection: the
/// header codec is a counter nonce sequence, so a second reader would restart at
/// zero and fail to decrypt. The writer is the opposite — the relay reads the
/// initial frame and its continuation frames through two separate decoders, each
/// starting at zero, so the caller has to build a **fresh** writer after this
/// returns before sending anything else.
pub async fn register_v2_control(
    reader: &mut impl MessageReader,
    writer: &mut impl MessageWriter,
    key: &str,
    spec: V2ControlSpec,
) -> (u32, u64) {
    let request = PbConnRequest::Register {
        need_codec: false,
        is_datagram: false,
        key: key.to_string(),
        protocol_version: Some(2),
        client_instance_id: Some(spec.instance_id),
        heartbeat_interval_ms: Some(spec.heartbeat_interval_ms),
        heartbeat_tolerance_ms: Some(spec.heartbeat_tolerance_ms),
    }
    .encode()
    .unwrap();
    writer.write_msg(&request).await.unwrap();

    let response = timeout(spec.response_timeout, reader.read_msg())
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
