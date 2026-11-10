//! Session observations: reports from both ends of established direct
//! sessions, kept for inventory and never used to authorize anything.
use super::DirectState;
use pb_mapper_auth::AuthContext;
use pb_mapper_protocol::direct::*;
use std::time::Instant;

impl DirectState {
    pub(super) fn observe(
        &mut self,
        auth: &AuthContext,
        namespace: u64,
        update: SessionUpdate,
        consumer: bool,
    ) {
        let SessionUpdate {
            session,
            secret,
            mut report,
        } = update;
        if auth.ensure_active().is_err()
            || session.namespace != namespace
            || session.id != report.id
            || auth.key_id.as_u64()
                != if consumer {
                    session.consumer_key
                } else {
                    session.provider_key
                }
            || session.service.len() > 1024
            || crate::scoped_service_key(auth, namespace, &session.service).is_err()
        {
            return;
        }
        let identity = (session.server_instance, session.id);
        if !self.observations.contains_key(&identity) {
            let known = self
                .entries
                .get(&session.id)
                .filter(|_| session.server_instance == self.instance);
            if let Some(entry) = known
                && (secret != entry.grant.secret || session != SessionSummary::from(&entry.grant))
            {
                return;
            }
            // A control outage can outlive both the setup and observation TTL
            // without interrupting direct data. Restore an authenticated claim
            // even in this incarnation, but never recreate setup authority.
            let restored = known.is_none();
            if !self.make_observation_room(namespace) {
                return;
            }
            self.observations.insert(
                identity,
                Observation {
                    restored,
                    session: session.clone(),
                    secret,
                    consumer: None,
                    provider: None,
                    touched: Instant::now(),
                },
            );
        }
        let Some(observation) = self.observations.get_mut(&identity) else {
            return;
        };
        if observation.secret != secret || observation.session != session {
            return;
        }
        let slot = if consumer {
            &mut observation.consumer
        } else {
            &mut observation.provider
        };
        if slot.as_ref().is_some_and(|(old, _)| old.seq >= report.seq) {
            return;
        }
        report.reason = report.reason.map(|r| r.chars().take(256).collect());
        observation.touched = Instant::now();
        *slot = Some((report, observation.touched));
    }
    fn make_observation_room(&mut self, namespace: u64) -> bool {
        let namespace_full = self
            .observations
            .values()
            .filter(|o| o.session.namespace == namespace)
            .count()
            >= 1024;
        if !namespace_full && self.observations.len() < 4096 {
            return true;
        }
        // Retention is a history budget, not a reason to hide new active
        // sessions. Reclaim the oldest closed observation in the constrained
        // scope, preserving active/conflicting reports and namespace isolation.
        let oldest = self
            .observations
            .iter()
            .filter(|(_, o)| !namespace_full || o.session.namespace == namespace)
            .filter(|(_, o)| o.closed())
            .min_by_key(|(_, o)| o.touched)
            .map(|(id, _)| *id);
        if let Some(id) = oldest {
            self.observations.remove(&id);
            true
        } else {
            false
        }
    }
    pub fn observations(&self, namespace: u64) -> Vec<SessionObservation> {
        self.observations
            .values()
            .filter(|o| o.session.namespace == namespace)
            .map(|o| {
                let observed = |r: &Option<(SessionReport, Instant)>| {
                    r.as_ref().map(|(report, at)| ObservedReport {
                        report: report.clone(),
                        age_ms: at.elapsed().as_millis() as u64,
                    })
                };
                let consumer = observed(&o.consumer);
                let provider = observed(&o.provider);
                let stale = [&consumer, &provider]
                    .into_iter()
                    .any(|r| r.as_ref().is_none_or(|r| r.age_ms > 45_000));
                let conflicting = consumer
                    .as_ref()
                    .zip(provider.as_ref())
                    .is_some_and(|(c, p)| {
                        c.report.path != p.report.path || c.report.state != p.report.state
                    });
                SessionObservation {
                    server_instance: o.session.server_instance,
                    restored: o.restored,
                    id: o.session.id,
                    service: o.session.service.clone(),
                    is_datagram: o.session.is_datagram,
                    consumer,
                    provider,
                    stale,
                    conflicting,
                }
            })
            .collect()
    }
}
pub(super) struct Observation {
    restored: bool,
    session: SessionSummary,
    secret: SessionSecret,
    consumer: Option<(SessionReport, Instant)>,
    provider: Option<(SessionReport, Instant)>,
    pub(super) touched: Instant,
}
impl Observation {
    fn closed(&self) -> bool {
        (self.consumer.is_some() || self.provider.is_some())
            && [&self.consumer, &self.provider]
                .into_iter()
                .flatten()
                .all(|(report, _)| report.state == SessionState::Closed)
    }
}
