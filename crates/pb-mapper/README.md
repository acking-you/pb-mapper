# pb-mapper

Client SDK for [pb-mapper](https://github.com/acking-you/pb-mapper): publish a
private TCP/UDP service with `register` and reach it on a local port with
`connect`. Every connection goes peer to peer over QUIC when the networks allow
it and falls back to the relay before any byte is sent when they do not.
Reconnection, DNS refresh and recovery after network changes are automatic.

```toml
[dependencies]
pb-mapper = "0.8"
tokio = { version = "1", features = ["full"] }
```

```rust,no_run
use pb_mapper::{Client, ClientConfig, ConnectRequest, RegisterRequest, Transport};

# async fn example() -> pb_mapper::Result<()> {
let client = Client::new(ClientConfig {
    server: "relay.example.com:7666".into(),
    credential: "pbmt1_...".into(),
    keep_alive: true,
    namespace: None,
})?;

// Publish a local service...
let registration = client
    .register(RegisterRequest {
        key: "web".into(),
        local_addr: "127.0.0.1:8080".into(),
        transport: Transport::Tcp,
        codec: false,
        force_namespace: false,
    })
    .await?;
registration.wait_ready().await?;

// ...and reach it on a local port, direct first.
let connection = client
    .connect(ConnectRequest {
        key: "web".into(),
        local_addr: "127.0.0.1:9090".into(),
        transport: Transport::Tcp,
    })
    .await?;
connection.wait_ready().await?;

connection.stop().await?;
registration.stop().await?;
# Ok(())
# }
```

Requires a Tokio runtime. The `pb-mapper` CLI and relay live in the
`pb-mapper-cli` package.

## What the SDK covers

| Need | API |
| --- | --- |
| Publish / reach a TCP or UDP service | `Client::register`, `Client::connect`, `Transport::{Tcp, Udp}` |
| Choose the path | `RegisterOptions { allow_direct }`, `ConnectOptions { path: PathPolicy::{Auto, RelayOnly} }` |
| Ask the router to map the direct port | `port_mapping: true` in either options, with the `port-mapping` Cargo feature; off by default |
| Reclaim abandoned TCP data connections | `register_with_idle_timeout`, `RegisterOptions { idle_timeout }` |
| Listen to a tunnel | `subscribe()` for status, `subscribe_events()` for every event as it happens, `diagnostics()` for a snapshot |
| Wake recovery after a network change or resume | `Client::notify_network_change` |
| Inspect a namespace | `list_keys`, `service_status`, `sessions`, `remote_id` |
| Manage credentials (administrator key) | `admin()`: `issue_key`, `show_key`, `reveal_key`, `renew_key`, `revoke_key`, `list_keys_all`, `gc_keys`, `auth_status` |
| Operate the relay | `admin()`: `list_services_all`, `list_connections_all`, `retire_connections`, `set_legacy_protocol`, `rotate_root_key_to_file` |

Each row has a runnable example in
[`examples/`](https://github.com/acking-you/pb-mapper/tree/master/crates/pb-mapper/examples);
`cargo run -p pb-mapper --example quickstart` runs a relay in-process, so it
needs no setup.

## Paths

A tunnel may also ask this network's router to map its direct UDP port,
with PCP, NAT-PMP or UPnP: build with the `port-mapping` Cargo feature and set
`port_mapping: true` in `RegisterOptions` or `ConnectOptions`. It is off by
default and ignored when the tunnel stays on the relay. The mapping starts with
the tunnel's first direct session, never holds one up, is renewed while the
tunnel runs and released when it stops. A router that maps nothing leaves
direct and the relay fallback as they were. `diagnostics().port_mapping` says
where it stands; `active` stays true while a renewal is pending or has failed,
until the lease expires.

`subscribe_events()` yields the outcomes the tunnel kept
(`diagnostics().events`), then every event as it happens, until the tunnel
stops and its cleanup, the mapping's release included, has reported:

```rust,ignore
let mut events = connection.subscribe_events();
while let Some(event) = events.recv().await {
    match event {
        Ok(event) => println!("{}", serde_json::to_string(&event)?),
        // Fell more than 128 events behind; the snapshot is current.
        Err(lagged) => eprintln!("{lagged}: {:?}", connection.diagnostics()),
    }
}
```

Besides the outcomes, a stream carries the steps that lead to them: relay
phases, direct stages, sessions opening and closing, and each change of the
port mapping. `release_sent` is a PCP or NAT-PMP deletion the router does not
acknowledge; `released` is one UPnP confirmed. Each subscriber reads at its
own pace, and none keeps the tunnel alive.

The relay carrier defaults to TCP. Select QUIC with
`client.with_relay_transport(RelayTransport::Quic)?` before registering or
connecting, and inspect `client.relay_diagnostics().await` for connection reuse,
RTT, loss and byte counters. Clients of the same relay and carrier in one
process share one pool, so every tunnel rides one QUIC connection per
credential; a connection left without streams closes after 30 seconds.
`client.relay_quic_port().await?` asks, over TCP, whether the relay offers QUIC
before you switch. The relay needs `--quic-listen IP:UDPPORT`; authenticated
discovery still requires its TCP port. Against a relay without it, QUIC fails
with `Error::Remote { code: QUIC_UNAVAILABLE, retryable: false, .. }`, and a
tunnel ends as failed rather than retrying — unless this process has reached
that relay over QUIC before, when the refusal is retryable and running tunnels
wait for QUIC to come back. The `relay-quic` feature is
independent of `p2p`; disabling both removes QUIC dependencies.

With the default `p2p` feature, `register` allows direct sessions and `connect`
uses `PathPolicy::Auto`. A connection goes direct when the relay serves hole
punching (its default), both ends allow it, and the NATs can be punched;
otherwise it uses the relay. The fallback happens before the publisher opens its
local service, so nothing is replayed. After a fallback the tunnel sends its
next connections straight to the relay for a while (10 s doubling to 5 min,
cleared by a direct success or `notify_network_change`).

Established direct sessions survive coordinator loss and credential revocation;
the local handle's `stop()` closes them. `client.sessions()` returns
namespace-scoped observations, never authorization.

To stay on the relay, pass `RegisterOptions { allow_direct: false, .. }` or
`ConnectOptions { path: PathPolicy::RelayOnly }`, or build with
`default-features = false`, which also drops quinn, rcgen and stun-proto.
