use crate::{InputEvent, Result, VideoCodec, error};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub(crate) const MAX_JSON: usize = 8192;
pub(crate) const MAX_FRAME: usize = 4 * 1024 * 1024;
pub(crate) const IO_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

#[derive(Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Command {
    Authenticate {
        token: String,
        #[serde(default)]
        codecs: Vec<VideoCodec>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        video: Option<crate::VideoPreferences>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        presentation_feedback: bool,
    },
    Control {
        enabled: bool,
    },
    Input {
        event: InputEvent,
    },
    Release,
    Ping,
    Keyframe,
    Presented {
        generation: u64,
        frame_id: u64,
    },
}

pub(crate) async fn write_json<W: AsyncWrite + Unpin, T: Serialize>(
    io: &mut W,
    value: &T,
) -> Result<()> {
    let bytes = serde_json::to_vec(value).map_err(error)?;
    write_bytes(io, &bytes, MAX_JSON).await
}
pub(crate) async fn read_json<R: AsyncRead + Unpin, T: DeserializeOwned>(io: &mut R) -> Result<T> {
    let bytes = read_bytes(io, MAX_JSON).await?;
    serde_json::from_slice(&bytes).map_err(|_| error("invalid desktop message"))
}
pub(crate) async fn write_bytes<W: AsyncWrite + Unpin>(
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
pub(crate) async fn read_bytes<R: AsyncRead + Unpin>(io: &mut R, limit: usize) -> Result<Vec<u8>> {
    let len = io.read_u32().await? as usize;
    if len > limit {
        return Err(error("desktop message too large"));
    }
    let mut bytes = vec![0; len];
    io.read_exact(&mut bytes).await?;
    Ok(bytes)
}
