use pb_mapper::{Client, ClientConfig, ConnectRequest, Transport};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let server = std::env::var("PB_MAPPER_SERVER")?;
    let credential = std::env::var("MSG_HEADER_KEY")?;
    let local_addr = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "127.0.0.1:9090".to_string());
    let key = std::env::args()
        .nth(2)
        .unwrap_or_else(|| "echo".to_string());

    let client = Client::new(ClientConfig {
        server,
        credential,
        keep_alive: true,
        namespace: None,
    })?;
    let connection = client
        .connect(ConnectRequest {
            key,
            local_addr,
            transport: Transport::Tcp,
        })
        .await?;
    connection.wait_ready().await?;
    println!("connected; status={:?}", connection.status());
    tokio::signal::ctrl_c().await?;
    connection.stop().await?;
    Ok(())
}
