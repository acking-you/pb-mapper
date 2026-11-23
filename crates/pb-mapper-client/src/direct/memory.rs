//! What a consumer's direct setups learn beyond one connection: when direct,
//! and TCP within a LAN, may be tried again after one of them missed.
//!
//! A connect tunnel's listener keeps its own [`Pauses`] for every stream it
//! accepts. A `Client` keeps one per service in its [`DirectMemory`] for its
//! `connect_tcp` calls, each of which sets up a consumer of its own: without
//! it, every reconnect over a path that cannot be punched would spend the
//! whole setup budget again.
use pb_mapper_core::timeout::RetryBackoff;
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::time::Instant;

/// First pause after a direct setup falls back to the relay.
pub(super) const COOLDOWN_INITIAL: Duration = Duration::from_secs(10);
/// Longest pause between direct attempts on a network that keeps refusing them.
pub(super) const COOLDOWN_MAX: Duration = Duration::from_secs(300);
const _: () = assert!(
    !COOLDOWN_INITIAL.is_zero() && COOLDOWN_MAX.as_nanos() >= COOLDOWN_INITIAL.as_nanos(),
    "RetryBackoff needs a first pause, and a cap no shorter"
);

/// The services one [`DirectMemory`] keeps pauses for.
pub(super) const MAX_SERVICES: usize = 64;

/// When a tunnel may try direct again after falling back.
///
/// A path that cannot be punched — a symmetric NAT, blocked UDP, a relay or
/// publisher without direct — fails the same way for every connection, and
/// each failure costs that connection its whole setup budget before the relay
/// takes over. Pausing direct after a fallback sends the next connections
/// straight to the relay instead. The pause doubles while fallbacks repeat; a
/// direct success or a network change, which can make a path punchable,
/// clears it.
#[derive(Debug)]
pub(super) struct Cooldown {
    until: Option<Instant>,
    /// The pauses, from [`COOLDOWN_INITIAL`] doubling to [`COOLDOWN_MAX`].
    pauses: RetryBackoff,
}

impl Default for Cooldown {
    fn default() -> Self {
        Self {
            until: None,
            pauses: RetryBackoff::new(COOLDOWN_INITIAL, COOLDOWN_MAX),
        }
    }
}

impl Cooldown {
    pub(super) fn active(&self, now: Instant) -> bool {
        self.until.is_some_and(|until| now < until)
    }

    /// When the latest pause ends, or ended.
    pub(super) fn until(&self) -> Option<Instant> {
        self.until
    }

    /// Start the next pause, unless one is running. A burst of connections
    /// that were all mid-attempt when the path failed is one failure, not one
    /// per connection: counting each would jump straight to the cap.
    pub(super) fn back_off(&mut self, now: Instant) {
        if self.active(now) {
            return;
        }
        self.until = Some(now + self.pauses.next_delay());
    }

    pub(super) fn clear(&mut self) {
        *self = Self::default();
    }
}

/// The pauses the direct setups to one service are under, on the network
/// they were learned on.
#[derive(Debug, Default)]
pub(crate) struct Pauses {
    /// The network generation the pauses below were learned on.
    generation: u64,
    /// When direct may be tried again, after a fallback to the relay.
    pub(super) direct: Cooldown,
    /// When sessions may ask for TCP again, after one whose attempts missed:
    /// a provider that refuses TCP, or whose firewall drops it, does so for
    /// every session, and each would pay the head start. Never the relay's
    /// pause: QUIC is still tried.
    pub(super) tcp: Cooldown,
    /// A fault for tests: the endpoints of the consumers under these pauses
    /// drop every QUIC packet they send, so no direct handshake completes.
    #[cfg(test)]
    pub(super) block_quic: bool,
}

impl Pauses {
    /// Bring these pauses to network `generation`: one learned on another
    /// network is forgotten, as that network's change may have made the
    /// path punchable. A consumer reads them through this, so a change it
    /// was not woken for still clears them.
    pub(super) fn on_network(&mut self, generation: u64) {
        if self.generation != generation {
            self.generation = generation;
            self.direct.clear();
            self.tcp.clear();
        }
    }

    /// Whether these pauses hold nothing back at `now` on network
    /// `generation`.
    fn idle(&self, now: Instant, generation: u64) -> bool {
        self.generation != generation || !(self.direct.active(now) || self.tcp.active(now))
    }

    /// When the last of these pauses ends.
    fn ends(&self) -> Option<Instant> {
        self.direct.until().max(self.tcp.until())
    }
}

/// Pauses one or more consumers share.
pub(crate) type SharedPauses = Arc<parking_lot::Mutex<Pauses>>;

/// What a `Client`'s `connect_tcp` calls learn of direct, by service, so
/// that a fallback in one pauses direct for the next calls to that service.
///
/// A memory belongs to one client and the clients on its other carriers,
/// which share its relay and namespace, so the service names an entry. It
/// keeps [`MAX_SERVICES`] at most.
#[derive(Debug, Default)]
pub(crate) struct DirectMemory {
    services: HashMap<Box<str>, SharedPauses>,
}

impl DirectMemory {
    /// The pauses of `service`, kept from now on. Past the bound, the
    /// services that hold nothing back at `now` on network `generation` are
    /// forgotten first, then the one whose pause ends soonest.
    pub(crate) fn pauses(&mut self, service: &str, now: Instant, generation: u64) -> SharedPauses {
        if let Some(pauses) = self.services.get(service) {
            return pauses.clone();
        }
        if self.services.len() >= MAX_SERVICES {
            self.services
                .retain(|_, pauses| !pauses.lock().idle(now, generation));
        }
        if self.services.len() >= MAX_SERVICES
            && let Some(soonest) = self
                .services
                .iter()
                .min_by_key(|(_, pauses)| pauses.lock().ends())
                .map(|(service, _)| service.clone())
        {
            self.services.remove(&soonest);
        }
        let pauses = SharedPauses::default();
        self.services.insert(service.into(), pauses.clone());
        pauses
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.services.len()
    }
}
