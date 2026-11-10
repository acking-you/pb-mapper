//! The one deregistration a connection task owes the routing manager.
//!
//! A socket task that told the manager about itself must tell it when it ends,
//! whether it finishes, fails or is cancelled; the manager recycles the
//! connection's ID only then. [`DeregisterGuard`] sends that task exactly once:
//! awaited on the normal path, and from `Drop` otherwise, without blocking and
//! without needing a runtime.

use crate::manager::{ManagerClosed, TrySendError};
use crate::task::{ManagerTask, ManagerTaskSender};

/// What one kind of connection deregisters, and what it logs at each step.
///
/// The log calls stay with each kind of connection, so their text and target do
/// not depend on this module.
pub(crate) trait Deregistration: 'static {
    /// The task to send. Built afresh for every attempt rather than moved into a
    /// send future, which a cancelled send would drop along with the task.
    fn task(&self) -> ManagerTask;
    /// The awaited send delivered the task.
    fn sent(&self) {}
    /// The awaited send found the manager gone.
    fn manager_closed(&self);
    /// Dropped without deregistering, and the task was queued.
    fn dropped_sent(&self) {}
    /// Dropped without deregistering, and the manager was gone.
    fn dropped_manager_closed(&self);
    /// Dropped without deregistering into a full queue; the send is deferred.
    fn dropped_queue_full(&self);
    /// A deferred send found the manager gone.
    fn deferred_manager_closed();
    /// Dropped into a full queue with no runtime to defer the send on.
    fn no_runtime();
}

pub(crate) struct DeregisterGuard<D: Deregistration> {
    what: D,
    sender: ManagerTaskSender,
    active: bool,
}

impl<D: Deregistration> DeregisterGuard<D> {
    pub(crate) fn new(what: D, sender: ManagerTaskSender) -> Self {
        Self {
            what,
            sender,
            active: true,
        }
    }

    /// Send the deregistration and wait until the manager has it.
    pub(crate) async fn deregister(&mut self) {
        if !self.active {
            return;
        }
        let result = self.sender.send(self.what.task()).await;
        // Cleared only once the send has finished: a send cancelled while it
        // waits leaves the guard active, and dropping it delivers the task.
        self.active = false;
        match result {
            Ok(()) => self.what.sent(),
            Err(ManagerClosed) => self.what.manager_closed(),
        }
    }
}

impl<D: Deregistration> Drop for DeregisterGuard<D> {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        match self.sender.try_send(self.what.task()) {
            Ok(()) => self.what.dropped_sent(),
            Err(TrySendError::Closed(_)) => self.what.dropped_manager_closed(),
            Err(TrySendError::Full(task)) => {
                self.what.dropped_queue_full();
                match tokio::runtime::Handle::try_current() {
                    Ok(handle) => {
                        let sender = self.sender.clone();
                        handle.spawn(async move {
                            if sender.send(task).await.is_err() {
                                D::deferred_manager_closed();
                            }
                        });
                    }
                    Err(_) => D::no_runtime(),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::future::{Future, poll_fn};
    use std::task::Poll;
    use std::time::Duration;

    use pb_mapper_core::conn_id::RemoteConnId;

    use super::*;
    use crate::manager::task_channel;

    struct Probe;

    impl Deregistration for Probe {
        fn task(&self) -> ManagerTask {
            ManagerTask::DeRegisterClientConn {
                server_id: None,
                client_id: RemoteConnId::from(7),
            }
        }
        fn manager_closed(&self) {}
        fn dropped_manager_closed(&self) {}
        fn dropped_queue_full(&self) {}
        fn deferred_manager_closed() {}
        fn no_runtime() {}
    }

    fn is_deregistration(task: &ManagerTask) -> bool {
        matches!(task, ManagerTask::DeRegisterClientConn { client_id, .. } if *client_id == RemoteConnId::from(7))
    }

    #[tokio::test]
    async fn cancelled_pending_deregister_delivers_exactly_one_cleanup() {
        let (sender, mut receiver) = task_channel(1);
        sender.send(ManagerTask::SweepServerLeases).await.unwrap();
        let mut guard = DeregisterGuard::new(Probe, sender.clone());
        let mut deregister = Box::pin(guard.deregister());
        poll_fn(|cx| {
            assert!(deregister.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        assert!(matches!(
            receiver.recv().await.unwrap(),
            ManagerTask::SweepServerLeases
        ));
        assert!(
            receiver.is_empty(),
            "a pending send must not deliver before it is polled"
        );
        drop(deregister);
        drop(guard);
        let delivered = tokio::time::timeout(Duration::from_secs(1), receiver.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(is_deregistration(&delivered));
        let duplicate = tokio::time::timeout(Duration::from_millis(20), receiver.recv()).await;
        assert!(
            duplicate.is_err(),
            "cancelling cleanup after channel delivery emitted a second deregistration"
        );
    }

    #[tokio::test]
    async fn a_guard_dropped_into_a_full_queue_still_deregisters() {
        let (sender, mut receiver) = task_channel(1);
        sender.send(ManagerTask::Shutdown).await.unwrap();

        drop(DeregisterGuard::new(Probe, sender.clone()));

        assert!(matches!(
            receiver.recv().await.unwrap(),
            ManagerTask::Shutdown
        ));
        let task = tokio::time::timeout(Duration::from_millis(200), receiver.recv())
            .await
            .expect("deregister task was lost when manager queue was full")
            .unwrap();
        assert!(is_deregistration(&task));
    }
}
