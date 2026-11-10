//! The routing manager's work queue, and its registry of connection IDs and
//! the mailboxes they are reached through.

use std::fmt;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use pb_mapper_core::conn_id::RemoteConnId;
use tracing::instrument;

use crate::{ConnTask, ManagerTask};

pub type SenderChan<T> = kanal::AsyncSender<T>;
pub type ReceiverChan<T> = kanal::AsyncReceiver<T>;

/// The routing manager's queue is closed: the relay has stopped routing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ManagerClosed;

impl fmt::Display for ManagerClosed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("routing manager queue closed")
    }
}

impl std::error::Error for ManagerClosed {}

/// Why [`TaskSender::try_send`] did not queue a task, with the task handed back.
pub enum TrySendError {
    /// The queue is at capacity; the task can still be sent by waiting.
    Full(ManagerTask),
    /// The manager has stopped and will never serve the task.
    Closed(ManagerTask),
}

impl fmt::Debug for TrySendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Full(_) => f.write_str("Full(..)"),
            Self::Closed(_) => f.write_str("Closed(..)"),
        }
    }
}

struct Envelope {
    task: ManagerTask,
    scope: Option<Arc<AtomicBool>>,
}

/// Bounded manager mailbox with an unambiguous, cancellation-safe send boundary.
#[derive(Clone)]
pub struct TaskSender {
    sender: tokio::sync::mpsc::Sender<Envelope>,
    scope: Option<Arc<AtomicBool>>,
}

impl fmt::Debug for TaskSender {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TaskSender")
            .field("connection_scoped", &self.scope.is_some())
            .finish()
    }
}

impl TaskSender {
    /// A fresh connection incarnation; clones share its one-time cleanup fence.
    pub(crate) fn for_connection(&self) -> Self {
        Self {
            sender: self.sender.clone(),
            scope: Some(Arc::new(AtomicBool::new(true))),
        }
    }

    pub async fn send(&self, task: ManagerTask) -> Result<(), ManagerClosed> {
        self.sender
            .send(Envelope {
                task,
                scope: self.scope.clone(),
            })
            .await
            .map_err(|_| ManagerClosed)
    }

    #[expect(
        clippy::result_large_err,
        reason = "the task is handed back so the caller can send it again by waiting"
    )]
    pub fn try_send(&self, task: ManagerTask) -> Result<(), TrySendError> {
        self.sender
            .try_send(Envelope {
                task,
                scope: self.scope.clone(),
            })
            .map_err(|error| match error {
                tokio::sync::mpsc::error::TrySendError::Full(message) => {
                    TrySendError::Full(message.task)
                }
                tokio::sync::mpsc::error::TrySendError::Closed(message) => {
                    TrySendError::Closed(message.task)
                }
            })
    }
}

pub(crate) struct TaskReceiver(tokio::sync::mpsc::Receiver<Envelope>);

impl TaskReceiver {
    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The next task from a live connection incarnation, or `None` once every
    /// sender is gone.
    pub(crate) async fn recv(&mut self) -> Option<ManagerTask> {
        while let Some(message) = self.0.recv().await {
            if let Some(scope) = message.scope {
                let valid = if message.task.ends_connection() {
                    scope.swap(false, Ordering::AcqRel)
                } else {
                    scope.load(Ordering::Acquire)
                };
                if !valid {
                    tracing::debug!(
                        event = "stale_connection_task_ignored",
                        "ignored a task from a closed connection incarnation"
                    );
                    continue;
                }
            }
            return Some(message.task);
        }
        None
    }
}

pub(crate) fn task_channel(capacity: usize) -> (TaskSender, TaskReceiver) {
    let (sender, receiver) = tokio::sync::mpsc::channel(capacity);
    (
        TaskSender {
            sender,
            scope: None,
        },
        TaskReceiver(receiver),
    )
}

const MANAGER_QUEUE_CAPACITY: usize = 1024;

/// Hands out connection IDs, and routes each registered ID to its mailbox.
pub struct TaskManager {
    /// The next ID never handed out; every ID below it has been.
    next_conn_id: RemoteConnId,
    /// Held for the manager's whole life, so its receiver never reports closed.
    sender: TaskSender,
    receiver: TaskReceiver,
    idle_conn_ids: Vec<RemoteConnId>,
    active_conn_map: hashbrown::HashMap<RemoteConnId, SenderChan<ConnTask>>,
}

impl Default for TaskManager {
    fn default() -> Self {
        Self::new()
    }
}

impl TaskManager {
    pub fn new() -> Self {
        let (sender, receiver) = task_channel(MANAGER_QUEUE_CAPACITY);
        Self {
            next_conn_id: RemoteConnId::default(),
            sender,
            receiver,
            idle_conn_ids: Vec::new(),
            active_conn_map: hashbrown::HashMap::new(),
        }
    }

    /// The next task to route.
    ///
    /// `None` would mean every sender is gone, which cannot happen while the
    /// manager holds one of its own.
    #[inline]
    pub async fn wait_for_task(&mut self) -> Option<ManagerTask> {
        self.receiver.recv().await
    }

    pub fn get_task_sender(&self) -> TaskSender {
        self.sender.clone()
    }

    #[inline]
    pub fn get_conn_sender_chan(&self, conn_id: &RemoteConnId) -> Option<SenderChan<ConnTask>> {
        self.active_conn_map.get(conn_id).map(|v| v.to_owned())
    }

    #[inline]
    pub fn has_conn_sender(&self, conn_id: &RemoteConnId) -> bool {
        self.active_conn_map.contains_key(conn_id)
    }

    #[inline]
    pub fn active_conn_count(&self) -> usize {
        self.active_conn_map.len()
    }

    #[inline]
    pub fn idle_conn_count(&self) -> usize {
        self.idle_conn_ids.len()
    }

    /// Stop routing to a connection, without returning its ID to the idle pool.
    ///
    /// Dropping the sender and recycling the ID are separate steps because they
    /// become safe at different moments. The sender must go the instant the
    /// connection stops being usable — a retirement, a subscribe that found it
    /// unreachable — but the socket task is still running then, and still owns a
    /// guard that will deregister when it unwinds. Recycling at that point would
    /// let a freshly accepted socket take the ID and register under it, and the
    /// old task's deregistration, which matches on ID alone, would unwind the
    /// replacement instead.
    ///
    /// So the ID stays out of circulation until [`Self::recycle_conn_id`], which
    /// the manager calls when the guard's deregistration finally arrives — the one
    /// point at which nothing will speak for that ID again. Every registered
    /// connection produces exactly one such task, so no ID is stranded.
    #[inline]
    pub fn drop_conn_sender(&mut self, conn_id: RemoteConnId) -> bool {
        self.active_conn_map.remove(&conn_id).is_some()
    }

    /// Return an ID to the idle pool, if it is not already there.
    #[inline]
    pub fn recycle_conn_id(&mut self, conn_id: RemoteConnId) {
        if !self.idle_conn_ids.contains(&conn_id) {
            self.idle_conn_ids.push(conn_id);
        }
    }

    /// An ID for a newly accepted connection: an idle one when one is free,
    /// otherwise one never handed out. `registered` names the IDs the service
    /// map still holds, which are never reused.
    pub fn get_conn_id(
        &mut self,
        registered: impl Iterator<Item = RemoteConnId> + Clone,
    ) -> RemoteConnId {
        while let Some(conn_id) = self.idle_conn_ids.pop() {
            if self.is_issued(&conn_id)
                && !self.active_conn_map.contains_key(&conn_id)
                && !registered.clone().any(|id| id == conn_id)
            {
                return conn_id;
            } else {
                tracing::warn!(
                    "You got invalid or active `conn_id` from `idle_list`!
                That may nerver happen!
                 We will generate a new connId by default"
                );
            }
        }

        let conn_id = self.next_conn_id;
        self.next_conn_id += 1;
        conn_id
    }

    #[instrument(skip(self, sender))]
    pub fn sign_up_conn_sender(&mut self, conn_id: RemoteConnId, sender: SenderChan<ConnTask>) {
        if !self.is_issued(&conn_id) {
            tracing::warn!("invalid conn id,may be index out of bound,we do nothing");
            return;
        }
        if self.active_conn_map.insert(conn_id, sender).is_some() {
            tracing::error!("conn id {conn_id} is already active; replacing sender");
        }
    }

    fn is_issued(&self, conn_id: &RemoteConnId) -> bool {
        conn_id < &self.next_conn_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sender() -> SenderChan<ConnTask> {
        kanal::bounded_async(1).0
    }

    #[test]
    fn recycling_an_id_twice_queues_it_once() {
        let mut manager = TaskManager::new();
        let first = manager.get_conn_id(std::iter::empty());
        manager.sign_up_conn_sender(first, sender());
        assert!(manager.drop_conn_sender(first));

        manager.recycle_conn_id(first);
        manager.recycle_conn_id(first);

        assert_eq!(manager.idle_conn_count(), 1);
        assert_eq!(manager.get_conn_id(std::iter::empty()), first);
        assert_eq!(
            manager.get_conn_id(std::iter::empty()),
            RemoteConnId::from(1)
        );
    }

    #[test]
    fn idle_id_is_not_reused_while_active() {
        let mut manager = TaskManager::new();
        let active = manager.get_conn_id(std::iter::empty());
        manager.sign_up_conn_sender(active, sender());
        manager.idle_conn_ids.push(active);

        assert_eq!(
            manager.get_conn_id(std::iter::empty()),
            RemoteConnId::from(1)
        );
        assert!(manager.get_conn_sender_chan(&active).is_some());
    }

    #[tokio::test]
    async fn old_connection_cannot_clean_up_or_update_a_reused_id() {
        let (sender, mut receiver) = task_channel(8);
        let old = sender.for_connection();
        let replacement = sender.for_connection();
        let cleanup = |key: &str| ManagerTask::DeRegisterServerConn {
            key: key.into(),
            conn_id: 7_u32.into(),
        };
        old.send(cleanup("old")).await.unwrap();
        assert!(matches!(
            receiver.recv().await.unwrap(),
            ManagerTask::DeRegisterServerConn { .. }
        ));
        // The routing ID may now be recycled. Its new scope must remain usable.
        old.send(cleanup("old")).await.unwrap();
        old.send(ManagerTask::ServerConnActivity {
            key: "old".into(),
            conn_id: 7_u32.into(),
        })
        .await
        .unwrap();
        replacement.send(cleanup("new")).await.unwrap();
        assert!(
            matches!(receiver.recv().await.unwrap(), ManagerTask::DeRegisterServerConn { key, .. } if key.as_ref() == "new")
        );
        assert!(receiver.is_empty());
    }
}
