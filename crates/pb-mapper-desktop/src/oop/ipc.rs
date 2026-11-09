//! The out-of-process backend's framing: `u32 BE len | bytes` records over
//! any transport.
use crate::{Result, error};
use serde::{Serialize, de::DeserializeOwned};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// The pipes' records are `u32 BE len | bytes`: JSON of at most `MAX_JSON`,
/// or a captured image of at most `MAX_FRAME`.
pub const MAX_JSON: usize = 8192;
pub const MAX_FRAME: usize = 4 * 1024 * 1024;

pub async fn write_json<W: AsyncWrite + Unpin, T: Serialize>(io: &mut W, value: &T) -> Result<()> {
    let bytes = serde_json::to_vec(value).map_err(error)?;
    write_bytes(io, &bytes, MAX_JSON).await
}
pub async fn read_json<R: AsyncRead + Unpin, T: DeserializeOwned>(io: &mut R) -> Result<T> {
    let bytes = read_bytes(io, MAX_JSON).await?;
    serde_json::from_slice(&bytes).map_err(|_| error("invalid desktop message"))
}
pub async fn write_bytes<W: AsyncWrite + Unpin>(
    io: &mut W,
    bytes: &[u8],
    limit: usize,
) -> Result<()> {
    if bytes.len() > limit {
        return Err(error("desktop message too large"));
    }
    io.write_u32(bytes.len() as u32).await?;
    io.write_all(bytes).await?;
    io.flush().await
}
pub async fn read_bytes<R: AsyncRead + Unpin>(io: &mut R, limit: usize) -> Result<Vec<u8>> {
    let len = io.read_u32().await? as usize;
    if len > limit {
        return Err(error("desktop message too large"));
    }
    let mut bytes = vec![0; len];
    io.read_exact(&mut bytes).await?;
    Ok(bytes)
}
