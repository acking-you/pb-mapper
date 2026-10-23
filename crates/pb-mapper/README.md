# pb-mapper

Client SDK for a deployed [pb-mapper](https://github.com/acking-you/pb-mapper) relay.

```toml
pb-mapper = "0.6.1"
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

For TCP services with application heartbeats, use
`client.register_with_idle_timeout(request, std::time::Duration::from_secs(90))`
to reclaim abandoned data connections on the publisher. Activity in either
direction resets the deadline; `Duration::ZERO` disables it. UDP is rejected.
Ordinary `register` calls, other registrations, and control connections keep
their existing behavior. No relay or subscriber upgrade is required.


For optional direct connections, enable the `p2p` Cargo feature. The relay must
explicitly enable its UDP discovery service. Use
`register_with_options(request, RegisterOptions { allow_direct: true, ..Default::default() })`
and `connect_with_options(request, ConnectOptions { path: PathPolicy::Auto })`.
Existing `register`/`connect` calls retain relay-only behavior and request structs.
`client.sessions()` returns namespace-scoped observations; `diagnostics().direct`
reports local path selection and fallback counts.

Direct setup failure falls back before commit. Established direct sessions
survive coordinator loss and credential revocation; call the local handle's
`stop()` to close them. Data is never replayed onto relay after direct commit.
