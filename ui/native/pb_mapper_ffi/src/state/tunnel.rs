//! A tunnel this computer runs: how one is set up, the SDK handle that runs
//! it, and what the list shows of it.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use parking_lot::Mutex as SyncMutex;
use tokio::sync::Mutex;
use uni_stream::stream::{ListenerProvider, TcpListenerProvider, UdpListenerProvider};

use pb_mapper_client::sdk::{
    Client, ConnectOptions, ConnectRequest, Connection, Error as SdkError, PathPolicy,
    RecoveryFailure, RegisterOptions, RegisterRequest, Registration, RelayTransport, Transport,
    TunnelDiagnostics, TunnelStatus,
};
use pb_mapper_core::checksum::{Credential, get_process_credential};
use pb_mapper_core::config::{ResolvedAddrs, resolve_addrs_async, resolve_pb_mapper_server_async};

use super::{DirectOptions, PathInfo, PbMapperState};
use crate::ctl::Origin;
use crate::error::{CtlError, no_quic, sdk_error, sdk_refusal};
use crate::events;

/// Whether the relay can carry a tunnel now, asked before one starts so that
/// a relay it cannot use is refused with its reason instead of retried: one
/// that cannot be reached, refuses this credential for good, or over QUIC,
/// offers no QUIC. Anything that may clear by itself — a slow answer, a
/// refusal the relay calls retryable — is left to the tunnel's own retries.
///
/// One authenticated question over TCP, the one QUIC discovery asks. A bare
/// connect would prove less, and the relay logs each as a failed login.
async fn preflight(client: &Client) -> Result<(), CtlError> {
    match (client.relay_transport(), client.relay_quic_port().await) {
        (_, Err(error)) if refuses_for_now(&error) => Err(sdk_error(error)),
        (_, Err(error)) => {
            tracing::debug!("Starting a tunnel the relay could not be asked about: {error}");
            Ok(())
        }
        (RelayTransport::Quic, Ok(None)) => Err(no_quic()),
        (RelayTransport::Tcp, Ok(_)) | (RelayTransport::Quic, Ok(Some(_))) => Ok(()),
    }
}

/// Whether `error`, the relay's answer to [`preflight`], refuses a tunnel
/// now: the relay cannot be reached, its address does not resolve, or it
/// refuses this credential for good. Everything else may clear by itself.
fn refuses_for_now(error: &SdkError) -> bool {
    match error {
        SdkError::Connect { .. } | SdkError::Address { .. } => true,
        SdkError::Remote { retryable, .. } => !retryable,
        SdkError::InvalidConfig { .. }
        | SdkError::NotAdministrator
        | SdkError::Protocol { .. }
        | SdkError::ReadyTimeout { .. }
        | SdkError::TimedOut { .. }
        | SdkError::TunnelFailed { .. }
        | SdkError::Stopped
        | SdkError::Status { .. }
        | SdkError::Io { .. }
        | SdkError::AuthFile { .. }
        | SdkError::RootRotationUncertain { .. } => false,
    }
}

/// Holds a service key for the duration of one setup, and releases it however
/// that setup ends.
///
/// Setting a tunnel up runs its slow parts — DNS, the preflight connect — with
/// the state lock released. That reopens a window the old always-locked version
/// closed by accident: two callers, say the window and a terminal, could both
/// get through the preflight and both insert, and the second would overwrite
/// the first's [`JoinHandle`], leaving a tunnel running that nothing could
/// abort. Holding the key across the gap is what closes it again.
///
/// The set is behind a blocking mutex rather than tokio's so that `Drop` can
/// release it; it is only ever held for a set insert or remove.
pub(super) struct KeyClaim {
    key: String,
    claims: Arc<SyncMutex<HashSet<String>>>,
}

impl Drop for KeyClaim {
    fn drop(&mut self) {
        self.claims.lock().remove(&self.key);
    }
}

/// Claims `key`, or reports that someone else is already setting it up.
pub(super) fn claim_key(
    claims: &Arc<SyncMutex<HashSet<String>>>,
    key: &str,
    what: &str,
) -> Result<KeyClaim, CtlError> {
    let mut guard = claims.lock();
    if !guard.insert(key.to_string()) {
        return Err(CtlError::already_in_progress(format!(
            "'{key}' is already {what}"
        )));
    }
    Ok(KeyClaim {
        key: key.to_string(),
        claims: claims.clone(),
    })
}

#[derive(Clone)]
pub(super) struct PinnedTunnel {
    pub(super) credential: Credential,
    pub(super) endpoint: ResolvedAddrs,
    /// The relay address it was started against, as configured then.
    pub(super) relay: String,
    /// The namespace a connection subscribes in, when not its credential's own.
    pub(super) namespace: Option<u64>,
    /// Whether it may go direct: only then does LAN-first reach it.
    pub(super) allow_direct: bool,
}

impl PinnedTunnel {
    /// The namespace the relay routes this tunnel in.
    pub(super) fn routed_namespace(&self) -> u64 {
        self.namespace.unwrap_or_else(|| self.credential.key_id())
    }

    /// Whether it runs on `relay`: started under that address, or dialling
    /// any address `endpoint` resolves to, since the same relay may have been
    /// written another way (a hostname, its IP) when it started.
    pub(super) fn is_on(&self, relay: &str, endpoint: Option<&ResolvedAddrs>) -> bool {
        self.relay == relay
            || endpoint.is_some_and(|endpoint| {
                self.endpoint
                    .as_slice()
                    .iter()
                    .any(|addr| endpoint.as_slice().contains(addr))
            })
    }
}

/// A running tunnel, as the SDK runs it for the CLI too. Dropping it stops
/// the tunnel.
pub(crate) enum TunnelHandle {
    Share(Registration),
    Connect(Connection),
}

impl TunnelHandle {
    fn status(&self) -> TunnelStatus {
        match self {
            Self::Share(handle) => handle.status(),
            Self::Connect(handle) => handle.status(),
        }
    }

    pub(crate) fn diagnostics(&self) -> TunnelDiagnostics {
        match self {
            Self::Share(handle) => handle.diagnostics(),
            Self::Connect(handle) => handle.diagnostics(),
        }
    }

    pub(crate) fn subscribe_events(&self) -> pb_mapper_client::sdk::TunnelEvents {
        match self {
            Self::Share(handle) => handle.subscribe_events(),
            Self::Connect(handle) => handle.subscribe_events(),
        }
    }

    pub(crate) fn metrics(&self) -> pb_mapper_client::sdk::TunnelMetrics {
        match self {
            Self::Share(handle) => handle.metrics(),
            Self::Connect(handle) => handle.metrics(),
        }
    }

    pub(crate) fn subscribe_metrics(
        &self,
        every: std::time::Duration,
    ) -> pb_mapper_client::sdk::TunnelMetricsStream {
        match self {
            Self::Share(handle) => handle.subscribe_metrics(every),
            Self::Connect(handle) => handle.subscribe_metrics(every),
        }
    }

    pub(super) fn subscribe(&self) -> tokio::sync::watch::Receiver<TunnelStatus> {
        match self {
            Self::Share(handle) => handle.subscribe(),
            Self::Connect(handle) => handle.subscribe(),
        }
    }

    /// Its state as the list shows it, and how its sessions travel.
    pub(super) fn view(&self) -> TunnelView {
        let diagnostics = self.diagnostics();
        let transport = diagnostics.relay_transport;
        let (status, message) = match self.status() {
            TunnelStatus::Connected => ("running", String::new()),
            // Still dialling, or failed and trying again: "failed" is what a
            // tunnel whose last attempt failed has always reported, while its
            // retry loop runs on.
            TunnelStatus::Starting | TunnelStatus::Retrying => {
                match diagnostics
                    .last_failure
                    .filter(|_| diagnostics.consecutive_failures > 0)
                {
                    Some(failure) => ("failed", failure_message(failure, self, transport)),
                    None => ("retrying", "Connecting to the relay".to_string()),
                }
            }
            TunnelStatus::Failed(reason) => ("failed", reason),
            // Nothing here stops a tunnel but removing it, so a stopped one
            // that is still held ended on its own.
            TunnelStatus::Stopped => ("failed", "The tunnel stopped".to_string()),
        };
        TunnelView {
            status,
            message,
            path: PathInfo::of(&diagnostics),
            transport: Some(transport),
        }
    }
}

/// What the relay's last answer, or the lack of one, means for a tunnel.
/// Over QUIC the relay is reached on a UDP port of its own, which a network
/// can block while TCP gets through, so that is named.
fn failure_message(
    failure: RecoveryFailure,
    handle: &TunnelHandle,
    transport: RelayTransport,
) -> String {
    let quic = transport == RelayTransport::Quic;
    match failure {
        RecoveryFailure::Dns => "The relay's address does not resolve",
        RecoveryFailure::Transport if quic => {
            "Cannot reach the relay over QUIC; check that it offers QUIC and its UDP port is open"
        }
        RecoveryFailure::Transport => "Cannot reach the relay",
        RecoveryFailure::Timeout if quic => "The relay did not answer over QUIC in time",
        RecoveryFailure::Timeout => "The relay did not answer in time",
        RecoveryFailure::ServiceUnavailable => match handle {
            TunnelHandle::Share(_) => "The relay cannot take this service now",
            TunnelHandle::Connect(_) => "The relay does not list this service",
        },
        RecoveryFailure::Rejected => "The relay refused this computer's credential",
        RecoveryFailure::NetworkChanged => "The network changed; reconnecting",
    }
    .to_string()
}

/// A tunnel's state as the list shows it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) struct TunnelView {
    pub(super) status: &'static str,
    pub(super) message: String,
    pub(super) path: Option<PathInfo>,
    pub(super) transport: Option<RelayTransport>,
}

impl TunnelView {
    /// The view of a tunnel this computer has configured but is not running.
    pub(super) fn stopped(message: &str) -> Self {
        Self {
            status: "stopped",
            message: message.to_string(),
            path: None,
            transport: None,
        }
    }
}

pub(super) struct TunnelRuntime {
    /// Shared only with the task that reports its changes, which holds it
    /// weakly: removing the runtime still stops the tunnel.
    pub(super) handle: Arc<TunnelHandle>,
    pub(super) pin: PinnedTunnel,
}

/// Stops the tunnel under `key`, if there is one: dropping its handle does,
/// since the SDK's `LiveTunnel` cancels and aborts its worker on drop.
pub(super) fn abort_runtime(map: &mut HashMap<String, TunnelRuntime>, key: &str) -> bool {
    map.remove(key).is_some()
}

/// A share as its caller asks for it, and as it is saved.
pub struct ShareSpec {
    pub service_key: String,
    /// The local service it exposes, as `ip:port`.
    pub local_address: String,
    /// `TCP`, or anything else for UDP.
    pub protocol: String,
    pub enable_encryption: bool,
    pub enable_keep_alive: bool,
    pub direct: DirectOptions,
}

/// A connection as its caller asks for it, and as it is saved.
pub struct ConnectSpec {
    pub service_key: String,
    /// Where it listens locally, as `ip:port`.
    pub local_address: String,
    /// `TCP`, or anything else for UDP.
    pub protocol: String,
    pub enable_keep_alive: bool,
    /// Whose service it is when that is not this credential's own, which
    /// only the administrator key may ask for.
    pub namespace: Option<u64>,
    pub direct: DirectOptions,
}

/// Everything [`PbMapperState::finish_register`] needs once the slow work is
/// done, the tunnel itself included: it starts before the commit, and stops
/// again if the commit fails.
pub(super) struct RegisterCommit {
    pub(super) spec: ShareSpec,
    pub(super) handle: Registration,
    pub(super) pin: PinnedTunnel,
}

/// Everything [`PbMapperState::finish_connect`] needs once the slow work is
/// done. See [`RegisterCommit`].
pub(super) struct ConnectCommit {
    pub(super) spec: ConnectSpec,
    pub(super) handle: Connection,
    pub(super) pin: PinnedTunnel,
}

/// The transport `protocol` names: TCP, or else UDP.
fn transport_of(protocol: &str) -> Transport {
    if protocol.eq_ignore_ascii_case("TCP") {
        Transport::Tcp
    } else {
        Transport::Udp
    }
}

/// Registers a service, holding the state lock only for the bookkeeping.
///
/// Three phases. The middle one is the point: resolving addresses and dialling
/// the pb-mapper server are unbounded — a blackholed address costs the whole
/// connect timeout — and while they ran under the lock every other caller waited
/// them out, including a status read that wanted nothing from this key at all.
pub async fn register_service(
    state: &Arc<Mutex<PbMapperState>>,
    spec: ShareSpec,
) -> Result<(), CtlError> {
    // 1. Claim the key and take what the slow work needs. Microseconds.
    //    `_claim` is held to the end of the function on purpose: dropping it
    //    early would release the key while the setup is still running.
    //    The credential and carrier are captured with the relay address so a
    //    later config change cannot pair a new key with the already-resolved
    //    socket; LAN-first with them, as this computer's config says now.
    let (_claim, server_address, credential, relay_transport, lan_first) = {
        let state = state.lock().await;
        let claim = state.claim_registering(&spec.service_key)?;
        let credential = get_process_credential().map_err(CtlError::invalid_argument)?;
        (
            claim,
            state.config.server_address.clone(),
            credential,
            state.config.relay_transport,
            state.config.lan_first,
        )
    };

    // 2. The slow parts, with the lock released, ending with the tunnel
    //    started.
    resolve_addrs_async(&spec.local_address)
        .await
        .map_err(|e| CtlError::invalid_address(format!("Invalid local address: {e}")))?;
    let remote_addrs = resolve_pb_mapper_server_async(Some(&server_address))
        .await
        .map_err(|e| CtlError::invalid_address(format!("Invalid server address: {e}")))?;
    let client = Client::from_credential(&server_address, credential, spec.enable_keep_alive, None)
        .with_relay_transport(relay_transport)
        .map_err(sdk_refusal)?;
    // Surface a relay this tunnel cannot use now, rather than retrying it.
    preflight(&client).await?;
    let handle = client
        .register_with_options(
            RegisterRequest {
                key: spec.service_key.clone(),
                local_addr: spec.local_address.clone(),
                transport: transport_of(&spec.protocol),
                codec: spec.enable_encryption,
                force_namespace: false,
            },
            RegisterOptions {
                allow_direct: spec.direct.allow_direct,
                idle_timeout: None,
                port_mapping: spec.direct.port_mapping,
                lan_first,
                ..Default::default()
            },
        )
        .await
        .map_err(sdk_refusal)?;

    // 3. Commit. Microseconds again.
    let pin = PinnedTunnel {
        credential,
        endpoint: remote_addrs,
        relay: server_address,
        namespace: None,
        allow_direct: spec.direct.allow_direct,
    };
    state
        .lock()
        .await
        .finish_register(RegisterCommit { spec, handle, pin })
}

/// Connects to a registered service. Phased exactly like [`register_service`];
/// the slow step here is the local bind that proves the port is free. With
/// `allow_direct` each session tries a direct path first, and the relay when
/// that fails.
pub async fn connect_service(
    state: &Arc<Mutex<PbMapperState>>,
    spec: ConnectSpec,
) -> Result<(), CtlError> {
    let (_claim, server_address, credential, relay_transport, lan_first, lan_transport) = {
        let state = state.lock().await;
        let claim = state.claim_connecting(&spec.service_key)?;
        let credential = get_process_credential().map_err(CtlError::invalid_argument)?;
        (
            claim,
            state.config.server_address.clone(),
            credential,
            state.config.relay_transport,
            state.config.lan_first,
            state.config.lan_transport,
        )
    };

    let local_addrs = resolve_addrs_async(&spec.local_address)
        .await
        .map_err(|e| CtlError::invalid_address(format!("Invalid local address: {e}")))?;
    let remote_addrs = resolve_pb_mapper_server_async(Some(&server_address))
        .await
        .map_err(|e| CtlError::invalid_address(format!("Invalid server address: {e}")))?;

    // Preflight local bind to detect "port already in use" before starting client.
    let transport = transport_of(&spec.protocol);
    let bound = match transport {
        Transport::Tcp => TcpListenerProvider::bind(local_addrs.as_slice())
            .await
            .map(drop),
        Transport::Udp => UdpListenerProvider::bind(local_addrs.as_slice())
            .await
            .map(drop),
    };
    bound.map_err(|e| {
        CtlError::address_in_use(format!(
            "Failed to bind local address {}: {e}",
            spec.local_address
        ))
    })?;
    let client = Client::from_credential(
        &server_address,
        credential,
        spec.enable_keep_alive,
        spec.namespace,
    )
    .with_relay_transport(relay_transport)
    .map_err(sdk_refusal)?;
    // A relay that is down is retried, as it always was: the connection
    // listens locally meanwhile. One that offers no QUIC never will.
    if relay_transport == RelayTransport::Quic {
        preflight(&client).await?;
    }
    let handle = client
        .connect_with_options(
            ConnectRequest {
                key: spec.service_key.clone(),
                local_addr: spec.local_address.clone(),
                transport,
            },
            ConnectOptions {
                path: if spec.direct.allow_direct {
                    PathPolicy::Auto
                } else {
                    PathPolicy::RelayOnly
                },
                port_mapping: spec.direct.port_mapping,
                lan_first,
                lan_transport,
            },
        )
        .await
        .map_err(sdk_refusal)?;

    let pin = PinnedTunnel {
        credential,
        endpoint: remote_addrs,
        relay: server_address,
        namespace: spec.namespace,
        allow_direct: spec.direct.allow_direct,
    };
    state
        .lock()
        .await
        .finish_connect(ConnectCommit { spec, handle, pin })
}

/// Tell the window when `handle`'s state or path changes, until the tunnel
/// ends. Both are read when its status or one of its events says something
/// may have changed, and announced only when they did: which paths its
/// sessions use, not how many, and not the message, which for a failing
/// tunnel says when it tries again. Holds the handle weakly, so it never
/// keeps a tunnel alive.
pub(super) fn watch_tunnel(handle: &Arc<TunnelHandle>, kind: events::ChangeKind, key: &str) {
    let tunnel = Arc::downgrade(handle);
    let mut status = handle.subscribe();
    let mut events = handle.subscribe_events();
    let key = key.to_string();
    let shape = |view: TunnelView| (view.status, view.path.map(|path| path.shape()));
    let mut seen = shape(handle.view());
    tokio::spawn(async move {
        let mut status_open = true;
        loop {
            let live = tokio::select! {
                changed = status.changed(), if status_open => {
                    status_open = changed.is_ok();
                    true
                },
                // A missed event may have been a change too: read it all again.
                event = events.recv() => event.is_some(),
            };
            let Some(handle) = tunnel.upgrade() else {
                return;
            };
            let now = shape(handle.view());
            drop(handle);
            if std::mem::replace(&mut seen, now) != now {
                events::emit(kind, Some(&key), Origin::Internal);
            }
            if !live {
                return;
            }
        }
    });
}
