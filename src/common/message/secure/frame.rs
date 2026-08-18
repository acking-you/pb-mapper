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

use super::*;

#[derive(Clone)]
pub(super) struct V2Material {
    pub(super) key_id: u64,
    pub(super) flags: u8,
    pub(super) salt: [u8; CONNECTION_SALT_LEN],
    pub(super) client_to_server: AesKeyType,
    pub(super) server_to_client: AesKeyType,
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
    pub(super) fn new(
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

    pub(super) async fn read_msg_with_limit(&mut self, max_plaintext_len: u32) -> Result<&'_ [u8]> {
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
        let max_encrypted_len = max_plaintext_len.saturating_add(AES_256_GCM.tag_len() as u32);
        if datalen < AES_256_GCM.tag_len() as u32 || datalen > max_encrypted_len {
            return Err(protocol_error(format!(
                "protocol-v2 payload length {datalen} exceeds the {max_plaintext_len}-byte limit"
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

impl<T: AsyncReadExt + Unpin> MessageReader for V2MessageReader<'_, T> {
    async fn read_msg(&mut self) -> Result<&'_ [u8]> {
        self.read_msg_with_limit(MAX_MSG_LEN - AES_256_GCM.tag_len() as u32)
            .await
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
    pub(super) fn new(
        writer: &'a mut T,
        material: V2Material,
        direction: u8,
        counter: u64,
    ) -> Result<Self> {
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

pub(super) fn open_v2_payload(
    material: &V2Material,
    direction: u8,
    counter: u64,
    ciphertext: &mut [u8],
) -> Result<Vec<u8>> {
    let key_bytes = direction_key(material, direction);
    let key = LessSafeKey::new(
        UnboundKey::new(&AES_256_GCM, key_bytes)
            .map_err(|_| protocol_error("invalid protocol-v2 read key"))?,
    );
    let datalen = u32::try_from(ciphertext.len())
        .map_err(|_| protocol_error("protocol-v2 payload length is invalid"))?;
    let aad = frame_aad(material, direction, counter, datalen);
    let plain = key
        .open_in_place(nonce(counter), Aad::from(aad.as_slice()), ciphertext)
        .map_err(|_| protocol_error("protocol-v2 payload authentication failed"))?;
    Ok(plain.to_vec())
}

pub(super) fn derive_material(
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

pub(super) fn first_prefix(material: &V2Material) -> Vec<u8> {
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
