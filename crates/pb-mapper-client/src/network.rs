//! Process-shared network hints. A hint never declares a tunnel healthy.

use std::sync::{Arc, Mutex, Weak};
use tokio::sync::watch;
use tokio::time::Duration;
#[cfg(any(target_os = "linux", target_os = "windows"))]
use tokio::time::Instant;
use tokio_util::task::AbortOnDropHandle;

pub(crate) struct NetworkHints {
    changes: watch::Sender<u64>,
    task: Mutex<Option<AbortOnDropHandle<()>>>,
    last_manual_hint: Mutex<Option<std::time::Instant>>,
}

impl NetworkHints {
    #[cfg(test)]
    pub(crate) fn isolated() -> Arc<Self> {
        let (changes, _) = watch::channel(0);
        Arc::new(Self {
            changes,
            task: Mutex::new(None),
            last_manual_hint: Mutex::new(None),
        })
    }
    pub(crate) fn shared() -> Arc<Self> {
        static SHARED: Mutex<Weak<NetworkHints>> = Mutex::new(Weak::new());
        let mut slot = SHARED.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(shared) = slot.upgrade() {
            return shared;
        }
        let (changes, _) = watch::channel(0);
        let shared = Arc::new(Self {
            changes,
            task: Mutex::new(None),
            last_manual_hint: Mutex::new(None),
        });
        *slot = Arc::downgrade(&shared);
        shared
    }

    pub(crate) fn start(&self) {
        let mut slot = self.task.lock().unwrap_or_else(|e| e.into_inner());
        if slot.as_ref().is_some_and(|task| !task.is_finished()) {
            return;
        }
        let changes = self.changes.clone();
        *slot = Some(AbortOnDropHandle::new(tokio::spawn(async move {
            if let Err(error) = watch_changes(changes).await {
                tracing::debug!(event = "network_hints_unavailable", %error, "protocol probes remain active");
            }
        })));
    }

    pub(crate) fn subscribe(&self) -> watch::Receiver<u64> {
        self.changes.subscribe()
    }
    pub(crate) fn generation(&self) -> u64 {
        *self.changes.borrow()
    }
    pub(crate) fn notify(&self) {
        let mut last = self
            .last_manual_hint
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if last.is_some_and(|time| time.elapsed() < Duration::from_millis(250)) {
            return;
        }
        *last = Some(std::time::Instant::now());
        self.changes
            .send_modify(|generation| *generation = generation.wrapping_add(1));
    }
}

#[cfg(any(target_os = "linux", target_os = "windows"))]
async fn watch_changes(changes: watch::Sender<u64>) -> std::io::Result<()> {
    let mut watcher = if_watch::tokio::IfWatcher::new()?;
    // IfWatcher emits the initial address dump as Up events. Seed the set so
    // starting a watcher cannot cancel an otherwise healthy first handshake.
    let mut known: std::collections::HashSet<_> = tokio::task::spawn_blocking(|| {
        if_addrs::get_if_addrs().map(|items| items.into_iter().map(|item| item.ip()).collect())
    })
    .await
    .map_err(std::io::Error::other)??;
    let mut last_hint = None::<Instant>;
    loop {
        let event = std::future::poll_fn(|cx| watcher.poll_if_event(cx)).await?;
        let (address, changed) = match event {
            if_watch::IfEvent::Up(address) => (address, known.insert(address.addr())),
            if_watch::IfEvent::Down(address) => (address, known.remove(&address.addr())),
        };
        if address.addr().is_loopback() || !changed {
            continue;
        }
        // An initial address dump or a burst of DHCP changes is one hint.
        if last_hint.is_none_or(|last| last.elapsed() >= Duration::from_millis(250)) {
            tracing::debug!(event = "network_address_changed", %address);
            changes.send_modify(|generation| *generation = generation.wrapping_add(1));
            last_hint = Some(Instant::now());
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
async fn watch_changes(changes: watch::Sender<u64>) -> std::io::Result<()> {
    // A single owned poller avoids an unjoinable platform callback thread.
    let mut previous = None;
    let mut interval = tokio::time::interval(Duration::from_secs(2));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        interval.tick().await;
        let mut addresses = tokio::task::spawn_blocking(|| {
            if_addrs::get_if_addrs().map(|items| {
                items
                    .into_iter()
                    .filter(|item| !item.is_loopback())
                    .map(|item| (item.name.clone(), item.ip()))
                    .collect::<Vec<_>>()
            })
        })
        .await
        .map_err(std::io::Error::other)??;
        addresses.sort();
        if previous.as_ref().is_some_and(|old| old != &addresses) {
            changes.send_modify(|generation| *generation = generation.wrapping_add(1));
        }
        previous = Some(addresses);
    }
}
