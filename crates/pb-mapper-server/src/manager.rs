use snafu::{ResultExt, Snafu};
use tracing::instrument;

use pb_mapper_core::conn_id::{ConnId, ConnIdProvider, ConnIdTrait};

/// The manager owns this rather than the core error enum: it is the only thing
/// that waits on a task channel, and it keeps `kanal` out of the bottom layer.
#[derive(Debug, Snafu)]
#[snafu(display("`TaskManager` fails while waiting for a task"))]
pub struct MngWaitForTaskError {
    source: kanal::ReceiveError,
}

type Result<T, E = MngWaitForTaskError> = std::result::Result<T, E>;

/// The [`ConnId::local_id`] of the server is the same as the client. and it is only generated
/// by the client. The [`ConnId::remote_id`] and [`ConnId::local_id`] of the client can be used to
/// uniquely identify a socket connection with that client on the server side. So for now, the
/// server-side local_id has no effect.
#[derive(Debug)]
pub struct ForwardMessage {
    pub from: ConnId,
    pub to: ConnId,
    pub msg: Vec<u8>,
}

pub type SenderChan<T> = kanal::AsyncSender<T>;
pub type ReceiverChan<T> = kanal::AsyncReceiver<T>;

/// hashmap for index(`ConnId`) to `SenderChan`
pub type ConnMap<K, V> = hashbrown::HashMap<K, SenderChan<V>>;

pub struct TaskManager<ManagerTaskType, ConnTaskType, ConnIdType, ConnIdProviderType> {
    conn_id_provider: ConnIdProviderType,
    manager_chan: (SenderChan<ManagerTaskType>, ReceiverChan<ManagerTaskType>),
    idle_conn_id_list: Vec<ConnIdType>,
    active_conn_map: ConnMap<ConnIdType, ConnTaskType>,
}

const DEFAULT_CHAN_CAP: usize = 1024;

impl<
    MangerChanType,
    ConnChanType,
    ConnIdType: ConnIdTrait,
    ConnIdProviderType: ConnIdProvider<ConnIdType>,
> TaskManager<MangerChanType, ConnChanType, ConnIdType, ConnIdProviderType>
{
    pub fn new(
        conn_id_provider: ConnIdProviderType,
    ) -> TaskManager<MangerChanType, ConnChanType, ConnIdType, ConnIdProviderType> {
        let manager_chan = kanal::bounded_async(DEFAULT_CHAN_CAP);
        Self {
            conn_id_provider,
            manager_chan,
            idle_conn_id_list: vec![],
            active_conn_map: ConnMap::new(),
        }
    }

    #[inline]
    pub async fn wait_for_task(&self) -> Result<MangerChanType> {
        self.manager_chan
            .1
            .recv()
            .await
            .context(MngWaitForTaskSnafu)
    }

    pub fn get_task_sender(&self) -> SenderChan<MangerChanType> {
        self.manager_chan.0.clone()
    }

    #[inline]
    pub fn get_conn_sender_chan(&self, conn_id: &ConnIdType) -> Option<SenderChan<ConnChanType>> {
        self.active_conn_map.get(conn_id).map(|v| v.to_owned())
    }

    #[inline]
    pub fn active_conn_count(&self) -> usize {
        self.active_conn_map.len()
    }

    #[inline]
    pub fn idle_conn_count(&self) -> usize {
        self.idle_conn_id_list.len()
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
    pub fn drop_conn_sender(&mut self, conn_id: ConnIdType) -> bool {
        self.active_conn_map.remove(&conn_id).is_some()
    }

    /// Return an ID to the idle pool, if it is not already there.
    #[inline]
    pub fn recycle_conn_id(&mut self, conn_id: ConnIdType) {
        if !self.idle_conn_id_list.contains(&conn_id) {
            self.idle_conn_id_list.push(conn_id);
        }
    }

    pub fn get_conn_id(
        &mut self,
        action_server_ids: impl Iterator<Item = ConnIdType>,
    ) -> ConnIdType {
        let active_server_ids = action_server_ids.collect::<Vec<_>>();
        while let Some(conn_id) = self.idle_conn_id_list.pop() {
            if self.conn_id_provider.is_valid_id(&conn_id)
                && !self.active_conn_map.contains_key(&conn_id)
                && !active_server_ids.contains(&conn_id)
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

        self.conn_id_provider.get_next_id()
    }

    #[instrument(skip(self, sender))]
    pub fn sign_up_conn_sender(&mut self, conn_id: ConnIdType, sender: SenderChan<ConnChanType>) {
        if !self.conn_id_provider.is_valid_id(&conn_id) {
            tracing::warn!("invalid conn id,may be index out of bound,we do nothing");
            return;
        }
        if self.active_conn_map.insert(conn_id, sender).is_some() {
            tracing::error!("conn id {conn_id} is already active; replacing sender");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fmt;

    use super::*;

    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
    struct TestConnId(u32);

    impl From<TestConnId> for u32 {
        fn from(value: TestConnId) -> Self {
            value.0
        }
    }

    impl fmt::Display for TestConnId {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "TestConnId({})", self.0)
        }
    }

    impl ConnIdTrait for TestConnId {}

    struct TestConnIdProvider {
        next_id: TestConnId,
    }

    impl TestConnIdProvider {
        fn new(next_id: u32) -> Self {
            Self {
                next_id: TestConnId(next_id),
            }
        }
    }

    impl ConnIdProvider<TestConnId> for TestConnIdProvider {
        fn get_next_id(&mut self) -> TestConnId {
            let ret = self.next_id;
            self.next_id.0 += 1;
            ret
        }

        fn is_valid_id(&self, id: &TestConnId) -> bool {
            id < &self.next_id
        }
    }

    fn manager() -> TaskManager<(), (), TestConnId, TestConnIdProvider> {
        TaskManager::new(TestConnIdProvider::new(1))
    }

    fn sender() -> SenderChan<()> {
        kanal::bounded_async(1).0
    }

    #[test]
    fn dropping_a_sender_reports_whether_it_was_registered() {
        let mut manager = manager();
        manager.sign_up_conn_sender(TestConnId(0), sender());

        assert!(manager.drop_conn_sender(TestConnId(0)));
        assert!(!manager.drop_conn_sender(TestConnId(0)));
    }

    /// The reason the two steps are separate: a retirement drops the sender while
    /// the socket task is still unwinding, and its ID must not be handed out again
    /// until that task's deregistration arrives.
    #[test]
    fn a_dropped_sender_does_not_release_its_id_until_recycled() {
        let mut manager = manager();
        manager.sign_up_conn_sender(TestConnId(0), sender());

        assert!(manager.drop_conn_sender(TestConnId(0)));
        assert_eq!(
            manager.get_conn_id(std::iter::empty()),
            TestConnId(1),
            "a retired id is not reused while its socket task may still deregister"
        );

        manager.recycle_conn_id(TestConnId(0));
        assert_eq!(manager.get_conn_id(std::iter::empty()), TestConnId(0));
    }

    #[test]
    fn recycling_an_id_twice_queues_it_once() {
        let mut manager = manager();
        manager.sign_up_conn_sender(TestConnId(0), sender());
        assert!(manager.drop_conn_sender(TestConnId(0)));

        manager.recycle_conn_id(TestConnId(0));
        manager.recycle_conn_id(TestConnId(0));

        assert_eq!(manager.idle_conn_count(), 1);
        assert_eq!(manager.get_conn_id(std::iter::empty()), TestConnId(0));
        assert_eq!(manager.get_conn_id(std::iter::empty()), TestConnId(1));
    }

    #[test]
    fn idle_id_is_not_reused_while_active() {
        let mut manager = manager();
        manager.sign_up_conn_sender(TestConnId(0), sender());
        manager.idle_conn_id_list.push(TestConnId(0));

        assert_eq!(manager.get_conn_id(std::iter::empty()), TestConnId(1));
        assert!(manager.get_conn_sender_chan(&TestConnId(0)).is_some());
    }

    #[test]
    fn exposes_active_and_idle_connection_counts() {
        let mut manager = manager();
        manager.sign_up_conn_sender(TestConnId(0), sender());

        assert_eq!(manager.active_conn_count(), 1);
        assert_eq!(manager.idle_conn_count(), 0);

        assert!(manager.drop_conn_sender(TestConnId(0)));
        manager.recycle_conn_id(TestConnId(0));

        assert_eq!(manager.active_conn_count(), 0);
        assert_eq!(manager.idle_conn_count(), 1);
    }
}
