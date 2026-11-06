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
    Probe {
        nonce: u64,
    },
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
/// EOF is normal only before the next message. TLS reports an unannounced
/// transport close as an error; truncated headers/bodies remain errors too.
pub(crate) async fn read_json_or_eof<R: AsyncRead + Unpin, T: DeserializeOwned>(
    io: &mut R,
) -> Result<Option<T>> {
    let mut header = [0; 4];
    if io.read(&mut header[..1]).await? == 0 {
        return Ok(None);
    }
    io.read_exact(&mut header[1..]).await?;
    let bytes = read_payload(io, u32::from_be_bytes(header) as usize, MAX_JSON).await?;
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_| error("invalid desktop message"))
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
    read_payload(io, len, limit).await
}
async fn read_payload<R: AsyncRead + Unpin>(
    io: &mut R,
    len: usize,
    limit: usize,
) -> Result<Vec<u8>> {
    if len > limit {
        return Err(error("desktop message too large"));
    }
    let mut bytes = vec![0; len];
    io.read_exact(&mut bytes).await?;
    Ok(bytes)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    async fn tls_pair() -> (
        tokio_rustls::client::TlsStream<tokio::io::DuplexStream>,
        tokio_rustls::server::TlsStream<tokio::io::DuplexStream>,
    ) {
        let (acceptor, certificate) = crate::security::server().unwrap();
        let connector = crate::security::client(&certificate).unwrap();
        let (client, server) = tokio::io::duplex(16 * 1024);
        let (client, server) = tokio::join!(
            connector.connect(
                quinn::rustls::pki_types::ServerName::try_from(crate::security::NAME).unwrap(),
                client,
            ),
            acceptor.accept(server),
        );
        (client.unwrap(), server.unwrap())
    }

    #[tokio::test]
    async fn clean_tls_close_is_eof_only_between_messages() {
        let (mut client, mut server) = tls_pair().await;
        write_json(&mut client, &Command::Ping).await.unwrap();
        client.shutdown().await.unwrap();
        assert!(matches!(
            read_json_or_eof::<_, Command>(&mut server).await.unwrap(),
            Some(Command::Ping)
        ));
        assert!(
            read_json_or_eof::<_, Command>(&mut server)
                .await
                .unwrap()
                .is_none()
        );

        let (mut client, mut server) = tls_pair().await;
        client.write_all(&[0, 0]).await.unwrap();
        client.shutdown().await.unwrap();
        assert_eq!(
            read_json_or_eof::<_, Command>(&mut server)
                .await
                .err()
                .unwrap()
                .kind(),
            std::io::ErrorKind::UnexpectedEof,
        );

        let (client, mut server) = tls_pair().await;
        drop(client);
        assert_eq!(
            read_json_or_eof::<_, Command>(&mut server)
                .await
                .err()
                .unwrap()
                .kind(),
            std::io::ErrorKind::UnexpectedEof,
        );
    }
}
