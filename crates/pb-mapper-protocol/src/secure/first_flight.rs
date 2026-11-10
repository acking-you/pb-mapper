//! First-flight authentication, replay admission, and stale-root classification.

use pb_mapper_auth::{AuthContext, AuthFailure, AuthRuntime, KeyId};
use pb_mapper_core::checksum::AesKeyType;

use super::frame::{Direction, V2Material, derive_material, open_v2_payload};
use super::replay::{FirstFlightAdmit, ReplayGuard};
use super::{
    CONNECTION_SALT_LEN, CONNECTION_SALT_REPLAYED, ServerHeaderSession, ServerInitialError,
    SessionKeys, unix_seconds,
};

pub(super) enum FirstFlightWork {
    Live {
        key: AesKeyType,
        payload: Vec<u8>,
        error_session: ServerHeaderSession,
    },
    Stale(ServerInitialError),
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

#[expect(
    clippy::result_large_err,
    reason = "the Ok variant is as large, so boxing the error saves nothing"
)]
pub(super) fn evaluate_first_flight(
    auth: &AuthRuntime,
    replay: &parking_lot::Mutex<ReplayGuard>,
    key_id: KeyId,
    fingerprint: [u8; 32],
    work: FirstFlightWork,
) -> std::result::Result<(Vec<u8>, AuthContext), ServerInitialError> {
    let mut replay = replay.lock();
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
                FirstFlightAdmit::Replayed => Err(ServerInitialError::fail_key(
                    CONNECTION_SALT_REPLAYED,
                    "protocol-v2 connection salt was already accepted",
                    true,
                    key_id,
                )),
                FirstFlightAdmit::Limited => Err(ServerInitialError::fail_key(
                    "connection_admission_limited",
                    "this credential has opened too many new connections in the current window",
                    true,
                    key_id,
                )),
                FirstFlightAdmit::Unavailable => Err(ServerInitialError::fail_key(
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
        Direction::ClientToServer,
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

/// An unauthenticated v2 session: enough to answer the presenter.
pub(super) fn v2_session(key: AesKeyType, material: V2Material) -> ServerHeaderSession {
    ServerHeaderSession {
        framing_key: key,
        keys: SessionKeys::V2(material),
        context: None,
        _legacy_guard: None,
    }
}
