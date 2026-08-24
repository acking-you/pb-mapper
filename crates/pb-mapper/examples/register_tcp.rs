use pb_mapper::{Client, ClientConfig, RegisterRequest, Transport};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let server = std::env::var("PB_MAPPER_SERVER")?;
    let credential = std::env::var("MSG_HEADER_KEY")?;
    let local_addr = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "127.0.0.1:8080".to_string());
    let key = std::env::args()
        .nth(2)
        .unwrap_or_else(|| "echo".to_string());

    let client = Client::new(ClientConfig {
        server,
        credential,
        keep_alive: true,
        namespace: None,
    })?;
    let registration = client
        .register(RegisterRequest {
            key,
            local_addr,
            transport: Transport::Tcp,
            codec: false,
            force_namespace: false,
        })
        .await?;
    registration.wait_ready().await?;
    println!("registered; status={:?}", registration.status());
    tokio::signal::ctrl_c().await?;
    registration.stop().await?;
    Ok(())
}
