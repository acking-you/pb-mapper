//! How many QUIC handshakes and authenticated connections the listener holds,
//! in all, per address and per credential.
use pb_mapper_auth::{AuthFailure, KeyId};
use std::{
    collections::HashMap,
    net::IpAddr,
    sync::{Arc, Mutex as StdMutex},
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Handshakes in flight, before any credential is known.
const HANDSHAKES: usize = 64;
/// Of those, how many may come from addresses not yet shown to receive what
/// is sent to them. A forged source address costs nothing to send from, so
/// past this a sender first gets a stateless retry: it costs the relay
/// nothing, and an honest client one round trip.
const UNVALIDATED_HANDSHAKES: usize = 8;
/// And how many one validated address may have in flight: enough for the
/// clients behind one NAT reconnecting at once, too few for one host to
/// take every slot by finishing handshakes and never authenticating.
const HANDSHAKES_PER_ADDRESS: usize = 8;
/// Authenticated connections the listener holds at once.
const CONNECTIONS: usize = 128;
/// Of those, how many one temporary credential may hold: a client pools one
/// per network it has been on and closes those it stops using, so this is
/// generous, and it keeps one tenant from taking every slot. The
/// administrator key, which operators share between their own machines, is
/// bounded by [`CONNECTIONS`] alone.
const CONNECTIONS_PER_CREDENTIAL: usize = 16;
/// Of [`CONNECTIONS`], how many only the administrator may take, so that
/// temporary credentials between them cannot lock it out.
const ADMIN_RESERVED: usize = 8;

const _: () = assert!(
    UNVALIDATED_HANDSHAKES <= HANDSHAKES,
    "unvalidated handshakes are a share of all handshakes"
);
const _: () = assert!(
    HANDSHAKES_PER_ADDRESS <= HANDSHAKES,
    "one address cannot hold more than every slot"
);
const _: () = assert!(
    ADMIN_RESERVED < CONNECTIONS,
    "the reserve must leave room for temporary credentials"
);
const _: () = assert!(
    CONNECTIONS_PER_CREDENTIAL <= CONNECTIONS - ADMIN_RESERVED,
    "one credential cannot exceed what all temporary credentials share"
);

/// Authenticated connections held, in all and per temporary credential.
pub(super) struct Admission {
    total: Arc<Semaphore>,
    per_key: StdMutex<HashMap<KeyId, usize>>,
}

/// One admitted connection's share of [`Admission`], returned on drop.
pub(super) struct Admitted {
    _total: OwnedSemaphorePermit,
    key: KeyId,
    admission: Arc<Admission>,
}

impl Admission {
    pub(super) fn new() -> Arc<Self> {
        Arc::new(Self {
            total: Arc::new(Semaphore::new(CONNECTIONS)),
            per_key: StdMutex::new(HashMap::new()),
        })
    }

    /// A slot for `key`, or the retryable refusal of the limit it reached.
    pub(super) fn admit(self: &Arc<Self>, key: KeyId) -> Result<Admitted, AuthFailure> {
        let full = || {
            AuthFailure::new(
                "quic_connection_capacity",
                "relay QUIC connection capacity reached",
                true,
            )
        };
        let total = self.total.clone().try_acquire_owned().map_err(|_| full())?;
        if !key.is_admin() && self.total.available_permits() < ADMIN_RESERVED {
            return Err(full());
        }
        if !key.is_admin() {
            let mut per_key = self.per_key.lock().unwrap_or_else(|e| e.into_inner());
            let held = per_key.entry(key).or_default();
            if *held >= CONNECTIONS_PER_CREDENTIAL {
                return Err(AuthFailure::new(
                    "quic_credential_capacity",
                    "this credential has reached its relay QUIC connection limit",
                    true,
                ));
            }
            *held += 1;
        }
        Ok(Admitted {
            _total: total,
            key,
            admission: self.clone(),
        })
    }
}

impl Drop for Admitted {
    fn drop(&mut self) {
        if self.key.is_admin() {
            return;
        }
        let mut per_key = self
            .admission
            .per_key
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(held) = per_key.get_mut(&self.key) {
            *held -= 1;
            if *held == 0 {
                per_key.remove(&self.key);
            }
        }
    }
}

/// Handshakes in flight per validated address.
type PerAddress = Arc<StdMutex<HashMap<IpAddr, usize>>>;

/// Handshakes in flight: in all, from addresses not yet validated, and per
/// validated address.
#[derive(Clone)]
pub(super) struct Handshakes {
    total: Arc<Semaphore>,
    unvalidated: Arc<Semaphore>,
    per_address: PerAddress,
}

/// What to do with an incoming handshake.
pub(super) enum Claim {
    Accept(Handshake),
    /// Ask the sender to show it receives at its address first.
    Retry,
    Refuse,
}

/// One handshake's share of [`Handshakes`], returned when it ends, either way.
pub(super) struct Handshake {
    _total: OwnedSemaphorePermit,
    _unvalidated: Option<OwnedSemaphorePermit>,
    address: Option<(IpAddr, PerAddress)>,
}

impl Handshakes {
    pub(super) fn new() -> Self {
        Self {
            total: Arc::new(Semaphore::new(HANDSHAKES)),
            unvalidated: Arc::new(Semaphore::new(UNVALIDATED_HANDSHAKES)),
            per_address: Arc::default(),
        }
    }

    /// Only a validated address is counted as itself: an unvalidated one may
    /// be forged, and would spend someone else's share.
    pub(super) fn claim(&self, validated: bool, may_retry: bool, ip: IpAddr) -> Claim {
        let unvalidated = if validated {
            None
        } else {
            match self.unvalidated.clone().try_acquire_owned() {
                Ok(permit) => Some(permit),
                Err(_) if may_retry => return Claim::Retry,
                Err(_) => return Claim::Refuse,
            }
        };
        let Ok(total) = self.total.clone().try_acquire_owned() else {
            return Claim::Refuse;
        };
        let address = if validated {
            let mut held = self.per_address.lock().unwrap_or_else(|e| e.into_inner());
            let held = held.entry(ip).or_default();
            if *held >= HANDSHAKES_PER_ADDRESS {
                return Claim::Refuse;
            }
            *held += 1;
            Some((ip, self.per_address.clone()))
        } else {
            None
        };
        Claim::Accept(Handshake {
            _total: total,
            _unvalidated: unvalidated,
            address,
        })
    }
}

impl Drop for Handshake {
    fn drop(&mut self) {
        let Some((ip, per_address)) = &self.address else {
            return;
        };
        let mut held = per_address.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(count) = held.get_mut(ip) {
            *count -= 1;
            if *count == 0 {
                held.remove(ip);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Forged sources hold no more than their share before being asked to
    /// prove their address, and one proven address no more than its own:
    /// another still gets in, and a slot comes back when its handshake ends.
    #[test]
    fn no_one_source_takes_every_handshake_slot() {
        let handshakes = Handshakes::new();
        let host: IpAddr = [192, 0, 2, 1].into();
        let other: IpAddr = [192, 0, 2, 2].into();
        let accept = |claim| match claim {
            Claim::Accept(handshake) => handshake,
            _ => panic!("expected the handshake to be accepted"),
        };

        let forged: Vec<_> = (0..UNVALIDATED_HANDSHAKES)
            .map(|_| accept(handshakes.claim(false, true, host)))
            .collect();
        assert!(matches!(handshakes.claim(false, true, other), Claim::Retry));
        assert!(matches!(
            handshakes.claim(false, false, other),
            Claim::Refuse
        ));

        let mut held: Vec<_> = (0..HANDSHAKES_PER_ADDRESS)
            .map(|_| accept(handshakes.claim(true, false, host)))
            .collect();
        assert!(matches!(handshakes.claim(true, false, host), Claim::Refuse));
        let _other = accept(handshakes.claim(true, false, other));
        held.pop();
        let _again = accept(handshakes.claim(true, false, host));
        drop(forged);

        let total: Vec<_> = (0..HANDSHAKES)
            .map_while(
                |n| match handshakes.claim(true, false, [10, 0, (n / 8) as u8, 1].into()) {
                    Claim::Accept(handshake) => Some(handshake),
                    _ => None,
                },
            )
            .collect();
        assert_eq!(total.len() + held.len() + 2, HANDSHAKES);
        assert!(matches!(
            handshakes.claim(true, false, [10, 9, 9, 9].into()),
            Claim::Refuse
        ));
    }

    /// Temporary credentials between them leave the administrator its
    /// reserve.
    #[test]
    fn temporary_credentials_leave_the_administrator_a_reserve() {
        let admission = Admission::new();
        let mut admitted = Vec::new();
        let mut key = 1;
        while let Ok(slot) = admission.admit(KeyId::from_u64(key)) {
            admitted.push(slot);
            if admitted.len() % CONNECTIONS_PER_CREDENTIAL == 0 {
                key += 1;
            }
        }
        assert_eq!(admitted.len(), CONNECTIONS - ADMIN_RESERVED);
        let admin: Vec<_> = (0..ADMIN_RESERVED)
            .map(|_| admission.admit(pb_mapper_auth::ADMIN_KEY_ID).unwrap())
            .collect();
        assert_eq!(admin.len(), ADMIN_RESERVED);
    }
}
