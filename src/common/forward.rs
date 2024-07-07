use snafu::ResultExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::buffer::{BufferReader, BufferedReader};
use super::error::{FwdNetworkWriteWithNormalSnafu, Result};

pub trait ForwardReader {
    fn read(&mut self) -> impl std::future::Future<Output = Result<&'_ [u8]>> + Send;
}

pub trait ForwardWriter {
    fn write(&mut self, src: &[u8]) -> impl std::future::Future<Output = Result<()>> + Send;
}

pub struct NormalForwardReader<'a, T> {
    buffered_reader: BufferReader<'a, T>,
}

impl<'a, T: AsyncReadExt + Send + Unpin> NormalForwardReader<'a, T> {
    pub fn new(reader: &'a mut T) -> Self {
        Self {
            buffered_reader: BufferReader::new(reader),
        }
    }
}

impl<'a, T: AsyncReadExt + Send + Unpin> ForwardReader for NormalForwardReader<'a, T> {
    fn read(&mut self) -> impl std::future::Future<Output = Result<&'_ [u8]>> + Send {
        self.buffered_reader.read()
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
    fn write(&mut self, src: &[u8]) -> impl std::future::Future<Output = Result<()>> + Send {
        self.write_inner(src)
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
            return Ok(length);
        }
        writer.write(src).await?;
        length += n;
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
    let client_to_server = copy(client_reader, server_writer);
    let server_to_client = copy(server_reader, client_writer);
    tokio::select! {
        result = client_to_server =>{
            handle_forward_result( result,"client->server");
        },
        result = server_to_client =>{
            handle_forward_result( result,"server->client");
        }
    }
}

fn handle_forward_result(result: Result<usize>, detail: &'static str) {
    match result {
        Ok(len) => tracing::info!("forward finish! we send {len} bytes,detail:{detail}"),
        Err(e) => tracing::error!("got forward error:{e},detail:{detail}"),
    }
}
