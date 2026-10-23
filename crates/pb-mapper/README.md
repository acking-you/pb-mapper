# pb-mapper

Client SDK for [pb-mapper](https://github.com/acking-you/pb-mapper): publish a
private TCP/UDP service with `register` and reach it on a local port with
`connect`, through the relay by default or end to end when a connection opts
into direct and the network allows it.

```toml
pb-mapper = "0.7"
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

## Direct connections with relay fallback

Enable the `p2p` feature. Direct is opt-in on every side: the relay enables its
UDP service, the publisher calls
`register_with_options(request, RegisterOptions { allow_direct: true, ..Default::default() })`,
and each connection asks with
`connect_with_options(request, ConnectOptions { path: PathPolicy::Auto })`.
Existing `register`/`connect` calls and request structs keep the relay path.

A failed setup falls back to the relay before commit; after commit nothing is
replayed onto the relay. Established direct sessions survive coordinator loss
and credential revocation; the local handle's `stop()` closes them.
`client.sessions()` returns namespace-scoped observations and
`diagnostics().direct` reports local path selection and fallback counts.
