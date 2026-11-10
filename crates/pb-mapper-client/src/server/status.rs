use std::sync::Mutex;

// Callback for notifying status changes to external systems
pub type StatusCallback = Box<dyn Fn(&str) + Send + Sync>;

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
    callback: StatusCallback,
    workers: Mutex<PoolStatusState>,
}

/// What each worker last reported, plus what the pool last published.
struct PoolStatusState {
    workers: Vec<WorkerStatus>,
    published: Option<WorkerStatus>,
}

/// One worker's latest view of its own control connection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum WorkerStatus {
    /// Connecting, or reconnecting after a retryable failure.
    Retrying,
    /// Registered: this worker alone makes the service reachable.
    Connected,
    /// Permanently rejected; this worker will not try again.
    Failed(String),
}

impl PoolStatus {
    pub(super) fn new(callback: StatusCallback, pool_size: usize) -> Self {
        Self {
            callback,
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
        // Outside the lock: the callback runs caller code, which must not be able
        // to deadlock a worker reporting its own transition.
        match next {
            WorkerStatus::Connected => (self.callback)("connected"),
            WorkerStatus::Retrying => (self.callback)("retrying"),
            WorkerStatus::Failed(reason) => (self.callback)(&format!("failed: {reason}")),
        }
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
    use std::sync::Arc;
    use std::sync::Mutex as StdMutex;

    use super::*;

    /// Records what the pool published, in order.
    fn recording_pool(pool_size: usize) -> (PoolStatus, Arc<StdMutex<Vec<String>>>) {
        let seen = Arc::new(StdMutex::new(Vec::new()));
        let sink = seen.clone();
        let callback: StatusCallback = Box::new(move |status: &str| {
            sink.lock().unwrap().push(status.to_string());
        });
        (PoolStatus::new(callback, pool_size), seen)
    }

    fn published(seen: &Arc<StdMutex<Vec<String>>>) -> Vec<String> {
        seen.lock().unwrap().clone()
    }

    /// One connected worker is enough: the service is reachable through it, so
    /// the pool must not report the other worker's retry as the pool's state.
    #[test]
    fn one_connected_worker_makes_the_pool_connected() {
        let (pool, seen) = recording_pool(2);
        pool.report(0, WorkerStatus::Connected);
        pool.report(1, WorkerStatus::Retrying);
        assert_eq!(published(&seen), vec!["connected".to_string()]);
    }

    /// The guarantee `wait_ready()` depends on: while any worker might still
    /// bring the service up, one worker's permanent rejection is not the pool's.
    #[test]
    fn a_single_failure_does_not_fail_the_pool() {
        let (pool, seen) = recording_pool(2);
        pool.report(0, WorkerStatus::Failed("service_transport_mismatch".into()));
        assert_eq!(published(&seen), vec!["retrying".to_string()]);

        pool.report(1, WorkerStatus::Connected);
        assert_eq!(
            published(&seen),
            vec!["retrying".to_string(), "connected".to_string()]
        );
    }

    /// Once every worker has given up, the pool reports failed and says why —
    /// otherwise a caller awaiting readiness with no timeout waits forever.
    #[test]
    fn the_pool_fails_only_when_every_worker_has() {
        let (pool, seen) = recording_pool(2);
        pool.report(0, WorkerStatus::Failed("service_transport_mismatch".into()));
        pool.report(1, WorkerStatus::Failed("namespace_access_denied".into()));
        assert_eq!(
            published(&seen),
            vec![
                "retrying".to_string(),
                "failed: service_transport_mismatch".to_string(),
            ],
            "the first permanent rejection is the one that explains the pool"
        );
    }

    /// A worker that reconnects repeatedly must not turn into a stream of
    /// identical status lines for the caller.
    #[test]
    fn unchanged_aggregates_are_not_republished() {
        let (pool, seen) = recording_pool(2);
        pool.report(0, WorkerStatus::Connected);
        pool.report(1, WorkerStatus::Connected);
        pool.report(0, WorkerStatus::Connected);
        assert_eq!(published(&seen), vec!["connected".to_string()]);
    }

    /// Losing the last connected worker has to be visible: the service is no
    /// longer reachable, and the caller's status must say so.
    #[test]
    fn losing_the_last_connection_publishes_retrying() {
        let (pool, seen) = recording_pool(1);
        pool.report(0, WorkerStatus::Connected);
        pool.report(0, WorkerStatus::Retrying);
        assert_eq!(
            published(&seen),
            vec!["connected".to_string(), "retrying".to_string()]
        );
    }
}
