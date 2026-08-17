//! Protocol-v2 single-flight authentication framing.
//!
//! The first client frame carries a clear-text routing prefix and an authenticated encrypted
//! request. It does not add a handshake or round trip. All following control messages on the
//! same TCP connection use independently derived directional keys and monotonically increasing
//! 64-bit counters.

use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use rand::RngExt;
use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM};
use ring::digest::{digest, SHA256};
use ring::hkdf::{Salt, HKDF_SHA256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::{
    CodecMessageReader, CodecMessageWriter, DataLenType, MessageReader, MessageWriter, MAX_MSG_LEN,
};
use crate::common::auth::{AuthContext, AuthFailure, AuthRuntime, LegacyConnectionGuard};
use crate::common::checksum::{get_process_credential, valid_checksum, AesKeyType, Credential};
use crate::common::error::{Error, Result};
use crate::utils::codec::{Aes256GcmDeCodec, Aes256GcmEnCodec, Decryptor};

pub const PROTOCOL_V2_MAGIC: [u8; 4] = *b"PBM2";
pub const PROTOCOL_V2_VERSION: u8 = 2;
const CONNECTION_SALT_LEN: usize = 16;
const FIRST_PREFIX_REMAINDER_LEN: usize = 28;
const FRAME_HEADER_LEN: usize = 12;
const DIRECTION_CLIENT_TO_SERVER: u8 = 0;
const DIRECTION_SERVER_TO_CLIENT: u8 = 1;
const DEFAULT_REPLAY_WINDOW_SECONDS: u64 = 60;
const DEFAULT_REPLAY_FILTER_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HeaderProtocol {
    Legacy,
    V2,
}

#[derive(Clone)]
struct V2Material {
    key_id: u64,
    flags: u8,
    salt: [u8; CONNECTION_SALT_LEN],
    client_to_server: AesKeyType,
    server_to_client: AesKeyType,
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
        let mut rng = rand::rng();
        for byte in &mut salt {
            *byte = rng.random();
        }
        let material = derive_material(credential.key_id(), credential.key(), salt)?;
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
                let codec = Aes256GcmEnCodec::try_new(&self.legacy_key)
                    .map_err(|_| protocol_error("failed to initialize legacy writer"))?;
                CodecMessageWriter::new(writer, codec)
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
            HeaderProtocol::Legacy => Ok(HeaderMessageReader::Legacy(CodecMessageReader::new(
                reader,
                Aes256GcmDeCodec::try_new(&self.legacy_key)
                    .map_err(|_| protocol_error("failed to initialize legacy reader"))?,
            ))),
            HeaderProtocol::V2 => Ok(HeaderMessageReader::V2(V2MessageReader::new(
                reader,
                self.v2.as_ref().expect("v2 session material").clone(),
                DIRECTION_SERVER_TO_CLIENT,
                0,
            )?)),
        }
    }

    pub fn continuation_writer<'a, T: AsyncWriteExt + Unpin>(
        &self,
        writer: &'a mut T,
    ) -> Result<HeaderMessageWriter<'a, T>> {
        match self.protocol {
            HeaderProtocol::Legacy => Ok(HeaderMessageWriter::Legacy(CodecMessageWriter::new(
                writer,
                Aes256GcmEnCodec::try_new(&self.legacy_key)
                    .map_err(|_| protocol_error("failed to initialize legacy writer"))?,
            ))),
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

    pub fn key_id(&self) -> u64 {
        self.context
            .as_ref()
            .map(|context| context.key_id)
            .unwrap_or_else(|| {
                self.v2
                    .as_ref()
                    .map(|material| material.key_id)
                    .unwrap_or_default()
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
            HeaderProtocol::Legacy => Ok(HeaderMessageWriter::Legacy(CodecMessageWriter::new(
                writer,
                Aes256GcmEnCodec::try_new(&self.legacy_key)
                    .map_err(|_| protocol_error("failed to initialize legacy response writer"))?,
            ))),
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
            HeaderProtocol::Legacy => Ok(HeaderMessageReader::Legacy(CodecMessageReader::new(
                reader,
                Aes256GcmDeCodec::try_new(&self.legacy_key)
                    .map_err(|_| protocol_error("failed to initialize legacy reader"))?,
            ))),
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
}

pub struct ServerInitialError {
    pub failure: AuthFailure,
    pub response_session: Option<ServerHeaderSession>,
}

impl fmt::Debug for ServerInitialError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ServerInitialError")
            .field("failure", &self.failure)
            .field("has_response_session", &self.response_session.is_some())
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
    replay: Arc<Mutex<RotatingBloom>>,
    failure_logs: Arc<Mutex<FailureLogLimiter>>,
}

impl ServerSecurity {
    pub fn new(auth: AuthRuntime) -> Self {
        Self {
            auth,
            replay: Arc::new(Mutex::new(RotatingBloom::new(
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
        key_id: u64,
        reason: &str,
    ) -> FailureLogDecision {
        self.failure_logs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .record(peer_ip, key_id, reason, unix_seconds())
    }

    pub async fn read_initial<T: AsyncReadExt + Unpin>(
        &self,
        reader: &mut T,
    ) -> std::result::Result<ServerInitialMessage, ServerInitialError> {
        let mut first = [0_u8; 4];
        reader
            .read_exact(&mut first)
            .await
            .map_err(|error| ServerInitialError {
                failure: AuthFailure::new(
                    "protocol_header_read_failed",
                    format!("failed to read initial protocol header: {error}"),
                    true,
                ),
                response_session: None,
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
            return Err(ServerInitialError {
                failure: AuthFailure::new(
                    "legacy_protocol_disabled",
                    "legacy protocol is disabled by the administrator",
                    false,
                ),
                response_session: None,
            });
        }
        let key = self
            .auth
            .admin_key()
            .map_err(|failure| ServerInitialError {
                failure,
                response_session: None,
            })?;
        let context = self
            .auth
            .authenticate(0)
            .map_err(|failure| ServerInitialError {
                failure,
                response_session: None,
            })?;
        let checksum = u32::from_be_bytes(checksum_bytes);
        let datalen = reader
            .read_u32()
            .await
            .map_err(|error| ServerInitialError {
                failure: AuthFailure::new(
                    "legacy_frame_invalid",
                    format!("failed to read legacy frame length: {error}"),
                    true,
                ),
                response_session: None,
            })?;
        if !valid_checksum(datalen, checksum) || datalen > MAX_MSG_LEN {
            return Err(ServerInitialError {
                failure: AuthFailure::new(
                    "legacy_frame_invalid",
                    "legacy frame checksum or length is invalid",
                    false,
                ),
                response_session: None,
            });
        }
        let mut encrypted = vec![0_u8; datalen as usize];
        reader
            .read_exact(&mut encrypted)
            .await
            .map_err(|error| ServerInitialError {
                failure: AuthFailure::new(
                    "legacy_frame_invalid",
                    format!("failed to read legacy frame body: {error}"),
                    true,
                ),
                response_session: None,
            })?;
        let mut codec = Aes256GcmDeCodec::try_new(&key).map_err(|_| ServerInitialError {
            failure: AuthFailure::new(
                "legacy_decrypt_failed",
                "failed to initialize legacy decryption",
                false,
            ),
            response_session: None,
        })?;
        let plain = codec
            .decrypt(&mut encrypted)
            .map_err(|_| ServerInitialError {
                failure: AuthFailure::new(
                    "legacy_decrypt_failed",
                    "legacy credential or encrypted frame is invalid",
                    false,
                ),
                response_session: None,
            })?;
        let legacy_guard =
            self.auth
                .record_legacy_connection()
                .map_err(|failure| ServerInitialError {
                    failure,
                    response_session: None,
                })?;
        Ok(ServerInitialMessage {
            payload: plain.to_vec(),
            session: ServerHeaderSession {
                protocol: HeaderProtocol::Legacy,
                legacy_key: key,
                v2: None,
                context: Some(context),
                _legacy_guard: Some(legacy_guard),
            },
        })
    }

    async fn read_v2_initial<T: AsyncReadExt + Unpin>(
        &self,
        reader: &mut T,
    ) -> std::result::Result<ServerInitialMessage, ServerInitialError> {
        let mut remainder = [0_u8; FIRST_PREFIX_REMAINDER_LEN];
        reader
            .read_exact(&mut remainder)
            .await
            .map_err(|error| ServerInitialError {
                failure: AuthFailure::new(
                    "protocol_v2_header_invalid",
                    format!("failed to read protocol-v2 header: {error}"),
                    true,
                ),
                response_session: None,
            })?;
        let version = remainder[0];
        let flags = remainder[1];
        let reserved = u16::from_be_bytes([remainder[2], remainder[3]]);
        if version != PROTOCOL_V2_VERSION || flags != 0 || reserved != 0 {
            return Err(ServerInitialError {
                failure: AuthFailure::new(
                    if version != PROTOCOL_V2_VERSION {
                        "protocol_version_unsupported"
                    } else {
                        "protocol_v2_header_invalid"
                    },
                    format!(
                        "unsupported protocol header version={version} flags={flags} reserved={reserved}"
                    ),
                    false,
                ),
                response_session: None,
            });
        }
        let key_id = u64::from_be_bytes(remainder[4..12].try_into().expect("fixed key id"));
        let salt: [u8; CONNECTION_SALT_LEN] =
            remainder[12..28].try_into().expect("fixed connection salt");
        let key = self
            .auth
            .derive_key(key_id)
            .map_err(|failure| ServerInitialError {
                failure,
                response_session: None,
            })?;
        let material = derive_material(key_id, &key, salt).map_err(|error| ServerInitialError {
            failure: AuthFailure::new(
                "protocol_v2_key_derivation_failed",
                error.to_string(),
                false,
            ),
            response_session: None,
        })?;
        let mut session = ServerHeaderSession {
            protocol: HeaderProtocol::V2,
            legacy_key: key,
            v2: Some(material.clone()),
            context: None,
            _legacy_guard: None,
        };
        let mut message_reader = V2MessageReader::new(
            reader,
            material,
            DIRECTION_CLIENT_TO_SERVER,
            0,
        )
        .map_err(|error| ServerInitialError {
            failure: AuthFailure::new("protocol_v2_decrypt_failed", error.to_string(), false),
            response_session: Some(session_without_context(&session)),
        })?;
        let payload = message_reader
            .read_msg()
            .await
            .map_err(|error| ServerInitialError {
                failure: AuthFailure::new("protocol_v2_decrypt_failed", error.to_string(), false),
                response_session: None,
            })?
            .to_vec();

        let fingerprint = replay_fingerprint(key_id, &salt);
        let replayed = self
            .replay
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .contains(&fingerprint, unix_seconds());
        if replayed {
            return Err(ServerInitialError {
                failure: AuthFailure::new(
                    "connection_salt_replayed",
                    "protocol-v2 connection salt was already accepted",
                    true,
                ),
                response_session: Some(session),
            });
        }

        let context = self
            .auth
            .authenticate(key_id)
            .map_err(|failure| ServerInitialError {
                failure,
                response_session: Some(session_without_context(&session)),
            })?;
        self.replay
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(&fingerprint, unix_seconds());
        session.context = Some(context);
        Ok(ServerInitialMessage { payload, session })
    }
}

#[derive(Clone, Copy, Debug)]
pub struct FailureLogDecision {
    pub emit: bool,
    pub suppressed: u64,
}

struct FailureLogEntry {
    window_started_at: u64,
    emitted: u8,
    suppressed: u64,
}

#[derive(Default)]
struct FailureLogLimiter {
    entries: std::collections::HashMap<(std::net::IpAddr, u64, String), FailureLogEntry>,
    overflow: Option<FailureLogEntry>,
}

impl FailureLogLimiter {
    fn record(
        &mut self,
        peer_ip: std::net::IpAddr,
        key_id: u64,
        reason: &str,
        now: u64,
    ) -> FailureLogDecision {
        let key = (peer_ip, key_id, reason.to_string());
        if !self.entries.contains_key(&key) && self.entries.len() >= 4096 {
            self.entries
                .retain(|_, entry| now.saturating_sub(entry.window_started_at) < 120);
            if self.entries.len() >= 4096 {
                let entry = self.overflow.get_or_insert(FailureLogEntry {
                    window_started_at: now,
                    emitted: 0,
                    suppressed: 0,
                });
                return record_failure_entry(entry, now);
            }
        }
        let entry = self.entries.entry(key).or_insert(FailureLogEntry {
            window_started_at: now,
            emitted: 0,
            suppressed: 0,
        });
        record_failure_entry(entry, now)
    }
}

fn record_failure_entry(entry: &mut FailureLogEntry, now: u64) -> FailureLogDecision {
    if now.saturating_sub(entry.window_started_at) >= 60 {
        let suppressed = entry.suppressed;
        *entry = FailureLogEntry {
            window_started_at: now,
            emitted: 1,
            suppressed: 0,
        };
        return FailureLogDecision {
            emit: true,
            suppressed,
        };
    }
    if entry.emitted < 5 {
        entry.emitted += 1;
        FailureLogDecision {
            emit: true,
            suppressed: 0,
        }
    } else {
        entry.suppressed = entry.suppressed.saturating_add(1);
        FailureLogDecision {
            emit: false,
            suppressed: 0,
        }
    }
}

fn session_without_context(session: &ServerHeaderSession) -> ServerHeaderSession {
    ServerHeaderSession {
        protocol: session.protocol,
        legacy_key: session.legacy_key,
        v2: session.v2.clone(),
        context: None,
        _legacy_guard: None,
    }
}

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

pub struct V2MessageReader<'a, T: AsyncReadExt + Unpin> {
    reader: &'a mut T,
    material: V2Material,
    key: LessSafeKey,
    direction: u8,
    expected_counter: u64,
    buffer: Vec<u8>,
}

impl<'a, T: AsyncReadExt + Unpin> V2MessageReader<'a, T> {
    fn new(
        reader: &'a mut T,
        material: V2Material,
        direction: u8,
        expected_counter: u64,
    ) -> Result<Self> {
        let key_bytes = direction_key(&material, direction);
        let key = LessSafeKey::new(
            UnboundKey::new(&AES_256_GCM, key_bytes)
                .map_err(|_| protocol_error("invalid protocol-v2 read key"))?,
        );
        Ok(Self {
            reader,
            material,
            key,
            direction,
            expected_counter,
            buffer: Vec::new(),
        })
    }
}

impl<T: AsyncReadExt + Unpin> MessageReader for V2MessageReader<'_, T> {
    async fn read_msg(&mut self) -> Result<&'_ [u8]> {
        let counter = self
            .reader
            .read_u64()
            .await
            .map_err(|error| protocol_error(format!("failed to read v2 counter: {error}")))?;
        if counter != self.expected_counter {
            return Err(protocol_error(format!(
                "protocol-v2 counter mismatch: expected {}, got {counter}",
                self.expected_counter
            )));
        }
        let datalen = self
            .reader
            .read_u32()
            .await
            .map_err(|error| protocol_error(format!("failed to read v2 length: {error}")))?;
        if datalen < AES_256_GCM.tag_len() as u32 || datalen > MAX_MSG_LEN {
            return Err(protocol_error(format!(
                "protocol-v2 payload length {datalen} is invalid"
            )));
        }
        self.buffer.resize(datalen as usize, 0);
        self.reader
            .read_exact(&mut self.buffer)
            .await
            .map_err(|error| protocol_error(format!("failed to read v2 payload: {error}")))?;
        let aad = frame_aad(&self.material, self.direction, counter, datalen);
        let plain = self
            .key
            .open_in_place(nonce(counter), Aad::from(aad.as_slice()), &mut self.buffer)
            .map_err(|_| protocol_error("protocol-v2 payload authentication failed"))?;
        let plain_len = plain.len();
        self.buffer.truncate(plain_len);
        self.expected_counter = self
            .expected_counter
            .checked_add(1)
            .ok_or_else(|| protocol_error("protocol-v2 receive counter exhausted"))?;
        Ok(&self.buffer)
    }
}

pub struct V2MessageWriter<'a, T: AsyncWriteExt + Unpin> {
    writer: &'a mut T,
    material: V2Material,
    key: LessSafeKey,
    direction: u8,
    counter: u64,
}

impl<'a, T: AsyncWriteExt + Unpin> V2MessageWriter<'a, T> {
    fn new(writer: &'a mut T, material: V2Material, direction: u8, counter: u64) -> Result<Self> {
        let key_bytes = direction_key(&material, direction);
        let key = LessSafeKey::new(
            UnboundKey::new(&AES_256_GCM, key_bytes)
                .map_err(|_| protocol_error("invalid protocol-v2 write key"))?,
        );
        Ok(Self {
            writer,
            material,
            key,
            direction,
            counter,
        })
    }
}

impl<T: AsyncWriteExt + Unpin> MessageWriter for V2MessageWriter<'_, T> {
    async fn write_msg(&mut self, message: &[u8]) -> Result<()> {
        let encrypted_len = message
            .len()
            .checked_add(AES_256_GCM.tag_len())
            .and_then(|len| DataLenType::try_from(len).ok())
            .ok_or_else(|| protocol_error("protocol-v2 message is too large"))?;
        if encrypted_len > MAX_MSG_LEN {
            return Err(protocol_error(
                "protocol-v2 message exceeds the maximum length",
            ));
        }
        let counter = self.counter;
        let aad = frame_aad(&self.material, self.direction, counter, encrypted_len);
        let mut encrypted = message.to_vec();
        self.key
            .seal_in_place_append_tag(nonce(counter), Aad::from(aad.as_slice()), &mut encrypted)
            .map_err(|_| protocol_error("failed to encrypt protocol-v2 message"))?;
        self.writer
            .write_u64(counter)
            .await
            .map_err(|error| protocol_error(format!("failed to write v2 frame header: {error}")))?;
        self.writer
            .write_u32(encrypted_len)
            .await
            .map_err(|error| protocol_error(format!("failed to write v2 frame header: {error}")))?;
        self.writer
            .write_all(&encrypted)
            .await
            .map_err(|error| protocol_error(format!("failed to write v2 frame body: {error}")))?;
        self.counter = self
            .counter
            .checked_add(1)
            .ok_or_else(|| protocol_error("protocol-v2 send counter exhausted"))?;
        Ok(())
    }
}

fn derive_material(
    key_id: u64,
    credential_key: &AesKeyType,
    salt_bytes: [u8; CONNECTION_SALT_LEN],
) -> Result<V2Material> {
    let salt = Salt::new(HKDF_SHA256, &salt_bytes);
    let pseudo_random_key = salt.extract(credential_key);
    let client_to_server = expand_direction(&pseudo_random_key, b"pb-mapper-v2-c2s")?;
    let server_to_client = expand_direction(&pseudo_random_key, b"pb-mapper-v2-s2c")?;
    Ok(V2Material {
        key_id,
        flags: 0,
        salt: salt_bytes,
        client_to_server,
        server_to_client,
    })
}

fn expand_direction(
    pseudo_random_key: &ring::hkdf::Prk,
    label: &'static [u8],
) -> Result<AesKeyType> {
    let info = [label];
    let output = pseudo_random_key
        .expand(&info, HkdfLen(32))
        .map_err(|_| protocol_error("failed to derive protocol-v2 direction key"))?;
    let mut key = [0_u8; 32];
    output
        .fill(&mut key)
        .map_err(|_| protocol_error("failed to fill protocol-v2 direction key"))?;
    Ok(key)
}

struct HkdfLen(usize);

impl ring::hkdf::KeyType for HkdfLen {
    fn len(&self) -> usize {
        self.0
    }
}

fn direction_key(material: &V2Material, direction: u8) -> &AesKeyType {
    if direction == DIRECTION_CLIENT_TO_SERVER {
        &material.client_to_server
    } else {
        &material.server_to_client
    }
}

fn first_prefix(material: &V2Material) -> Vec<u8> {
    let mut prefix = Vec::with_capacity(PROTOCOL_V2_MAGIC.len() + FIRST_PREFIX_REMAINDER_LEN);
    prefix.extend_from_slice(&PROTOCOL_V2_MAGIC);
    prefix.push(PROTOCOL_V2_VERSION);
    prefix.push(material.flags);
    prefix.extend_from_slice(&0_u16.to_be_bytes());
    prefix.extend_from_slice(&material.key_id.to_be_bytes());
    prefix.extend_from_slice(&material.salt);
    prefix
}

fn frame_aad(material: &V2Material, direction: u8, counter: u64, datalen: u32) -> Vec<u8> {
    let mut aad = Vec::with_capacity(
        PROTOCOL_V2_MAGIC.len() + FIRST_PREFIX_REMAINDER_LEN + 1 + FRAME_HEADER_LEN,
    );
    aad.extend_from_slice(&first_prefix(material));
    aad.push(direction);
    aad.extend_from_slice(&counter.to_be_bytes());
    aad.extend_from_slice(&datalen.to_be_bytes());
    aad
}

fn nonce(counter: u64) -> Nonce {
    let mut bytes = [0_u8; 12];
    bytes[4..].copy_from_slice(&counter.to_be_bytes());
    Nonce::assume_unique_for_key(bytes)
}

fn replay_fingerprint(key_id: u64, salt: &[u8; CONNECTION_SALT_LEN]) -> [u8; 32] {
    let mut input = [0_u8; 8 + CONNECTION_SALT_LEN];
    input[..8].copy_from_slice(&key_id.to_be_bytes());
    input[8..].copy_from_slice(salt);
    digest(&SHA256, &input)
        .as_ref()
        .try_into()
        .expect("SHA-256 width")
}

struct RotatingBloom {
    current: Vec<u8>,
    previous: Vec<u8>,
    current_started_at: u64,
    window_seconds: u64,
}

impl RotatingBloom {
    fn new(bytes: usize, window_seconds: u64) -> Self {
        Self {
            current: vec![0; bytes],
            previous: vec![0; bytes],
            current_started_at: unix_seconds(),
            window_seconds,
        }
    }

    fn contains(&mut self, fingerprint: &[u8; 32], now: u64) -> bool {
        self.rotate(now);
        bloom_contains(&self.current, fingerprint) || bloom_contains(&self.previous, fingerprint)
    }

    fn insert(&mut self, fingerprint: &[u8; 32], now: u64) {
        self.rotate(now);
        bloom_insert(&mut self.current, fingerprint);
    }

    fn rotate(&mut self, now: u64) {
        let elapsed = now.saturating_sub(self.current_started_at);
        if elapsed < self.window_seconds {
            return;
        }
        if elapsed >= self.window_seconds.saturating_mul(2) {
            self.current.fill(0);
            self.previous.fill(0);
        } else {
            std::mem::swap(&mut self.current, &mut self.previous);
            self.current.fill(0);
        }
        self.current_started_at = now;
    }
}

fn bloom_positions(filter_len: usize, fingerprint: &[u8; 32]) -> [usize; 4] {
    let bits = filter_len * 8;
    std::array::from_fn(|index| {
        let offset = index * 8;
        let hash = u64::from_be_bytes(
            fingerprint[offset..offset + 8]
                .try_into()
                .expect("fingerprint chunk"),
        );
        hash as usize % bits
    })
}

fn bloom_contains(filter: &[u8], fingerprint: &[u8; 32]) -> bool {
    bloom_positions(filter.len(), fingerprint)
        .into_iter()
        .all(|position| filter[position / 8] & (1 << (position % 8)) != 0)
}

fn bloom_insert(filter: &mut [u8], fingerprint: &[u8; 32]) {
    for position in bloom_positions(filter.len(), fingerprint) {
        filter[position / 8] |= 1 << (position % 8);
    }
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
mod tests {
    use super::*;
    use crate::common::auth::{AuthConfig, LegacyProtocolPolicy};
    use crate::common::checksum::encode_temporary_credential;

    fn temp_config() -> AuthConfig {
        let mut random = [0_u8; 8];
        let mut rng = rand::rng();
        for byte in &mut random {
            *byte = rng.random();
        }
        AuthConfig {
            state_dir: std::env::temp_dir()
                .join(format!("pb-mapper-v2-{}", u64::from_be_bytes(random))),
            max_temporary_keys: 8,
            max_temporary_key_ttl: std::time::Duration::from_secs(3600),
            legacy_protocol: LegacyProtocolPolicy::Allow,
        }
    }

    #[tokio::test]
    async fn v2_round_trip_uses_directional_counters() {
        let credential = Credential::Admin(*b"0123456789abcdefghijklmnopqrstuv");
        let client = ClientHeaderSession::new_v2(&credential).unwrap();
        let config = temp_config();
        let auth = AuthRuntime::start(*credential.key(), config.clone())
            .await
            .unwrap();
        let security = ServerSecurity::new(auth);
        let (mut client_io, mut server_io) = tokio::io::duplex(4096);

        let client_task = async {
            client
                .write_initial(&mut client_io, b"request")
                .await
                .unwrap();
            let mut reader = client.response_reader(&mut client_io).unwrap();
            assert_eq!(reader.read_msg().await.unwrap(), b"response");
        };
        let server_task = async {
            let initial = security.read_initial(&mut server_io).await.unwrap();
            assert_eq!(initial.payload, b"request");
            let mut writer = initial.session.response_writer(&mut server_io).unwrap();
            writer.write_msg(b"response").await.unwrap();
        };
        tokio::join!(client_task, server_task);
        let _ = std::fs::remove_dir_all(config.state_dir);
    }

    #[tokio::test]
    async fn temporary_credential_authenticates_without_storing_secret() {
        let admin = *b"0123456789abcdefghijklmnopqrstuv";
        let config = temp_config();
        let auth = AuthRuntime::start(admin, config.clone()).await.unwrap();
        let issued = auth
            .issue(std::time::Duration::from_secs(60), None)
            .await
            .unwrap();
        let Credential::Temporary { key_id, key } =
            crate::common::checksum::parse_credential(&issued.credential).unwrap()
        else {
            panic!("expected temporary credential")
        };
        assert_eq!(issued.credential, encode_temporary_credential(key_id, &key));
        let client = ClientHeaderSession::new_v2(&Credential::Temporary { key_id, key }).unwrap();
        let security = ServerSecurity::new(auth);
        let (mut client_io, mut server_io) = tokio::io::duplex(4096);
        let client_task = client.write_initial(&mut client_io, b"temporary");
        let server_task = security.read_initial(&mut server_io);
        let (client_result, server_result) = tokio::join!(client_task, server_task);
        client_result.unwrap();
        let initial = server_result.unwrap();
        assert_eq!(initial.payload, b"temporary");
        assert_eq!(initial.session.context().unwrap().namespace, key_id);
        let _ = std::fs::remove_dir_all(config.state_dir);
    }

    #[test]
    fn rotating_bloom_covers_current_and_previous_window() {
        let mut bloom = RotatingBloom::new(1024, 60);
        let value = [7_u8; 32];
        let start = bloom.current_started_at;
        assert!(!bloom.contains(&value, start));
        bloom.insert(&value, start);
        assert!(bloom.contains(&value, start + 60));
        assert!(!bloom.contains(&value, start + 121));
    }

    #[test]
    fn failure_log_limiter_has_a_hard_cardinality_bound() {
        let mut limiter = FailureLogLimiter::default();
        let peer = "127.0.0.1".parse().unwrap();
        for key_id in 0..10_000 {
            limiter.record(peer, key_id, "invalid", 1_000);
        }
        assert_eq!(limiter.entries.len(), 4096);
        assert!(limiter.overflow.is_some());
    }
}
