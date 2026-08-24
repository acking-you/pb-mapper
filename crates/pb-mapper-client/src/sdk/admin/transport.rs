//! One-shot administrator request/response over a fresh protocol-v2 session.

use std::time::Duration;

use pb_mapper_core::checksum::Credential;
use pb_mapper_protocol::MessageReader;
use pb_mapper_protocol::command::{
    AdminRequest, AdminResponse, MessageSerializer, PbConnRequest, PbConnResponse,
};
use pb_mapper_protocol::secure::ClientHeaderSession;
use snafu::ResultExt;
use tokio::net::TcpStream;

use super::super::Error;
use super::super::error::{ConnectSnafu, Result};

/// The relay refuses a first flight whose salt it has already seen, and answers
/// with this code. It is retryable by construction: a fresh session gets a fresh
/// salt, so one retry is enough.
const REPLAYED_SALT_CODE: &str = "connection_salt_replayed";

/// How many times a request may be sent. Two: one attempt, plus one retry for
/// the two failures a retry can actually fix — a replayed salt, and a failure
/// that happened before any bytes went out.
const MAX_ATTEMPTS: usize = 2;

/// Send one administrator request and return the relay's response.
///
/// Retries at most once, and only when the retry is safe: either nothing was
/// sent (so the relay cannot have acted on it), or the relay itself rejected the
/// session salt before looking at the request. An administrator RPC is not
/// generally idempotent — `RootKeyRotate` least of all — so a failure after the
/// request went out is reported rather than repeated.
pub(super) async fn send_admin_request(
    server: &str,
    credential: Credential,
    request: AdminRequest,
    io_timeout: Duration,
) -> Result<AdminResponse> {
    let addr = pb_mapper_core::config::get_sockaddr_async(server)
        .await
        .map_err(|source| Error::Address {
            addr: server.to_string(),
            source,
        })?;
    let encoded = PbConnRequest::Admin(request).encode().map_err(protocol)?;

    for attempt in 0..MAX_ATTEMPTS {
        let last_attempt = attempt + 1 == MAX_ATTEMPTS;
        let mut exchange = Exchange::new();
        let outcome = tokio::time::timeout(io_timeout, exchange.run(addr, &credential, &encoded))
            .await
            .unwrap_or(Err(Error::TimedOut {
                timeout: io_timeout,
            }));

        let response = match outcome {
            Ok(response) => response,
            // Nothing reached the relay, so re-sending cannot duplicate an
            // effect. Anything past that point is reported as-is.
            Err(error) => {
                if exchange.sent || last_attempt {
                    return Err(error);
                }
                continue;
            }
        };

        match response {
            PbConnResponse::Admin(response) => return Ok(response),
            PbConnResponse::Error(error)
                if error.code == REPLAYED_SALT_CODE && error.retryable && !last_attempt =>
            {
                continue;
            }
            PbConnResponse::Error(error) => return Err(Error::from_remote(error)),
            other => {
                return Err(Error::protocol(format!(
                    "unexpected administrator response: {other:?}"
                )));
            }
        }
    }
    Err(Error::protocol(
        "connection salt replay retry was exhausted",
    ))
}

/// One attempt at the exchange, tracking whether the request left the process.
///
/// That flag is the whole reason this is a struct: it has to survive the attempt
/// failing, and it decides whether a retry is safe.
struct Exchange {
    sent: bool,
}

impl Exchange {
    fn new() -> Self {
        Self { sent: false }
    }

    async fn run(
        &mut self,
        addr: std::net::SocketAddr,
        credential: &Credential,
        encoded: &[u8],
    ) -> Result<PbConnResponse> {
        let mut stream = TcpStream::connect(addr).await.context(ConnectSnafu {
            addr: addr.to_string(),
        })?;
        let session = ClientHeaderSession::new_v2(credential).map_err(protocol)?;
        session
            .write_initial(&mut stream, encoded)
            .await
            .map_err(protocol)?;
        self.sent = true;
        let mut reader = session.response_reader(&mut stream).map_err(protocol)?;
        let message = reader.read_msg().await.map_err(protocol)?;
        PbConnResponse::decode(message).map_err(protocol)
    }
}

/// Flatten a framing or session failure into the SDK's protocol error.
fn protocol(error: impl std::fmt::Display) -> Error {
    Error::protocol(error.to_string())
}
