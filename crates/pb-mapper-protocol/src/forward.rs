use bytes::Bytes;
use snafu::ResultExt;
use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::time::Instant;

use super::{
    CodecMessageReader, CodecMessageWriter, MessageReader, MessageWriter, NormalMessageReader,
    NormalMessageWriter,
};
use crate::buffer::BufferReader;
use crate::data::DataCodec;
use pb_mapper_core::checksum::AesKeyType;
use pb_mapper_core::codec::{Decryptor, Encryptor};
use pb_mapper_core::config;
use pb_mapper_core::error::{FwdNetworkWriteWithNormalSnafu, Result};
use uni_stream::stream::{StreamSplit, TcpStreamImpl, UdpStreamImpl};
use uni_stream::udp::{UdpStreamReadHalf, UdpStreamWriteHalf};

#[expect(
    async_fn_in_trait,
    reason = "awaited only on the connection task that owns the stream, never sent to another"
)]
pub trait ForwardReader {
    async fn read(&mut self) -> Result<&'_ [u8]>;
}

#[expect(
    async_fn_in_trait,
    reason = "awaited only on the connection task that owns the stream, never sent to another"
)]
pub trait ForwardWriter {
    async fn write(&mut self, src: &[u8]) -> Result<()>;

    // Gracefully close the write side to support half-closed TCP streams.
    async fn shutdown(&mut self);
}

#[expect(
    async_fn_in_trait,
    reason = "awaited only on the connection task that owns the stream, never sent to another"
)]
pub trait DatagramReader {
    async fn recv(&mut self) -> Result<Bytes>;
}

#[expect(
    async_fn_in_trait,
    reason = "awaited only on the connection task that owns the stream, never sent to another"
)]
pub trait DatagramWriter {
    async fn send(&mut self, src: &[u8]) -> Result<()>;
}

/// Application bytes forwarded in each direction, as they pass: `sent` from
/// the local end to the remote one, `received` back. Shared by whoever wants
/// to read them while forwarding runs; each chunk or datagram costs one
/// relaxed add.
#[derive(Debug, Default)]
pub struct ByteMeter {
    sent: AtomicU64,
    received: AtomicU64,
}

impl ByteMeter {
    /// What has passed so far.
    pub fn totals(&self) -> ForwardTotals {
        ForwardTotals {
            sent: self.sent.load(Ordering::Relaxed),
            received: self.received.load(Ordering::Relaxed),
        }
    }

    /// Count `bytes` sent, for a transport that forwards on its own.
    pub fn add_sent(&self, bytes: u64) {
        self.sent.fetch_add(bytes, Ordering::Relaxed);
    }

    /// Count `bytes` received, for a transport that forwards on its own.
    pub fn add_received(&self, bytes: u64) {
        self.received.fetch_add(bytes, Ordering::Relaxed);
    }
}

/// Application bytes one forward moved: `sent` from the local end to the
/// remote one, `received` back.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ForwardTotals {
    pub sent: u64,
    pub received: u64,
}

/// Where one direction's bytes are counted: its own total, and the shared
/// meter's counter for that direction when there is one.
#[derive(Clone, Copy)]
struct Tally<'a> {
    own: &'a AtomicU64,
    meter: Option<&'a AtomicU64>,
}

impl Tally<'_> {
    fn add(self, n: usize) {
        let n = n as u64;
        self.own.fetch_add(n, Ordering::Relaxed);
        if let Some(meter) = self.meter {
            meter.fetch_add(n, Ordering::Relaxed);
        }
    }
}

tokio::task_local! {
    static TUNNEL_IDLE_OVERRIDE: Duration;
}

/// Apply an idle deadline to forwarding polled by this future only.
///
/// Activity in either direction resets the deadline. This does not change
/// process-wide defaults, half-close draining, or independently spawned tasks.
/// A zero duration disables the idle deadline.
pub async fn with_tunnel_idle_timeout<F: Future>(timeout: Duration, forward: F) -> F::Output {
    TUNNEL_IDLE_OVERRIDE.scope(timeout, forward).await
}

#[derive(Debug, Clone, Copy)]
struct ForwardTimeoutConfig {
    tunnel_idle_timeout: Duration,
    half_close_idle_timeout: Duration,
}

impl ForwardTimeoutConfig {
    fn from_env() -> Self {
        Self {
            tunnel_idle_timeout: TUNNEL_IDLE_OVERRIDE
                .try_with(|timeout| *timeout)
                .unwrap_or_else(|_| config::tunnel_idle_timeout()),
            half_close_idle_timeout: config::half_close_idle_timeout(),
        }
    }
}

pub struct NormalForwardReader<'a, T> {
    buffered_reader: BufferReader<'a, T>,
}

impl<'a, T: AsyncReadExt + Unpin> NormalForwardReader<'a, T> {
    pub fn new(reader: &'a mut T) -> Self {
        Self {
            buffered_reader: BufferReader::new(reader),
        }
    }
}

impl<'a, T: AsyncReadExt + Unpin> ForwardReader for NormalForwardReader<'a, T> {
    async fn read(&mut self) -> Result<&'_ [u8]> {
        self.buffered_reader.read().await
    }
}

pub struct NormalDatagramReader<'a, T> {
    reader: NormalMessageReader<'a, T>,
}

impl<'a, T: AsyncReadExt + Unpin> NormalDatagramReader<'a, T> {
    pub fn new(reader: &'a mut T) -> Self {
        Self {
            reader: NormalMessageReader::new(reader),
        }
    }

    pub fn with_checksum_key(self, key: AesKeyType) -> Self {
        Self {
            reader: self.reader.with_checksum_key(key),
        }
    }
}

impl<'a, T: AsyncReadExt + Unpin> DatagramReader for NormalDatagramReader<'a, T> {
    async fn recv(&mut self) -> Result<Bytes> {
        let msg = self.reader.read_msg().await?;
        Ok(Bytes::copy_from_slice(msg))
    }
}

pub struct NormalForwardWriter<'a, T> {
    writer: &'a mut T,
}

impl<'a, T: AsyncWriteExt + Unpin> NormalForwardWriter<'a, T> {
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

impl<'a, T: AsyncWriteExt + Unpin> ForwardWriter for NormalForwardWriter<'a, T> {
    async fn write(&mut self, src: &[u8]) -> Result<()> {
        self.write_inner(src).await
    }

    async fn shutdown(&mut self) {
        let _ = self.writer.shutdown().await;
    }
}

pub struct NormalDatagramWriter<'a, T> {
    writer: NormalMessageWriter<'a, T>,
}

impl<'a, T: AsyncWriteExt + Unpin> NormalDatagramWriter<'a, T> {
    pub fn new(writer: &'a mut T) -> Self {
        Self {
            writer: NormalMessageWriter::new(writer),
        }
    }

    pub fn with_checksum_key(self, key: AesKeyType) -> Self {
        Self {
            writer: self.writer.with_checksum_key(key),
        }
    }
}

impl<'a, T: AsyncWriteExt + Unpin> DatagramWriter for NormalDatagramWriter<'a, T> {
    async fn send(&mut self, src: &[u8]) -> Result<()> {
        self.writer.write_msg(src).await
    }
}

pub struct CodecForwardReader<'a, T, D>(CodecMessageReader<'a, T, D>);

impl<'a, T: AsyncReadExt + Unpin, D: Decryptor> CodecForwardReader<'a, T, D> {
    pub fn new(reader: &'a mut T, decryptor: D) -> Self {
        Self(CodecMessageReader::new(reader, decryptor))
    }

    pub fn with_checksum_key(self, key: AesKeyType) -> Self {
        Self(self.0.with_checksum_key(key))
    }
}

impl<'a, T: AsyncReadExt + Unpin, D: Decryptor> ForwardReader for CodecForwardReader<'a, T, D> {
    async fn read(&mut self) -> Result<&'_ [u8]> {
        self.0.read_stream().await
    }
}

pub struct CodecDatagramReader<'a, T, D>(CodecMessageReader<'a, T, D>);

impl<'a, T: AsyncReadExt + Unpin, D: Decryptor> CodecDatagramReader<'a, T, D> {
    pub fn new(reader: &'a mut T, decryptor: D) -> Self {
        Self(CodecMessageReader::new(reader, decryptor))
    }

    pub fn with_checksum_key(self, key: AesKeyType) -> Self {
        Self(self.0.with_checksum_key(key))
    }
}

impl<'a, T: AsyncReadExt + Unpin, D: Decryptor> DatagramReader for CodecDatagramReader<'a, T, D> {
    async fn recv(&mut self) -> Result<Bytes> {
        let msg = self.0.read_msg().await?;
        Ok(Bytes::copy_from_slice(msg))
    }
}

pub struct CodecForwardWriter<'a, T, E>(CodecMessageWriter<'a, T, E>);

impl<'a, T: AsyncWriteExt + Unpin, E: Encryptor> CodecForwardWriter<'a, T, E> {
    pub fn new(writer: &'a mut T, encryptor: E) -> Self {
        Self(CodecMessageWriter::new(writer, encryptor))
    }

    pub fn with_checksum_key(self, key: AesKeyType) -> Self {
        Self(self.0.with_checksum_key(key))
    }
}

impl<'a, T: AsyncWriteExt + Unpin, E: Encryptor> ForwardWriter for CodecForwardWriter<'a, T, E> {
    async fn write(&mut self, src: &[u8]) -> Result<()> {
        self.0.write_msg(src).await
    }

    async fn shutdown(&mut self) {
        let _ = self.0.shutdown().await;
    }
}

pub struct CodecDatagramWriter<'a, T, E>(CodecMessageWriter<'a, T, E>);

impl<'a, T: AsyncWriteExt + Unpin, E: Encryptor> CodecDatagramWriter<'a, T, E> {
    pub fn new(writer: &'a mut T, encryptor: E) -> Self {
        Self(CodecMessageWriter::new(writer, encryptor))
    }

    pub fn with_checksum_key(self, key: AesKeyType) -> Self {
        Self(self.0.with_checksum_key(key))
    }
}

impl<'a, T: AsyncWriteExt + Unpin, E: Encryptor> DatagramWriter for CodecDatagramWriter<'a, T, E> {
    async fn send(&mut self, src: &[u8]) -> Result<()> {
        let buf = src.to_vec();
        self.0.write_msg(&buf).await
    }
}

/// Relay datagrams until either side fails, counting each into `tally`.
async fn transfer_counted<R: DatagramReader, W: DatagramWriter>(
    label: &'static str,
    mut reader: R,
    mut writer: W,
    tally: Tally<'_>,
) -> Result<Infallible> {
    loop {
        let src = reader.recv().await?;
        let n = src.len();
        tracing::debug!("datagram forward {label} {n} bytes");
        writer.send(&src).await?;
        tally.add(n);
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
        None,
    )
    .await;
}

/// Both directions are polled by the same forwarding task. A short, uncontended
/// stack-owned lock makes the futures Send without allocating or queueing progress.
struct ForwardActivity<'a> {
    state: parking_lot::Mutex<(u64, Instant)>,
    /// The shared meter's counter for this direction, if any.
    meter: Option<&'a AtomicU64>,
}
impl<'a> ForwardActivity<'a> {
    fn new(meter: Option<&'a AtomicU64>) -> Self {
        Self {
            state: parking_lot::Mutex::new((0, Instant::now())),
            meter,
        }
    }
    fn record(&self, n: usize) {
        if let Some(meter) = self.meter {
            meter.fetch_add(n as u64, Ordering::Relaxed);
        }
        let mut state = self.state.lock();
        state.0 += n as u64;
        state.1 = Instant::now();
    }
    fn snapshot(&self) -> (u64, Instant) {
        *self.state.lock()
    }
}

async fn copy_with_activity<R: ForwardReader, W: ForwardWriter>(
    mut reader: R,
    mut writer: W,
    activity: &ForwardActivity<'_>,
) -> Result<usize> {
    let mut length = 0;
    loop {
        let src = reader.read().await?;
        if src.is_empty() {
            break;
        }
        writer.write(src).await?;
        length += src.len();
        activity.record(src.len());
    }
    writer.shutdown().await;
    Ok(length)
}

async fn start_forward_with_config<
    CR: ForwardReader,
    CW: ForwardWriter,
    SR: ForwardReader,
    SW: ForwardWriter,
>(
    client_reader: CR,
    client_writer: CW,
    server_reader: SR,
    server_writer: SW,
    config: ForwardTimeoutConfig,
    meter: Option<&ByteMeter>,
) -> ForwardTotals {
    let client_activity = ForwardActivity::new(meter.map(|meter| &meter.sent));
    let server_activity = ForwardActivity::new(meter.map(|meter| &meter.received));
    let client_to_server = copy_with_activity(client_reader, server_writer, &client_activity);
    let server_to_client = copy_with_activity(server_reader, client_writer, &server_activity);
    tokio::pin!(client_to_server, server_to_client);
    let idle = tokio::time::sleep(config.tunnel_idle_timeout);
    let half_close = tokio::time::sleep(config.half_close_idle_timeout);
    tokio::pin!(idle, half_close);
    let mut client_result = None;
    let mut server_result = None;
    let mut half_closed_at = None;
    loop {
        let client_done = client_result.is_some();
        let server_done = server_result.is_some();
        if client_done && server_done {
            break;
        }
        tokio::select! {
            biased;
            result = &mut client_to_server, if !client_done => {
                let failed = result.is_err();
                client_result = Some(result);
                half_closed_at = Some(Instant::now());
                reset_sleep(&mut half_close, config.half_close_idle_timeout);
                if failed { break; }
            }
            result = &mut server_to_client, if !server_done => {
                let failed = result.is_err();
                server_result = Some(result);
                half_closed_at = Some(Instant::now());
                reset_sleep(&mut half_close, config.half_close_idle_timeout);
                if failed { break; }
            }
            _ = &mut idle, if !config.tunnel_idle_timeout.is_zero() && half_closed_at.is_none() => {
                let last = client_activity.snapshot().1.max(server_activity.snapshot().1);
                if last.elapsed() >= config.tunnel_idle_timeout { break; }
                reset_sleep_at(&mut idle, config.tunnel_idle_timeout, last);
            }
            _ = &mut half_close, if !config.half_close_idle_timeout.is_zero() && half_closed_at.is_some() => {
                let last = client_activity.snapshot().1.max(server_activity.snapshot().1).max(half_closed_at.unwrap_or_else(Instant::now));
                if last.elapsed() >= config.half_close_idle_timeout { break; }
                reset_sleep_at(&mut half_close, config.half_close_idle_timeout, last);
            }
        }
    }
    let totals = ForwardTotals {
        sent: client_activity.snapshot().0,
        received: server_activity.snapshot().0,
    };
    handle_forward_final_result(client_result, totals.sent, "client->server");
    handle_forward_final_result(server_result, totals.received, "server->client");
    totals
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

fn handle_forward_final_result(result: Option<Result<usize>>, len: u64, detail: &'static str) {
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
    datagram_forward(
        client_reader,
        client_writer,
        server_reader,
        server_writer,
        None,
    )
    .await;
}

/// [`start_datagram_forward`], counting both directions: the first pair is
/// the local end, so what it reads is sent.
async fn datagram_forward<
    ClientReader: DatagramReader,
    ClientWriter: DatagramWriter,
    ServerReader: DatagramReader,
    ServerWriter: DatagramWriter,
>(
    client_reader: ClientReader,
    client_writer: ClientWriter,
    server_reader: ServerReader,
    server_writer: ServerWriter,
    meter: Option<&ByteMeter>,
) -> ForwardTotals {
    let (sent, received) = (AtomicU64::new(0), AtomicU64::new(0));
    let sending = Tally {
        own: &sent,
        meter: meter.map(|meter| &meter.sent),
    };
    let receiving = Tally {
        own: &received,
        meter: meter.map(|meter| &meter.received),
    };
    let client_to_server = transfer_counted("udp->tcp", client_reader, server_writer, sending);
    let server_to_client = transfer_counted("tcp->udp", server_reader, client_writer, receiving);
    tokio::select! {
        result = client_to_server => {
            let Err(error) = result;
            handle_forward_result(Err(error), "udp->tcp");
        },
        result = server_to_client => {
            let Err(error) = result;
            handle_forward_result(Err(error), "tcp->udp");
        }
    }
    ForwardTotals {
        sent: sent.into_inner(),
        received: received.into_inner(),
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
            .map_err(|e| pb_mapper_core::error::Error::MsgForward {
                action: "read",
                source: e,
            })
    }
}

impl DatagramWriter for UdpStreamWriteHalf<'_> {
    async fn send(&mut self, src: &[u8]) -> Result<()> {
        self.send_datagram(src)
            .await
            .map_err(|e| pb_mapper_core::error::Error::MsgForward {
                action: "write",
                source: e,
            })
    }
}

pub trait StreamForward: StreamSplit + Sized {
    fn forward_local_to_remote<'a, R, W>(
        codec_key: Option<AesKeyType>,
        framing_key: AesKeyType,
        local_reader: Self::ReaderRef<'a>,
        local_writer: Self::WriterRef<'a>,
        remote_reader: R,
        remote_writer: W,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>
    where
        R: AsyncReadExt + Unpin + Send + 'a,
        W: AsyncWriteExt + Unpin + Send + 'a;

    /// Custom transports keep the legacy data format unless they implement the
    /// negotiated forwarding method and explicitly advertise support.
    fn supports_data_v2() -> bool {
        false
    }

    /// Forward using the format explicitly selected in the authenticated setup.
    fn forward_local_to_remote_with_codec<'a, R, W>(
        codec: Option<DataCodec>,
        framing_key: AesKeyType,
        local_reader: Self::ReaderRef<'a>,
        local_writer: Self::WriterRef<'a>,
        remote_reader: R,
        remote_writer: W,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>
    where
        R: AsyncReadExt + Unpin + Send + 'a,
        W: AsyncWriteExt + Unpin + Send + 'a,
    {
        if codec.is_some_and(|codec| codec.protocol().is_some()) {
            return Box::pin(async {
                Err(crate::secure::protocol_error(
                    "transport does not implement data-v2",
                ))
            });
        }
        Self::forward_local_to_remote(
            codec.map(DataCodec::key),
            framing_key,
            local_reader,
            local_writer,
            remote_reader,
            remote_writer,
        )
    }

    /// [`Self::forward_local_to_remote_with_codec`], counting the
    /// application bytes it moves: into `meter` as they pass, and in the
    /// totals it returns once both directions are done. Sent is local to
    /// remote. A transport that does not implement it forwards as before and
    /// counts nothing.
    fn forward_local_to_remote_metered<'a, R, W>(
        codec: Option<DataCodec>,
        framing_key: AesKeyType,
        local_reader: Self::ReaderRef<'a>,
        local_writer: Self::WriterRef<'a>,
        remote_reader: R,
        remote_writer: W,
        meter: Option<Arc<ByteMeter>>,
    ) -> Pin<Box<dyn Future<Output = Result<ForwardTotals>> + Send + 'a>>
    where
        R: AsyncReadExt + Unpin + Send + 'a,
        W: AsyncWriteExt + Unpin + Send + 'a,
    {
        let _ = meter;
        let forward = Self::forward_local_to_remote_with_codec(
            codec,
            framing_key,
            local_reader,
            local_writer,
            remote_reader,
            remote_writer,
        );
        Box::pin(async move { forward.await.map(|()| ForwardTotals::default()) })
    }
}

/// The legacy and negotiated entry points of a transport that implements
/// [`StreamForward::forward_local_to_remote_metered`] itself: both forward
/// through it and count nothing. The trait's defaults run the other way, for
/// transports that implement only the legacy entry point.
macro_rules! forward_through_metered {
    () => {
        fn forward_local_to_remote<'a, R, W>(
            codec_key: Option<AesKeyType>,
            framing_key: AesKeyType,
            local_reader: Self::ReaderRef<'a>,
            local_writer: Self::WriterRef<'a>,
            remote_reader: R,
            remote_writer: W,
        ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>
        where
            R: AsyncReadExt + Unpin + Send + 'a,
            W: AsyncWriteExt + Unpin + Send + 'a,
        {
            Self::forward_local_to_remote_with_codec(
                codec_key.map(DataCodec::legacy),
                framing_key,
                local_reader,
                local_writer,
                remote_reader,
                remote_writer,
            )
        }

        fn forward_local_to_remote_with_codec<'a, R, W>(
            codec_key: Option<DataCodec>,
            framing_key: AesKeyType,
            local_reader: Self::ReaderRef<'a>,
            local_writer: Self::WriterRef<'a>,
            remote_reader: R,
            remote_writer: W,
        ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>
        where
            R: AsyncReadExt + Unpin + Send + 'a,
            W: AsyncWriteExt + Unpin + Send + 'a,
        {
            let forward = Self::forward_local_to_remote_metered(
                codec_key,
                framing_key,
                local_reader,
                local_writer,
                remote_reader,
                remote_writer,
                None,
            );
            Box::pin(async move { forward.await.map(|_| ()) })
        }
    };
}

impl StreamForward for TcpStreamImpl {
    fn supports_data_v2() -> bool {
        true
    }

    forward_through_metered!();

    fn forward_local_to_remote_metered<'a, R, W>(
        codec_key: Option<DataCodec>,
        framing_key: AesKeyType,
        local_reader: Self::ReaderRef<'a>,
        local_writer: Self::WriterRef<'a>,
        remote_reader: R,
        remote_writer: W,
        meter: Option<Arc<ByteMeter>>,
    ) -> Pin<Box<dyn Future<Output = Result<ForwardTotals>> + Send + 'a>>
    where
        R: AsyncReadExt + Unpin + Send + 'a,
        W: AsyncWriteExt + Unpin + Send + 'a,
    {
        Box::pin(async move {
            // Both the accepted subscriber socket and the publisher's target
            // socket pass here. Small RPC frames must not wait for Nagle's
            // coalescing after the relay legs have already disabled it.
            if let Err(error) = local_reader.as_ref().set_nodelay(true) {
                tracing::warn!(%error, "failed to disable Nagle on local TCP stream");
            }
            let mut local_reader = local_reader;
            let mut local_writer = local_writer;
            let mut remote_reader = remote_reader;
            let mut remote_writer = remote_writer;
            // Read here, inside any idle deadline the caller scoped this in.
            let config = ForwardTimeoutConfig::from_env();
            let totals = match codec_key {
                Some(key) => {
                    let (decoder, encoder) = key.endpoint_codecs()?;
                    start_forward_with_config(
                        NormalForwardReader::new(&mut local_reader),
                        NormalForwardWriter::new(&mut local_writer),
                        CodecForwardReader::new(&mut remote_reader, decoder)
                            .with_checksum_key(framing_key),
                        CodecForwardWriter::new(&mut remote_writer, encoder)
                            .with_checksum_key(framing_key),
                        config,
                        meter.as_deref(),
                    )
                    .await
                }
                None => {
                    start_forward_with_config(
                        NormalForwardReader::new(&mut local_reader),
                        NormalForwardWriter::new(&mut local_writer),
                        NormalForwardReader::new(&mut remote_reader),
                        NormalForwardWriter::new(&mut remote_writer),
                        config,
                        meter.as_deref(),
                    )
                    .await
                }
            };
            Ok(totals)
        })
    }
}

impl StreamForward for UdpStreamImpl {
    fn supports_data_v2() -> bool {
        true
    }

    forward_through_metered!();

    fn forward_local_to_remote_metered<'a, R, W>(
        codec_key: Option<DataCodec>,
        framing_key: AesKeyType,
        local_reader: Self::ReaderRef<'a>,
        local_writer: Self::WriterRef<'a>,
        remote_reader: R,
        remote_writer: W,
        meter: Option<Arc<ByteMeter>>,
    ) -> Pin<Box<dyn Future<Output = Result<ForwardTotals>> + Send + 'a>>
    where
        R: AsyncReadExt + Unpin + Send + 'a,
        W: AsyncWriteExt + Unpin + Send + 'a,
    {
        Box::pin(async move {
            let mut remote_reader = remote_reader;
            let mut remote_writer = remote_writer;
            let totals = match codec_key {
                Some(key) => {
                    let (decoder, encoder) = key.endpoint_codecs()?;
                    datagram_forward(
                        local_reader,
                        local_writer,
                        CodecDatagramReader::new(&mut remote_reader, decoder)
                            .with_checksum_key(framing_key),
                        CodecDatagramWriter::new(&mut remote_writer, encoder)
                            .with_checksum_key(framing_key),
                        meter.as_deref(),
                    )
                    .await
                }
                None => {
                    datagram_forward(
                        local_reader,
                        local_writer,
                        NormalDatagramReader::new(&mut remote_reader)
                            .with_checksum_key(framing_key),
                        NormalDatagramWriter::new(&mut remote_writer)
                            .with_checksum_key(framing_key),
                        meter.as_deref(),
                    )
                    .await
                }
            };
            Ok(totals)
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::io;
    use std::sync::Arc;
    use std::time::Duration;

    use parking_lot::Mutex;
    use pb_mapper_core::error::Error;
    use tokio::sync::Notify;

    use super::*;

    #[tokio::test]
    async fn encrypted_stream_fin_requires_complete_authenticated_frames() {
        use crate::{data::DATA_PROTOCOL_V2, secure::HeaderProtocol};
        for protocol in [None, Some(DATA_PROTOCOL_V2)] {
            let codec = DataCodec::negotiate([7; 32], protocol, HeaderProtocol::V2).unwrap();
            let (_, encoder) = codec.endpoint_codecs().unwrap();
            let mut wire = Vec::new();
            CodecMessageWriter::for_session_key(&mut wire, encoder, [8; 32])
                .write_msg(b"response tail")
                .await
                .unwrap();
            let (decoder, _) = codec.relay_codecs().unwrap();
            let mut input = wire.as_slice();
            let mut reader =
                CodecForwardReader::new(&mut input, decoder).with_checksum_key([8; 32]);
            assert_eq!(reader.read().await.unwrap(), b"response tail");
            assert!(reader.read().await.unwrap().is_empty());

            // EOF within either the header or encrypted body is never FIN.
            for end in [3, wire.len() - 1] {
                let (decoder, _) = codec.relay_codecs().unwrap();
                let mut input = &wire[..end];
                assert!(
                    CodecForwardReader::new(&mut input, decoder)
                        .with_checksum_key([8; 32])
                        .read()
                        .await
                        .is_err()
                );
            }
            *wire.last_mut().unwrap() ^= 1;
            let (decoder, _) = codec.relay_codecs().unwrap();
            let mut input = wire.as_slice();
            assert!(
                CodecForwardReader::new(&mut input, decoder)
                    .with_checksum_key([8; 32])
                    .read()
                    .await
                    .is_err()
            );

            // A complete authenticated empty record is valid; missing control
            // and datagram frames still use the strict MessageReader API.
            let (_, encoder) = codec.endpoint_codecs().unwrap();
            let mut empty = Vec::new();
            CodecMessageWriter::for_session_key(&mut empty, encoder, [8; 32])
                .write_msg(&[])
                .await
                .unwrap();
            let (decoder, _) = codec.relay_codecs().unwrap();
            let mut input = empty.as_slice();
            let mut reader = CodecMessageReader::for_session_key(&mut input, decoder, [8; 32]);
            assert!(reader.read_msg().await.unwrap().is_empty());
            assert!(reader.read_msg().await.is_err());
        }
    }

    #[tokio::test]
    async fn tcp_forward_disables_nagle_on_the_local_leg() {
        use tokio::net::{TcpListener, TcpStream};
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (local, accepted) = tokio::join!(
            TcpStream::connect(listener.local_addr().unwrap()),
            listener.accept()
        );
        let socket = local.unwrap().into_std().unwrap();
        let observer = socket.try_clone().unwrap();
        assert!(!observer.nodelay().unwrap());
        let mut local = TcpStreamImpl::new(TcpStream::from_std(socket).unwrap());
        let (mut peer, _) = accepted.unwrap();
        let (remote, mut echo) = tokio::io::duplex(64);
        let forwarding = tokio::spawn(async move {
            let (reader, writer) = local.split();
            let (remote_reader, remote_writer) = tokio::io::split(remote);
            TcpStreamImpl::forward_local_to_remote(
                None,
                [0; 32],
                reader,
                writer,
                remote_reader,
                remote_writer,
            )
            .await
            .unwrap();
        });
        peer.write_all(b"x").await.unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), echo.read_u8())
                .await
                .unwrap()
                .unwrap(),
            b'x'
        );
        assert!(observer.nodelay().unwrap());
        forwarding.abort();
    }

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
            self.state.lock().chunks.clone()
        }

        fn shutdowns(&self) -> usize {
            self.state.lock().shutdowns
        }
    }

    impl ForwardWriter for ScriptedWriter {
        async fn write(&mut self, src: &[u8]) -> Result<()> {
            self.state.lock().chunks.push(src.to_vec());
            Ok(())
        }

        async fn shutdown(&mut self) {
            self.state.lock().shutdowns += 1;
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
            self.state.lock().chunks.clone()
        }
    }

    impl ForwardWriter for DelayedWriter {
        async fn write(&mut self, src: &[u8]) -> Result<()> {
            self.write_started.notify_one();
            tokio::time::sleep(self.delay).await;
            self.state.lock().chunks.push(src.to_vec());
            Ok(())
        }

        async fn shutdown(&mut self) {
            self.state.lock().shutdowns += 1;
        }
    }

    #[tokio::test(start_paused = true)]
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
                None,
            ),
        )
        .await
        .expect("half-closed tunnel did not stop after half-close idle timeout");

        assert_eq!(server_writer_state.shutdowns(), 1);
    }

    #[tokio::test(start_paused = true)]
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
                None,
            ),
        )
        .await
        .expect("expected disconnect did not stop the tunnel");
    }

    #[tokio::test(start_paused = true)]
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
                None,
            ),
        )
        .await
        .expect("half-closed tunnel failed to drain the peer");

        assert_eq!(client_writer_state.chunks(), vec![b"response".to_vec()]);
        assert_eq!(client_writer_state.shutdowns(), 1);
    }

    #[tokio::test(start_paused = true)]
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
                None,
            ),
        )
        .await
        .expect("delayed response tail was lost after the peer half-closed");

        assert_eq!(client_writer_state.chunks(), vec![tail]);
    }

    #[tokio::test(start_paused = true)]
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
                None,
            ),
        )
        .await
        .expect("inactive open tunnel did not stop after tunnel idle timeout");
    }

    /// Datagrams from a script, then a failure: a datagram forward ends only
    /// when one side fails. The pause lets the other side drain first.
    struct ScriptedDatagrams(VecDeque<&'static [u8]>);

    impl DatagramReader for ScriptedDatagrams {
        async fn recv(&mut self) -> Result<Bytes> {
            match self.0.pop_front() {
                Some(datagram) => Ok(Bytes::from_static(datagram)),
                None => {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    Err(Error::MsgForward {
                        action: "read",
                        source: io::ErrorKind::ConnectionReset.into(),
                    })
                }
            }
        }
    }

    struct Discard;

    impl DatagramWriter for Discard {
        async fn send(&mut self, _: &[u8]) -> Result<()> {
            Ok(())
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_metered_forward_counts_each_direction_into_its_totals_and_the_meter() {
        let meter = ByteMeter::default();
        let streamed = start_forward_with_config(
            ScriptedReader::new([ReadAction::Data(b"abc".to_vec()), ReadAction::Eof]),
            ScriptedWriter::default(),
            ScriptedReader::new([ReadAction::Data(b"hello".to_vec()), ReadAction::Eof]),
            ScriptedWriter::default(),
            ForwardTimeoutConfig {
                tunnel_idle_timeout: Duration::from_secs(60),
                half_close_idle_timeout: Duration::from_secs(1),
            },
            Some(&meter),
        )
        .await;
        assert_eq!(
            streamed,
            ForwardTotals {
                sent: 3,
                received: 5
            }
        );
        // Sent is what the local end read; the meter adds up every forward.
        let datagrams = datagram_forward(
            ScriptedDatagrams(VecDeque::from([&b"ab"[..], b"cde", b"f"])),
            Discard,
            ScriptedDatagrams(VecDeque::from([&b"xyz"[..]])),
            Discard,
            Some(&meter),
        )
        .await;
        assert_eq!(
            datagrams,
            ForwardTotals {
                sent: 6,
                received: 3
            }
        );
        assert_eq!(
            meter.totals(),
            ForwardTotals {
                sent: 9,
                received: 8
            }
        );
    }
}
