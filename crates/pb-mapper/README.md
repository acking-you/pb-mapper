# pb-mapper

Client SDK for a deployed [pb-mapper](https://github.com/acking-you/pb-mapper) relay.

```toml
pb-mapper = "0.5"
```

```rust,no_run
use pb_mapper::{Client, ClientConfig, RegisterRequest, Transport};

# async fn example() -> pb_mapper::Result<()> {
let client = Client::new(ClientConfig {
    server: "relay.example.com:7666".into(),
    credential: "0123456789abcdefghijklmnopqrstuv".into(),
    keep_alive: true,
    namespace: None,
})?;

let registration = client
    .register(RegisterRequest {
        key: "echo".into(),
        local_addr: "127.0.0.1:8080".into(),
        transport: Transport::Tcp,
        codec: false,
        force_namespace: false,
    })
    .await?;
registration.wait_ready().await?;
registration.stop().await?;
# Ok(())
# }
```

Requires a Tokio runtime. The unified CLI is a separate package (`pb-mapper-cli`).
