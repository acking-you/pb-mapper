//! Protocol-v2 key derivation and directional authenticated frame codecs.
//!
//! ```text
//! credential + connection salt -> HKDF -> c2s key / s2c key
//! plaintext -> counter + length + AEAD(AAD) -> encrypted frame
//! encrypted frame -> bound length -> verify counter/tag -> plaintext
//! ```
//!
//! Counters are monotonic per direction and are included in both the nonce and AAD.
//! The initial reader can impose a smaller pre-authentication limit before allocating
//! a body; continuation frames retain the normal protocol maximum.

use pb_mapper_auth::KeyId;
use pb_mapper_core::checksum::AesKeyType;
use pb_mapper_core::error::Result;
use rand::RngExt;
use ring::aead::{AES_256_GCM, Aad, LessSafeKey, NONCE_LEN, Nonce, UnboundKey};
use ring::hkdf::{HKDF_SHA256, Prk, Salt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::{
    CONNECTION_SALT_LEN, FIRST_PREFIX_REMAINDER_LEN, FRAME_HEADER_LEN, PROTOCOL_V2_MAGIC,
    PROTOCOL_V2_VERSION, pack, protocol_error,
};
use crate::frame_read::FrameRead;
use crate::{
    CODEC_TAG_LEN, DataLenType, MAX_MSG_LEN, MAX_PLAINTEXT_LEN, MessageReader, MessageWriter,
};

/// Which way a frame travels. Each direction has its own key and its own
/// counter, and the authenticated data names it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(super) enum Direction {
    ClientToServer = 0,
    ServerToClient = 1,
}

/// The flags this version writes; the relay rejects any other value.
pub(super) const PREFIX_FLAGS: u8 = 0;
/// The reserved field's only accepted value.
pub(super) const PREFIX_RESERVED: u16 = 0;
/// A first flight's clear-text prefix: the magic, then its remainder.
const FIRST_PREFIX_LEN: usize = PROTOCOL_V2_MAGIC.len() + FIRST_PREFIX_REMAINDER_LEN;
/// What a frame authenticates besides its payload: the connection's prefix,
/// the direction, then the frame header.
const FRAME_AAD_LEN: usize = FIRST_PREFIX_LEN + size_of::<Direction>() + FRAME_HEADER_LEN;

#[derive(Clone)]
pub(super) struct V2Material {
    pub(super) key_id: KeyId,
    salt: [u8; CONNECTION_SALT_LEN],
    client_to_server: AesKeyType,
    server_to_client: AesKeyType,
}

impl V2Material {
    /// The AEAD key for frames travelling in `direction`; `invalid` reports a
    /// key ring refuses.
    fn aead_key(&self, direction: Direction, invalid: &'static str) -> Result<LessSafeKey> {
        let key = match direction {
            Direction::ClientToServer => &self.client_to_server,
            Direction::ServerToClient => &self.server_to_client,
        };
        UnboundKey::new(&AES_256_GCM, key)
            .map(LessSafeKey::new)
            .map_err(|_| protocol_error(invalid))
    }
}

pub struct V2MessageReader<'a, T: AsyncReadExt + Unpin> {
    reader: &'a mut T,
    material: V2Material,
    key: LessSafeKey,
    direction: Direction,
    expected_counter: u64,
    buffer: Vec<u8>,
    frame: FrameRead<FRAME_HEADER_LEN>,
}

impl<'a, T: AsyncReadExt + Unpin> V2MessageReader<'a, T> {
    pub(super) fn new(
        reader: &'a mut T,
        material: V2Material,
        direction: Direction,
        expected_counter: u64,
    ) -> Result<Self> {
        let key = material.aead_key(direction, "invalid protocol-v2 read key")?;
        Ok(Self {
            reader,
            material,
            key,
            direction,
            expected_counter,
            buffer: Vec::new(),
            frame: FrameRead::new(),
        })
    }

    pub(super) async fn read_msg_with_limit(&mut self, max_plaintext_len: u32) -> Result<&'_ [u8]> {
        self.frame
            .header(self.reader)
            .await
            .map_err(|error| protocol_error(format!("failed to read v2 header: {error}")))?;
        let (counter, datalen) =
            validate_frame_header(&self.frame.header, self.expected_counter, max_plaintext_len)?;
        self.buffer.resize(datalen as usize, 0);
        self.frame
            .body(self.reader, &mut self.buffer)
            .await
            .map_err(|error| protocol_error(format!("failed to read v2 payload: {error}")))?;
        let aad = frame_aad(&self.material, self.direction, counter, datalen);
        let plain = self
            .key
            .open_in_place(nonce(counter), Aad::from(aad), &mut self.buffer)
            .map_err(|_| protocol_error("protocol-v2 payload authentication failed"))?;
        let plain_len = plain.len();
        self.buffer.truncate(plain_len);
        self.expected_counter = self
            .expected_counter
            .checked_add(1)
            .ok_or_else(|| protocol_error("protocol-v2 receive counter exhausted"))?;
        self.frame.finish();
        Ok(&self.buffer)
    }
}

pub(super) async fn read_v2_frame<T: AsyncReadExt + Unpin>(
    reader: &mut T,
    expected_counter: u64,
    max_plaintext_len: u32,
) -> Result<(u64, Vec<u8>)> {
    let mut frame = FrameRead::<FRAME_HEADER_LEN>::new();
    frame
        .header(reader)
        .await
        .map_err(|error| protocol_error(format!("failed to read v2 header: {error}")))?;
    let (counter, datalen) =
        validate_frame_header(&frame.header, expected_counter, max_plaintext_len)?;
    let mut ciphertext = vec![0_u8; datalen as usize];
    reader
        .read_exact(&mut ciphertext)
        .await
        .map_err(|error| protocol_error(format!("failed to read v2 payload: {error}")))?;
    Ok((counter, ciphertext))
}

fn validate_frame_header(
    header: &[u8; FRAME_HEADER_LEN],
    expected_counter: u64,
    max_plaintext_len: u32,
) -> Result<(u64, u32)> {
    let [counter @ .., l0, l1, l2, l3] = *header;
    let counter = u64::from_be_bytes(counter);
    if counter != expected_counter {
        return Err(protocol_error(format!(
            "protocol-v2 counter mismatch: expected {expected_counter}, got {counter}"
        )));
    }
    let datalen = u32::from_be_bytes([l0, l1, l2, l3]);
    let max_encrypted_len = max_plaintext_len.saturating_add(CODEC_TAG_LEN);
    if datalen < CODEC_TAG_LEN || datalen > max_encrypted_len {
        return Err(protocol_error(format!(
            "protocol-v2 payload length {datalen} exceeds the {max_plaintext_len}-byte limit"
        )));
    }
    Ok((counter, datalen))
}

impl<T: AsyncReadExt + Unpin> MessageReader for V2MessageReader<'_, T> {
    async fn read_msg(&mut self) -> Result<&'_ [u8]> {
        self.read_msg_with_limit(MAX_PLAINTEXT_LEN).await
    }
}

pub struct V2MessageWriter<'a, T: AsyncWriteExt + Unpin> {
    writer: &'a mut T,
    material: V2Material,
    key: LessSafeKey,
    direction: Direction,
    counter: u64,
    poisoned: bool,
}

impl<'a, T: AsyncWriteExt + Unpin> V2MessageWriter<'a, T> {
    pub(super) fn new(
        writer: &'a mut T,
        material: V2Material,
        direction: Direction,
        counter: u64,
    ) -> Result<Self> {
        let key = material.aead_key(direction, "invalid protocol-v2 write key")?;
        Ok(Self {
            writer,
            material,
            key,
            direction,
            counter,
            poisoned: false,
        })
    }
}

impl<T: AsyncWriteExt + Unpin> MessageWriter for V2MessageWriter<'_, T> {
    async fn write_msg(&mut self, message: &[u8]) -> Result<()> {
        if self.poisoned {
            return Err(protocol_error(
                "protocol-v2 writer interrupted; close the connection",
            ));
        }
        let encrypted_len = message
            .len()
            .checked_add(CODEC_TAG_LEN as usize)
            .and_then(|len| DataLenType::try_from(len).ok())
            .ok_or_else(|| protocol_error("protocol-v2 message is too large"))?;
        if encrypted_len > MAX_MSG_LEN {
            return Err(protocol_error(
                "protocol-v2 message exceeds the maximum length",
            ));
        }
        let counter = self.counter;
        // Reserve before sealing or awaiting I/O. Cancellation leaves a
        // terminal writer; restarting a frame would desynchronize the stream.
        self.counter = self
            .counter
            .checked_add(1)
            .ok_or_else(|| protocol_error("protocol-v2 send counter exhausted"))?;
        self.poisoned = true;
        let aad = frame_aad(&self.material, self.direction, counter, encrypted_len);
        let mut encrypted = message.to_vec();
        self.key
            .seal_in_place_append_tag(nonce(counter), Aad::from(aad), &mut encrypted)
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
        self.poisoned = false;
        Ok(())
    }
}

pub(super) fn open_v2_payload(
    material: &V2Material,
    direction: Direction,
    counter: u64,
    ciphertext: &mut [u8],
) -> Result<Vec<u8>> {
    let key = material.aead_key(direction, "invalid protocol-v2 read key")?;
    let datalen = u32::try_from(ciphertext.len())
        .map_err(|_| protocol_error("protocol-v2 payload length is invalid"))?;
    let aad = frame_aad(material, direction, counter, datalen);
    let plain = key
        .open_in_place(nonce(counter), Aad::from(aad), ciphertext)
        .map_err(|_| protocol_error("protocol-v2 payload authentication failed"))?;
    Ok(plain.to_vec())
}

pub(super) fn derive_material(
    key_id: KeyId,
    credential_key: &AesKeyType,
    salt_bytes: [u8; CONNECTION_SALT_LEN],
) -> Result<V2Material> {
    let salt = Salt::new(HKDF_SHA256, &salt_bytes);
    let pseudo_random_key = salt.extract(credential_key);
    let client_to_server = expand_direction(&pseudo_random_key, b"pb-mapper-v2-c2s")?;
    let server_to_client = expand_direction(&pseudo_random_key, b"pb-mapper-v2-s2c")?;
    Ok(V2Material {
        key_id,
        salt: salt_bytes,
        client_to_server,
        server_to_client,
    })
}

fn expand_direction(pseudo_random_key: &Prk, label: &'static [u8]) -> Result<AesKeyType> {
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

/// A connection salt: the client's clock, then random bytes.
pub(super) fn connection_salt(timestamp: u64) -> [u8; CONNECTION_SALT_LEN] {
    pack(&[&timestamp.to_be_bytes(), &rand::rng().random::<[u8; 8]>()])
}

/// The client's clock when it opened the connection, from its salt.
pub(super) fn salt_timestamp(salt: &[u8; CONNECTION_SALT_LEN]) -> u64 {
    let [t0, t1, t2, t3, t4, t5, t6, t7, ..] = *salt;
    u64::from_be_bytes([t0, t1, t2, t3, t4, t5, t6, t7])
}

/// A first flight's clear-text prefix; [`PrefixFields::parse`] reads it back.
pub(super) fn first_prefix(material: &V2Material) -> [u8; FIRST_PREFIX_LEN] {
    pack(&[
        &PROTOCOL_V2_MAGIC,
        &[PROTOCOL_V2_VERSION, PREFIX_FLAGS],
        &PREFIX_RESERVED.to_be_bytes(),
        &material.key_id.to_be_bytes(),
        &material.salt,
    ])
}

/// What a first flight's prefix says after its magic, unchecked.
pub(super) struct PrefixFields {
    pub(super) version: u8,
    pub(super) flags: u8,
    pub(super) reserved: u16,
    pub(super) key_id: KeyId,
    pub(super) salt: [u8; CONNECTION_SALT_LEN],
}

impl PrefixFields {
    /// The inverse of [`first_prefix`] past the magic.
    pub(super) fn parse(remainder: [u8; FIRST_PREFIX_REMAINDER_LEN]) -> Self {
        let [
            version,
            flags,
            r0,
            r1,
            k0,
            k1,
            k2,
            k3,
            k4,
            k5,
            k6,
            k7,
            salt @ ..,
        ] = remainder;
        Self {
            version,
            flags,
            reserved: u16::from_be_bytes([r0, r1]),
            key_id: KeyId::from_u64(u64::from_be_bytes([k0, k1, k2, k3, k4, k5, k6, k7])),
            salt,
        }
    }
}

fn frame_aad(
    material: &V2Material,
    direction: Direction,
    counter: u64,
    datalen: u32,
) -> [u8; FRAME_AAD_LEN] {
    pack(&[
        &first_prefix(material),
        &[direction as u8],
        &counter.to_be_bytes(),
        &datalen.to_be_bytes(),
    ])
}

/// Four zero bytes, then the frame counter.
fn nonce(counter: u64) -> Nonce {
    Nonce::assume_unique_for_key(pack(&[&[0; 4], &counter.to_be_bytes()]))
}

const _: () = assert!(
    NONCE_LEN == 4 + size_of::<u64>(),
    "a nonce is a zero prefix, then the counter"
);
