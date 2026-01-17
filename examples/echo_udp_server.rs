use std::error::Error;
use std::net::SocketAddr;
use std::str::FromStr;

use pb_mapper::common::config::init_tracing;
use uni_stream::udp::UdpListener;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const UDP_BUFFER_SIZE: usize = 16 * 1024; // 16kb

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    init_tracing();

    let listener = UdpListener::bind(SocketAddr::from_str("[::1]:8080")?).await?;
    loop {
        let (mut stream, _) = listener.accept().await?;
        tracing::info!("start handle addr:{}", stream.peer_addr().unwrap());
        tokio::spawn(async move {
            let id = std::thread::current().id();
            let block = async move {
                let mut buf = vec![0u8; UDP_BUFFER_SIZE];
                loop {
                    let n = stream.read(&mut buf).await?;
                    stream.write_all(&buf[0..n]).await?;

                    tracing::info!(
                        "{:?} echoed {:?} for {} bytes,text:{}",
                        id,
                        stream.peer_addr(),
                        n,
                        String::from_utf8_lossy(&buf[0..n])
                    );
                }
                #[allow(unreachable_code)]
                Ok::<(), std::io::Error>(())
            };
            if let Err(e) = block.await {
                tracing::error!("error: {:?}", e);
            }
        });
    }
}
