//! Shared relay resolution and bounded setup admission; no established data is owned here.

use crate::network::NetworkHints;
use pb_mapper_core::config::ResolvedAddrs;
use std::{
    collections::HashMap,
    net::{SocketAddr, ToSocketAddrs},
    sync::{
        Arc, LazyLock, Mutex as StdMutex, Weak,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::{
    sync::{Mutex, OwnedSemaphorePermit, Semaphore, watch},
    time::{Duration, Instant},
};
use tokio_util::task::AbortOnDropHandle;

const DNS_TTL: Duration = Duration::from_secs(60);
const DNS_RETRY: Duration = Duration::from_secs(2);
const DNS_WAIT: Duration = Duration::from_secs(2);
pub(crate) const CONTROL_SETUPS: usize = 8;
pub(crate) const DATA_SETUPS: usize = 64;

type Lookup = AbortOnDropHandle<std::io::Result<ResolvedAddrs>>;

struct Cache {
    addresses: Option<ResolvedAddrs>,
    resolved_at: Option<Instant>,
    attempted_at: Option<Instant>,
    generation: u64,
    lookup: Option<Lookup>,
    lookup_generation: u64,
}

struct RelayState {
    name: String,
    fixed: bool,
    cache: Mutex<Cache>,
    refresh: AtomicBool,
    failed: AtomicBool,
    success: watch::Sender<u64>,
    network: Arc<NetworkHints>,
    control_slots: Arc<Semaphore>,
    data_slots: Arc<Semaphore>,
    #[cfg(test)]
    resolver: Option<TestResolver>,
}

#[cfg(test)]
type TestResolver = Arc<
    dyn Fn() -> std::pin::Pin<
            Box<dyn std::future::Future<Output = std::io::Result<ResolvedAddrs>> + Send>,
        > + Send
        + Sync,
>;

#[derive(Clone)]
pub(crate) struct RelayEndpoint(Arc<RelayState>);

impl std::fmt::Debug for RelayEndpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("RelayEndpoint").field(&self.0.name).finish()
    }
}
impl std::fmt::Display for RelayEndpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0.name)
    }
}

impl RelayEndpoint {
    pub(crate) fn validate(&self) -> bool {
        if self.0.fixed {
            return true;
        }
        self.0.name.rsplit_once(':').is_some_and(|(host, port)| {
            !host.is_empty()
                && !host
                    .chars()
                    .any(|c| c.is_whitespace() || matches!(c, '/' | '[' | ']' | ':'))
                && port.parse::<u16>().is_ok()
        })
    }
    pub(crate) fn shared(name: &str) -> Self {
        static RELAYS: LazyLock<StdMutex<HashMap<String, Weak<RelayState>>>> =
            LazyLock::new(|| StdMutex::new(HashMap::new()));
        let mut relays = RELAYS.lock().unwrap_or_else(|e| e.into_inner());
        relays.retain(|_, state| state.strong_count() > 0);
        if let Some(state) = relays.get(name).and_then(Weak::upgrade) {
            return Self(state);
        }
        let initial = name.parse::<SocketAddr>().ok().map(ResolvedAddrs::from);
        let relay = Self::new(name.to_string(), initial.is_some(), initial);
        relays.insert(name.to_string(), Arc::downgrade(&relay.0));
        relay
    }

    pub(crate) fn fixed(addresses: ResolvedAddrs) -> Self {
        Self::new(addresses.to_string(), true, Some(addresses))
    }

    fn new(name: String, fixed: bool, addresses: Option<ResolvedAddrs>) -> Self {
        let (success, _) = watch::channel(0);
        Self(Arc::new(RelayState {
            name,
            fixed,
            cache: Mutex::new(Cache {
                resolved_at: addresses.as_ref().map(|_| Instant::now()),
                addresses,
                attempted_at: None,
                generation: 0,
                lookup: None,
                lookup_generation: 0,
            }),
            refresh: AtomicBool::new(false),
            failed: AtomicBool::new(false),
            success,
            network: NetworkHints::shared(),
            control_slots: Arc::new(Semaphore::new(CONTROL_SETUPS)),
            data_slots: Arc::new(Semaphore::new(DATA_SETUPS)),
            #[cfg(test)]
            resolver: None,
        }))
    }

    pub(crate) fn start(&self) {
        self.0.network.start();
    }
    pub(crate) fn notify_network_change(&self) {
        self.0.network.notify();
    }
    pub(crate) fn wake(&self) -> RecoveryWake {
        RecoveryWake {
            network: self.0.network.subscribe(),
            success: self.0.success.subscribe(),
        }
    }
    pub(crate) fn network_generation(&self) -> u64 {
        self.0.network.generation()
    }
    pub(crate) fn data_slots(&self) -> Arc<Semaphore> {
        self.0.data_slots.clone()
    }
    pub(crate) fn active_setups(&self) -> (usize, usize) {
        (
            CONTROL_SETUPS - self.0.control_slots.available_permits(),
            DATA_SETUPS - self.0.data_slots.available_permits(),
        )
    }
    pub(crate) fn dns_age(&self) -> Option<Duration> {
        self.0
            .cache
            .try_lock()
            .ok()
            .and_then(|cache| cache.resolved_at.map(|time| time.elapsed()))
    }

    pub(crate) async fn control_permit(&self) -> OwnedSemaphorePermit {
        // These semaphores are never closed; endpoint owners retain them.
        self.0
            .control_slots
            .clone()
            .acquire_owned()
            .await
            .unwrap_or_else(|_| unreachable!("relay setup semaphore is never closed"))
    }

    pub(crate) fn transport_failed(&self) {
        self.0.refresh.store(true, Ordering::Release);
        self.0.failed.store(true, Ordering::Release);
    }

    pub(crate) fn protocol_succeeded(&self) {
        if self.0.failed.swap(false, Ordering::AcqRel) {
            self.0
                .success
                .send_modify(|generation| *generation = generation.wrapping_add(1));
        }
    }

    pub(crate) async fn addresses(&self) -> std::io::Result<ResolvedAddrs> {
        let mut cache = self.0.cache.lock().await;
        let generation = self.network_generation();
        let urgent = cache.generation != generation || self.0.refresh.load(Ordering::Acquire);
        let stale = cache
            .resolved_at
            .is_none_or(|time| time.elapsed() >= DNS_TTL)
            || urgent;
        if let Some(addresses) = &cache.addresses
            && (self.0.fixed || (!stale && cache.lookup.is_none()))
        {
            return Ok(addresses.clone());
        }
        if cache.lookup.is_none() {
            if cache
                .attempted_at
                .is_some_and(|time| time.elapsed() < DNS_RETRY)
            {
                return cache.addresses.clone().ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        "relay DNS retry is cooling down",
                    )
                });
            }
            cache.attempted_at = Some(Instant::now());
            cache.lookup_generation = generation;
            self.0.refresh.store(false, Ordering::Release);
            let name = self.0.name.clone();
            #[cfg(test)]
            let resolver = self.0.resolver.clone();
            cache.lookup = Some(AbortOnDropHandle::new(tokio::spawn(async move {
                #[cfg(test)]
                if let Some(resolver) = resolver {
                    return resolver().await;
                }
                // Use the OS resolver so hosts, VPN and TUN split DNS remain authoritative.
                // getaddrinfo cannot be cancelled once running. Keep its permit
                // inside the blocking closure, even if the tunnel is dropped.
                static DNS_SLOTS: LazyLock<Arc<Semaphore>> =
                    LazyLock::new(|| Arc::new(Semaphore::new(8)));
                let permit = DNS_SLOTS
                    .clone()
                    .acquire_owned()
                    .await
                    .map_err(std::io::Error::other)?;
                let addresses = tokio::task::spawn_blocking(move || {
                    let _permit = permit;
                    name.to_socket_addrs().map(|addresses| addresses.collect())
                })
                .await
                .map_err(std::io::Error::other)??;
                ResolvedAddrs::from_candidates(addresses).ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        "relay DNS returned no addresses",
                    )
                })
            })));
        }
        if !urgent
            && cache
                .lookup
                .as_ref()
                .is_some_and(|lookup| !lookup.is_finished())
            && let Some(addresses) = &cache.addresses
        {
            // Routine TTL refresh must not delay healthy data setup.
            return Ok(addresses.clone());
        }
        let Some(lookup) = cache.lookup.as_mut() else {
            unreachable!("lookup created above")
        };
        let result = match tokio::time::timeout(DNS_WAIT, lookup).await {
            Ok(result) => {
                cache.lookup.take();
                result
                    .map_err(std::io::Error::other)
                    .and_then(|result| result)
            }
            Err(_) => {
                // Keep the lookup owned across caller cancellation/timeouts. In particular,
                // a blocking OS resolver must not spawn another thread on every retry.
                return cache.addresses.clone().ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "relay DNS query is still pending",
                    )
                });
            }
        };
        match result {
            Ok(addresses) => {
                cache.addresses = Some(addresses.clone());
                cache.resolved_at = Some(Instant::now());
                cache.generation = cache.lookup_generation;
                Ok(addresses)
            }
            Err(error) => cache.addresses.clone().ok_or(error),
        }
    }

    pub(crate) async fn connect(&self) -> std::io::Result<tokio::net::TcpStream> {
        crate::addr::connect_tcp(&self.addresses().await?).await
    }
}

pub(crate) struct RecoveryWake {
    network: watch::Receiver<u64>,
    success: watch::Receiver<u64>,
}

impl RecoveryWake {
    pub(crate) fn acknowledge(&mut self) {
        self.network.borrow_and_update();
        self.success.borrow_and_update();
    }
    pub(crate) async fn network_changed(&mut self) {
        let _ = self.network.changed().await;
        self.network.borrow_and_update();
    }
    pub(crate) async fn changed(&mut self) {
        tokio::select! { _ = self.network.changed() => {}, _ = self.success.changed() => {} }
        self.acknowledge();
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn resolver_endpoint(resolver: TestResolver) -> RelayEndpoint {
        let mut endpoint = RelayEndpoint::new("relay.test:7666".into(), false, None);
        let state = Arc::get_mut(&mut endpoint.0).unwrap();
        state.resolver = Some(resolver);
        state.network = NetworkHints::isolated();
        endpoint
    }

    #[tokio::test(start_paused = true)]
    async fn initial_dns_failure_retries_and_changed_records_replace_candidates() {
        let port = Arc::new(AtomicUsize::new(0));
        let queries = Arc::new(AtomicUsize::new(0));
        let endpoint = resolver_endpoint(Arc::new({
            let port = port.clone();
            let queries = queries.clone();
            move || {
                let port = port.load(Ordering::SeqCst);
                queries.fetch_add(1, Ordering::SeqCst);
                Box::pin(async move {
                    if port == 0 {
                        Err(std::io::ErrorKind::NotFound.into())
                    } else {
                        Ok(SocketAddr::from(([127, 0, 0, 1], port as u16)).into())
                    }
                })
            }
        }));
        assert!(endpoint.addresses().await.is_err());
        assert!(endpoint.addresses().await.is_err());
        assert_eq!(queries.load(Ordering::SeqCst), 1);
        port.store(7001, Ordering::SeqCst);
        tokio::time::advance(DNS_RETRY).await;
        assert_eq!(endpoint.addresses().await.unwrap().primary().port(), 7001);
        port.store(7002, Ordering::SeqCst);
        endpoint.transport_failed();
        assert_eq!(endpoint.addresses().await.unwrap().primary().port(), 7001);
        tokio::time::advance(DNS_RETRY).await;
        assert_eq!(endpoint.addresses().await.unwrap().primary().port(), 7002);
        port.store(0, Ordering::SeqCst);
        tokio::time::advance(DNS_TTL).await;
        assert_eq!(
            endpoint.addresses().await.unwrap().primary().port(),
            7002,
            "a failed refresh must retain the last candidates"
        );
        tokio::task::yield_now().await;
        assert_eq!(endpoint.addresses().await.unwrap().primary().port(), 7002);
        assert_eq!(queries.load(Ordering::SeqCst), 4);
    }

    #[tokio::test(start_paused = true)]
    async fn cancelled_dns_waiters_reuse_one_owned_lookup() {
        let release = Arc::new(tokio::sync::Notify::new());
        let queries = Arc::new(AtomicUsize::new(0));
        let endpoint = resolver_endpoint(Arc::new({
            let release = release.clone();
            let queries = queries.clone();
            move || {
                let release = release.clone();
                queries.fetch_add(1, Ordering::SeqCst);
                Box::pin(async move {
                    release.notified().await;
                    Ok(SocketAddr::from(([127, 0, 0, 1], 7666)).into())
                })
            }
        }));
        for _ in 0..100 {
            assert!(
                tokio::time::timeout(Duration::from_millis(10), endpoint.addresses())
                    .await
                    .is_err()
            );
        }
        assert_eq!(queries.load(Ordering::SeqCst), 1);
        release.notify_one();
        assert_eq!(endpoint.addresses().await.unwrap().primary().port(), 7666);
        assert_eq!(queries.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn dropping_an_endpoint_aborts_its_pending_lookup() {
        struct Active(Arc<AtomicUsize>);
        impl Drop for Active {
            fn drop(&mut self) {
                self.0.fetch_sub(1, Ordering::SeqCst);
            }
        }
        let active = Arc::new(AtomicUsize::new(0));
        let endpoint = resolver_endpoint(Arc::new({
            let active = active.clone();
            move || {
                let active = active.clone();
                Box::pin(async move {
                    active.fetch_add(1, Ordering::SeqCst);
                    let _active = Active(active);
                    std::future::pending().await
                })
            }
        }));
        assert!(
            tokio::time::timeout(Duration::from_millis(10), endpoint.addresses())
                .await
                .is_err()
        );
        assert_eq!(active.load(Ordering::SeqCst), 1);
        drop(endpoint);
        tokio::task::yield_now().await;
        assert_eq!(active.load(Ordering::SeqCst), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn long_outage_wakes_immediately_and_repeated_cancellation_releases_capacity() {
        let endpoint = resolver_endpoint(Arc::new(|| {
            Box::pin(async { Err(std::io::ErrorKind::NotFound.into()) })
        }));
        let mut wake = endpoint.wake();
        // Simulate the observed long outage without changing the host network.
        tokio::time::advance(Duration::from_secs(130)).await;
        endpoint.notify_network_change();
        tokio::time::timeout(Duration::from_millis(1), wake.changed())
            .await
            .unwrap();
        for _ in 0..100 {
            let mut permits = Vec::new();
            for _ in 0..CONTROL_SETUPS {
                permits.push(endpoint.control_permit().await);
            }
            assert!(
                tokio::time::timeout(Duration::from_millis(1), endpoint.control_permit())
                    .await
                    .is_err()
            );
            assert_eq!(endpoint.active_setups().0, CONTROL_SETUPS);
            drop(permits);
            assert_eq!(endpoint.active_setups(), (0, 0));
        }
    }

    #[tokio::test(start_paused = true)]
    async fn only_recovery_after_failure_broadcasts_protocol_success() {
        let endpoint = resolver_endpoint(Arc::new(|| {
            Box::pin(async { Err(std::io::ErrorKind::NotFound.into()) })
        }));
        let mut wake = endpoint.wake();
        endpoint.protocol_succeeded();
        assert!(
            tokio::time::timeout(Duration::from_millis(1), wake.changed())
                .await
                .is_err()
        );
        endpoint.transport_failed();
        endpoint.protocol_succeeded();
        tokio::time::timeout(Duration::from_millis(1), wake.changed())
            .await
            .unwrap();
        endpoint.protocol_succeeded();
        assert!(
            tokio::time::timeout(Duration::from_millis(1), wake.changed())
                .await
                .is_err()
        );
    }
}
