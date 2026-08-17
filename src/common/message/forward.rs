use bytes::Bytes;
use snafu::ResultExt;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::time::Instant;

use super::super::buffer::{BufferReader, BufferedReader};
use super::error::{FwdNetworkWriteWithNormalSnafu, Result};
use super::{
    CodecMessageReader, CodecMessageWriter, MessageReader, MessageWriter, NormalMessageReader,
    NormalMessageWriter,
};
use crate::common::checksum::AesKeyType;
use crate::common::config::duration_from_env;
use crate::common::message::{get_decodec, get_encodec};
use crate::utils::codec::{Decryptor, Encryptor};
use crate::{
    create_component, snafu_error_get_or_return_ok, start_datagram_forward_with_codec_key,
    start_forward_with_codec_key,
};
use uni_stream::stream::{StreamSplit, TcpStreamImpl, UdpStreamImpl};
use uni_stream::udp::{UdpStreamReadHalf, UdpStreamWriteHalf};

pub trait ForwardReader {
    async fn read(&mut self) -> Result<&'_ [u8]>;
}

pub trait ForwardWriter {
    async fn write(&mut self, src: &[u8]) -> Result<()>;

    // Gracefully close the write side to support half-closed TCP streams.
    async fn shutdown(&mut self);
}

pub trait DatagramReader {
    async fn recv(&mut self) -> Result<Bytes>;
}

pub trait DatagramWriter {
    async fn send(&mut self, src: &[u8]) -> Result<()>;
}

const DEFAULT_TUNNEL_IDLE_TIMEOUT: Duration = Duration::from_secs(60 * 60);
const DEFAULT_HALF_CLOSE_IDLE_TIMEOUT: Duration = Duration::from_secs(60);
const PB_MAPPER_TUNNEL_IDLE_TIMEOUT: &str = "PB_MAPPER_TUNNEL_IDLE_TIMEOUT";
const PB_MAPPER_HALF_CLOSE_IDLE_TIMEOUT: &str = "PB_MAPPER_HALF_CLOSE_IDLE_TIMEOUT";

#[derive(Debug, Clone, Copy)]
struct ForwardTimeoutConfig {
    tunnel_idle_timeout: Duration,
    half_close_idle_timeout: Duration,
}

impl ForwardTimeoutConfig {
    fn from_env() -> Self {
        Self {
            tunnel_idle_timeout: duration_from_env(
                PB_MAPPER_TUNNEL_IDLE_TIMEOUT,
                DEFAULT_TUNNEL_IDLE_TIMEOUT,
            ),
            half_close_idle_timeout: duration_from_env(
                PB_MAPPER_HALF_CLOSE_IDLE_TIMEOUT,
                DEFAULT_HALF_CLOSE_IDLE_TIMEOUT,
            ),
        }
    }
}

pub struct NormalForwardReader<'a, T> {
    buffered_reader: BufferReader<'a, T>,
}

impl<'a, T: AsyncReadExt + Unpin + Send> NormalForwardReader<'a, T> {
    pub fn new(reader: &'a mut T) -> Self {
        Self {
            buffered_reader: BufferReader::new(reader),
        }
    }
}

impl<'a, T: AsyncReadExt + Unpin + Send> ForwardReader for NormalForwardReader<'a, T> {
    async fn read(&mut self) -> Result<&'_ [u8]> {
        self.buffered_reader.read().await
    }
}

pub struct NormalDatagramReader<'a, T: AsyncReadExt + Unpin> {
    reader: NormalMessageReader<'a, T>,
}

impl<'a, T: AsyncReadExt + Unpin + Send> NormalDatagramReader<'a, T> {
    pub fn new(reader: &'a mut T) -> Self {
        Self {
            reader: NormalMessageReader::new(reader),
        }
    }
}

impl<'a, T: AsyncReadExt + Unpin + Send> DatagramReader for NormalDatagramReader<'a, T> {
    async fn recv(&mut self) -> Result<Bytes> {
        let msg = self.reader.read_msg().await?;
        Ok(Bytes::copy_from_slice(msg))
    }
}

pub struct NormalForwardWriter<'a, T> {
    writer: &'a mut T,
}

impl<'a, T: AsyncWriteExt + Unpin + Send> NormalForwardWriter<'a, T> {
    pub fn new(writer: &'a mut T) -> Self {
        Self { writer }
    }

    async fn write_inner(&mut self, src: &[u8]) -> Result<()> {
        self.writer
            .write_all(src)
            .await
            .context(FwdNetworkWriteWithNormalSnafu)
    }
}

impl<'a, T: AsyncWriteExt + Unpin + Send> ForwardWriter for NormalForwardWriter<'a, T> {
    async fn write(&mut self, src: &[u8]) -> Result<()> {
        self.write_inner(src).await
    }

    async fn shutdown(&mut self) {
        let _ = self.writer.shutdown().await;
    }
}

pub struct NormalDatagramWriter<'a, T: AsyncWriteExt + Unpin> {
    writer: NormalMessageWriter<'a, T>,
}

impl<'a, T: AsyncWriteExt + Unpin + Send> NormalDatagramWriter<'a, T> {
    pub fn new(writer: &'a mut T) -> Self {
        Self {
            writer: NormalMessageWriter::new(writer),
        }
    }
}

impl<'a, T: AsyncWriteExt + Unpin + Send> DatagramWriter for NormalDatagramWriter<'a, T> {
    async fn send(&mut self, src: &[u8]) -> Result<()> {
        self.writer.write_msg(src).await
    }
}

pub struct CodecForwardReader<'a, T: AsyncReadExt + Unpin + Send, D: Decryptor>(
    CodecMessageReader<'a, T, D>,
);

impl<'a, T: AsyncReadExt + Send + Unpin, D: Decryptor> CodecForwardReader<'a, T, D> {
    pub fn new(reader: &'a mut T, decryptor: D) -> Self {
        Self(CodecMessageReader::new(reader, decryptor))
    }
}

impl<'a, T: AsyncReadExt + Send + Unpin, D: Decryptor> ForwardReader
    for CodecForwardReader<'a, T, D>
{
    async fn read(&mut self) -> Result<&'_ [u8]> {
        self.0.read_msg().await
    }
}

pub struct CodecDatagramReader<'a, T: AsyncReadExt + Unpin + Send, D: Decryptor>(
    CodecMessageReader<'a, T, D>,
);

impl<'a, T: AsyncReadExt + Send + Unpin, D: Decryptor> CodecDatagramReader<'a, T, D> {
    pub fn new(reader: &'a mut T, decryptor: D) -> Self {
        Self(CodecMessageReader::new(reader, decryptor))
    }
}

impl<'a, T: AsyncReadExt + Send + Unpin, D: Decryptor> DatagramReader
    for CodecDatagramReader<'a, T, D>
{
    async fn recv(&mut self) -> Result<Bytes> {
        let msg = self.0.read_msg().await?;
        Ok(Bytes::copy_from_slice(msg))
    }
}

pub struct CodecForwardWriter<'a, T: AsyncWriteExt + Send + Unpin, E: Encryptor>(
    CodecMessageWriter<'a, T, E>,
);

impl<'a, T: AsyncWriteExt + Send + Unpin, E: Encryptor> CodecForwardWriter<'a, T, E> {
    pub fn new(writer: &'a mut T, encryptor: E) -> Self {
        Self(CodecMessageWriter::new(writer, encryptor))
    }
}

impl<'a, T: AsyncWriteExt + Send + Unpin, E: Encryptor> ForwardWriter
    for CodecForwardWriter<'a, T, E>
{
    /// SAFETY: Same as [`CodecMessageWriter`]
    async fn write(&mut self, src: &[u8]) -> Result<()> {
        self.0.write_msg(src).await
    }

    async fn shutdown(&mut self) {
        let _ = self.0.shutdown().await;
    }
}

pub struct CodecDatagramWriter<'a, T: AsyncWriteExt + Send + Unpin, E: Encryptor>(
    CodecMessageWriter<'a, T, E>,
);

impl<'a, T: AsyncWriteExt + Send + Unpin, E: Encryptor> CodecDatagramWriter<'a, T, E> {
    pub fn new(writer: &'a mut T, encryptor: E) -> Self {
        Self(CodecMessageWriter::new(writer, encryptor))
    }
}

impl<'a, T: AsyncWriteExt + Send + Unpin, E: Encryptor> DatagramWriter
    for CodecDatagramWriter<'a, T, E>
{
    async fn send(&mut self, src: &[u8]) -> Result<()> {
        let buf = src.to_vec();
        self.0.write_msg(&buf).await
    }
}

pub async fn copy<R: ForwardReader, W: ForwardWriter>(
    mut reader: R,
    mut writer: W,
) -> Result<usize> {
    let mut length: usize = 0;
    loop {
        let src = reader.read().await?;
        let n = src.len();
        if n == 0 {
            break;
        }
        writer.write(src).await?;
        length += n;
    }
    writer.shutdown().await;
    Ok(length)
}

pub async fn transfer_datagrams<R: DatagramReader, W: DatagramWriter>(
    label: &'static str,
    mut reader: R,
    mut writer: W,
) -> Result<usize> {
    let mut _length: usize = 0;
    loop {
        let src = reader.recv().await?;
        let n = src.len();
        tracing::debug!("datagram forward {label} {n} bytes");
        writer.send(&src).await?;
        _length += n;
    }
}

pub async fn start_forward<
    ClientReader: ForwardReader,
    ClientWriter: ForwardWriter,
    ServerReader: ForwardReader,
    ServerWriter: ForwardWriter,
>(
    client_reader: ClientReader,
    client_writer: ClientWriter,
    server_reader: ServerReader,
    server_writer: ServerWriter,
) {
    start_forward_with_config(
        client_reader,
        client_writer,
        server_reader,
        server_writer,
        ForwardTimeoutConfig::from_env(),
    )
    .await
}

#[derive(Default)]
struct ForwardDirectionState {
    len: usize,
    result: Option<Result<usize>>,
}

impl ForwardDirectionState {
    fn is_done(&self) -> bool {
        self.result.is_some()
    }
}

#[derive(Clone, Copy)]
struct ForwardActivity {
    bytes: usize,
    at: Instant,
}

async fn copy_with_activity<R: ForwardReader, W: ForwardWriter>(
    mut reader: R,
    mut writer: W,
    activity_tx: tokio::sync::mpsc::UnboundedSender<ForwardActivity>,
) -> Result<usize> {
    let mut length = 0;
    loop {
        let src = reader.read().await?;
        let n = src.len();
        if n == 0 {
            break;
        }
        writer.write(src).await?;
        length += n;
        let _ = activity_tx.send(ForwardActivity {
            bytes: n,
            at: Instant::now(),
        });
    }
    writer.shutdown().await;
    Ok(length)
}

async fn start_forward_with_config<
    ClientReader: ForwardReader,
    ClientWriter: ForwardWriter,
    ServerReader: ForwardReader,
    ServerWriter: ForwardWriter,
>(
    client_reader: ClientReader,
    client_writer: ClientWriter,
    server_reader: ServerReader,
    server_writer: ServerWriter,
    timeout_config: ForwardTimeoutConfig,
) {
    let tunnel_idle_enabled = !timeout_config.tunnel_idle_timeout.is_zero();
    let half_close_idle_enabled = !timeout_config.half_close_idle_timeout.is_zero();
    let tunnel_idle_sleep = tokio::time::sleep(timeout_config.tunnel_idle_timeout);
    let half_close_idle_sleep = tokio::time::sleep(timeout_config.half_close_idle_timeout);
    tokio::pin!(tunnel_idle_sleep);
    tokio::pin!(half_close_idle_sleep);

    let (client_activity_tx, mut client_activity_rx) = tokio::sync::mpsc::unbounded_channel();
    let (server_activity_tx, mut server_activity_rx) = tokio::sync::mpsc::unbounded_channel();
    // Keep each direction as one pinned future. Recreating a combined read/write future in every
    // `select!` iteration can cancel it after the read consumed bytes but before the write
    // completed, silently dropping a response tail when the peer direction finishes first.
    let client_to_server = copy_with_activity(client_reader, server_writer, client_activity_tx);
    let server_to_client = copy_with_activity(server_reader, client_writer, server_activity_tx);
    tokio::pin!(client_to_server);
    tokio::pin!(server_to_client);

    let mut client_state = ForwardDirectionState::default();
    let mut server_state = ForwardDirectionState::default();

    loop {
        let client_done = client_state.is_done();
        let server_done = server_state.is_done();
        let half_closed = client_done ^ server_done;
        if client_done && server_done {
            break;
        }

        tokio::select! {
            biased;

            result = &mut client_to_server, if !client_done => {
                let failed = result.is_err();
                client_state.result = Some(result);
                reset_sleep(&mut half_close_idle_sleep, timeout_config.half_close_idle_timeout);
                if failed {
                    break;
                }
            }
            result = &mut server_to_client, if !server_done => {
                let failed = result.is_err();
                server_state.result = Some(result);
                reset_sleep(&mut half_close_idle_sleep, timeout_config.half_close_idle_timeout);
                if failed {
                    break;
                }
            }
            Some(activity) = client_activity_rx.recv(), if !client_done => {
                record_forward_activity(
                    activity,
                    &mut client_state,
                    &mut tunnel_idle_sleep,
                    timeout_config.tunnel_idle_timeout,
                    &mut half_close_idle_sleep,
                    timeout_config.half_close_idle_timeout,
                    server_done,
                );
            }
            Some(activity) = server_activity_rx.recv(), if !server_done => {
                record_forward_activity(
                    activity,
                    &mut server_state,
                    &mut tunnel_idle_sleep,
                    timeout_config.tunnel_idle_timeout,
                    &mut half_close_idle_sleep,
                    timeout_config.half_close_idle_timeout,
                    client_done,
                );
            }
            _ = &mut tunnel_idle_sleep, if tunnel_idle_enabled && !half_closed => {
                tracing::debug!(
                    "forward tunnel idle timeout after {:?}",
                    timeout_config.tunnel_idle_timeout
                );
                break;
            }
            _ = &mut half_close_idle_sleep, if half_close_idle_enabled && half_closed => {
                tracing::debug!(
                    "forward half-close idle timeout after {:?}",
                    timeout_config.half_close_idle_timeout
                );
                break;
            }
        }
    }

    let client_len = client_state.len;
    let server_len = server_state.len;
    handle_forward_final_result(client_state.result, client_len, "client->server");
    handle_forward_final_result(server_state.result, server_len, "server->client");
}

fn record_forward_activity(
    activity: ForwardActivity,
    state: &mut ForwardDirectionState,
    tunnel_idle_sleep: &mut Pin<&mut tokio::time::Sleep>,
    tunnel_idle_timeout: Duration,
    half_close_idle_sleep: &mut Pin<&mut tokio::time::Sleep>,
    half_close_idle_timeout: Duration,
    peer_done: bool,
) {
    state.len += activity.bytes;
    reset_sleep_at(tunnel_idle_sleep, tunnel_idle_timeout, activity.at);
    if peer_done {
        reset_sleep_at(half_close_idle_sleep, half_close_idle_timeout, activity.at);
    }
}

fn reset_sleep(sleep: &mut Pin<&mut tokio::time::Sleep>, timeout: Duration) {
    if !timeout.is_zero() {
        sleep.as_mut().reset(Instant::now() + timeout);
    }
}

fn reset_sleep_at(
    sleep: &mut Pin<&mut tokio::time::Sleep>,
    timeout: Duration,
    activity_at: Instant,
) {
    if !timeout.is_zero() {
        sleep.as_mut().reset(activity_at + timeout);
    }
}

fn handle_forward_final_result(result: Option<Result<usize>>, len: usize, detail: &'static str) {
    if let Some(result) = result {
        handle_forward_result(result, detail);
    } else {
        tracing::debug!("forward stopped before peer closed; we send {len} bytes,detail:{detail}");
    }
}

pub async fn start_datagram_forward<
    ClientReader: DatagramReader,
    ClientWriter: DatagramWriter,
    ServerReader: DatagramReader,
    ServerWriter: DatagramWriter,
>(
    client_reader: ClientReader,
    client_writer: ClientWriter,
    server_reader: ServerReader,
    server_writer: ServerWriter,
) {
    let client_to_server = transfer_datagrams("udp->tcp", client_reader, server_writer);
    let server_to_client = transfer_datagrams("tcp->udp", server_reader, client_writer);
    tokio::select! {
        result = client_to_server =>{
            handle_forward_result( result,"udp->tcp");
        },
        result = server_to_client =>{
            handle_forward_result( result,"tcp->udp");
        }
    }
}

fn handle_forward_result(result: Result<usize>, detail: &'static str) {
    match result {
        Ok(len) => tracing::info!("forward finish! we send {len} bytes,detail:{detail}"),
        Err(e) => {
            // Treat peer-initiated shutdowns as expected to avoid noisy error logs.
            if e.is_expected_disconnect() {
                tracing::debug!("forward closed by peer:{e},detail:{detail}");
            } else {
                tracing::error!("got forward error:{e},detail:{detail}");
            }
        }
    }
}

impl DatagramReader for UdpStreamReadHalf {
    async fn recv(&mut self) -> Result<Bytes> {
        self.recv_datagram()
            .await
            .map_err(|e| super::error::Error::MsgForward {
                action: "read",
                source: e,
            })
    }
}

impl DatagramWriter for UdpStreamWriteHalf<'_> {
    async fn send(&mut self, src: &[u8]) -> Result<()> {
        self.send_datagram(src)
            .await
            .map_err(|e| super::error::Error::MsgForward {
                action: "write",
                source: e,
            })
    }
}

pub trait StreamForward: StreamSplit + Sized {
    fn forward_local_to_remote<'a, R, W>(
        codec_key: Option<AesKeyType>,
        local_reader: Self::ReaderRef<'a>,
        local_writer: Self::WriterRef<'a>,
        remote_reader: R,
        remote_writer: W,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>
    where
        R: AsyncReadExt + Unpin + Send + 'a,
        W: AsyncWriteExt + Unpin + Send + 'a;
}

impl StreamForward for TcpStreamImpl {
    fn forward_local_to_remote<'a, R, W>(
        codec_key: Option<AesKeyType>,
        local_reader: Self::ReaderRef<'a>,
        local_writer: Self::WriterRef<'a>,
        remote_reader: R,
        remote_writer: W,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>
    where
        R: AsyncReadExt + Unpin + Send + 'a,
        W: AsyncWriteExt + Unpin + Send + 'a,
    {
        Box::pin(async move {
            let mut local_reader = local_reader;
            let mut local_writer = local_writer;
            let mut remote_reader = remote_reader;
            let mut remote_writer = remote_writer;
            start_forward_with_codec_key!(
                codec_key,
                &mut local_reader,
                &mut local_writer,
                &mut remote_reader,
                &mut remote_writer,
                false,
                false,
                true,
                true
            );
            Ok(())
        })
    }
}

impl StreamForward for UdpStreamImpl {
    fn forward_local_to_remote<'a, R, W>(
        codec_key: Option<AesKeyType>,
        local_reader: Self::ReaderRef<'a>,
        local_writer: Self::WriterRef<'a>,
        remote_reader: R,
        remote_writer: W,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>
    where
        R: AsyncReadExt + Unpin + Send + 'a,
        W: AsyncWriteExt + Unpin + Send + 'a,
    {
        Box::pin(async move {
            let mut remote_reader = remote_reader;
            let mut remote_writer = remote_writer;
            start_datagram_forward_with_codec_key!(
                codec_key,
                local_reader,
                local_writer,
                &mut remote_reader,
                &mut remote_writer
            );
            Ok(())
        })
    }
}

#[macro_export]
macro_rules! create_component {
    (Reader, $stream:expr,true, $key:expr, $get_codec:ident, $name:expr) => {
        CodecForwardReader::new(
            $stream,
            snafu_error_get_or_return_ok!(
                $get_codec(&$key),
                concat!("failed to create decoder when `", $name, "` forward msg")
            ),
        )
    };
    (Reader, $stream:expr,false, $key:expr, $get_codec:ident, $name:expr) => {
        NormalForwardReader::new($stream)
    };
    (Writer, $stream:expr,true, $key:expr, $get_codec:ident, $name:expr) => {
        CodecForwardWriter::new(
            $stream,
            snafu_error_get_or_return_ok!(
                $get_codec(&$key),
                concat!("failed to create encoder when `", $name, "` forward msg")
            ),
        )
    };
    (Writer, $stream:expr,false, $key:expr, $get_codec:ident, $name:expr) => {
        NormalForwardWriter::new($stream)
    };
}

/// When using it, please remember to manually import the following symbols:
/// - [`start_forward`]
/// - [`crate::create_component`]
/// - [`ForwardReader`]
/// - [`ForwardWriter`]
/// - [`CodecForwardReader`]
/// - [`CodecForwardWriter`]
/// - [`crate::snafu_error_get_or_return_ok`]
/// - [`super::get_decodec`]
/// - [`super::get_encodec`]
#[macro_export]
macro_rules! start_forward_with_codec_key {
    (
        $codec_key:expr,
        $client_reader:expr,
        $client_writer:expr,
        $server_reader:expr,
        $server_writer:expr,
        $client_reader_codec:tt,
        $client_writer_codec:tt,
        $server_reader_codec:tt,
        $server_writer_codec:tt
    ) => {
        match $codec_key {
            Some(key) => {
                (start_forward(
                    create_component!(
                        Reader,
                        $client_reader,
                        $client_reader_codec,
                        key,
                        get_decodec,
                        "client_reader"
                    ),
                    create_component!(
                        Writer,
                        $client_writer,
                        $client_writer_codec,
                        key,
                        get_encodec,
                        "client_writer"
                    ),
                    create_component!(
                        Reader,
                        $server_reader,
                        $server_reader_codec,
                        key,
                        get_decodec,
                        "server_reader"
                    ),
                    create_component!(
                        Writer,
                        $server_writer,
                        $server_writer_codec,
                        key,
                        get_encodec,
                        "server_writer"
                    ),
                )
                .await)
            }
            None => {
                (start_forward(
                    NormalForwardReader::new($client_reader),
                    NormalForwardWriter::new($client_writer),
                    NormalForwardReader::new($server_reader),
                    NormalForwardWriter::new($server_writer),
                )
                .await)
            }
        }
    };
}

#[macro_export]
macro_rules! start_datagram_forward_with_codec_key {
    (
        $codec_key:expr,
        $udp_reader:expr,
        $udp_writer:expr,
        $tcp_reader:expr,
        $tcp_writer:expr
    ) => {
        match $codec_key {
            Some(key) => {
                (start_datagram_forward(
                    $udp_reader,
                    $udp_writer,
                    CodecDatagramReader::new(
                        $tcp_reader,
                        snafu_error_get_or_return_ok!(
                            $crate::common::message::get_decodec(&key),
                            "failed to create decoder when datagram forward"
                        ),
                    ),
                    CodecDatagramWriter::new(
                        $tcp_writer,
                        snafu_error_get_or_return_ok!(
                            $crate::common::message::get_encodec(&key),
                            "failed to create encoder when datagram forward"
                        ),
                    ),
                )
                .await)
            }
            None => {
                (start_datagram_forward(
                    $udp_reader,
                    $udp_writer,
                    NormalDatagramReader::new($tcp_reader),
                    NormalDatagramWriter::new($tcp_writer),
                )
                .await)
            }
        }
    };
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::io;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use super::*;
    use crate::common::config::parse_duration;
    use crate::common::error::Error;
    use tokio::sync::Notify;

    enum ReadAction {
        Data(Vec<u8>),
        Eof,
        Pending,
        Error(io::ErrorKind),
    }

    struct ScriptedReader {
        actions: VecDeque<ReadAction>,
        current: Vec<u8>,
    }

    impl ScriptedReader {
        fn new(actions: impl IntoIterator<Item = ReadAction>) -> Self {
            Self {
                actions: actions.into_iter().collect(),
                current: Vec::new(),
            }
        }
    }

    impl ForwardReader for ScriptedReader {
        async fn read(&mut self) -> Result<&'_ [u8]> {
            match self.actions.pop_front().unwrap_or(ReadAction::Pending) {
                ReadAction::Data(data) => {
                    self.current = data;
                    Ok(&self.current)
                }
                ReadAction::Eof => {
                    self.current.clear();
                    Ok(&self.current)
                }
                ReadAction::Pending => std::future::pending().await,
                ReadAction::Error(kind) => Err(Error::MsgForward {
                    action: "read",
                    source: io::Error::new(kind, "scripted read error"),
                }),
            }
        }
    }

    #[derive(Default)]
    struct WriterState {
        chunks: Vec<Vec<u8>>,
        shutdowns: usize,
    }

    #[derive(Clone, Default)]
    struct ScriptedWriter {
        state: Arc<Mutex<WriterState>>,
    }

    impl ScriptedWriter {
        fn chunks(&self) -> Vec<Vec<u8>> {
            self.state.lock().unwrap().chunks.clone()
        }

        fn shutdowns(&self) -> usize {
            self.state.lock().unwrap().shutdowns
        }
    }

    impl ForwardWriter for ScriptedWriter {
        async fn write(&mut self, src: &[u8]) -> Result<()> {
            self.state.lock().unwrap().chunks.push(src.to_vec());
            Ok(())
        }

        async fn shutdown(&mut self) {
            self.state.lock().unwrap().shutdowns += 1;
        }
    }

    struct EofAfterWriteStartsReader {
        write_started: Arc<Notify>,
        returned_eof: bool,
        empty: Vec<u8>,
    }

    impl EofAfterWriteStartsReader {
        fn new(write_started: Arc<Notify>) -> Self {
            Self {
                write_started,
                returned_eof: false,
                empty: Vec::new(),
            }
        }
    }

    impl ForwardReader for EofAfterWriteStartsReader {
        async fn read(&mut self) -> Result<&'_ [u8]> {
            if self.returned_eof {
                return std::future::pending().await;
            }
            self.write_started.notified().await;
            self.returned_eof = true;
            Ok(&self.empty)
        }
    }

    #[derive(Clone)]
    struct DelayedWriter {
        state: Arc<Mutex<WriterState>>,
        write_started: Arc<Notify>,
        delay: Duration,
    }

    impl DelayedWriter {
        fn new(write_started: Arc<Notify>, delay: Duration) -> Self {
            Self {
                state: Arc::new(Mutex::new(WriterState::default())),
                write_started,
                delay,
            }
        }

        fn chunks(&self) -> Vec<Vec<u8>> {
            self.state.lock().unwrap().chunks.clone()
        }
    }

    impl ForwardWriter for DelayedWriter {
        async fn write(&mut self, src: &[u8]) -> Result<()> {
            self.write_started.notify_one();
            tokio::time::sleep(self.delay).await;
            self.state.lock().unwrap().chunks.push(src.to_vec());
            Ok(())
        }

        async fn shutdown(&mut self) {
            self.state.lock().unwrap().shutdowns += 1;
        }
    }

    #[test]
    fn parse_duration_accepts_suffixes_and_plain_seconds() {
        assert_eq!(parse_duration("42"), Some(Duration::from_secs(42)));
        assert_eq!(parse_duration("500ms"), Some(Duration::from_millis(500)));
        assert_eq!(parse_duration("2s"), Some(Duration::from_secs(2)));
        assert_eq!(parse_duration("3m"), Some(Duration::from_secs(180)));
        assert_eq!(parse_duration("1h"), Some(Duration::from_secs(3600)));
        assert_eq!(parse_duration(""), None);
        assert_eq!(parse_duration("bad"), None);
        assert_eq!(parse_duration("18446744073709551615h"), None);
    }

    #[tokio::test]
    async fn half_close_idle_timeout_closes_stalled_peer() {
        let client_reader = ScriptedReader::new([ReadAction::Eof]);
        let client_writer = ScriptedWriter::default();
        let server_reader = ScriptedReader::new([ReadAction::Pending]);
        let server_writer = ScriptedWriter::default();
        let server_writer_state = server_writer.clone();

        tokio::time::timeout(
            Duration::from_millis(200),
            start_forward_with_config(
                client_reader,
                client_writer,
                server_reader,
                server_writer,
                ForwardTimeoutConfig {
                    tunnel_idle_timeout: Duration::from_secs(60 * 60),
                    half_close_idle_timeout: Duration::from_millis(20),
                },
            ),
        )
        .await
        .expect("half-closed tunnel did not stop after half-close idle timeout");

        assert_eq!(server_writer_state.shutdowns(), 1);
    }

    #[tokio::test]
    async fn expected_disconnect_stops_waiting_for_pending_peer() {
        let client_reader =
            ScriptedReader::new([ReadAction::Error(io::ErrorKind::ConnectionReset)]);
        let client_writer = ScriptedWriter::default();
        let server_reader = ScriptedReader::new([ReadAction::Pending]);
        let server_writer = ScriptedWriter::default();

        tokio::time::timeout(
            Duration::from_millis(200),
            start_forward_with_config(
                client_reader,
                client_writer,
                server_reader,
                server_writer,
                ForwardTimeoutConfig {
                    tunnel_idle_timeout: Duration::from_secs(60 * 60),
                    half_close_idle_timeout: Duration::from_secs(60),
                },
            ),
        )
        .await
        .expect("expected disconnect did not stop the tunnel");
    }

    #[tokio::test]
    async fn half_closed_tunnel_drains_peer_before_timeout() {
        let client_reader = ScriptedReader::new([ReadAction::Eof]);
        let client_writer = ScriptedWriter::default();
        let client_writer_state = client_writer.clone();
        let server_reader =
            ScriptedReader::new([ReadAction::Data(b"response".to_vec()), ReadAction::Eof]);
        let server_writer = ScriptedWriter::default();

        tokio::time::timeout(
            Duration::from_millis(200),
            start_forward_with_config(
                client_reader,
                client_writer,
                server_reader,
                server_writer,
                ForwardTimeoutConfig {
                    tunnel_idle_timeout: Duration::from_secs(60 * 60),
                    half_close_idle_timeout: Duration::from_millis(200),
                },
            ),
        )
        .await
        .expect("half-closed tunnel failed to drain the peer");

        assert_eq!(client_writer_state.chunks(), vec![b"response".to_vec()]);
        assert_eq!(client_writer_state.shutdowns(), 1);
    }

    #[tokio::test]
    async fn delayed_tail_write_survives_peer_half_close() {
        let write_started = Arc::new(Notify::new());
        let client_reader = EofAfterWriteStartsReader::new(write_started.clone());
        let client_writer = DelayedWriter::new(write_started, Duration::from_millis(20));
        let client_writer_state = client_writer.clone();
        let tail = vec![0x5a; 499];
        let server_reader = ScriptedReader::new([ReadAction::Data(tail.clone()), ReadAction::Eof]);
        let server_writer = ScriptedWriter::default();

        tokio::time::timeout(
            Duration::from_millis(300),
            start_forward_with_config(
                client_reader,
                client_writer,
                server_reader,
                server_writer,
                ForwardTimeoutConfig {
                    tunnel_idle_timeout: Duration::from_secs(60 * 60),
                    half_close_idle_timeout: Duration::from_millis(200),
                },
            ),
        )
        .await
        .expect("delayed response tail was lost after the peer half-closed");

        assert_eq!(client_writer_state.chunks(), vec![tail]);
    }

    #[tokio::test]
    async fn open_tunnel_idle_timeout_closes_inactive_tunnel() {
        let client_reader = ScriptedReader::new([ReadAction::Pending]);
        let client_writer = ScriptedWriter::default();
        let server_reader = ScriptedReader::new([ReadAction::Pending]);
        let server_writer = ScriptedWriter::default();

        tokio::time::timeout(
            Duration::from_millis(200),
            start_forward_with_config(
                client_reader,
                client_writer,
                server_reader,
                server_writer,
                ForwardTimeoutConfig {
                    tunnel_idle_timeout: Duration::from_millis(20),
                    half_close_idle_timeout: Duration::from_secs(60),
                },
            ),
        )
        .await
        .expect("inactive open tunnel did not stop after tunnel idle timeout");
    }
}
