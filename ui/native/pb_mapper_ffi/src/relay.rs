//! The relay as a whole, for the UI's Relay page: who this computer is to
//! the relay, the direct sessions it observes, and — with the administrator
//! key — every administrator operation, through the same SDK the CLI's
//! `pb-mapper admin` uses, so the two cannot drift.
//!
//! One export, [`pb_mapper_relay_call_json`], takes `{"op": …, …}` and answers
//! with the usual envelope, rather than a symbol per operation: the
//! operations share everything but their arguments, and the Dart side keeps
//! a typed method for each. Only the relay's address and keep-alive are read
//! under the state lock; the network call runs without it, so a slow relay
//! holds up nothing else.

use std::ffi::c_char;
use std::time::Duration;

use pb_mapper_auth::generate_admin_key;
use pb_mapper_client::sdk::{
    Admin, AuthStatusInfo, Client, ConnectionInfo, Error as SdkError, IssuedKey, KeyMetadata,
    LegacyProtocol, QUIC_UNAVAILABLE, RelayDiagnostics, RelayTransport, ServiceInfo,
    SessionObservation,
};
use pb_mapper_core::checksum::{Credential, get_process_credential, parse_credential};
use pb_mapper_core::config::resolve_pb_mapper_server_async;
use serde_json::{Value, json};

use crate::ctl::Origin;
use crate::error::CtlError;
use crate::events;
use crate::handle::PbMapperHandle;
use crate::response::{err_ctl, err_null_handle, ok_data, parse_c_string};

/// Long enough for an inventory paged across a large relay, short enough
/// that a relay that stopped answering is reported rather than waited on.
const CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// How long to wait when asking the relay which administrator key it
/// accepts, after a rotation whose outcome was lost.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// Runs one relay operation; see the module docs.
///
/// # Safety
///
/// `handle` must come from `pb_mapper_create` and not yet be destroyed, and
/// `request` must be a NUL-terminated UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pb_mapper_relay_call_json(
    handle: *mut PbMapperHandle,
    request: *const c_char,
) -> *mut c_char {
    if handle.is_null() {
        return err_null_handle();
    }
    let request = match parse_c_string(request, "request")
        .and_then(|raw| serde_json::from_str::<Value>(&raw).map_err(bad_request))
    {
        Ok(request) => request,
        Err(error) => return err_ctl(&error),
    };

    let handle = unsafe { &*handle };
    let state = handle.state.clone();
    let result = handle.runtime.block_on(async move {
        let config = state.lock().await.get_config_status().await;
        let credential = get_process_credential();
        // The key a rotation retires: the tunnels still holding it are the
        // ones to start again on the new key.
        let retired = credential.as_ref().ok().copied();
        let relay = Target {
            server: &config.server_address,
            keep_alive: config.keep_alive_enabled,
            transport: config.relay_transport,
        };
        let outcome = call(&relay, credential, &request).await?;
        let mut data = outcome.data;
        // A rotated administrator key is the only one that relay accepts
        // now: this computer switches to it, or every later call would fail.
        // Unless Settings moved it to another relay or credential while the
        // rotation ran — then the key is not this computer's to save, and
        // saving it would undo that change — or saving it fails. Either way
        // the rotation happened, so it is answered as one: `"saved"` tells
        // the page whether to keep the key for the person, and
        // `"saveError"` why it could not be saved. Once saved, `"restart"`
        // names this computer's tunnels on that relay: those still holding
        // the retired key, for the page to start again on the new one, and
        // those on a temporary credential the rotation erased, which only
        // a newly issued one can bring back.
        if let Some(key) = outcome.rotated_key {
            // Where that relay is, to know its tunnels by address as well as
            // by name. Resolved before the state is locked: it can be slow.
            let endpoint = resolve_pb_mapper_server_async(Some(&config.server_address))
                .await
                .ok();
            let mut state = state.lock().await;
            let mut save_error = None;
            let saved = state.get_config_status().await == config
                && match state
                    .update_config(
                        config.server_address.clone(),
                        config.keep_alive_enabled,
                        key,
                        config.relay_transport,
                    )
                    .await
                {
                    Ok(()) => true,
                    Err(error) => {
                        tracing::error!("Saving the rotated administrator key failed: {error}");
                        save_error = Some(error.to_string());
                        false
                    }
                };
            let restart = retired.filter(|_| saved).map(|retired| {
                state.tunnels_on(&config.server_address, endpoint.as_ref(), retired)
            });
            drop(state);
            if saved {
                events::emit(events::ChangeKind::Config, None, Origin::Ui);
            }
            if let Some(fields) = data.as_object_mut() {
                fields.insert("saved".to_owned(), saved.into());
                if let Some(error) = save_error {
                    fields.insert("saveError".to_owned(), error.into());
                }
                if let Some(tunnels) = restart {
                    fields.insert(
                        "restart".to_owned(),
                        json!({
                            "shares": tunnels.shares,
                            "connections": tunnels.connections,
                            "stranded": tunnels.stranded,
                        }),
                    );
                }
            }
        }
        Ok::<_, CtlError>(data)
    });
    match result {
        Ok(data) => ok_data(data),
        Err(error) => err_ctl(&error),
    }
}

/// What an operation answered, and the administrator key it rotated to, if
/// it rotated one.
struct Outcome {
    data: Value,
    rotated_key: Option<String>,
}

impl From<Value> for Outcome {
    fn from(data: Value) -> Self {
        Self {
            data,
            rotated_key: None,
        }
    }
}

fn op(request: &Value) -> Result<&str, CtlError> {
    request
        .get("op")
        .and_then(Value::as_str)
        .ok_or_else(|| CtlError::invalid_argument("The relay request has no `op`"))
}

fn bad_request(error: serde_json::Error) -> CtlError {
    CtlError::invalid_argument(format!("The relay request is not JSON: {error}"))
}

fn u64_arg(request: &Value, name: &str) -> Result<u64, CtlError> {
    request
        .get(name)
        .and_then(Value::as_u64)
        .ok_or_else(|| CtlError::invalid_argument(format!("`{name}` must be a whole number")))
}

fn opt_u64_arg(request: &Value, name: &str) -> Result<Option<u64>, CtlError> {
    match request.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(_) => u64_arg(request, name).map(Some),
    }
}

fn str_arg<'a>(request: &'a Value, name: &str) -> Result<&'a str, CtlError> {
    request
        .get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| CtlError::invalid_argument(format!("`{name}` must be text")))
}

/// The relay as Settings has it: where it is, and how it is reached.
struct Target<'a> {
    server: &'a str,
    keep_alive: bool,
    transport: RelayTransport,
}

impl Target<'_> {
    /// A client of this relay as `credential`. Clients of one relay and
    /// carrier share the process's connections to it, the tunnels' too.
    fn client(&self, credential: Credential) -> Result<Client, CtlError> {
        Client::from_credential(self.server, credential, self.keep_alive, None)
            .with_relay_transport(self.transport)
            .map_err(sdk_error)
    }
}

/// The operation itself, against `relay` with `credential`. Separate from
/// the export so a test can drive it against a relay of its own.
async fn call(
    relay: &Target<'_>,
    credential: Result<Credential, String>,
    request: &Value,
) -> Result<Outcome, CtlError> {
    let op = op(request)?;
    // Who this computer is comes from its own credential, not from the relay,
    // so it needs no address: the page says it before a relay is even set.
    if op == "credential" {
        return Ok(credential_json(&credential).into());
    }
    let server = relay.server;
    if server.trim().is_empty() {
        return Err(CtlError::invalid_argument("No relay address is set"));
    }
    let credential = credential.map_err(|error| {
        CtlError::invalid_argument(format!("This computer has no usable credential: {error}"))
    })?;
    let client = relay.client(credential)?;
    // Rotation keeps its own time: a timeout here could drop the new key
    // while the relay installs it.
    if op == "rotate_root_key" {
        let admin = client.admin().map_err(sdk_error)?;
        return rotate(relay, credential, &admin).await;
    }
    tokio::time::timeout(CALL_TIMEOUT, dispatch(&client, op, request))
        .await
        .map_err(|_| {
            CtlError::timeout(format!(
                "{server} did not finish `{op}` within {} seconds",
                CALL_TIMEOUT.as_secs()
            ))
        })?
}

/// Every operation but rotation, which [`call`] keeps apart.
async fn dispatch(client: &Client, op: &str, request: &Value) -> Result<Outcome, CtlError> {
    if op == "sessions" {
        let sessions = client.sessions().await.map_err(sdk_error)?;
        return Ok(Value::Array(sessions.iter().map(session_json).collect()).into());
    }
    // How this computer reaches the relay, sampled from the connections it
    // holds now: no question goes to the relay.
    if op == "link" {
        return Ok(link_json(&client.relay_diagnostics().await).into());
    }

    let admin = client.admin().map_err(sdk_error)?;
    let data = match op {
        "auth_status" => auth_status_json(&admin.auth_status().await.map_err(sdk_error)?),
        "services" => Value::Array(
            admin
                .list_services_all(None)
                .await
                .map_err(sdk_error)?
                .iter()
                .map(service_json)
                .collect(),
        ),
        "connections" => Value::Array(
            admin
                .list_connections_all(None)
                .await
                .map_err(sdk_error)?
                .iter()
                .map(connection_json)
                .collect(),
        ),
        "keys" => Value::Array(
            admin
                .list_keys_all()
                .await
                .map_err(sdk_error)?
                .iter()
                .map(key_json)
                .collect(),
        ),
        "key_issue" => {
            let ttl = Duration::from_secs(u64_arg(request, "ttlSeconds")?);
            let label = request
                .get("label")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|label| !label.is_empty())
                .map(str::to_owned);
            issued_json(&admin.issue_key(ttl, label).await.map_err(sdk_error)?)
        }
        "key_reveal" => issued_json(
            &admin
                .reveal_key(u64_arg(request, "keyId")?)
                .await
                .map_err(sdk_error)?,
        ),
        "key_renew" => issued_json(
            &admin
                .renew_key(
                    u64_arg(request, "keyId")?,
                    Duration::from_secs(u64_arg(request, "ttlSeconds")?),
                )
                .await
                .map_err(sdk_error)?,
        ),
        "key_revoke" => key_json(
            &admin
                .revoke_key(u64_arg(request, "keyId")?)
                .await
                .map_err(sdk_error)?,
        ),
        "key_gc" => json!({ "removed": admin.gc_keys().await.map_err(sdk_error)? }),
        "retire" => json!({
            "retired": admin
                .retire_connections(
                    opt_u64_arg(request, "keyId")?,
                    str_arg(request, "service")?.to_owned(),
                    opt_u64_arg(request, "connId")?
                        .map(u32::try_from)
                        .transpose()
                        .map_err(|_| CtlError::invalid_argument("`connId` is out of range"))?,
                )
                .await
                .map_err(sdk_error)?,
        }),
        "legacy_protocol" => {
            let allow = request
                .get("allow")
                .and_then(Value::as_bool)
                .ok_or_else(|| CtlError::invalid_argument("`allow` must be true or false"))?;
            admin
                .set_legacy_protocol(if allow {
                    LegacyProtocol::Allow
                } else {
                    LegacyProtocol::Deny
                })
                .await
                .map_err(sdk_error)?;
            json!({ "allow": allow })
        }
        "reset_auth_state" => {
            admin.reset_auth_state().await.map_err(sdk_error)?;
            json!({})
        }
        other => {
            return Err(CtlError::invalid_argument(format!(
                "Unknown relay operation `{other}`"
            )));
        }
    };
    Ok(data.into())
}

/// Rotate to a new administrator key, and hand it back to be saved as this
/// computer's credential.
///
/// The key is made here, before anything is sent, so nothing that cuts the
/// request short can lose it. When the reply is lost the relay may or may not
/// have switched, so it is asked, with each key in turn, which it accepts.
/// If it answers neither, the key comes back unconfirmed (`"confirmed":
/// false`) for the person to keep, and this computer stays on the old one.
async fn rotate(relay: &Target<'_>, old: Credential, admin: &Admin) -> Result<Outcome, CtlError> {
    let server = relay.server;
    let candidate = generate_admin_key();
    let reason =
        match tokio::time::timeout(CALL_TIMEOUT, admin.rotate_root_key(Some(candidate.clone())))
            .await
        {
            Ok(Ok(key)) => return Ok(rotated(key)),
            // Refused before the request left this computer: nothing changed.
            Ok(Err(
                error @ (SdkError::NotAdministrator
                | SdkError::InvalidConfig { .. }
                | SdkError::Address { .. }
                | SdkError::Connect { .. }),
            )) => return Err(sdk_error(error)),
            Ok(Err(error)) => error.to_string(),
            Err(_) => format!(
                "{server} did not answer within {} seconds",
                CALL_TIMEOUT.as_secs()
            ),
        };
    let new = parse_credential(&candidate).map_err(CtlError::internal)?;
    if accepts(relay, new).await {
        return Ok(rotated(candidate));
    }
    if accepts(relay, old).await {
        return Err(CtlError::protocol(format!(
            "The relay kept its administrator key: {reason}"
        )));
    }
    Ok(json!({ "adminKey": candidate, "confirmed": false, "reason": reason }).into())
}

fn rotated(key: String) -> Outcome {
    Outcome {
        data: json!({ "adminKey": key, "confirmed": true }),
        rotated_key: Some(key),
    }
}

/// Whether the relay takes `credential` as its administrator key now.
async fn accepts(relay: &Target<'_>, credential: Credential) -> bool {
    let Ok(admin) = relay
        .client(credential)
        .and_then(|client| client.admin().map_err(sdk_error))
    else {
        return false;
    };
    matches!(
        tokio::time::timeout(PROBE_TIMEOUT, admin.auth_status()).await,
        Ok(Ok(_))
    )
}

/// Why nothing can reach this relay over QUIC, in the words the window shows.
pub(crate) fn no_quic() -> CtlError {
    CtlError::server_unreachable(
        "The relay does not offer QUIC. Choose TCP in Settings, or give the relay a QUIC port",
    )
}

/// What an SDK refusal means to the window, as a coded sentence.
pub(crate) fn sdk_error(error: SdkError) -> CtlError {
    match error {
        SdkError::NotAdministrator => CtlError::invalid_argument(
            "This computer's credential is not the relay's administrator key",
        ),
        SdkError::InvalidConfig { message } => CtlError::invalid_argument(message),
        SdkError::Address { .. } => CtlError::invalid_address(error.to_string()),
        SdkError::Connect { .. } => CtlError::server_unreachable(error.to_string()),
        SdkError::TimedOut { .. } => CtlError::timeout(error.to_string()),
        SdkError::Remote { code, .. } if code == QUIC_UNAVAILABLE => no_quic(),
        SdkError::Remote { code, message, .. } => {
            CtlError::protocol(format!("The relay refused: {message} ({code})"))
        }
        other => CtlError::protocol(other.to_string()),
    }
}

/// The relay carrier as the Relay page shows it.
fn link_json(link: &RelayDiagnostics) -> Value {
    json!({
        "transport": link.transport,
        "connectionsCreated": link.connections_created,
        "streamsOpened": link.streams_opened,
        "streamsReused": link.streams_reused,
        "lastError": link.last_error,
        "connections": link.connections.iter().map(|c| json!({
            "id": c.connection_id,
            "peer": c.peer_addr.to_string(),
            "pooled": c.pooled,
            "rttUs": c.rtt_us,
            "congestionWindow": c.congestion_window,
            "mtu": c.mtu,
            "sentPackets": c.sent_packets,
            "lostPackets": c.lost_packets,
            "congestionEvents": c.congestion_events,
            "sentBytes": c.sent_bytes,
            "receivedBytes": c.received_bytes,
        })).collect::<Vec<_>>(),
    })
}

fn credential_json(credential: &Result<Credential, String>) -> Value {
    match credential {
        Ok(credential) if credential.is_admin() => json!({ "role": "admin" }),
        Ok(credential) => json!({ "role": "temporary", "keyId": credential.key_id() }),
        Err(error) => json!({ "role": "none", "error": error }),
    }
}

fn session_json(session: &SessionObservation) -> Value {
    let reports = [&session.consumer, &session.provider];
    let latest = reports
        .iter()
        .filter_map(|report| report.as_ref())
        .min_by_key(|report| report.age_ms);
    let id: String = session.id[..6].iter().map(|b| format!("{b:02x}")).collect();
    json!({
        "id": id,
        "service": session.service,
        "transport": if session.is_datagram { "UDP" } else { "TCP" },
        "path": latest.map(|report| format!("{:?}", report.report.path).to_lowercase()),
        "state": latest.map(|report| format!("{:?}", report.report.state).to_lowercase()),
        "reason": latest.and_then(|report| report.report.reason.clone()),
        "ageMs": latest.map(|report| report.age_ms),
        "consumerSeen": session.consumer.is_some(),
        "providerSeen": session.provider.is_some(),
        "stale": session.stale,
        "conflicting": session.conflicting,
        "restored": session.restored,
    })
}

fn auth_status_json(status: &AuthStatusInfo) -> Value {
    json!({
        "safeMode": status.safe_mode,
        "capacity": status.capacity,
        "activeKeys": status.active_keys,
        "expiredKeys": status.expired_keys,
        "revokedKeys": status.revoked_keys,
        "legacyAllowed": status.legacy_protocol == LegacyProtocol::Allow,
        "activeLegacyConnections": status.active_legacy_connections,
        "lastLegacyConnectionAt": status.last_legacy_connection_at,
        "authSuccesses": status.auth_successes,
        "authFailures": status.auth_failures,
        "serverInstanceId": status.server_instance_id,
    })
}

fn service_json(service: &ServiceInfo) -> Value {
    json!({
        "keyId": service.key_id,
        "namespace": service.namespace,
        "name": service.service_name,
        "transport": service.transport,
        "encrypted": service.codec_enabled,
        "connections": service.connection_count,
    })
}

fn connection_json(connection: &ConnectionInfo) -> Value {
    json!({
        "keyId": connection.key_id,
        "namespace": connection.namespace,
        "service": connection.service_name,
        "connId": connection.conn_id,
        "generation": connection.generation,
        "protocolVersion": connection.protocol_version,
        "healthy": connection.healthy,
        "transport": connection.transport,
        "encrypted": connection.codec_enabled,
        "lastRxAgeMs": connection.last_rx_age_ms,
    })
}

fn key_json(key: &KeyMetadata) -> Value {
    json!({
        "keyId": key.key_id,
        "state": key.state,
        "issuedAt": key.issued_at,
        "expiresAt": key.expires_at,
        "label": key.label,
    })
}

fn issued_json(key: &IssuedKey) -> Value {
    json!({
        "keyId": key.key_id,
        "state": key.state,
        "issuedAt": key.issued_at,
        "expiresAt": key.expires_at,
        "label": key.label,
        "credential": key.credential,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pb_mapper_core::test_support::PROCESS_CREDENTIAL_TEST_LOCK;
    use pb_mapper_testkit::{Relay, TEST_ADMIN_KEY};

    async fn run_over(
        relay: &Relay,
        transport: RelayTransport,
        credential: &str,
        request: Value,
    ) -> Result<Outcome, CtlError> {
        let server = relay.addr().to_string();
        let target = Target {
            server: &server,
            keep_alive: false,
            transport,
        };
        call(&target, parse_credential(credential), &request).await
    }

    async fn run(relay: &Relay, credential: &str, request: Value) -> Result<Value, CtlError> {
        run_over(relay, RelayTransport::Tcp, credential, request)
            .await
            .map(|outcome| outcome.data)
    }

    #[tokio::test]
    async fn an_administrator_runs_the_credential_lifecycle_over_the_wire() {
        // The relay installs the test credential process-wide, and its root
        // rotation replaces it; `state::tests` set and clear that same
        // credential under this lock. Held until the relay is gone.
        let _process_credential_guard = PROCESS_CREDENTIAL_TEST_LOCK.lock().await;
        let relay = Relay::start("ffi-relay-call").await;
        let admin = |request| run(&relay, TEST_ADMIN_KEY, request);

        let role = admin(json!({ "op": "credential" })).await.unwrap();
        assert_eq!(role["role"], "admin");

        let issued = admin(json!({ "op": "key_issue", "ttlSeconds": 3600, "label": "laptop" }))
            .await
            .unwrap();
        let key_id = issued["keyId"].as_u64().unwrap();
        let credential = issued["credential"].as_str().unwrap().to_owned();
        assert!(credential.starts_with("pbmt1_"), "{credential}");
        assert_eq!(issued["label"], "laptop");

        let keys = admin(json!({ "op": "keys" })).await.unwrap();
        assert!(
            keys.as_array()
                .unwrap()
                .iter()
                .any(|key| key["keyId"] == key_id && key["state"] == "active"),
            "{keys}"
        );

        let revealed = admin(json!({ "op": "key_reveal", "keyId": key_id }))
            .await
            .unwrap();
        assert_eq!(revealed["credential"], credential.as_str());

        let status = admin(json!({ "op": "auth_status" })).await.unwrap();
        assert!(status["activeKeys"].as_u64().unwrap() >= 1, "{status}");
        assert!(status["capacity"].as_u64().unwrap() > 0, "{status}");

        // The temporary credential is not an administrator: refused before
        // anything reaches the relay.
        let temporary = run(&relay, &credential, json!({ "op": "keys" }))
            .await
            .unwrap_err();
        assert!(
            temporary.to_string().contains("administrator"),
            "{temporary}"
        );
        let role = run(&relay, &credential, json!({ "op": "credential" }))
            .await
            .unwrap();
        assert_eq!(role["role"], "temporary");
        assert_eq!(role["keyId"], key_id);

        let revoked = admin(json!({ "op": "key_revoke", "keyId": key_id }))
            .await
            .unwrap();
        assert_eq!(revoked["state"], "revoked");

        for op in ["services", "connections", "sessions"] {
            let listed = admin(json!({ "op": op })).await.unwrap();
            assert!(listed.is_array(), "{op}: {listed}");
        }

        let unknown = admin(json!({ "op": "teleport" })).await.unwrap_err();
        assert!(unknown.to_string().contains("teleport"), "{unknown}");

        // Rotation hands back the key the relay now takes, to be saved; the
        // old one stops working.
        let rotated = run_over(
            &relay,
            RelayTransport::Tcp,
            TEST_ADMIN_KEY,
            json!({ "op": "rotate_root_key" }),
        )
        .await
        .unwrap();
        assert_eq!(rotated.data["confirmed"], true);
        let key = rotated.rotated_key.unwrap();
        assert_eq!(rotated.data["adminKey"], key.as_str());
        run(&relay, &key, json!({ "op": "auth_status" }))
            .await
            .unwrap();
        run(&relay, TEST_ADMIN_KEY, json!({ "op": "auth_status" }))
            .await
            .unwrap_err();
    }

    #[tokio::test]
    async fn a_relay_that_does_not_answer_is_reported() {
        let target = Target {
            server: "127.0.0.1:1",
            keep_alive: false,
            transport: RelayTransport::Tcp,
        };
        let error = call(
            &target,
            parse_credential(TEST_ADMIN_KEY),
            &json!({ "op": "keys" }),
        )
        .await
        .err()
        .unwrap();
        assert_eq!(error.code(), crate::error::ErrorCode::ServerUnreachable);
    }

    /// Over QUIC the administrator's questions share one connection, which
    /// the link reports with its measurements; a relay with no QUIC is
    /// refused in words that say so, not as an unreachable one.
    #[tokio::test]
    async fn calls_go_over_quic_and_the_link_says_how() {
        let _process_credential_guard = PROCESS_CREDENTIAL_TEST_LOCK.lock().await;
        let relay = Relay::start_with_quic("ffi-relay-quic").await;
        let quic = |request| run_over(&relay, RelayTransport::Quic, TEST_ADMIN_KEY, request);
        // What the window's own client does: holds the process's pool for
        // this relay while each call makes and drops a client of its own.
        let _held = Client::from_credential(
            relay.addr().to_string(),
            parse_credential(TEST_ADMIN_KEY).unwrap(),
            false,
            None,
        )
        .with_relay_transport(RelayTransport::Quic)
        .unwrap();

        quic(json!({ "op": "auth_status" })).await.unwrap();
        quic(json!({ "op": "keys" })).await.unwrap();
        let link = quic(json!({ "op": "link" })).await.unwrap().data;
        assert_eq!(link["transport"], "quic", "{link}");
        assert_eq!(link["connectionsCreated"], 1, "{link}");
        let connections = link["connections"].as_array().unwrap();
        assert_eq!(connections.len(), 1, "{link}");
        assert_eq!(connections[0]["pooled"], true, "{link}");
        assert!(connections[0]["rttUs"].as_u64().is_some(), "{link}");

        let tcp_only = Relay::start("ffi-relay-tcp-only").await;
        let refused = run_over(
            &tcp_only,
            RelayTransport::Quic,
            TEST_ADMIN_KEY,
            json!({ "op": "keys" }),
        )
        .await
        .err()
        .unwrap();
        assert!(
            refused.to_string().contains("does not offer QUIC"),
            "{refused}"
        );
    }
}
