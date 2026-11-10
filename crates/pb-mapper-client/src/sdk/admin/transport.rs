//! One-shot administrator request/response over a fresh protocol-v2 session.

use crate::relay::CarrierId;
use std::time::Duration;

use crate::endpoint::RelayEndpoint;
use pb_mapper_core::checksum::Credential;
use pb_mapper_core::config::{ResolvedAddrs, resolve_addrs_async};
use pb_mapper_protocol::MessageReader;
use pb_mapper_protocol::command::{
    AdminRequest, AdminResponse, MessageSerializer, PbConnRequest, PbConnResponse,
};
use pb_mapper_protocol::secure::{CONNECTION_SALT_REPLAYED, ClientHeaderSession};

use super::super::Error;
use super::super::error::Result;

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
    endpoint: &RelayEndpoint,
    credential: Credential,
    request: AdminRequest,
    io_timeout: Duration,
) -> Result<AdminResponse> {
    // Preserve the administrator API's fresh resolution and public address error
    // boundary. The selected carrier may still reuse a matching QUIC connection.
    let addrs = resolve_addrs_async(endpoint.name())
        .await
        .map_err(|source| Error::Address {
            addr: endpoint.to_string(),
            source,
        })?;
    let encoded = PbConnRequest::Admin(request).encode().map_err(protocol)?;

    for attempt in 0..MAX_ATTEMPTS {
        let last_attempt = attempt + 1 == MAX_ATTEMPTS;
        let mut exchange = Exchange::new();
        let outcome = tokio::time::timeout(
            io_timeout,
            exchange.run(endpoint, &addrs, &credential, &encoded),
        )
        .await
        .unwrap_or(Err(Error::TimedOut {
            timeout: io_timeout,
        }));

        let response = match outcome {
            Ok(response) => response,
            // Nothing reached the relay, so re-sending cannot duplicate an
            // effect. Anything past that point is reported as-is.
            Err(error) => {
                if error.is_carrier_failure() {
                    endpoint.carrier_failed(exchange.carrier);
                }
                if exchange.sent || last_attempt || !error.may_resend_unsent() {
                    return Err(error);
                }
                continue;
            }
        };

        match response {
            PbConnResponse::Admin(response) => return Ok(response),
            // The relay refused a first flight whose salt it had already
            // seen, before looking at the request. The next attempt's fresh
            // session draws a fresh salt, so one retry is enough.
            PbConnResponse::Error(error)
                if error.code == CONNECTION_SALT_REPLAYED && error.retryable && !last_attempt =>
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
    carrier: Option<CarrierId>,
}

impl Exchange {
    fn new() -> Self {
        Self {
            sent: false,
            carrier: None,
        }
    }

    /// Select a carrier stream before sending the administrator request.
    /// Candidate racing and QUIC discovery cannot send this mutation: `sent`
    /// still describes exactly one authenticated protocol session.
    async fn run(
        &mut self,
        endpoint: &RelayEndpoint,
        addrs: &ResolvedAddrs,
        credential: &Credential,
        encoded: &[u8],
    ) -> Result<PbConnResponse> {
        let mut stream = endpoint
            .connect_addresses(addrs, *credential, &mut self.carrier)
            .await
            .map_err(|error| Error::from_connect(error, addrs.to_string()))?;
        let session = ClientHeaderSession::new_v2(credential).map_err(protocol)?;
        self.write_request(&mut stream, &session, encoded).await?;
        let mut reader = session.response_reader(&mut stream).map_err(protocol)?;
        let message = reader.read_msg().await.map_err(protocol)?;
        PbConnResponse::decode(message).map_err(protocol)
    }

    async fn write_request<T: tokio::io::AsyncWriteExt + Unpin>(
        &mut self,
        stream: &mut T,
        session: &ClientHeaderSession,
        encoded: &[u8],
    ) -> Result<()> {
        // A failed or cancelled write can have reached the relay. Mark the
        // mutation ambiguous before its first byte, not after write completion.
        self.sent = true;
        session
            .write_initial(stream, encoded)
            .await
            .map_err(protocol)
    }
}

/// A framing or session failure as the SDK's protocol error, its causes
/// written into its text.
fn protocol(error: impl std::error::Error) -> Error {
    Error::protocol(super::super::error::with_causes(&error))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pb_mapper_core::test_support::TEST_ADMIN_KEY_BYTES;
    use tokio::io::AsyncReadExt;

    #[tokio::test]
    async fn invalid_admin_address_preserves_the_sdk_error_variant() {
        let endpoint = RelayEndpoint::shared("localhost:invalid-port");
        let result = send_admin_request(
            &endpoint,
            Credential::Admin(TEST_ADMIN_KEY_BYTES),
            AdminRequest::AuthStatus,
            Duration::from_secs(1),
        )
        .await;
        assert!(matches!(result, Err(Error::Address { .. })));
    }

    /// A framing failure keeps what caused it in the text an administrator
    /// request fails with, which is all a Node or app caller gets of it.
    #[test]
    fn a_framing_failure_keeps_its_cause() {
        let cause = || std::io::Error::new(std::io::ErrorKind::ConnectionReset, "reset mid-reply");
        let failure = pb_mapper_core::error::Error::MsgNetworkReadBody { source: cause() };
        let Error::Protocol { message } = protocol(failure) else {
            panic!("a framing failure is a protocol error");
        };
        assert!(message.contains(&cause().to_string()), "{message}");
    }

    #[tokio::test(start_paused = true)]
    async fn cancelled_partial_admin_write_must_not_be_retried() {
        let credential = Credential::Admin(TEST_ADMIN_KEY_BYTES);
        let session = ClientHeaderSession::new_v2(&credential).unwrap();
        let (mut writer, mut reader) = tokio::io::duplex(1);
        let mut exchange = Exchange::new();
        assert!(
            tokio::time::timeout(
                Duration::from_millis(10),
                exchange.write_request(&mut writer, &session, b"mutation")
            )
            .await
            .is_err()
        );
        assert!(
            exchange.sent,
            "a partial mutation write is not safe to retry"
        );
        assert!(reader.read_u8().await.is_ok());
    }
}
