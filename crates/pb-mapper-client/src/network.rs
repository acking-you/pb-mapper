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
    /// When [`Self::notify`] last raised a hint. On the process clock, not
    /// tokio's: the shared hints serve every runtime in the process, and a
    /// tokio clock belongs to one.
    last_manual_hint: Mutex<Option<std::time::Instant>>,
}

impl NetworkHints {
    fn new() -> Arc<Self> {
        let (changes, _) = watch::channel(0);
        Arc::new(Self {
            changes,
            task: Mutex::new(None),
            last_manual_hint: Mutex::new(None),
        })
    }

    #[cfg(test)]
    pub(crate) fn isolated() -> Arc<Self> {
        Self::new()
    }

    pub(crate) fn shared() -> Arc<Self> {
        static SHARED: Mutex<Weak<NetworkHints>> = Mutex::new(Weak::new());
        let mut slot = SHARED.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(shared) = slot.upgrade() {
            return shared;
        }
        let shared = Self::new();
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
    let mut debounce = Debounce::default();
    let hint = || changes.send_modify(|generation| *generation = generation.wrapping_add(1));
    loop {
        let due = debounce.due();
        let event = tokio::select! {
            event = std::future::poll_fn(|cx| watcher.poll_if_event(cx)) => event?,
            () = async {
                match due {
                    Some(due) => tokio::time::sleep_until(due).await,
                    None => std::future::pending().await,
                }
            } => {
                tracing::debug!(event = "network_address_burst_ended");
                hint();
                debounce.raised(Instant::now());
                continue;
            }
        };
        let (address, changed) = match event {
            if_watch::IfEvent::Up(address) => (address, known.insert(address.addr())),
            if_watch::IfEvent::Down(address) => (address, known.remove(&address.addr())),
        };
        if address.addr().is_loopback() || !changed {
            continue;
        }
        if debounce.changed(Instant::now()) {
            tracing::debug!(event = "network_address_changed", %address);
            hint();
        }
    }
}

/// How long after a hint further address changes belong to the same burst.
#[cfg(any(target_os = "linux", target_os = "windows"))]
const BURST: Duration = Duration::from_millis(250);

/// Which address changes raise a hint: one at the start of a burst, such as
/// an initial address dump or a run of DHCP changes, and one more at its end
/// if it went on. A hint wakes every tunnel, and the trailing one makes what
/// follows the network (discovery, the direct pause, a router mapping) react
/// to where it settled: a switch from one network to another is a Down
/// then, a moment later, an Up.
#[cfg(any(target_os = "linux", target_os = "windows"))]
#[derive(Default)]
struct Debounce {
    /// When the last hint was raised.
    last_hint: Option<Instant>,
    /// Whether a change came after it, inside its burst.
    held: bool,
}

#[cfg(any(target_os = "linux", target_os = "windows"))]
impl Debounce {
    /// An address changed at `now`: whether to raise a hint for it now.
    fn changed(&mut self, now: Instant) -> bool {
        if self
            .last_hint
            .is_some_and(|last| now.duration_since(last) < BURST)
        {
            self.held = true;
            return false;
        }
        self.raised(now);
        true
    }

    /// When a hint is due for changes held back, if any are.
    fn due(&self) -> Option<Instant> {
        self.last_hint
            .filter(|_| self.held)
            .map(|last| last + BURST)
    }

    /// A hint was raised at `now`.
    fn raised(&mut self, now: Instant) {
        self.last_hint = Some(now);
        self.held = false;
    }
}

#[cfg(all(test, any(target_os = "linux", target_os = "windows")))]
mod tests {
    use super::*;

    /// A burst of changes raises a hint at once, and one more at its end for
    /// the changes it held back: what follows the network then reacts to
    /// where it settled, not only to where it began. A change on its own
    /// raises one hint.
    #[test]
    fn a_burst_hints_at_its_start_and_again_at_its_end() {
        let start = Instant::now();
        let mut debounce = Debounce::default();
        assert!(debounce.changed(start));
        assert_eq!(debounce.due(), None);
        assert!(!debounce.changed(start + Duration::from_millis(100)));
        assert!(!debounce.changed(start + Duration::from_millis(200)));
        assert_eq!(debounce.due(), Some(start + BURST));
        debounce.raised(start + BURST);
        assert_eq!(debounce.due(), None);
        assert!(debounce.changed(start + BURST * 3));
        assert_eq!(debounce.due(), None);
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
