//! Publish or reach a UDP service, such as a game server or a DNS resolver.
//!
//! ```text
//! # next to the service
//! cargo run -p pb-mapper --example udp -- register game 127.0.0.1:8211
//! # wherever players are
//! cargo run -p pb-mapper --example udp -- connect game 127.0.0.1:8211
//! ```
//!
//! Both read `PB_MAPPER_SERVER` and `MSG_HEADER_KEY`. Datagram boundaries are
//! preserved on the direct path and on the relay alike; the only difference
//! from TCP is `Transport::Udp`.

use pb_mapper::{Client, ClientConfig, ConnectRequest, RegisterRequest, Transport};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let usage = "usage: udp <register|connect> <key> <addr>";
    let (role, key, addr) = match (args.next(), args.next(), args.next()) {
        (Some(role), Some(key), Some(addr)) => (role, key, addr),
        _ => return Err(usage.into()),
    };

    let client = Client::new(ClientConfig {
        server: std::env::var("PB_MAPPER_SERVER")?,
        credential: std::env::var("MSG_HEADER_KEY")?,
        keep_alive: true,
        namespace: None,
    })?;

    match role.as_str() {
        "register" => {
            let registration = client
                .register(RegisterRequest {
                    key,
                    local_addr: addr,
                    transport: Transport::Udp,
                    codec: false,
                    force_namespace: false,
                })
                .await?;
            registration.wait_ready().await?;
            println!("publishing UDP `{}`", registration.key());
            tokio::signal::ctrl_c().await?;
            registration.stop().await?;
        }
        "connect" => {
            let connection = client
                .connect(ConnectRequest {
                    key,
                    local_addr: addr.clone(),
                    transport: Transport::Udp,
                })
                .await?;
            connection.wait_ready().await?;
            println!("UDP `{}` is reachable at {addr}", connection.key());
            tokio::signal::ctrl_c().await?;
            connection.stop().await?;
        }
        _ => return Err(usage.into()),
    }
    Ok(())
}
