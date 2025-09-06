use pb_mapper::common::stream::{StreamProvider, TcpStreamProvider};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn echo_client<Stream: StreamProvider>(addr: &str) -> Result<(), Box<dyn std::error::Error>> {
    let mut stream = Stream::from_addr(addr).await?;
    println!("Ready to Connected to {addr}");
    let mut buffer = String::new();
    loop {
        std::io::stdin().read_line(&mut buffer)?;
        stream.write_all(buffer.as_bytes()).await?;
        let mut buf = vec![0u8; 1024];
        let n = stream.read(&mut buf).await?;
        println!("-> {}", String::from_utf8_lossy(&buf[..n]));
        buffer.clear();
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    echo_client::<TcpStreamProvider>("127.0.0.1:9090").await
}
