//! The checksum + length framing every relay connection speaks, and the
//! reader and writer traits over it.

use snafu::{ResultExt, ensure};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::buffer::CommonBuffer;
use crate::{DataLenType, MAX_MSG_LEN, frame_read};
use pb_mapper_core::checksum::{
    AesKeyType, get_checksum, get_checksum_for_key, get_msg_header_key, process_checksum_is_ready,
    valid_checksum, valid_checksum_for_key,
};
use pb_mapper_core::codec::{Aes256GcmDeCodec, Aes256GcmEnCodec, Decryptor, Encryptor};
use pb_mapper_core::error::MsgDatalenExceededSnafu;
use pb_mapper_core::error::{
    self, MsgDatalenValidateSnafu, MsgNetworkReadBodySnafu, MsgNetworkReadCheckSumSnafu,
    MsgNetworkWriteBodySnafu, MsgNetworkWriteCheckSumSnafu, MsgNetworkWriteCodecMsgSnafu,
    MsgNetworkWriteCodecTagSnafu, MsgNetworkWriteDatalenSnafu, Result,
};

/// This message protocol contains header and body, and the header
/// includes checksum, datalen,respectively, u32, u32, where datalen
/// represents the length of the body, checksum is used to check the
/// datalen field. This is just the most basic pedestal protocol, in
/// order to solve the sticky packet problem with TCP streams. We
/// can build more advanced communication on top of this protocol, for
/// example, we can use json or other forms of data representation
///
/// ```text
/// ┌─────────────┐
/// │ u32 checksum│
/// │ u32 datalen │
/// └─────────────┘
/// ┌─────────────┐
/// │ Body        │
/// │(Actual Data)│
/// └─────────────┘
/// ```
#[expect(
    async_fn_in_trait,
    reason = "awaited only on the connection task that owns the stream, never sent to another"
)]
pub trait MessageReader {
    async fn read_msg(&mut self) -> Result<&'_ [u8]>;
}

#[expect(
    async_fn_in_trait,
    reason = "awaited only on the connection task that owns the stream, never sent to another"
)]
pub trait MessageWriter {
    async fn write_msg(&mut self, msg: &[u8]) -> Result<()>;
}

fn checksum_key_bytes(key: &Option<AesKeyType>) -> Option<&[u8]> {
    key.as_ref().map(|key| key.as_slice())
}

#[inline]
fn checksum_matches(datalen: DataLenType, checksum: u32, key: Option<&[u8]>) -> bool {
    match key {
        Some(key) => valid_checksum_for_key(datalen, checksum, key),
        None => valid_checksum(datalen, checksum),
    }
}

#[inline]
fn checksum_for(len: DataLenType, key: Option<&[u8]>) -> Result<u32> {
    match key {
        Some(key) => Ok(get_checksum_for_key(len, key)),
        None => {
            if !process_checksum_is_ready() {
                return Err(error::Error::MsgCodec {
                    action: "load configured credential",
                    detail:
                        "`MSG_HEADER_KEY` is required; no insecure default checksum is available"
                            .to_string(),
                });
            }
            Ok(get_checksum(len))
        }
    }
}

#[inline]
async fn set_msg_len<T: AsyncWriteExt + Unpin>(
    writer: &mut T,
    len: DataLenType,
    checksum_key: Option<&[u8]>,
) -> Result<()> {
    writer
        .write_u32(checksum_for(len, checksum_key)?)
        .await
        .context(MsgNetworkWriteCheckSumSnafu)?;
    writer
        .write_u32(len)
        .await
        .context(MsgNetworkWriteDatalenSnafu)
}

pub struct NormalMessageReader<'a, T: AsyncReadExt + Unpin> {
    reader: &'a mut T,
    buffer: CommonBuffer,
    frame: frame_read::FrameRead<8>,
    checksum_key: Option<AesKeyType>,
}

impl<'a, T: AsyncReadExt + Unpin> NormalMessageReader<'a, T> {
    pub fn new(reader: &'a mut T) -> Self {
        Self {
            reader,
            buffer: CommonBuffer::new(),
            frame: frame_read::FrameRead::new(),
            checksum_key: None,
        }
    }

    pub fn with_checksum_key(mut self, key: AesKeyType) -> Self {
        self.checksum_key = Some(key);
        self
    }

    async fn read_msg_inner<const ALLOW_EOF: bool>(&mut self) -> Result<Option<&'_ [u8]>> {
        if ALLOW_EOF {
            if !self
                .frame
                .header_or_eof(self.reader)
                .await
                .context(MsgNetworkReadCheckSumSnafu)?
            {
                return Ok(None);
            }
        } else {
            self.frame
                .header(self.reader)
                .await
                .context(MsgNetworkReadCheckSumSnafu)?;
        }
        let [c0, c1, c2, c3, l0, l1, l2, l3] = self.frame.header;
        let checksum = u32::from_be_bytes([c0, c1, c2, c3]);
        let datalen = u32::from_be_bytes([l0, l1, l2, l3]);
        ensure!(
            checksum_matches(datalen, checksum, checksum_key_bytes(&self.checksum_key)),
            MsgDatalenValidateSnafu { datalen, checksum }
        );
        ensure!(
            datalen <= MAX_MSG_LEN,
            MsgDatalenExceededSnafu {
                actual: datalen,
                max: MAX_MSG_LEN
            }
        );
        self.buffer.fixed_resize(datalen as usize);
        self.frame
            .body(self.reader, self.buffer.buffer_mut())
            .await
            .context(MsgNetworkReadBodySnafu)?;
        self.frame.finish();
        Ok(Some(self.buffer.buffer()))
    }
}

impl<'a, T: AsyncReadExt + Unpin> MessageReader for NormalMessageReader<'a, T> {
    async fn read_msg(&mut self) -> Result<&'_ [u8]> {
        self.read_msg_inner::<false>()
            .await
            .map(|message| message.unwrap_or_default())
    }
}

pub struct NormalMessageWriter<'a, T: AsyncWriteExt> {
    writer: &'a mut T,
    checksum_key: Option<AesKeyType>,
}

impl<'a, T: AsyncWriteExt + Unpin> NormalMessageWriter<'a, T> {
    pub fn new(writer: &'a mut T) -> Self {
        Self {
            writer,
            checksum_key: None,
        }
    }

    pub fn with_checksum_key(mut self, key: AesKeyType) -> Self {
        self.checksum_key = Some(key);
        self
    }
}

impl<'a, T: AsyncWriteExt + Unpin> MessageWriter for NormalMessageWriter<'a, T> {
    async fn write_msg(&mut self, msg: &[u8]) -> Result<()> {
        set_msg_len(
            &mut self.writer,
            msg.len() as u32,
            checksum_key_bytes(&self.checksum_key),
        )
        .await?;
        self.writer
            .write_all(msg)
            .await
            .context(MsgNetworkWriteBodySnafu)
    }
}

pub struct CodecMessageReader<'a, T: AsyncReadExt + Unpin, D: Decryptor> {
    reader: NormalMessageReader<'a, T>,
    decryptor: D,
}

impl<'a, T: AsyncReadExt + Unpin, D: Decryptor> CodecMessageReader<'a, T, D> {
    pub fn new(reader: &'a mut T, decryptor: D) -> Self {
        Self {
            reader: NormalMessageReader::new(reader),
            decryptor,
        }
    }

    /// Bind the length checksum to `key` instead of the process credential.
    /// Isolated relays keep a remote `MSG_HEADER_KEY` while speaking with a
    /// different local administrator key.
    pub fn for_session_key(reader: &'a mut T, decryptor: D, key: AesKeyType) -> Self {
        Self::new(reader, decryptor).with_checksum_key(key)
    }

    pub fn with_checksum_key(mut self, key: AesKeyType) -> Self {
        self.reader.checksum_key = Some(key);
        self
    }
}

impl<T: AsyncReadExt + Unpin, D: Decryptor> CodecMessageReader<'_, T, D> {
    /// Byte-stream framing permits FIN between records. Control/datagram readers remain strict.
    pub(crate) async fn read_stream(&mut self) -> Result<&'_ [u8]> {
        if self.reader.read_msg_inner::<true>().await?.is_none() {
            return Ok(&[]);
        }
        self.decrypt()
    }

    /// Decrypts the frame just read, in place.
    fn decrypt(&mut self) -> Result<&'_ [u8]> {
        self.decryptor
            .decrypt(self.reader.buffer.buffer_mut())
            .map(|bytes| &*bytes)
            .map_err(|e| error::Error::MsgCodec {
                action: "decrypt",
                detail: format!("got {e} when we read msg"),
            })
    }
}
impl<T: AsyncReadExt + Unpin, D: Decryptor> MessageReader for CodecMessageReader<'_, T, D> {
    async fn read_msg(&mut self) -> Result<&'_ [u8]> {
        self.reader.read_msg().await?;
        self.decrypt()
    }
}

/// Encrypts a copy of each message, leaving the caller's buffer untouched.
pub struct CodecMessageWriter<'a, T: AsyncWriteExt + Unpin, E: Encryptor> {
    writer: &'a mut T,
    encryptor: E,
    /// `None` uses the process `MSG_HEADER_KEY` hash. Isolated relays must set
    /// this to the session key so continuation frames stay decryptable.
    checksum_key: Option<AesKeyType>,
}

impl<'a, T: AsyncWriteExt + Unpin, E: Encryptor> CodecMessageWriter<'a, T, E> {
    pub fn new(writer: &'a mut T, encryptor: E) -> Self {
        Self {
            writer,
            encryptor,
            checksum_key: None,
        }
    }

    pub fn for_session_key(writer: &'a mut T, encryptor: E, key: AesKeyType) -> Self {
        Self::new(writer, encryptor).with_checksum_key(key)
    }

    pub fn with_checksum_key(mut self, key: AesKeyType) -> Self {
        self.checksum_key = Some(key);
        self
    }

    pub async fn shutdown(&mut self) -> std::io::Result<()> {
        self.writer.shutdown().await
    }
}

impl<'a, T: AsyncWriteExt + Unpin, E: Encryptor> MessageWriter for CodecMessageWriter<'a, T, E> {
    async fn write_msg(&mut self, msg: &[u8]) -> Result<()> {
        let mut buf = msg.to_vec();
        let tag = self
            .encryptor
            .encrypt(&mut buf)
            .map_err(|e| error::Error::MsgCodec {
                action: "encrypt",
                detail: format!("got {e} when we read msg"),
            })?;
        let msg_len = (buf.len() + tag.as_ref().len()) as DataLenType;

        set_msg_len(self.writer, msg_len, checksum_key_bytes(&self.checksum_key)).await?;
        self.writer
            .write_all(&buf)
            .await
            .context(MsgNetworkWriteCodecMsgSnafu)?;
        self.writer
            .write_all(tag.as_ref())
            .await
            .context(MsgNetworkWriteCodecTagSnafu)
    }
}

#[inline]
pub fn get_header_msg_reader<T: AsyncReadExt + Unpin>(
    reader: &mut T,
) -> Result<CodecMessageReader<'_, T, Aes256GcmDeCodec>> {
    Ok(CodecMessageReader::new(reader, get_default_decodec()?))
}

#[inline]
pub fn get_header_msg_writer<T: AsyncWriteExt + Unpin>(
    writer: &mut T,
) -> Result<CodecMessageWriter<'_, T, Aes256GcmEnCodec>> {
    Ok(CodecMessageWriter::new(writer, get_default_encodec()?))
}

#[inline]
pub fn get_default_encodec() -> Result<Aes256GcmEnCodec> {
    let key = get_msg_header_key().map_err(|detail| error::Error::MsgCodec {
        action: "load configured credential",
        detail,
    })?;
    Aes256GcmEnCodec::try_new(&key).map_err(|e| error::Error::MsgCodec {
        action: "create default encodec",
        detail: format!("{e}"),
    })
}

#[inline]
pub fn get_default_decodec() -> Result<Aes256GcmDeCodec> {
    let key = get_msg_header_key().map_err(|detail| error::Error::MsgCodec {
        action: "load configured credential",
        detail,
    })?;
    Aes256GcmDeCodec::try_new(&key).map_err(|e| error::Error::MsgCodec {
        action: "create default decodec",
        detail: format!("{e}"),
    })
}

#[inline]
pub fn get_encodec(key: &[u8]) -> Result<Aes256GcmEnCodec> {
    Aes256GcmEnCodec::try_new(key).map_err(|e| error::Error::MsgCodec {
        action: "create encodec",
        detail: format!("{e}"),
    })
}

#[inline]
pub fn get_decodec(key: &[u8]) -> Result<Aes256GcmDeCodec> {
    Aes256GcmDeCodec::try_new(key).map_err(|e| error::Error::MsgCodec {
        action: "create decodec",
        detail: format!("{e}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame_read::cancel_partial_read;

    #[tokio::test]
    async fn legacy_fragmented_frame_survives_read_cancellation() {
        let mut wire = Vec::new();
        let mut writer = NormalMessageWriter::new(&mut wire).with_checksum_key([7; 32]);
        writer.write_msg(b"first fragmented message").await.unwrap();
        writer.write_msg(b"next message").await.unwrap();
        for split in 1..8 + b"first fragmented message".len() {
            let (mut tx, mut rx) = tokio::io::duplex(4096);
            let mut reader = NormalMessageReader::new(&mut rx).with_checksum_key([7; 32]);
            tx.write_all(&wire[..split]).await.unwrap();
            cancel_partial_read(&mut reader).await;
            tx.write_all(&wire[split..]).await.unwrap();
            drop(tx);
            assert_eq!(
                reader.read_msg().await.unwrap(),
                b"first fragmented message",
                "split {split}"
            );
            assert_eq!(
                reader.read_msg().await.unwrap(),
                b"next message",
                "split {split}"
            );
        }
    }
}
