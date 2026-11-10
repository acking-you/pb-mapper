//! The one deregistration a connection task owes the routing manager.
//!
//! Every accepted connection must tell the manager when it ends, whether it
//! finishes, fails or is cancelled; the manager recycles the connection's ID
//! only then. [`DeregisterGuard`] sends that task exactly once: awaited on the
//! normal path, and from `Drop` otherwise, without blocking and without
//! needing a runtime. The debt starts as a [`ConnLease`], made by the accept
//! that takes the ID, and passes to the guard for the connection's role once
//! the connection has one.

use std::fmt;

use pb_mapper_core::conn_id::RemoteConnId;

use crate::manager::{ManagerClosed, TrySendError};
use crate::task::{ManagerTask, ManagerTaskSender};

/// An accepted connection's ID, owed back to the manager from the accept that
/// took it until the connection ends, however it ends.
///
/// A connection that registers nothing under its ID, such as one that never
/// authenticates or an administrator request, returns it bare when the lease
/// drops. One that does hands the debt to the guard for its role
/// ([`hand_to`](Self::hand_to)), which returns the ID along with what it
/// registered. A provider's stream socket travels to its subscriber with its
/// lease, and is returned by the subscriber's deregistration once the
/// subscriber takes it ([`transfer`](Self::transfer)), or by the lease if it
/// is dropped on the way.
pub struct ConnLease(DeregisterGuard<IdRelease>);

impl ConnLease {
    pub(crate) fn new(conn_id: RemoteConnId, sender: ManagerTaskSender) -> Self {
        Self(DeregisterGuard::new(IdRelease { conn_id }, sender))
    }

    pub(crate) fn conn_id(&self) -> RemoteConnId {
        self.0.what.conn_id
    }

    /// The connection now registers `what` under its ID: its guard owes the
    /// deregistration from here on, and this lease nothing.
    pub(crate) fn hand_to<D: Deregistration>(mut self, what: D) -> DeregisterGuard<D> {
        self.0.owed = Owed::Nothing;
        DeregisterGuard::new(what, self.0.sender.clone())
    }

    /// Another connection's deregistration returns this ID from here on, so
    /// this lease owes nothing. The ID is for that deregistration to name.
    pub(crate) fn transfer(mut self) -> RemoteConnId {
        self.0.owed = Owed::Nothing;
        self.conn_id()
    }

    /// Return the ID and wait until the manager has it.
    pub(crate) async fn release(mut self) {
        self.0.deregister().await;
    }
}

impl fmt::Debug for ConnLease {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("ConnLease").field(&self.conn_id()).finish()
    }
}

/// A connection that registered nothing under its ID: only the ID goes back.
pub(crate) struct IdRelease {
    conn_id: RemoteConnId,
}

impl Deregistration for IdRelease {
    fn task(&self) -> ManagerTask {
        ManagerTask::DeRegisterClientConn {
            server_id: None,
            client_id: self.conn_id,
        }
    }

    fn manager_closed(&self) {}

    fn dropped_manager_closed(&self) {}

    fn dropped_queue_full(&self) {
        tracing::warn!(
            "manager queue is full; defer returning connection id {}",
            self.conn_id
        );
    }

    fn deferred_manager_closed() {}

    fn no_runtime() {
        tracing::warn!("cannot defer returning a connection id: no Tokio runtime is available");
    }
}

/// What one kind of connection deregisters, and what it logs at each step.
///
/// The log calls stay with each kind of connection, so their text and target do
/// not depend on this module.
pub(crate) trait Deregistration: 'static {
    /// A task releasing what the manager keeps for this connection besides its
    /// ID, sent before [`task`](Self::task). It has to go first: `task` ends the
    /// connection's scope, and the manager discards whatever the scope sends
    /// after that.
    fn release(&self) -> Option<ManagerTask> {
        None
    }
    /// The task that ends the connection. Built afresh for every attempt rather
    /// than moved into a send future, which a cancelled send would drop along
    /// with the task.
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
    owed: Owed,
}

/// What a guard still owes the manager.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Owed {
    /// The release, if the kind has one, then the task.
    All,
    /// The task; the release was delivered.
    Task,
    Nothing,
}

impl<D: Deregistration> DeregisterGuard<D> {
    fn new(what: D, sender: ManagerTaskSender) -> Self {
        Self {
            what,
            sender,
            owed: Owed::All,
        }
    }

    /// What this guard will deregister, for a connection that learns more
    /// about itself after the guard was armed.
    pub(crate) fn what_mut(&mut self) -> &mut D {
        &mut self.what
    }

    /// Send the deregistration and wait until the manager has it.
    pub(crate) async fn deregister(&mut self) {
        if self.owed == Owed::All {
            if let Some(release) = self.what.release()
                && self.sender.send(release).await.is_err()
            {
                self.owed = Owed::Nothing;
                self.what.manager_closed();
                return;
            }
            self.owed = Owed::Task;
        }
        if self.owed == Owed::Nothing {
            return;
        }
        let result = self.sender.send(self.what.task()).await;
        // Cleared only once the send has finished: a send cancelled while it
        // waits leaves the task owed, and dropping the guard delivers it.
        self.owed = Owed::Nothing;
        match result {
            Ok(()) => self.what.sent(),
            Err(ManagerClosed) => self.what.manager_closed(),
        }
    }
}

impl<D: Deregistration> Drop for DeregisterGuard<D> {
    fn drop(&mut self) {
        let release = match self.owed {
            Owed::Nothing => return,
            Owed::All => self.what.release(),
            Owed::Task => None,
        };
        let mut owed = release.into_iter().chain([self.what.task()]);
        while let Some(task) = owed.next() {
            match self.sender.try_send(task) {
                Ok(()) => {}
                Err(TrySendError::Closed(_)) => {
                    self.what.dropped_manager_closed();
                    return;
                }
                Err(TrySendError::Full(task)) => {
                    self.what.dropped_queue_full();
                    match tokio::runtime::Handle::try_current() {
                        Ok(handle) => {
                            let sender = self.sender.clone();
                            // In order: the release must reach the manager
                            // before the task that ends the connection's scope.
                            handle.spawn(async move {
                                for task in std::iter::once(task).chain(owed) {
                                    if sender.send(task).await.is_err() {
                                        D::deferred_manager_closed();
                                        return;
                                    }
                                }
                            });
                        }
                        Err(_) => D::no_runtime(),
                    }
                    return;
                }
            }
        }
        self.what.dropped_sent();
    }
}

#[cfg(test)]
mod tests {
    use std::future::{Future, poll_fn};
    use std::task::Poll;
    use std::time::Duration;

    use pb_mapper_core::conn_id::RemoteConnId;

    use super::*;
    use crate::direct::DirectTask;
    use crate::manager::task_channel;

    /// Deregisters connection 7, releasing its direct state first when
    /// `release` is set.
    struct Probe {
        release: bool,
    }

    impl Deregistration for Probe {
        fn release(&self) -> Option<ManagerTask> {
            self.release.then(|| {
                ManagerTask::Direct(DirectTask::Disconnect {
                    connection: RemoteConnId::from(7),
                })
            })
        }
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
        let mut guard = DeregisterGuard::new(Probe { release: false }, sender.clone());
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
        // Let anything the drop deferred to a task run, so a second
        // deregistration on its way would be in the queue by now.
        for _ in 0..4 {
            tokio::task::yield_now().await;
        }
        assert!(
            receiver.is_empty(),
            "cancelling cleanup after channel delivery emitted a second deregistration"
        );
    }

    #[tokio::test]
    async fn a_guard_dropped_into_a_full_queue_still_deregisters() {
        let (sender, mut receiver) = task_channel(1);
        sender.send(ManagerTask::Shutdown).await.unwrap();

        drop(DeregisterGuard::new(
            Probe { release: false },
            sender.clone(),
        ));

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

    /// The manager discards what a connection's scope sends after the task
    /// that ends it, so a release overtaken by that task would be lost: a
    /// direct consumer left registered for good.
    #[tokio::test]
    async fn a_release_deferred_by_a_full_queue_still_precedes_the_task() {
        let (sender, mut receiver) = task_channel(1);
        sender.send(ManagerTask::Shutdown).await.unwrap();

        drop(DeregisterGuard::new(
            Probe { release: true },
            sender.clone(),
        ));

        assert!(matches!(
            receiver.recv().await.unwrap(),
            ManagerTask::Shutdown
        ));
        assert!(matches!(
            receiver.recv().await.unwrap(),
            ManagerTask::Direct(DirectTask::Disconnect { connection })
                if connection == RemoteConnId::from(7)
        ));
        assert!(is_deregistration(&receiver.recv().await.unwrap()));
    }
}
