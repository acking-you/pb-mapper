//! Direct-only sessions: [`DirectDialer`] and [`Client::connect_tcp_direct`],
//! for a caller that keeps a relayed session and wants a direct one beside
//! it. Not part of the supported API.

use std::sync::Arc;
use std::time::Duration;

use snafu::Snafu;
use tokio::net::TcpStream;
use uni_stream::stream::TcpStreamImpl;

use super::handle::Connection;
use super::worker::TunnelWorker;
use super::{Client, CongestionControl, ConnectOptions, Error, FallbackReason};
use crate::diagnostics::Diagnostics;
use crate::direct::{Consumer, DirectOnly, PauseScope};
use crate::endpoint::RelayEndpoint;
use crate::worker_status::WorkerStatus;

/// What one direct-only setup may take, at most: as long as a session's
/// setup with direct first may.
const SETUP_BUDGET: Duration = Duration::from_secs(8);

/// Why [`Client::connect_tcp_direct`] set up no direct session. None of them
/// took the relay. Not part of the supported API.
#[doc(hidden)]
#[derive(Debug, Snafu)]
#[snafu(visibility(pub(crate)))]
pub enum DirectOnlyError {
    /// Direct is paused, after a recent fallback to this service, until
    /// `until`, or until the network changes ([`DirectDialer::wait`]).
    #[snafu(display("direct is paused for this service"))]
    Paused { until: std::time::Instant },
    /// The relay or the publisher offers no direct: trying again is no use.
    #[snafu(display("direct is not offered for this service"))]
    NotOffered,
    /// The setup missed for `reason`. When that says the path will not punch,
    /// direct is paused until `until`, as after a fallback.
    #[snafu(display("direct setup failed: {}", reason.as_str()))]
    Failed {
        reason: FallbackReason,
        until: Option<std::time::Instant>,
    },
    /// The setup took longer than its budget.
    #[snafu(display("direct setup timed out after {budget:?}"))]
    TimedOut { budget: Duration },
    #[snafu(display("direct setup failed: {source}"))]
    Setup { source: std::io::Error },
    /// The tunnel that would carry the session could not start.
    #[snafu(display("{source}"))]
    Tunnel { source: Error },
}

/// Direct-only setups to one service, for a caller that keeps a relayed
/// session and tries for a direct one beside it from time to time. Every
/// setup reuses one consumer and one direct endpoint, and goes by the
/// pauses this client's `connect_tcp` calls to the service go by: a setup
/// that says the path will not punch pauses them, and a direct session
/// clears them. Its attempts are its own, in diagnostics of its own: none
/// counts as a fallback, and none takes the relay. Not part of the
/// supported API.
#[doc(hidden)]
pub struct DirectDialer {
    key: Arc<str>,
    pub(crate) consumer: Arc<Consumer>,
    endpoint: RelayEndpoint,
}

impl DirectDialer {
    /// Wait until `until`, or until the network changes, which clears the
    /// pauses: what to wait after [`DirectOnlyError::Paused`] or a failure.
    pub async fn wait(&self, until: std::time::Instant) {
        let mut wake = self.endpoint.wake();
        wake.acknowledge();
        tokio::select! {
            () = tokio::time::sleep_until(until.into()) => {}
            () = wake.network_changed() => {}
        }
    }
}

impl Client {
    /// Set up direct-only sessions to `key`: [`Self::connect_tcp_direct`].
    /// Fails without `p2p` or for an empty key. `options.path` is ignored.
    /// Not part of the supported API.
    #[doc(hidden)]
    pub fn direct_dialer(
        &self,
        key: impl Into<String>,
        options: ConnectOptions,
    ) -> Result<DirectDialer, Error> {
        let key: Arc<str> = Arc::from(key.into());
        if key.trim().is_empty() {
            return Err(Error::invalid_config("service key is required"));
        }
        options.port_mapping.check_supported()?;
        let endpoint = self.inner.endpoint.clone();
        endpoint.start();
        let pauses = self.inner.direct_memory.lock().pauses(
            &key,
            tokio::time::Instant::now(),
            endpoint.network_generation(),
        );
        let policy = crate::direct_policy::DirectPolicy {
            port_mapping: if cfg!(feature = "port-mapping") {
                options.port_mapping
            } else {
                super::PortMapping::Off
            },
            lan_first: options.lan_first,
            lan_transport: options.lan_transport,
            lan_tcp: false,
            congestion: CongestionControl::Cubic,
        };
        let consumer = Consumer::new(
            endpoint.clone(),
            self.credential(),
            self.inner.namespace,
            false,
            self.inner.keep_alive,
            Diagnostics::default(),
            policy,
            PauseScope::Client(pauses),
        );
        Ok(DirectDialer {
            key,
            consumer,
            endpoint,
        })
    }

    /// One TCP session to `dialer`'s service over a direct path, or none:
    /// never the relay. It returns once the session committed, with the
    /// stream and the connection that carries it, whose metrics count that
    /// session; keep the connection while the stream is used. Not part of
    /// the supported API.
    #[doc(hidden)]
    pub async fn connect_tcp_direct(
        &self,
        dialer: &DirectDialer,
    ) -> Result<(TcpStream, Connection), DirectOnlyError> {
        let started = tokio::time::Instant::now();
        let deadline = started + SETUP_BUDGET;
        let timed_out = || DirectOnlyError::TimedOut {
            budget: SETUP_BUDGET,
        };
        // Setups share the process's admission with every other tunnel's.
        let _admitted =
            tokio::time::timeout_at(deadline, self.inner.endpoint.data_slots().acquire_owned())
                .await
                .map_err(|_| timed_out())?
                .map_err(|_| timed_out())?;
        let prepared = tokio::time::timeout_at(
            deadline,
            dialer.consumer.prepare_direct(&dialer.key, deadline),
        )
        .await
        .map_err(|_| timed_out())?
        .map_err(|missed| match missed {
            DirectOnly::Paused(until) => DirectOnlyError::Paused {
                until: until.into_std(),
            },
            DirectOnly::NotOffered => DirectOnlyError::NotOffered,
            DirectOnly::Failed { reason, until } => DirectOnlyError::Failed {
                reason,
                until: until.map(tokio::time::Instant::into_std),
            },
            DirectOnly::Io(source) => DirectOnlyError::Setup { source },
        })?;
        let (stream, accepted) = super::client::loopback_pair()
            .await
            .map_err(|source| DirectOnlyError::Setup { source })?;
        let worker = TunnelWorker::prepare(&self.inner.endpoint, &dialer.key, "127.0.0.1:0")
            .await
            .map_err(|source| DirectOnlyError::Tunnel { source })?;
        // Noted in its diagnostics as any direct-first tunnel's are.
        let _ = worker.direct_policy(
            true,
            super::PortMapping::Off,
            false,
            super::LanTransport::Quic,
            false,
            CongestionControl::Cubic,
        );
        let credential = self.credential();
        let handle = worker.spawn(move |context| async move {
            context.diagnostics.succeeded(started.elapsed());
            (context.status)(&WorkerStatus::Connected);
            let forward = prepared.forward_on(
                TcpStreamImpl::new(accepted),
                credential,
                &context.diagnostics,
            );
            tokio::select! {
                biased;
                () = context.shutdown.cancelled() => {}
                forwarded = forward => if let Err(error) = forwarded {
                    let failure = crate::client::error::with_causes(&error);
                    (context.status)(&WorkerStatus::Failed(Some(failure)));
                }
            }
        });
        Ok((stream, Connection::new(handle, dialer.key.to_string())))
    }
}
