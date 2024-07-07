//! Define message protocols and tools for reading and writing
//! messages
pub mod command;
use snafu::{ensure, ResultExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::buffer::{BufferGetter, CommonBuffer, FixedSizeBuffer};
use super::checksum::{get_checksum, valid_checksum};
use super::error::{
    MsgDatalenValidateSnafu, MsgNetworkReadBodySnafu, MsgNetworkReadCheckSumSnafu,
    MsgNetworkReadDatalenSnafu, MsgNetworkWriteBodySnafu, MsgNetworkWriteCheckSumSnafu,
    MsgNetworkWriteDatalenSnafu, Result,
};
use crate::common::error::MsgDatalenExceededSnafu;

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
