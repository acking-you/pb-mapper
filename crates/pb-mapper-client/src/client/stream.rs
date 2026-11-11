use crate::diagnostics::{Diagnostics, RecoveryFailure};
use crate::endpoint::RelayEndpoint;
use std::sync::Arc;

use crate::recovery::{MIN_SUBSCRIBE_ATTEMPT, SharedTiming};
use crate::relay::CarrierId;
use pb_mapper_protocol::transport::RelayStream;
use snafu::ResultExt;
use tokio::sync::{OwnedSemaphorePermit, mpsc};
use tokio::time::Instant;
use tracing::{Instrument, info_span, instrument};

use super::error::{
    DecodeSubcribeRespSnafu, EncodeSubcribeReqSnafu, Result, SubcribeRespNotMatchSnafu,
    WriteSubcribeReqSnafu,
};
use crate::client::error::CreateHeaderToolSnafu;
use pb_mapper_core::checksum::Credential;
use pb_mapper_core::config::control_io_timeout;
use pb_mapper_core::snafu_error_handle;
use pb_mapper_protocol::command::{MessageSerializer, PbConnRequest, PbConnResponse};
use pb_mapper_protocol::data::{DATA_PROTOCOL_V2, DataCodec};
use pb_mapper_protocol::forward::StreamForward;
use pb_mapper_protocol::secure::ClientHeaderSession;
use uni_stream::stream::NetworkStream;

pub(crate) struct StreamSetup {
    /// The listener's setup slot, given back once the stream forwards. A
    /// stream the caller handed in has none.
    pub permit: Option<OwnedSemaphorePermit>,
    pub timing: Arc<SharedTiming>,
    pub events: mpsc::Sender<bool>,
    pub diagnostics: Diagnostics,
    #[cfg(feature = "p2p")]
    pub direct: Option<Arc<crate::direct::Consumer>>,
}

#[instrument(skip(local_stream, credential, setup))]
pub(crate) async fn handle_local_stream<LocalStream: NetworkStream + StreamForward>(
    mut local_stream: LocalStream,
    key: Arc<str>,
    remote_addr: RelayEndpoint,
    keep_alive: bool,
    namespace: Option<u64>,
    credential: Credential,
    setup: StreamSetup,
) -> Result<()> {
    let started = Instant::now();
    // Retry only setup: no application byte is read until a subscription has
    // succeeded. A fresh socket/session avoids replaying payload or crypto state.
    let budget = std::time::Duration::from_secs(5).min(control_io_timeout());
    #[cfg(feature = "p2p")]
    let budget = if setup.direct.is_some() {
        std::time::Duration::from_secs(8).min(control_io_timeout())
    } else {
        budget
    };
    let deadline = started + budget;
    let global_permit = tokio::time::timeout_at(deadline, remote_addr.data_slots().acquire_owned())
        .await
        .ok()
        .and_then(std::result::Result::ok)
        .ok_or(super::error::Error::ControlIoTimeout {
            action: "setup admission",
            timeout: budget,
        })?;
    #[cfg(feature = "p2p")]
    if let Some(direct) = &setup.direct {
        let prepared = tokio::time::timeout_at(deadline, direct.prepare(&key, deadline))
            .await
            .map_err(|_| super::error::Error::ControlIoTimeout {
                action: "direct setup",
                timeout: budget,
            })?
            .map_err(direct_failed)?;
        if let Some(prepared) = prepared {
            drop(global_permit);
            setup.diagnostics.succeeded(started.elapsed());
            setup.timing.record(started.elapsed());
            let _ = setup.events.try_send(true);
            drop(setup.permit);
            return prepared
                .forward(local_stream, credential)
                .await
                .map_err(direct_failed);
        }
    }
    let subscription = Subscription {
        key: &key,
        remote_addr: &remote_addr,
        keep_alive,
        namespace,
        credential,
        offered_v2: LocalStream::supports_data_v2(),
    };
    let (
        Subscribed {
            stream: mut remote_stream,
            codec: codec_key,
            client_id,
            server_id,
        },
        setup_latency,
    ) = subscription.with_retry(&setup, deadline, budget).await?;
    drop(global_permit);
    remote_addr.protocol_succeeded();
    setup.diagnostics.succeeded(setup_latency);
    setup.timing.record(setup_latency);
    let _ = setup.events.try_send(true);
    drop(setup.permit);
    let session = setup
        .diagnostics
        .session_opened(pb_mapper_protocol::direct::SessionPath::Relay, None);
    let span = info_span!("forward", "client:{client_id} <-> server_id:{server_id}");
    pb_mapper_protocol::with_relay_stream!(&mut remote_stream, remote_stream, {
        let (client_reader, client_writer) = local_stream.split();
        let (server_reader, server_writer) = remote_stream.split();
        snafu_error_handle!(
            <LocalStream as StreamForward>::forward_local_to_remote_metered(
                codec_key,
                *credential.key(),
                client_reader,
                client_writer,
                server_reader,
                server_writer,
                Some(session.meter()),
            )
            .instrument(span)
            .await
        );
    });
    Ok(())
}

/// The error a stream ends with when its direct path failed.
#[cfg(feature = "p2p")]
fn direct_failed(source: std::io::Error) -> super::error::Error {
    super::error::Error::DirectSetup { source }
}

/// A subscribed relay stream, with what the relay answered about it.
pub(crate) struct Subscribed {
    pub(crate) stream: RelayStream,
    /// The data codec, when the subscription asked for one.
    pub(crate) codec: Option<DataCodec>,
    /// The ids the relay gave this end and the serving one.
    pub(crate) client_id: u32,
    pub(crate) server_id: u32,
}

/// What a local stream subscribes to over the relay, and how. Not generic:
/// the local stream type contributes only whether it offers data protocol v2.
struct Subscription<'a> {
    key: &'a str,
    remote_addr: &'a RelayEndpoint,
    keep_alive: bool,
    namespace: Option<u64>,
    credential: Credential,
    offered_v2: bool,
}

impl Subscription<'_> {
    /// Subscribe, retrying the failures a fresh attempt can fix until
    /// `deadline`, which is `budget` after the stream's setup began. Answers
    /// the subscription and how long its successful attempt took. Past the
    /// deadline it fails with the last attempt's own failure as the cause, if
    /// one failed by itself rather than by running out of time.
    async fn with_retry(
        &self,
        setup: &StreamSetup,
        deadline: Instant,
        budget: std::time::Duration,
    ) -> Result<(Subscribed, std::time::Duration)> {
        let mut wake = self.remote_addr.wake();
        let mut retry = pb_mapper_core::timeout::RetryBackoff::default().with_jitter();
        let mut last_failure = None;
        loop {
            let attempt_started = Instant::now();
            let attempt_budget = setup.timing.timeout().max(MIN_SUBSCRIBE_ATTEMPT);
            let attempt_deadline = (Instant::now() + attempt_budget).min(deadline);
            wake.acknowledge();
            let mut carrier = None;
            let result = tokio::select! {
                () = wake.network_changed(), if Instant::now() < deadline => continue,
                result = tokio::time::timeout_at(attempt_deadline, self.attempt(&mut carrier)) => result,
            };
            match result {
                Ok(Ok(ready)) => return Ok((ready, attempt_started.elapsed())),
                Ok(Err(error)) => {
                    if error.remote_retryable().is_some() {
                        self.remote_addr.protocol_succeeded();
                    } else {
                        self.remote_addr.transport_failed(carrier);
                    }
                    if !error.subscribe_retryable() || Instant::now() >= deadline {
                        return Err(error);
                    }
                    last_failure = Some(error);
                }
                Err(_) => {
                    self.remote_addr.transport_failed(carrier);
                    setup
                        .diagnostics
                        .failed(RecoveryFailure::Timeout, std::time::Duration::ZERO);
                    setup.timing.timed_out();
                    if Instant::now() >= deadline {
                        return Err(match last_failure {
                            Some(source) => super::error::Error::SubscribeTimedOut {
                                timeout: budget,
                                source: Box::new(source),
                            },
                            None => super::error::Error::ControlIoTimeout {
                                action: "subscribe",
                                timeout: budget,
                            },
                        });
                    }
                }
            }
            tokio::select! {
                () = tokio::time::sleep_until((Instant::now() + retry.next_delay()).min(deadline)) => {},
                _ = wake.changed() => retry.reset(),
            }
        }
    }

    /// One subscription attempt, over the carrier it reports in `carrier`.
    async fn attempt(&self, carrier: &mut Option<CarrierId>) -> Result<Subscribed> {
        let request = match self.namespace {
            Some(namespace) => PbConnRequest::SubcribeScoped {
                data_protocol: self.offered_v2.then_some(DATA_PROTOCOL_V2),
                key: self.key.to_string(),
                namespace,
            },
            None => PbConnRequest::Subcribe {
                data_protocol: self.offered_v2.then_some(DATA_PROTOCOL_V2),
                key: self.key.to_string(),
            },
        };
        subscribe_request(
            request,
            self.remote_addr.clone(),
            self.keep_alive,
            self.credential,
            self.offered_v2,
            carrier,
        )
        .await
    }
}

pub(crate) async fn subscribe_request(
    request: PbConnRequest,
    remote_addr: RelayEndpoint,
    keep_alive: bool,
    credential: Credential,
    offered_v2: bool,
    carrier: &mut Option<CarrierId>,
) -> Result<Subscribed> {
    let mut remote_stream = remote_addr
        .connect(credential, carrier)
        .await
        .map_err(|error| match error {
            crate::relay::ConnectError::Transport { source } => {
                super::error::Error::ConnectRemoteStream { source }
            }
            crate::relay::ConnectError::Rejected { response } => {
                super::error::Error::SubscribeRemoteError {
                    code: response.code,
                    message: response.message,
                    retryable: response.retryable,
                }
            }
        })?;

    snafu_error_handle!(remote_stream.configure(keep_alive));

    // start subcribe
    let (codec_key, client_id, server_id) = {
        let timeout = control_io_timeout();
        // handle request
        let msg = request.encode().context(EncodeSubcribeReqSnafu)?;
        let session = ClientHeaderSession::new_v2(&credential)
            .context(CreateHeaderToolSnafu { action: "session" })?;
        let response = session
            .exchange(&mut remote_stream, &msg, timeout)
            .await
            .context(WriteSubcribeReqSnafu)?;
        let resp = PbConnResponse::decode(&response).context(DecodeSubcribeRespSnafu)?;
        match resp {
            PbConnResponse::Subcribe {
                data_protocol,
                codec_key,
                client_id,
                server_id,
            } => (
                DataCodec::from_response(codec_key, data_protocol, offered_v2).context(
                    CreateHeaderToolSnafu {
                        action: "data codec",
                    },
                )?,
                client_id,
                server_id,
            ),
            PbConnResponse::Error(error) => super::error::SubscribeRemoteSnafu {
                code: error.code,
                message: error.message,
                retryable: error.retryable,
            }
            .fail()?,
            resp => SubcribeRespNotMatchSnafu {
                resp: format!("{resp:?}"),
            }
            .fail()?,
        }
    };
    Ok(Subscribed {
        stream: remote_stream,
        codec: codec_key,
        client_id,
        server_id,
    })
}

#[cfg(all(test, feature = "p2p"))]
mod tests {
    use super::*;

    /// A direct path that failed on a lost connection keeps why it was lost
    /// in the chain its stream's failure is logged with, not only the io
    /// error's own text, "connection lost".
    #[test]
    fn a_failed_direct_path_keeps_its_cause() {
        let lost = std::io::Error::from(quinn::ReadError::ConnectionLost(
            quinn::ConnectionError::TimedOut,
        ));
        let error = direct_failed(lost);
        let mut causes =
            std::iter::successors(std::error::Error::source(&error), |cause| cause.source());
        assert!(
            causes.any(|cause| cause.downcast_ref::<quinn::ConnectionError>().is_some()),
            "{}",
            snafu::Report::from_error(error)
        );
    }
}
