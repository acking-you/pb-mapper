//! What travels over the control channel.
//!
//! One JSON object per message, length-prefixed. Length prefixing rather than
//! newline delimiting because it lets the reader allocate exactly once against
//! input it has not validated yet, and because it matches how pb-mapper frames
//! its own messages.

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::ctl::Command;
use crate::error::{CtlError, ErrorCode};

/// Bumped only when a change would confuse an older peer. The UI and the CLI
/// are usually the same binary, but `pb-mapper-ctl` can be installed
/// separately and a UI updated in place keeps running the old code.
pub const PROTOCOL_VERSION: u32 = 1;

/// Larger than any real payload; the biggest is a server map dump.
const MAX_FRAME: u32 = 4 * 1024 * 1024;

#[derive(Serialize, Deserialize, Debug)]
pub struct Request {
    pub v: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(flatten)]
    pub command: Command,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Response {
    pub v: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub success: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    /// Present whenever `success` is false. See [`ErrorCode`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<ErrorCode>,
}

impl Response {
    pub fn ok(data: Option<serde_json::Value>, message: Option<String>) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            id: None,
            success: true,
            message,
            data,
            code: None,
        }
    }

    pub fn err(error: &CtlError) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            id: None,
            success: false,
            message: Some(error.to_string()),
            data: None,
            code: Some(error.code()),
        }
    }

    pub fn with_id(mut self, id: Option<String>) -> Self {
        self.id = id;
        self
    }

    /// Turn a response back into the error it describes, for the CLI side.
    pub fn as_error(&self) -> Option<CtlError> {
        if self.success {
            return None;
        }
        Some(CtlError::new(
            self.code.unwrap_or(ErrorCode::Internal),
            self.message.clone().unwrap_or_else(|| "failed".to_string()),
        ))
    }
}

pub async fn write_frame<W>(writer: &mut W, value: &impl Serialize) -> std::io::Result<()>
where
    W: AsyncWrite + Unpin,
{
    let body = serde_json::to_vec(value)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    if body.len() as u64 > MAX_FRAME as u64 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "control message too large",
        ));
    }
    writer.write_all(&(body.len() as u32).to_be_bytes()).await?;
    writer.write_all(&body).await?;
    writer.flush().await
}

pub async fn read_frame<R, T>(reader: &mut R) -> std::io::Result<T>
where
    R: AsyncRead + Unpin,
    T: serde::de::DeserializeOwned,
{
    let mut len = [0u8; 4];
    reader.read_exact(&mut len).await?;
    let len = u32::from_be_bytes(len);
    if len > MAX_FRAME {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "control message too large",
        ));
    }
    let mut body = vec![0u8; len as usize];
    reader.read_exact(&mut body).await?;
    serde_json::from_slice(&body)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_frame_survives_the_round_trip() {
        let mut buffer = Vec::new();
        let request = Request {
            v: PROTOCOL_VERSION,
            id: Some("abc".into()),
            command: Command::Status,
        };
        write_frame(&mut buffer, &request).await.unwrap();

        // Length prefix first, then exactly that many bytes.
        let len = u32::from_be_bytes(buffer[..4].try_into().unwrap()) as usize;
        assert_eq!(len, buffer.len() - 4);

        let mut cursor = std::io::Cursor::new(buffer);
        let back: Request = read_frame(&mut cursor).await.unwrap();
        assert_eq!(back.v, PROTOCOL_VERSION);
        assert_eq!(back.id.as_deref(), Some("abc"));
        assert!(matches!(back.command, Command::Status));
    }

    #[tokio::test]
    async fn an_error_response_carries_its_code_both_ways() {
        let sent = Response::err(&CtlError::not_found("no such service"));
        let mut buffer = Vec::new();
        write_frame(&mut buffer, &sent).await.unwrap();

        let mut cursor = std::io::Cursor::new(buffer);
        let back: Response = read_frame(&mut cursor).await.unwrap();
        assert!(!back.success);
        let rebuilt = back.as_error().expect("a failure must rebuild as an error");
        assert_eq!(rebuilt.code(), ErrorCode::NotFound);
        assert_eq!(rebuilt.to_string(), "no such service");
    }
}
