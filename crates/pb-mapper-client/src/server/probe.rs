use std::sync::Arc;

use super::control::ControlRegistration;
use crate::endpoint::RelayEndpoint;
use pb_mapper_core::checksum::Credential;
use pb_mapper_core::config::{ResolvedAddrs, registration_probe_timeout};
use pb_mapper_protocol::command::{PbConnStatusReq, PbConnStatusResp};

#[derive(Debug)]
pub(super) enum RegistrationProbeResult {
    Present,
    Missing,
    Failed(String),
}

pub(super) async fn probe_remote_registration(
    remote_addr: ResolvedAddrs,
    endpoint: RelayEndpoint,
    key: Arc<str>,
    registration: ControlRegistration,
    namespace: Option<u64>,
    credential: Credential,
) -> RegistrationProbeResult {
    let timeout = registration_probe_timeout();
    let result = tokio::time::timeout(timeout, async {
        let mut stream = endpoint
            .connect_addresses(&remote_addr, credential, &mut None)
            .await
            .map_err(|e| format!("connect remote status stream failed: {e}"))?;
        crate::client::status::get_status_with_credential(
            &mut stream,
            PbConnStatusReq::Service {
                direct_query: None,
                key: key.to_string(),
            },
            namespace,
            &credential,
        )
        .await
        .map_err(|e| {
            format!(
                "get service status failed: {}",
                snafu::Report::from_error(e)
            )
        })
    })
    .await;

    let status = match result {
        Ok(Ok(status)) => status,
        Ok(Err(reason)) => return RegistrationProbeResult::Failed(reason),
        Err(_) => {
            return RegistrationProbeResult::Failed(format!(
                "status probe timed out after {timeout:?}"
            ));
        }
    };

    match status {
        PbConnStatusResp::Service { connections, .. } => {
            let present = connections.iter().any(|conn| {
                conn.conn_id == registration.conn_id
                    && conn.generation == registration.generation
                    && conn.healthy
            });
            if present {
                RegistrationProbeResult::Present
            } else {
                RegistrationProbeResult::Missing
            }
        }
        PbConnStatusResp::Keys(keys) => {
            if keys.iter().any(|candidate| candidate == key.as_ref()) {
                RegistrationProbeResult::Present
            } else {
                RegistrationProbeResult::Missing
            }
        }
        other => RegistrationProbeResult::Failed(format!(
            "unexpected status response while probing registration: {other:?}"
        )),
    }
}
