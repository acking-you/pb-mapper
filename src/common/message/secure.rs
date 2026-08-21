//! Protocol-v2 single-flight authentication framing.
//!
//! The first client frame carries a clear-text routing prefix and an authenticated encrypted
//! request. It does not add a handshake or round trip. All following control messages on the
//! same TCP connection use independently derived directional keys and monotonically increasing
//! 64-bit counters.
//!
//! ```text
//! first flight: PBM2 | version | key id | timestamp+salt | counter | len | ciphertext
//!                         |           |                         |
//!                         |           +-> replay/time checks    +-> bounded AEAD open
//!                         +-> derive directional session keys
//!
//! continuation: counter(n+1) | len | ciphertext -> same authenticated session
//! ```
//!
//! This root module coordinates client/server sessions. Frame mechanics, replay admission,
//! log suppression, and protocol tests are isolated in focused child modules.

use std::sync::Arc;

use parking_lot::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rand::RngExt;
use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM};
use ring::digest::{digest, SHA256};
use ring::hkdf::{Salt, HKDF_SHA256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::{
    CodecMessageReader, CodecMessageWriter, DataLenType, MessageReader, MessageWriter, MAX_MSG_LEN,
};
use crate::common::auth::{
    AuthContext, AuthFailure, AuthRuntime, KeyId, LegacyConnectionGuard, ADMIN_KEY_ID,
};
use crate::common::checksum::{
    get_process_credential, valid_checksum_for_key, AesKeyType, Credential,
};
use crate::common::error::{Error, Result};
use crate::utils::codec::{Aes256GcmDeCodec, Aes256GcmEnCodec, Decryptor};

pub const PROTOCOL_V2_MAGIC: [u8; 4] = *b"PBM2";
pub const PROTOCOL_V2_VERSION: u8 = 2;
const CONNECTION_SALT_LEN: usize = 16;
const FIRST_PREFIX_REMAINDER_LEN: usize = 28;
const FRAME_HEADER_LEN: usize = 12;
const DIRECTION_CLIENT_TO_SERVER: u8 = 0;
const DIRECTION_SERVER_TO_CLIENT: u8 = 1;
const MAX_CONNECTION_CLOCK_SKEW_SECONDS: u64 = 5 * 60;
/// Each Bloom generation must outlive the accepted clock-skew interval. A
/// salt inserted at the end of a window with `ts = now + skew` stays valid
/// until `insert + 2*skew`, so one generation is `2 * skew`.
const DEFAULT_REPLAY_WINDOW_SECONDS: u64 = MAX_CONNECTION_CLOCK_SKEW_SECONDS.saturating_mul(2);
const DEFAULT_REPLAY_FILTER_BYTES: usize = 1024 * 1024;
const MAX_INITIAL_PLAINTEXT_LEN: u32 = 64 * 1024;
const MAX_INITIAL_CIPHERTEXT_LEN: u32 = MAX_INITIAL_PLAINTEXT_LEN + 16;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HeaderProtocol {
    Legacy,
    V2,
}

pub struct ClientHeaderSession {
    protocol: HeaderProtocol,
    legacy_key: AesKeyType,
    v2: Option<V2Material>,
}

impl ClientHeaderSession {
    /// New clients always use protocol v2, for both administrator and temporary credentials.
    pub fn from_process() -> Result<Self> {
        let credential = get_process_credential().map_err(protocol_error)?;
        Self::new_v2(&credential)
    }

    pub fn new_v2(credential: &Credential) -> Result<Self> {
        let mut salt = [0_u8; CONNECTION_SALT_LEN];
        salt[..8].copy_from_slice(&unix_seconds().to_be_bytes());
        let mut rng = rand::rng();
        for byte in &mut salt[8..] {
            *byte = rng.random();
        }
        let material =
            derive_material(KeyId::from_u64(credential.key_id()), credential.key(), salt)?;
        Ok(Self {
            protocol: HeaderProtocol::V2,
            legacy_key: *credential.key(),
            v2: Some(material),
        })
    }

    #[cfg(test)]
    pub fn new_legacy(key: AesKeyType) -> Self {
        Self {
            protocol: HeaderProtocol::Legacy,
            legacy_key: key,
            v2: None,
        }
    }

    pub fn protocol(&self) -> HeaderProtocol {
        self.protocol
    }

    pub async fn write_initial<T: AsyncWriteExt + Unpin>(
        &self,
        writer: &mut T,
        message: &[u8],
    ) -> Result<()> {
        match self.protocol {
            HeaderProtocol::Legacy => {
                legacy_message_writer(writer, &self.legacy_key, "legacy writer")?
                    .write_msg(message)
                    .await
            }
            HeaderProtocol::V2 => {
                let material = self.v2.as_ref().expect("v2 session material");
                writer
                    .write_all(&first_prefix(material))
                    .await
                    .map_err(|error| {
                        protocol_error(format!("failed to write v2 prefix: {error}"))
                    })?;
                V2MessageWriter::new(writer, material.clone(), DIRECTION_CLIENT_TO_SERVER, 0)?
                    .write_msg(message)
                    .await
            }
        }
    }

    pub fn response_reader<'a, T: AsyncReadExt + Unpin>(
        &self,
        reader: &'a mut T,
    ) -> Result<HeaderMessageReader<'a, T>> {
        match self.protocol {
            HeaderProtocol::Legacy => Ok(HeaderMessageReader::Legacy(legacy_message_reader(
                reader,
                &self.legacy_key,
                "legacy reader",
            )?)),
            HeaderProtocol::V2 => Ok(HeaderMessageReader::V2(V2MessageReader::new(
                reader,
                self.v2.as_ref().expect("v2 session material").clone(),
                DIRECTION_SERVER_TO_CLIENT,
                0,
            )?)),
        }
    }

    pub async fn exchange<T: AsyncReadExt + AsyncWriteExt + Unpin>(
        &self,
        stream: &mut T,
        payload: &[u8],
        timeout: Duration,
    ) -> Result<Vec<u8>> {
        match tokio::time::timeout(timeout, self.write_initial(stream, payload)).await {
            Ok(result) => result?,
            Err(_) => {
                return Err(protocol_error(format!(
                    "timed out writing first-flight request after {timeout:?}"
                )))
            }
        }
        let mut reader = self.response_reader(stream)?;
        let message = match tokio::time::timeout(timeout, reader.read_msg()).await {
            Ok(result) => result?,
            Err(_) => {
                return Err(protocol_error(format!(
                    "timed out reading first-flight response after {timeout:?}"
                )))
            }
        };
        Ok(message.to_vec())
    }

    pub fn continuation_writer<'a, T: AsyncWriteExt + Unpin>(
        &self,
        writer: &'a mut T,
    ) -> Result<HeaderMessageWriter<'a, T>> {
        match self.protocol {
            HeaderProtocol::Legacy => Ok(HeaderMessageWriter::Legacy(legacy_message_writer(
                writer,
                &self.legacy_key,
                "legacy writer",
            )?)),
            HeaderProtocol::V2 => Ok(HeaderMessageWriter::V2(V2MessageWriter::new(
                writer,
                self.v2.as_ref().expect("v2 session material").clone(),
                DIRECTION_CLIENT_TO_SERVER,
                1,
            )?)),
        }
    }
}

pub struct ServerHeaderSession {
    protocol: HeaderProtocol,
    legacy_key: AesKeyType,
    v2: Option<V2Material>,
    context: Option<AuthContext>,
    _legacy_guard: Option<LegacyConnectionGuard>,
}

impl fmt::Debug for ServerHeaderSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ServerHeaderSession")
            .field("protocol", &self.protocol)
            .field("key_id", &self.key_id())
            .field("authenticated", &self.context.is_some())
            .finish()
    }
}

impl ServerHeaderSession {
    pub fn protocol(&self) -> HeaderProtocol {
        self.protocol
    }

    pub fn framing_key(&self) -> AesKeyType {
        self.legacy_key
    }

    pub fn key_id(&self) -> KeyId {
        self.context
            .as_ref()
            .map(|context| context.key_id)
            .unwrap_or_else(|| {
                self.v2
                    .as_ref()
                    .map(|material| material.key_id)
                    .unwrap_or(ADMIN_KEY_ID)
            })
    }

    pub fn context(&self) -> Result<&AuthContext> {
        self.context
            .as_ref()
            .ok_or_else(|| protocol_error("server session was not authenticated"))
    }

    pub fn take_context(&mut self) -> Result<AuthContext> {
        self.context
            .take()
            .ok_or_else(|| protocol_error("server session was not authenticated"))
    }

    pub fn response_writer<'a, T: AsyncWriteExt + Unpin>(
        &self,
        writer: &'a mut T,
    ) -> Result<HeaderMessageWriter<'a, T>> {
        match self.protocol {
            HeaderProtocol::Legacy => Ok(HeaderMessageWriter::Legacy(legacy_message_writer(
                writer,
                &self.legacy_key,
                "legacy response writer",
            )?)),
            HeaderProtocol::V2 => Ok(HeaderMessageWriter::V2(V2MessageWriter::new(
                writer,
                self.v2.as_ref().expect("v2 session material").clone(),
                DIRECTION_SERVER_TO_CLIENT,
                0,
            )?)),
        }
    }

    pub fn continuation_reader<'a, T: AsyncReadExt + Unpin>(
        &self,
        reader: &'a mut T,
    ) -> Result<HeaderMessageReader<'a, T>> {
        match self.protocol {
            HeaderProtocol::Legacy => Ok(HeaderMessageReader::Legacy(legacy_message_reader(
                reader,
                &self.legacy_key,
                "legacy reader",
            )?)),
            HeaderProtocol::V2 => Ok(HeaderMessageReader::V2(V2MessageReader::new(
                reader,
                self.v2.as_ref().expect("v2 session material").clone(),
                DIRECTION_CLIENT_TO_SERVER,
                1,
            )?)),
        }
    }
}

pub struct ServerInitialMessage {
    pub payload: Vec<u8>,
    pub session: ServerHeaderSession,
    pub replay_fingerprint: Option<[u8; 32]>,
    pub client_timestamp: Option<u64>,
}

pub struct ServerInitialError {
    pub failure: AuthFailure,
    pub response_session: Option<ServerHeaderSession>,
    pub presented_key_id: Option<KeyId>,
}

impl ServerInitialError {
    fn new(failure: AuthFailure) -> Self {
        Self {
            failure,
            response_session: None,
            presented_key_id: None,
        }
    }

    fn fail(code: &'static str, message: impl Into<String>, retryable: bool) -> Self {
        Self::new(AuthFailure::new(code, message, retryable))
    }

    fn fail_key(
        code: &'static str,
        message: impl Into<String>,
        retryable: bool,
        key_id: KeyId,
    ) -> Self {
        Self {
            failure: AuthFailure::new(code, message, retryable),
            response_session: None,
            presented_key_id: Some(key_id),
        }
    }

    fn from_failure_key(failure: AuthFailure, key_id: KeyId) -> Self {
        Self {
            failure,
            response_session: None,
            presented_key_id: Some(key_id),
        }
    }
}

impl fmt::Debug for ServerInitialError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ServerInitialError")
            .field("failure", &self.failure)
            .field("has_response_session", &self.response_session.is_some())
            .field("presented_key_id", &self.presented_key_id)
            .finish()
    }
}

impl fmt::Display for ServerInitialError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.failure.fmt(formatter)
    }
}

impl std::error::Error for ServerInitialError {}

use std::fmt;

#[derive(Clone)]
pub struct ServerSecurity {
    auth: AuthRuntime,
    replay: Arc<Mutex<ReplayGuard>>,
    failure_logs: Arc<Mutex<FailureLogLimiter>>,
}

// `ServerInitialError` is 256 bytes, but the success type it is paired with,
// `ServerInitialMessage`, is 264 — so the `Result` is already sized by its `Ok`
// variant and boxing the error would buy an allocation for no size reduction.
#[allow(clippy::result_large_err)]
impl ServerSecurity {
    pub fn new(auth: AuthRuntime) -> Self {
        let replay_path = auth.config().state_dir.join("connection.replay");
        Self {
            auth,
            replay: Arc::new(Mutex::new(ReplayGuard::open(
                Some(replay_path),
                DEFAULT_REPLAY_FILTER_BYTES,
                DEFAULT_REPLAY_WINDOW_SECONDS,
            ))),
            failure_logs: Arc::new(Mutex::new(FailureLogLimiter::default())),
        }
    }

    pub fn auth(&self) -> &AuthRuntime {
        &self.auth
    }

    pub fn record_failure_log(
        &self,
        peer_ip: std::net::IpAddr,
        key_id: KeyId,
        reason: &str,
    ) -> FailureLogDecision {
        self.failure_logs
            .lock()
            .record(peer_ip, key_id, reason, unix_seconds())
    }

    pub async fn read_initial<T: AsyncReadExt + Unpin>(
        &self,
        reader: &mut T,
    ) -> std::result::Result<ServerInitialMessage, ServerInitialError> {
        let mut first = [0_u8; 4];
        reader.read_exact(&mut first).await.map_err(|error| {
            ServerInitialError::new(AuthFailure::new(
                "protocol_header_read_failed",
                format!("failed to read initial protocol header: {error}"),
                true,
            ))
        })?;
        if first == PROTOCOL_V2_MAGIC {
            self.read_v2_initial(reader).await
        } else {
            self.read_legacy_initial(reader, first).await
        }
    }

    async fn read_legacy_initial<T: AsyncReadExt + Unpin>(
        &self,
        reader: &mut T,
        checksum_bytes: [u8; 4],
    ) -> std::result::Result<ServerInitialMessage, ServerInitialError> {
        if !self.auth.legacy_protocol_allowed().unwrap_or(false) {
            return Err(ServerInitialError::fail(
                "legacy_protocol_disabled",
                "legacy protocol is disabled by the administrator",
                false,
            ));
        }
        let key = self.auth.admin_key().map_err(ServerInitialError::new)?;
        let checksum = u32::from_be_bytes(checksum_bytes);
        let datalen = reader.read_u32().await.map_err(|error| {
            ServerInitialError::fail(
                "legacy_frame_invalid",
                format!("failed to read legacy frame length: {error}"),
                true,
            )
        })?;
        if !valid_checksum_for_key(datalen, checksum, &key) || datalen > MAX_INITIAL_CIPHERTEXT_LEN
        {
            return Err(ServerInitialError::fail(
                "legacy_frame_invalid",
                "legacy frame checksum or length is invalid",
                false,
            ));
        }
        let mut encrypted = vec![0_u8; datalen as usize];
        reader.read_exact(&mut encrypted).await.map_err(|error| {
            ServerInitialError::fail(
                "legacy_frame_invalid",
                format!("failed to read legacy frame body: {error}"),
                true,
            )
        })?;
        let mut codec = Aes256GcmDeCodec::try_new(&key).map_err(|_| {
            ServerInitialError::fail(
                "legacy_decrypt_failed",
                "failed to initialize legacy decryption",
                false,
            )
        })?;
        let plain = codec.decrypt(&mut encrypted).map_err(|_| {
            ServerInitialError::fail(
                "legacy_decrypt_failed",
                "legacy credential or encrypted frame is invalid",
                false,
            )
        })?;
        let context = self
            .auth
            .authenticate_presented(ADMIN_KEY_ID, &key)
            .map_err(ServerInitialError::new)?;
        let legacy_guard = self
            .auth
            .record_legacy_connection()
            .map_err(ServerInitialError::new)?;
        Ok(ServerInitialMessage {
            payload: plain.to_vec(),
            session: ServerHeaderSession {
                protocol: HeaderProtocol::Legacy,
                legacy_key: key,
                v2: None,
                context: Some(context),
                _legacy_guard: Some(legacy_guard),
            },
            replay_fingerprint: None,
            client_timestamp: None,
        })
    }

    async fn read_v2_initial<T: AsyncReadExt + Unpin>(
        &self,
        reader: &mut T,
    ) -> std::result::Result<ServerInitialMessage, ServerInitialError> {
        let mut remainder = [0_u8; FIRST_PREFIX_REMAINDER_LEN];
        reader.read_exact(&mut remainder).await.map_err(|error| {
            ServerInitialError::fail(
                "protocol_v2_header_invalid",
                format!("failed to read protocol-v2 header: {error}"),
                true,
            )
        })?;
        let version = remainder[0];
        let flags = remainder[1];
        let reserved = u16::from_be_bytes([remainder[2], remainder[3]]);
        if version != PROTOCOL_V2_VERSION || flags != 0 || reserved != 0 {
            return Err(ServerInitialError::fail(
                if version != PROTOCOL_V2_VERSION {
                    "protocol_version_unsupported"
                } else {
                    "protocol_v2_header_invalid"
                },
                format!(
                    "unsupported protocol header version={version} flags={flags} reserved={reserved}"
                ),
                false,
            ));
        }
        let key_id = KeyId::from_u64(u64::from_be_bytes(
            remainder[4..12].try_into().expect("fixed key id"),
        ));
        let salt: [u8; CONNECTION_SALT_LEN] =
            remainder[12..28].try_into().expect("fixed connection salt");
        let client_timestamp = u64::from_be_bytes(salt[..8].try_into().expect("fixed timestamp"));
        let now = unix_seconds();
        if now.abs_diff(client_timestamp) > MAX_CONNECTION_CLOCK_SKEW_SECONDS {
            return Err(ServerInitialError::fail_key(
                "connection_timestamp_invalid",
                "protocol-v2 connection timestamp is outside the accepted clock-skew window",
                false,
                key_id,
            ));
        }
        let key = self
            .auth
            .derive_key(key_id)
            .map_err(|failure| ServerInitialError::from_failure_key(failure, key_id))?;
        let material = derive_material(key_id, &key, salt).map_err(|error| {
            ServerInitialError::fail_key(
                "protocol_v2_key_derivation_failed",
                error.to_string(),
                false,
                key_id,
            )
        })?;
        let mut session = v2_session(key, material.clone());
        let (counter, ciphertext) = read_v2_frame(reader, 0, MAX_INITIAL_PLAINTEXT_LEN)
            .await
            .map_err(|error| {
                ServerInitialError::fail_key(
                    "protocol_v2_decrypt_failed",
                    error.to_string(),
                    false,
                    key_id,
                )
            })?;
        let mut current_ciphertext = ciphertext.clone();
        let fingerprint = replay_fingerprint(key_id, &salt);
        let work = match open_v2_payload(
            &material,
            DIRECTION_CLIENT_TO_SERVER,
            counter,
            &mut current_ciphertext,
        ) {
            Ok(payload) => FirstFlightWork::Live {
                key,
                payload,
                error_session: session_without_context(&session),
            },
            Err(error) => {
                match stale_root_first_flight(&self.auth, key_id, salt, counter, &ciphertext) {
                    Some(stale) => FirstFlightWork::Stale(stale),
                    None => {
                        return Err(first_flight_error(
                            "protocol_v2_decrypt_failed",
                            error.to_string(),
                            false,
                            key_id,
                        ))
                    }
                }
            }
        };
        let replay = self.replay.clone();
        let auth = self.auth.clone();
        let (payload, context) = tokio::task::spawn_blocking(move || {
            evaluate_first_flight(&auth, &replay, key_id, fingerprint, work)
        })
        .await
        .unwrap_or_else(|_| {
            Err(first_flight_error(
                "connection_replay_store_unavailable",
                "failed to evaluate first-flight admission",
                true,
                key_id,
            ))
        })?;
        session.context = Some(context);
        Ok(ServerInitialMessage {
            payload,
            session,
            replay_fingerprint: Some(fingerprint),
            client_timestamp: Some(client_timestamp),
        })
    }
}

mod limiter;
pub use limiter::FailureLogDecision;
use limiter::FailureLogLimiter;

pub enum HeaderMessageReader<'a, T: AsyncReadExt + Unpin> {
    Legacy(CodecMessageReader<'a, T, Aes256GcmDeCodec>),
    V2(V2MessageReader<'a, T>),
}

impl<T: AsyncReadExt + Unpin> MessageReader for HeaderMessageReader<'_, T> {
    async fn read_msg(&mut self) -> Result<&'_ [u8]> {
        match self {
            Self::Legacy(reader) => reader.read_msg().await,
            Self::V2(reader) => reader.read_msg().await,
        }
    }
}

pub enum HeaderMessageWriter<'a, T: AsyncWriteExt + Unpin> {
    Legacy(CodecMessageWriter<'a, T, Aes256GcmEnCodec>),
    V2(V2MessageWriter<'a, T>),
}

impl<T: AsyncWriteExt + Unpin> MessageWriter for HeaderMessageWriter<'_, T> {
    async fn write_msg(&mut self, message: &[u8]) -> Result<()> {
        match self {
            Self::Legacy(writer) => writer.write_msg(message).await,
            Self::V2(writer) => writer.write_msg(message).await,
        }
    }
}

mod frame;
use frame::{derive_material, first_prefix, open_v2_payload, read_v2_frame, V2Material};
pub use frame::{V2MessageReader, V2MessageWriter};
mod replay;
#[cfg(test)]
use replay::RotatingBloom;
use replay::{replay_fingerprint, FirstFlightAdmit, ReplayGuard};
mod first_flight;
use first_flight::*;
fn legacy_message_reader<'a, T: AsyncReadExt + Unpin>(
    reader: &'a mut T,
    key: &AesKeyType,
    action: &str,
) -> Result<CodecMessageReader<'a, T, Aes256GcmDeCodec>> {
    Ok(CodecMessageReader::for_session_key(
        reader,
        Aes256GcmDeCodec::try_new(key)
            .map_err(|_| protocol_error(format!("failed to initialize {action}")))?,
        *key,
    ))
}

fn legacy_message_writer<'a, T: AsyncWriteExt + Unpin>(
    writer: &'a mut T,
    key: &AesKeyType,
    action: &str,
) -> Result<CodecMessageWriter<'a, T, Aes256GcmEnCodec>> {
    Ok(CodecMessageWriter::for_session_key(
        writer,
        Aes256GcmEnCodec::try_new(key)
            .map_err(|_| protocol_error(format!("failed to initialize {action}")))?,
        *key,
    ))
}

fn protocol_error(detail: impl Into<String>) -> Error {
    Error::MsgProtocol {
        detail: detail.into(),
    }
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests;
