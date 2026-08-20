//! First-flight authentication, replay admission, and stale-root classification.
use super::*;

pub(super) enum FirstFlightWork {
    Live {
        key: AesKeyType,
        payload: Vec<u8>,
        error_session: ServerHeaderSession,
    },
    Stale(ServerInitialError),
}

pub(super) fn first_flight_error(
    code: &'static str,
    message: impl Into<String>,
    retryable: bool,
    key_id: KeyId,
) -> ServerInitialError {
    ServerInitialError::fail_key(code, message, retryable, key_id)
}

fn reserved_error_session(
    replay: &mut ReplayGuard,
    fingerprint: &[u8; 32],
    session: ServerHeaderSession,
) -> Option<ServerHeaderSession> {
    matches!(
        replay.claim(fingerprint, unix_seconds()),
        FirstFlightAdmit::Fresh
    )
    .then_some(session)
}

#[allow(clippy::result_large_err)]
pub(super) fn evaluate_first_flight(
    auth: &AuthRuntime,
    replay: &std::sync::Mutex<ReplayGuard>,
    key_id: KeyId,
    fingerprint: [u8; 32],
    work: FirstFlightWork,
) -> std::result::Result<(Vec<u8>, AuthContext), ServerInitialError> {
    let mut replay = replay
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    match work {
        FirstFlightWork::Live {
            key,
            payload,
            error_session,
        } => {
            let context = auth
                .authenticate_presented(key_id, &key)
                .map_err(|failure| ServerInitialError {
                    failure,
                    response_session: reserved_error_session(
                        &mut replay,
                        &fingerprint,
                        error_session,
                    ),
                    presented_key_id: Some(key_id),
                })?;
            match replay.admit(key_id, &fingerprint, unix_seconds()) {
                FirstFlightAdmit::Fresh => Ok((payload, context)),
                FirstFlightAdmit::Replayed => Err(first_flight_error(
                    "connection_salt_replayed",
                    "protocol-v2 connection salt was already accepted",
                    true,
                    key_id,
                )),
                FirstFlightAdmit::Limited => Err(first_flight_error(
                    "connection_admission_limited",
                    "this credential has opened too many new connections in the current window",
                    true,
                    key_id,
                )),
                FirstFlightAdmit::Unavailable => Err(first_flight_error(
                    "connection_replay_store_unavailable",
                    "failed to persist first-flight replay admission",
                    true,
                    key_id,
                )),
            }
        }
        FirstFlightWork::Stale(mut error) => {
            if let Some(session) = error.response_session.take() {
                error.response_session = reserved_error_session(&mut replay, &fingerprint, session);
            }
            Err(error)
        }
    }
}

pub(super) fn stale_root_first_flight(
    auth: &AuthRuntime,
    key_id: KeyId,
    salt: [u8; CONNECTION_SALT_LEN],
    counter: u64,
    ciphertext: &[u8],
) -> Option<ServerInitialError> {
    let previous_key = auth.derive_previous_key(key_id)?;
    let previous_material = derive_material(key_id, &previous_key, salt).ok()?;
    let mut previous_ciphertext = ciphertext.to_vec();
    open_v2_payload(
        &previous_material,
        DIRECTION_CLIENT_TO_SERVER,
        counter,
        &mut previous_ciphertext,
    )
    .ok()?;
    let (code, message) = if key_id.is_admin() {
        (
            "administrator_key_invalid",
            "administrator credential does not match the active root key",
        )
    } else {
        (
            "temporary_key_rotated",
            "temporary credential was invalidated by administrator root rotation or auth-state reset",
        )
    };
    Some(ServerInitialError {
        failure: AuthFailure::new(code, message, false),
        response_session: Some(v2_session(previous_key, previous_material)),
        presented_key_id: Some(key_id),
    })
}

pub(super) fn v2_session(key: AesKeyType, material: V2Material) -> ServerHeaderSession {
    ServerHeaderSession {
        protocol: HeaderProtocol::V2,
        legacy_key: key,
        v2: Some(material),
        context: None,
        _legacy_guard: None,
    }
}

pub(super) fn session_without_context(session: &ServerHeaderSession) -> ServerHeaderSession {
    ServerHeaderSession {
        protocol: session.protocol,
        legacy_key: session.legacy_key,
        v2: session.v2.clone(),
        context: None,
        _legacy_guard: None,
    }
}
