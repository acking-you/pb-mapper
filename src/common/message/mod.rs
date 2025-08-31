//! Define message protocols and tools for reading and writing
//! messages
pub mod command;
pub mod forward;
use snafu::{ensure, ResultExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::buffer::{BufferGetter, CommonBuffer, FixedSizeBuffer};
use super::checksum::{get_checksum, valid_checksum, MSG_HEADER_KEY};
use super::error::{
    self, MsgDatalenValidateSnafu, MsgNetworkReadBodySnafu, MsgNetworkReadCheckSumSnafu,
    MsgNetworkReadDatalenSnafu, MsgNetworkWriteBodySnafu, MsgNetworkWriteCheckSumSnafu,
    MsgNetworkWriteCodecMsgSnafu, MsgNetworkWriteCodecTagSnafu, MsgNetworkWriteDatalenSnafu,
    Result,
};
use crate::common::error::MsgDatalenExceededSnafu;
use crate::utils::codec::{Aes256GcmDeCodec, Aes256GcmEnCodec, Decryptor, Encryptor};

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
pub trait MessageReader {
    async fn read_msg(&mut self) -> Result<&'_ [u8]>;
}

pub trait MessageWriter {
    async fn write_msg(&mut self, msg: &[u8]) -> Result<()>;

    /// TODO: Implement this method to fix the encryption zero-copy data corruption bug
    ///
    /// This method should be used for encryption scenarios where:
    /// - Zero-copy performance is needed
    /// - The caller can provide mutable data
    /// - No unsafe transmutation is required
    ///
    /// Default implementation falls back to the immutable version for compatibility.
    /// Encryption implementations should override this for true zero-copy operation.
    async fn write_msg_mut(&mut self, msg: &mut [u8]) -> Result<()> {
        // Default implementation: delegate to immutable version
        // This maintains backward compatibility but doesn't solve the corruption issue
        self.write_msg(msg).await
    }
}

/// Maximum value of `datalen` to prevent Out of Memory
const MAX_MSG_LEN: DataLenType = 8 * 1024 * 1024;

pub type DataLenType = u32;

macro_rules! gen_read_network_with_error {
    ($func_name:ident, $read_method:ident, $error:expr, $return_ty:ty) => {
        #[inline]
        async fn $func_name<T: AsyncReadExt + Unpin>(reader: &mut T) -> Result<$return_ty> {
            reader.$read_method().await.context($error)
        }
    };
    ($func_name:ident, $read_method:ident, $error:expr, $input_type:ty, $return_type:ty) => {
        #[inline]
        async fn $func_name<T: AsyncReadExt + Unpin>(
            reader: &mut T,
            input_type: $input_type,
        ) -> Result<$return_type> {
            reader.$read_method(input_type).await.context($error)
        }
    };
}

macro_rules! gen_write_network_with_error {
    ($func_name:ident, $write_method:ident, $error:expr, $input_type:ty) => {
        #[inline]
        async fn $func_name<T: AsyncWriteExt + Unpin>(
            writer: &mut T,
            data: $input_type,
        ) -> Result<()> {
            writer.$write_method(data).await.context($error)
        }
    };
}

gen_read_network_with_error!(read_checksum, read_u32, MsgNetworkReadCheckSumSnafu, u32);

gen_read_network_with_error!(read_datalen, read_u32, MsgNetworkReadDatalenSnafu, u32);

gen_write_network_with_error!(write_checksum, write_u32, MsgNetworkWriteCheckSumSnafu, u32);

gen_write_network_with_error!(write_datalen, write_u32, MsgNetworkWriteDatalenSnafu, u32);

gen_read_network_with_error!(
    read_msg_body,
    read_exact,
    MsgNetworkReadBodySnafu,
    &mut [u8],
    usize
);

gen_write_network_with_error!(write_msg_body, write_all, MsgNetworkWriteBodySnafu, &[u8]);

gen_write_network_with_error!(
    write_codec_msg,
    write_all,
    MsgNetworkWriteCodecMsgSnafu,
    &[u8]
);

gen_write_network_with_error!(
    write_codec_tag,
    write_all,
    MsgNetworkWriteCodecTagSnafu,
    &[u8]
);

#[inline]
async fn get_msg_len<T: AsyncReadExt + Unpin>(reader: &mut T) -> Result<DataLenType> {
    let checksum = read_checksum(reader).await?;
    let datalen = read_datalen(reader).await?;
    if valid_checksum(datalen, checksum) {
        ensure!(
            datalen <= MAX_MSG_LEN,
            MsgDatalenExceededSnafu {
                actual: datalen,
                max: MAX_MSG_LEN
            }
        );
        Ok(datalen)
    } else {
        MsgDatalenValidateSnafu { datalen, checksum }.fail()?
    }
}

#[inline]
async fn set_msg_len<T: AsyncWriteExt + Unpin>(writer: &mut T, len: DataLenType) -> Result<()> {
    write_checksum(writer, get_checksum(len)).await?;
    write_datalen(writer, len).await
}

pub struct NormalMessageReader<'a, T: AsyncReadExt + Unpin> {
    reader: &'a mut T,
    buffer: CommonBuffer,
}

impl<'a, T: AsyncReadExt + Unpin> NormalMessageReader<'a, T> {
    pub fn new(reader: &'a mut T) -> Self {
        Self {
            reader,
            buffer: CommonBuffer::new(),
        }
    }

    async fn read_msg_inner(&mut self) -> Result<&'_ [u8]> {
        let datalen = get_msg_len(&mut self.reader).await?;
        self.buffer.fixed_resize(datalen as usize);
        let n = read_msg_body(&mut self.reader, self.buffer.buffer_mut()).await?;
        Ok(&self.buffer.buffer()[0..n])
    }
}

impl<'a, T: AsyncReadExt + Unpin> MessageReader for NormalMessageReader<'a, T> {
    async fn read_msg(&mut self) -> Result<&'_ [u8]> {
        self.read_msg_inner().await
    }
}

pub struct NormalMessageWriter<'a, T: AsyncWriteExt> {
    writer: &'a mut T,
}

impl<'a, T: AsyncWriteExt + Unpin> NormalMessageWriter<'a, T> {
    pub fn new(writer: &'a mut T) -> Self {
        Self { writer }
    }

    async fn write_msg_inner(&mut self, msg: &[u8]) -> Result<()> {
        set_msg_len(&mut self.writer, msg.len() as u32).await?;

        write_msg_body(&mut self.writer, msg).await
    }
}

impl<'a, T: AsyncWriteExt + Unpin> MessageWriter for NormalMessageWriter<'a, T> {
    async fn write_msg(&mut self, msg: &[u8]) -> Result<()> {
        self.write_msg_inner(msg).await
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
}

impl<'a, T: AsyncReadExt + Unpin, D: Decryptor> MessageReader for CodecMessageReader<'a, T, D> {
    async fn read_msg(&mut self) -> Result<&'_ [u8]> {
        let n = self.reader.read_msg().await?.len();
        let v = self
            .decryptor
            .decrypt(&mut self.reader.buffer.buffer_mut()[..n])
            .map_err(|e| error::Error::MsgCodec {
                action: "decrypt",
                detail: format!("got {e} when we read msg"),
            })?;
        Ok(v)
    }
}

/// SAFETY: The use of `unsafe` here to convert external `&[u8]` into `&mut [u8]`.  Given the
/// logic of the [`MessageWriter`] trait, the `msg` should ideally be immutable. Otherwise,
/// it would affect the use of other features. However, the encryption API requires a
/// mutable reference `&mut`. To avoid unnecessary copying, `unsafe` is used here as a
/// compromise for the encryption API.
pub struct CodecMessageWriter<'a, T: AsyncWriteExt + Unpin, E: Encryptor> {
    writer: &'a mut T,
    encryptor: E,
}

impl<'a, T: AsyncWriteExt + Unpin, E: Encryptor> CodecMessageWriter<'a, T, E> {
    pub fn new(writer: &'a mut T, encryptor: E) -> Self {
        Self { writer, encryptor }
    }
}

impl<'a, T: AsyncWriteExt + Unpin, E: Encryptor> MessageWriter for CodecMessageWriter<'a, T, E> {
    /// **CRITICAL BUG WARNING**: This method has a fundamental design flaw that causes data corruption!
    ///
    /// **PROBLEM**:
    /// - The trait interface accepts `&[u8]` (immutable reference) but encryption needs `&mut [u8]`
    /// - We use unsafe transmutation to bypass Rust's safety guarantees
    /// - This causes SILENT DATA CORRUPTION when the same message is reused (like ping messages)
    /// - First use: encryption succeeds, original data gets mutated
    /// - Subsequent uses: corrupted data leads to decode failures on receiver side
    ///
    /// **REAL-WORLD IMPACT**:
    /// - Ping messages fail after first success: "We decode ping request error!"
    /// - Any reused message data becomes corrupted and unusable
    ///
    /// **WORKAROUND**: Always use fresh copies of message data, never reuse
    ///
    /// TODO: Add `write_msg_mut(&mut self, msg: &mut [u8])` method to MessageWriter trait
    ///       for zero-copy encryption without unsafe transmutation
    async fn write_msg(&mut self, msg: &[u8]) -> Result<()> {
        if msg.is_empty() {
            return Ok(());
        }
        // Pay attention here, &T -> &mut T
        let raw_ptr = msg.as_ptr() as *mut u8;
        let mut_msg = unsafe { std::slice::from_raw_parts_mut(raw_ptr, msg.len()) };

        let tag = self
            .encryptor
            .encrypt(mut_msg)
            .map_err(|e| error::Error::MsgCodec {
                action: "encrypt",
                detail: format!("got {e} when we read msg"),
            })?;
        let msg_len = (mut_msg.len() + tag.as_ref().len()) as DataLenType;

        set_msg_len(self.writer, msg_len).await?;
        write_codec_msg(self.writer, mut_msg).await?;
        write_codec_tag(self.writer, tag.as_ref()).await
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
    Aes256GcmEnCodec::try_new(&MSG_HEADER_KEY.0).map_err(|e| error::Error::MsgCodec {
        action: "create default encodec",
        detail: format!("{e}"),
    })
}

#[inline]
pub fn get_default_decodec() -> Result<Aes256GcmDeCodec> {
    Aes256GcmDeCodec::try_new(&MSG_HEADER_KEY.0).map_err(|e| error::Error::MsgCodec {
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
