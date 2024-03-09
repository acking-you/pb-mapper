use snafu::ResultExt;
use tracing::instrument;

use super::conn_id::{ConnId, ConnIdProvider, ConnIdTrait};
use super::error::{MngWaitForTaskSnafu, Result};

/// The [`ConnId::local_id`] of the server is the same as the client. and it is only generated
/// by the client. The [`ConnId::remote_id`] and [`ConnId::local_id`] of the client can be used to
/// uniquely identify a socket connection with that client on the server side. So for now, the
/// server-side local_id has no effect.
pub struct ForwardMessage {
    pub from: ConnId,
    pub to: ConnId,
    pub msg: Vec<u8>,
}

pub type SenderChan<T> = flume::Sender<T>;
pub type ReceiverChan<T> = flume::Receiver<T>;

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
        let manager_chan = flume::bounded(DEFAULT_CHAN_CAP);
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
            .recv_async()
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
    pub fn deregister_conn(&mut self, conn_id: ConnIdType) {
        self.active_conn_map.remove(&conn_id);
        self.idle_conn_id_list.push(conn_id);
    }

    pub fn active_conn_id_msg(&self) -> String {
        let mut ret = self
            .active_conn_map
            .iter()
            .map(|(&k, _)| -> u32 { k.into() })
            .collect::<Vec<_>>();
        ret.sort();
        let count = ret.len();
        let max = ret.last();
        format!(
            r#"
        count:{count},
        max:{max:?},
        list:{ret:?}
        "#
        )
    }

    pub fn idle_conn_id_msg(&self) -> String {
        let list = self
            .idle_conn_id_list
            .iter()
            .map(|&i| -> u32 { i.into() })
            .collect::<Vec<_>>();
        format!("list:{list:?}",)
    }

    pub fn get_conn_id(&mut self) -> ConnIdType {
        match self.idle_conn_id_list.pop() {
            Some(conn_id) => {
                if self.conn_id_provider.is_valid_id(&conn_id) {
                    conn_id
                } else {
                    tracing::warn!(
                        "You got invalid `conn_id` from `idle_list`! 
                    That may nerver happen!
                     We will generate a new connId by default"
                    );
                    self.conn_id_provider.get_next_id()
                }
            }
            None => self.conn_id_provider.get_next_id(),
        }
    }

    #[instrument(skip(self, sender))]
    pub fn sign_up_conn_sender(&mut self, conn_id: ConnIdType, sender: SenderChan<ConnChanType>) {
        if !self.conn_id_provider.is_valid_id(&conn_id) {
            tracing::warn!("invalid conn id,may be index out of bound,we do nothing");
            return;
        }
        self.active_conn_map.insert(conn_id, sender);
    }
}
