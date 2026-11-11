//! Read-only views for status and administrator requests, and the
//! administrator's retirement.

use std::time::Instant;

use pb_mapper_auth::ADMIN_SCHEMA_VERSION;
use pb_mapper_core::conn_id::RemoteConnId;
use pb_mapper_core::paging::paginate;
use pb_mapper_core::snafu_error_handle;
use pb_mapper_core::time::saturating_millis;
use pb_mapper_protocol::command::{
    AdminConnectionInfo, AdminConnectionPage, AdminServiceInfo, AdminServicePage, PbConnResponse,
    PbConnStatusReq, PbConnStatusResp,
};
use snafu::ResultExt;

use super::Router;
use crate::error::{MailboxError, TaskCenterSendStatusRespSnafu};
use crate::routing::{ImutableKey, ServerConnHealth, service_status_connections};
use crate::service_key::{compose_service_key, split_scoped_service_key};

impl Router {
    pub(super) fn on_status(
        &self,
        response_sender: tokio::sync::oneshot::Sender<PbConnResponse>,
        status: PbConnStatusReq,
        namespace: u64,
        conn_id: RemoteConnId,
    ) {
        let resp = match status {
            PbConnStatusReq::Sessions => PbConnResponse::Status(PbConnStatusResp::Sessions(
                self.direct_state.observations(namespace),
            )),
            PbConnStatusReq::RemoteId => {
                let scoped = self
                    .server_conn_map
                    .iter()
                    .filter(|(key, _)| split_scoped_service_key(key).0 == namespace)
                    .map(|(key, value)| (split_scoped_service_key(key).1, value))
                    .collect::<Vec<_>>();
                let registered_ids = scoped
                    .iter()
                    .flat_map(|(_, connections)| {
                        connections.iter().map(|connection| connection.conn_id)
                    })
                    .collect::<Vec<_>>();
                let client_ids = self
                    .pending_streams
                    .iter()
                    .filter_map(|(client_id, stream)| {
                        (split_scoped_service_key(&stream.key).0 == namespace).then_some(*client_id)
                    })
                    .collect::<Vec<_>>();
                PbConnResponse::Status(PbConnStatusResp::RemoteId {
                    server_map: format!("{scoped:?}"),
                    active: format!("registered={registered_ids:?}, clients={client_ids:?}"),
                    idle: "namespace scoped; use `pb-mapper admin connection list` for global inspection"
                        .to_string(),
                })
            }
            PbConnStatusReq::Keys => PbConnResponse::Status(PbConnStatusResp::Keys(
                self.server_conn_map
                    .keys()
                    .filter_map(|key| {
                        let (key_namespace, service_name) = split_scoped_service_key(key);
                        (key_namespace == namespace).then(|| service_name.to_string())
                    })
                    .collect(),
            )),
            PbConnStatusReq::Service { key, direct_query } => {
                let connections = service_status_connections(
                    &self.server_conn_map,
                    &compose_service_key(namespace, &key),
                );
                PbConnResponse::Status(PbConnStatusResp::Service {
                    key,
                    connections,
                    direct: direct_query
                        .filter(|q| *q)
                        .and(self.direct_state.capability.clone()),
                })
            }
        };
        snafu_error_handle!(
            response_sender
                .send(resp)
                .map_err(|_| MailboxError::Closed)
                .context(TaskCenterSendStatusRespSnafu { conn_id })
        );
    }

    pub(super) fn on_admin_service_list(
        &self,
        key_id: Option<u64>,
        page: u32,
        page_size: u16,
        response_sender: tokio::sync::oneshot::Sender<AdminServicePage>,
    ) {
        let mut all = self
            .server_conn_map
            .iter()
            .filter_map(|(key, connections)| {
                let (namespace, service_name) = split_scoped_service_key(key);
                if key_id.is_some_and(|key_id| key_id != namespace) {
                    return None;
                }
                let first = connections.first()?;
                Some(AdminServiceInfo {
                    key_id: namespace,
                    namespace,
                    service_name: service_name.to_string(),
                    transport: if first.is_datagram { "udp" } else { "tcp" }.to_string(),
                    codec_enabled: first.need_codec,
                    connection_count: connections.len() as u32,
                })
            })
            .collect::<Vec<_>>();
        all.sort_by(|left, right| {
            left.namespace
                .cmp(&right.namespace)
                .then_with(|| left.service_name.cmp(&right.service_name))
        });
        let (items, next_page) = paginate(&all, page, page_size);
        let _ = response_sender.send(AdminServicePage {
            schema_version: ADMIN_SCHEMA_VERSION,
            items,
            next_page,
        });
    }

    pub(super) fn on_admin_connection_list(
        &self,
        key_id: Option<u64>,
        page: u32,
        page_size: u16,
        response_sender: tokio::sync::oneshot::Sender<AdminConnectionPage>,
    ) {
        let now = Instant::now();
        let mut all = self
            .server_conn_map
            .iter()
            .flat_map(|(key, connections)| {
                let (namespace, service_name) = split_scoped_service_key(key);
                connections.iter().filter_map(move |connection| {
                    if key_id.is_some_and(|key_id| key_id != namespace) {
                        return None;
                    }
                    Some(AdminConnectionInfo {
                        key_id: namespace,
                        namespace,
                        service_name: service_name.to_string(),
                        conn_id: connection.conn_id.into(),
                        generation: connection.generation,
                        protocol_version: connection.protocol_version,
                        healthy: connection.health == ServerConnHealth::Healthy,
                        transport: if connection.is_datagram { "udp" } else { "tcp" }.to_string(),
                        codec_enabled: connection.need_codec,
                        last_rx_age_ms: saturating_millis(
                            now.duration_since(connection.last_rx_at),
                        ),
                    })
                })
            })
            .collect::<Vec<_>>();
        all.sort_by(|left, right| {
            left.namespace
                .cmp(&right.namespace)
                .then_with(|| left.service_name.cmp(&right.service_name))
                .then_with(|| left.conn_id.cmp(&right.conn_id))
        });
        let (items, next_page) = paginate(&all, page, page_size);
        let _ = response_sender.send(AdminConnectionPage {
            schema_version: ADMIN_SCHEMA_VERSION,
            items,
            next_page,
        });
    }

    pub(super) fn on_admin_connection_retire(
        &mut self,
        key: ImutableKey,
        conn_id: Option<RemoteConnId>,
        response_sender: tokio::sync::oneshot::Sender<u32>,
    ) {
        // Snapshot the targets before retiring any of them: `retire_server_conn`
        // mutates the very map that names them, and dropping the last connection
        // of a service removes its entry outright.
        let targets: Vec<RemoteConnId> = self
            .server_conn_map
            .get(&key)
            .map(|connections| {
                connections
                    .iter()
                    .map(|connection| connection.conn_id)
                    .filter(|candidate| conn_id.is_none_or(|wanted| wanted == *candidate))
                    .collect()
            })
            .unwrap_or_default();
        for target in &targets {
            self.retire_server_conn(&key, *target, "retired by administrator");
        }
        let _ = response_sender.send(u32::try_from(targets.len()).unwrap_or(u32::MAX));
    }
}
