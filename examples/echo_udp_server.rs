use std::error::Error;
use std::net::SocketAddr;
use std::str::FromStr;
use std::time::Duration;

use pb_mapper::common::config::init_tracing;
use pb_mapper::utils::udp::UdpListener;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::time::timeout;

const UDP_BUFFER_SIZE: usize = 17480; // 17kb
const UDP_TIMEOUT: u64 = 10 * 1000; // 10sec

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    init_tracing();

    let listener = UdpListener::bind(SocketAddr::from_str("127.0.0.1:8080")?).await?;
    loop {
        let (mut stream, _) = listener.accept().await?;
        tracing::info!("start handle addr:{}", stream.peer_addr().unwrap());
        tokio::spawn(async move {
            let id = std::thread::current().id();
            let block = async move {
                let mut buf = vec![0u8; UDP_BUFFER_SIZE];
                let duration = Duration::from_micros(UDP_TIMEOUT);
                loop {
                    let n = timeout(duration, stream.read(&mut buf)).await??;
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
