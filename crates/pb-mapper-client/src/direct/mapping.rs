//! A gateway lease for the direct UDP endpoint, asked for with PCP, NAT-PMP
//! or UPnP. It belongs to the endpoint, never its coordinator, and no
//! connection setup waits for it.
use crate::diagnostics::{
    Diagnostics, PortMappingProtocol, PortMappingStage, PortMappingStatus, ms,
};
use crate::network::NetworkHints;
use std::{
    net::SocketAddr,
    num::NonZeroU16,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

pub(super) struct Mapping {
    address: watch::Receiver<(u64, Option<SocketAddr>)>,
    network: Arc<NetworkHints>,
    stop: CancellationToken,
    done: CancellationToken,
}

impl Mapping {
    pub fn new(port: NonZeroU16, network: Arc<NetworkHints>, diagnostics: Diagnostics) -> Self {
        // The release is reported even when the tunnel stops first.
        let cleanup = diagnostics.defer_close();
        let (address_tx, address) = watch::channel((network.generation(), None));
        let stop = CancellationToken::new();
        let done = CancellationToken::new();
        let done_guard = done.clone().drop_guard();
        let changes = network.subscribe();
        let shutdown = stop.clone();
        // Drop signals cancellation instead of aborting: the actor gets a
        // bounded opportunity to release its lease even on an aborted setup.
        tokio::spawn(async move {
            let _done = done_guard;
            let _cleanup = cleanup;
            run(port, changes, address_tx, shutdown, diagnostics).await;
        });
        Self {
            address,
            network,
            stop,
            done,
        }
    }

    /// The public address of the current lease, if it is for this network.
    pub fn address(&self) -> Option<SocketAddr> {
        let (generation, address) = *self.address.borrow();
        (generation == self.network.generation())
            .then_some(address)
            .flatten()
    }

    /// Release the lease, waiting (at most 2 s) for the gateway.
    pub async fn shutdown(&self) {
        self.stop.cancel();
        self.done.cancelled().await;
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

async fn run(
    port: NonZeroU16,
    mut changes: watch::Receiver<u64>,
    address: watch::Sender<(u64, Option<SocketAddr>)>,
    stop: CancellationToken,
    diagnostics: Diagnostics,
) {
    loop {
        let generation = *changes.borrow_and_update();
        let reporter = Arc::new(Reporter::new(port, generation, diagnostics.clone()));
        let observer = reporter.clone();
        let client = portmapper::Client::with_observer(
            Default::default(),
            Default::default(),
            portmapper::Observer::new(move |event| observer.observe(event)),
        );
        let mut external = client.watch_external_address();
        // Discover supported protocols before mapping so the library can
        // prefer PCP/NAT-PMP to UPnP. No connection setup waits for this task.
        tokio::select! {
            biased;
            () = stop.cancelled() => {
                reporter.stage(PortMappingStage::Stopped, None);
                return;
            },
            _ = changes.changed() => {
                reporter.stage(PortMappingStage::NetworkChanged, None);
                continue;
            },
            result = tokio::time::timeout(Duration::from_secs(2), client.probe()) => {
                if result.is_err() {
                    reporter.stage(PortMappingStage::Failed, Some("gateway discovery timed out".into()));
                }
            }
        }
        client.update_local_port(port);
        let mut retry = tokio::time::interval_at(
            tokio::time::Instant::now() + Duration::from_secs(30),
            Duration::from_secs(30),
        );
        retry.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                biased;
                () = stop.cancelled() => break,
                _ = changes.changed() => break,
                result = external.changed() => {
                    if result.is_err() { break; }
                    let mapped = external.borrow_and_update().map(SocketAddr::V4);
                    tracing::debug!(?mapped, "direct gateway mapping changed");
                    address.send_replace((generation, mapped));
                }
                _ = retry.tick() => {
                    if external.borrow().is_none() {
                        reporter.stage(PortMappingStage::Retrying, None);
                        client.procure_mapping();
                    }
                },
            }
        }
        address.send_replace((generation, None));
        if !stop.is_cancelled() {
            reporter.stage(PortMappingStage::NetworkChanged, None);
        }
        if tokio::time::timeout(Duration::from_secs(2), client.deactivate_and_wait())
            .await
            .is_err()
        {
            reporter.stage(
                PortMappingStage::ReleaseFailed,
                Some("gateway cleanup timed out; lease may persist until expiry".into()),
            );
        }
        if stop.is_cancelled() || changes.has_changed().is_err() {
            reporter.stage(PortMappingStage::Stopped, None);
            return;
        }
    }
}

/// Turns what the mapper observes into the tunnel's [`PortMappingStatus`].
struct Reporter {
    current: Mutex<PortMappingStatus>,
    diagnostics: Diagnostics,
}

impl Reporter {
    fn new(port: NonZeroU16, generation: u64, diagnostics: Diagnostics) -> Self {
        let mut current = PortMappingStatus::new(PortMappingStage::WaitingForDirect, generation);
        current.local_port = Some(port.get());
        Self {
            current: Mutex::new(current),
            diagnostics,
        }
    }

    fn update(&self, change: impl FnOnce(&mut PortMappingStatus)) {
        let mut current = self.current.lock().unwrap_or_else(|e| e.into_inner());
        current.error = None;
        change(&mut current);
        self.diagnostics.port_mapping_changed(current.clone());
    }

    fn stage(&self, stage: PortMappingStage, error: Option<String>) {
        self.update(|current| {
            current.stage = stage;
            current.error = error;
            if matches!(
                stage,
                PortMappingStage::NetworkChanged
                    | PortMappingStage::Stopped
                    | PortMappingStage::ReleaseFailed
            ) {
                current.active = false;
            }
        });
    }

    fn observe(&self, event: portmapper::Event) {
        use portmapper::Event as E;
        self.update(|current| {
            current.stage = match event {
                E::Discovering => PortMappingStage::Discovering,
                E::Gateway { gateway, .. } => {
                    current.gateway = Some(gateway);
                    PortMappingStage::GatewayFound
                }
                E::Discovered(output) => {
                    current.available_protocols = [
                        (output.pcp, PortMappingProtocol::Pcp),
                        (output.nat_pmp, PortMappingProtocol::NatPmp),
                        (output.upnp, PortMappingProtocol::Upnp),
                    ]
                    .into_iter()
                    .filter_map(|(available, protocol)| available.then_some(protocol))
                    .collect();
                    PortMappingStage::Discovered
                }
                E::Requesting {
                    protocol,
                    gateway,
                    renewal,
                } => {
                    current.protocol = Some(protocol.into());
                    current.gateway = Some(gateway);
                    if renewal {
                        PortMappingStage::Renewing
                    } else {
                        PortMappingStage::Requesting
                    }
                }
                E::Active {
                    protocol,
                    external,
                    lifetime,
                } => {
                    current.protocol = Some(protocol.into());
                    current.external_addr = Some(external.into());
                    current.lease_lifetime_ms = Some(ms(lifetime));
                    current.active = true;
                    PortMappingStage::Active
                }
                E::DiscoveryFailed(error) | E::MappingFailed(error) => {
                    current.error = Some(error);
                    PortMappingStage::Failed
                }
                E::Expired(_) => {
                    current.active = false;
                    PortMappingStage::Expired
                }
                E::Releasing(_) => {
                    current.active = false;
                    PortMappingStage::Releasing
                }
                E::Released { confirmed, .. } => {
                    current.active = false;
                    if confirmed {
                        PortMappingStage::Released
                    } else {
                        PortMappingStage::ReleaseSent
                    }
                }
                E::ReleaseFailed { error, .. } => {
                    current.active = false;
                    current.error = Some(error);
                    PortMappingStage::ReleaseFailed
                }
                E::Cancelled => PortMappingStage::Cancelled,
                _ => return,
            };
        });
    }
}

impl From<portmapper::MappingProtocol> for PortMappingProtocol {
    fn from(value: portmapper::MappingProtocol) -> Self {
        match value {
            portmapper::MappingProtocol::Pcp => Self::Pcp,
            portmapper::MappingProtocol::NatPmp => Self::NatPmp,
            portmapper::MappingProtocol::Upnp => Self::Upnp,
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn renewal_failure_preserves_lease_until_expiry_and_bounds_errors() {
        let diagnostics = Diagnostics::default();
        let mut events = diagnostics.subscribe();
        let reporter = Reporter::new(NonZeroU16::new(12345).unwrap(), 7, diagnostics);
        let external = "203.0.113.7:40000".parse().unwrap();
        reporter.observe(portmapper::Event::Active {
            protocol: portmapper::MappingProtocol::Pcp,
            external,
            lifetime: Duration::from_secs(60),
        });
        reporter.observe(portmapper::Event::MappingFailed("x".repeat(1000)));
        reporter.observe(portmapper::Event::Expired(external));
        reporter.observe(portmapper::Event::Released {
            external,
            confirmed: false,
        });
        let mut mapping = Vec::new();
        for _ in 0..4 {
            if let crate::diagnostics::TunnelEventKind::PortMappingChanged { mapping: event } =
                events.recv().await.unwrap().unwrap().kind
            {
                mapping.push(event);
            }
        }
        assert!(mapping[0].active && mapping[1].active);
        assert_eq!(mapping[1].error.as_ref().unwrap().len(), 256);
        assert!(!mapping[2].active);
        assert_eq!(mapping[3].stage, PortMappingStage::ReleaseSent);
        assert!(
            mapping
                .iter()
                .all(|m| m.network_generation == 7 && m.local_port == Some(12345))
        );
    }
}
