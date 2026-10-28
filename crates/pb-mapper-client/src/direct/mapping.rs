//! Optional gateway leases belong to the UDP endpoint, never its coordinator.
use crate::network::NetworkHints;
use std::{net::SocketAddr, num::NonZeroU16, sync::Arc, time::Duration};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

pub(super) struct Mapping {
    address: watch::Receiver<(u64, Option<SocketAddr>)>,
    network: Arc<NetworkHints>,
    stop: CancellationToken,
    done: CancellationToken,
}

impl Mapping {
    pub fn new(port: NonZeroU16, network: Arc<NetworkHints>) -> Self {
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
            run(port, changes, address_tx, shutdown).await;
        });
        Self {
            address,
            network,
            stop,
            done,
        }
    }

    pub fn address(&self) -> Option<SocketAddr> {
        let (generation, address) = *self.address.borrow();
        (generation == self.network.generation())
            .then_some(address)
            .flatten()
    }

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
) {
    loop {
        let generation = *changes.borrow_and_update();
        let client = portmapper::Client::default();
        let mut external = client.watch_external_address();
        // Discover supported protocols before mapping so the library can
        // prefer PCP/NAT-PMP to UPnP. No connection setup waits for this task.
        tokio::select! {
            biased;
            () = stop.cancelled() => return,
            _ = changes.changed() => continue,
            _ = tokio::time::timeout(Duration::from_secs(2), client.probe()) => {}
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
                _ = retry.tick() => client.procure_mapping(),
            }
        }
        address.send_replace((generation, None));
        client.deactivate();
        // The service handles commands in order. A subsequent probe response
        // acknowledges that deactivation (including release) was processed.
        // Merely observing address=None is insufficient: it precedes release.
        let _ = tokio::time::timeout(Duration::from_secs(2), client.probe()).await;
        if stop.is_cancelled() || changes.has_changed().is_err() {
            return;
        }
    }
}
