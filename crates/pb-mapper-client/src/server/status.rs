//! The pool's one status, from its workers' own.

use std::sync::Mutex;

use crate::worker_status::{StatusSink, WorkerStatus};

/// Turns the control pool's per-worker statuses into the one status a caller sees.
///
/// A registration is served by [`control_conn_pool_size`](pb_mapper_core::config::control_conn_pool_size) control connections,
/// any one of which keeps the service reachable. Reporting only the first
/// worker's view — which is what this code did — meant a caller was told
/// `retrying` while the rest of the pool was registered and forwarding fine.
///
/// So the pool's status is the best of its workers, in this order:
///
/// * `connected` — at least one worker is registered.
/// * `retrying` — none is, but at least one is still trying.
/// * `failed` — every worker gave up permanently. Only then, because a caller
///   awaiting readiness with no timeout must not be released while any worker
///   could still bring the service up.
///
/// The reported status is recomputed on every worker transition and forwarded
/// only when it actually changes, so a caller sees pool-level transitions rather
/// than one line per worker.
pub(super) struct PoolStatus {
    sink: StatusSink,
    workers: Mutex<PoolStatusState>,
}

/// What each worker last reported, plus what the pool last published.
struct PoolStatusState {
    workers: Vec<WorkerStatus>,
    published: Option<WorkerStatus>,
}

impl PoolStatus {
    pub(super) fn new(sink: StatusSink, pool_size: usize) -> Self {
        Self {
            sink,
            workers: Mutex::new(PoolStatusState {
                workers: vec![WorkerStatus::Retrying; pool_size],
                published: None,
            }),
        }
    }

    /// Record `status` for `worker_index`, and publish the pool's status if the
    /// aggregate changed.
    pub(super) fn report(&self, worker_index: usize, status: WorkerStatus) {
        let next = {
            let mut state = self
                .workers
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(slot) = state.workers.get_mut(worker_index) {
                *slot = status;
            }
            let aggregate = Self::aggregate(&state.workers);
            if state.published.as_ref() == Some(&aggregate) {
                return;
            }
            state.published = Some(aggregate.clone());
            aggregate
        };
        // Outside the lock: the sink runs caller code, which must not be able
        // to deadlock a worker reporting its own transition.
        (self.sink)(&next);
    }

    /// The best status among the workers. `Failed` needs every one of them.
    ///
    /// `Connected` short-circuits; `Retrying` cannot, because a later worker may
    /// still be connected and outrank it.
    fn aggregate(workers: &[WorkerStatus]) -> WorkerStatus {
        let mut retrying = false;
        let mut first_failure = None;
        for status in workers {
            match status {
                WorkerStatus::Connected => return WorkerStatus::Connected,
                WorkerStatus::Retrying => retrying = true,
                WorkerStatus::Failed(reason) => {
                    first_failure.get_or_insert(reason);
                }
            }
        }
        match first_failure {
            // A pool that is out of workers reports the first permanent rejection
            // it collected, which is the one that explains the others.
            Some(reason) if !retrying => WorkerStatus::Failed(reason.clone()),
            _ => WorkerStatus::Retrying,
        }
    }
}

#[cfg(test)]
mod pool_status_tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use WorkerStatus::{Connected, Retrying};

    /// Records what the pool published, in order.
    fn recording_pool(pool_size: usize) -> (PoolStatus, Arc<Mutex<Vec<WorkerStatus>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        let sink: StatusSink = Box::new(move |status| sink.lock().unwrap().push(status.clone()));
        (PoolStatus::new(sink, pool_size), seen)
    }

    fn published(seen: &Arc<Mutex<Vec<WorkerStatus>>>) -> Vec<WorkerStatus> {
        seen.lock().unwrap().clone()
    }

    fn failed(reason: &str) -> WorkerStatus {
        WorkerStatus::Failed(Some(reason.into()))
    }

    /// The pool is as good as its best worker: connected beats retrying, and
    /// it fails only once every worker has.
    #[test]
    fn the_pool_reports_its_best_worker() {
        // One connected worker is enough: the service is reachable through
        // it, whatever the other is doing.
        let (pool, seen) = recording_pool(2);
        pool.report(0, Connected);
        pool.report(1, Retrying);
        assert_eq!(published(&seen), [Connected]);

        // The guarantee `wait_ready()` depends on: while any worker might
        // still bring the service up, one worker's permanent rejection is not
        // the pool's.
        let (pool, seen) = recording_pool(2);
        pool.report(0, failed("service_transport_mismatch"));
        pool.report(1, Connected);
        assert_eq!(published(&seen), [Retrying, Connected]);

        // Once every worker has given up, the pool fails and says why —
        // otherwise a caller awaiting readiness with no timeout waits forever.
        // The first permanent rejection is the one that explains the pool.
        let (pool, seen) = recording_pool(2);
        pool.report(0, failed("service_transport_mismatch"));
        pool.report(1, failed("namespace_access_denied"));
        assert_eq!(
            published(&seen),
            [Retrying, failed("service_transport_mismatch")]
        );
    }

    /// Only changes are published: a worker that reconnects repeatedly is not
    /// a stream of identical statuses, but losing the last connected worker
    /// has to be visible, since the service is no longer reachable.
    #[test]
    fn the_pool_publishes_only_changes() {
        let (pool, seen) = recording_pool(2);
        pool.report(0, Connected);
        pool.report(1, Connected);
        pool.report(0, Connected);
        assert_eq!(published(&seen), [Connected]);

        pool.report(0, Retrying);
        assert_eq!(published(&seen), [Connected]);
        pool.report(1, Retrying);
        assert_eq!(published(&seen), [Connected, Retrying]);
    }
}
