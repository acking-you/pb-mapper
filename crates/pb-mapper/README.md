# pb-mapper

The optional `desktop` feature exposes `pb_mapper::desktop::{Host, HostOptions,
HostIdentity, Viewer, Invitation}` for endpoint-owned screen sharing over an
existing Client. Windows hosting includes GPU H.264 video and an opt-in privileged
helper; the existing Flutter UI supplies Windows/Android native textures and
separate view/control permissions. `Host::start` and `start_with_identity`
use the platform's own capture and encoder, and refuse before registering
where `capabilities().host` is false: off Windows, or on Windows without Media
Foundation. `start_with_backend` and `start_with_backend_and_identity` take an
embedder's own backend instead. The helper's real UAC acceptance and the 60+
FPS end-to-end UI target are still unverified. See the
[setup, measurements and support matrix](../../docs/remote-desktop.md).

A host's `invitation()` encodes as a `pbd1.` string (`Invitation::encode`,
`Invitation::decode`) that names the service, pins the host certificate's
fingerprint and carries a bearer token. `Viewer::connect(&client, &invitation,
options, ViewerConfig { codecs, limits, quality, progress })` returns once the
host accepted, or fails with a typed `ConnectError`: identity changed, version
mismatch, busy, denied and so on. Both ends speak desktop protocol v2, and an
older peer fails at the handshake. `VideoLimits::for_viewer(refresh_hz)`
bounds the encoded size and frame rate independently of the physical display
geometry used for input; `set_quality` switches the `QualityPreset` live.

Video goes to exactly one consumer: `viewer.video_sink().attach()` returns a
`VideoConsumer` whose `next` blocks on the caller's thread, never on tokio. The
sink keeps only the newest decodable frames: when the consumer falls behind it
drops to the next IDR and caps the host's frame rate to what the consumer
presents. The consumer reports each frame it actually showed with `presented`,
asks for an IDR with `keyframe`, and reports a decoder error with
`failed(message)`, which also asks for an IDR and keeps the message for the
session's end. Requests are rate-limited, and a decoder that keeps failing
ends the session with `decoder_failed`. Input is authorised only by a recent,
validated presentation: `Viewer::input(generation, &inputs)` stamps the frame
and sequence itself and fails with a typed `InputError`; transport receipts
never grant input. `status()`, and per frame the allocation-free `view()`,
report the session; `pause` and `resume` stop and restart video while the
viewer is hidden.

Desktop backends are constructed and retained on their capture/input worker;
`Backend` need not be `Send`. Embedders coordinate other input writers with a
`ControlAdmission` lease, acquired only for an explicit control request.
`HostIdentity` permits an explicitly authorized restart, but its serialized form
contains private keys and a bearer grant: storage and revocation belong to the
embedding application. The UI uses Windows DPAPI. Ordinary SDK consumers incur
none of these desktop dependencies unless the feature is enabled.

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
| Ask the router to map the direct port | `port_mapping: PortMapping::{Auto, Always, Off}` in either options; `Auto`, once punching fails, by default |
| Try a shared Wi-Fi or LAN first | `lan_first` in either options, `true` by default; `false` probes in the peer's order and waits for nothing, while the LAN is still reported |
| Reclaim abandoned TCP data connections | `register_with_idle_timeout`, `RegisterOptions { idle_timeout }` |
| Listen to a tunnel | `subscribe()` for status, `subscribe_events()` for every event as it happens, `subscribe_metrics(every)` for sessions, bytes per path and round trips as they change, `diagnostics()` for a snapshot |
| Watch the relay carrier | `Client::relay_diagnostics`, `Client::subscribe_relay_metrics(every)` |
| Wake recovery after a network change or resume | `Client::notify_network_change` |
| Inspect a namespace | `list_keys`, `service_status`, `sessions`, `remote_id` |
| Manage credentials (administrator key) | `admin()`: `issue_key`, `show_key`, `reveal_key`, `renew_key`, `revoke_key`, `list_keys_all`, `gc_keys`, `auth_status` |
| Operate the relay | `admin()`: `list_services_all`, `list_connections_all`, `retire_connections`, `set_legacy_protocol`, `rotate_root_key_to_file` |

Each row has a runnable example in
[`examples/`](https://github.com/acking-you/pb-mapper/tree/master/crates/pb-mapper/examples);
`cargo run -p pb-mapper --example quickstart` runs a relay in-process, so it
needs no setup.

## Paths

When hole punching fails, a tunnel asks this network's router to map its
direct UDP port, with PCP, NAT-PMP or UPnP, and offers the mapped address to
later sessions: `PortMapping::Auto`, the default in `RegisterOptions` and
`ConnectOptions`. Where punching works the router is never asked. `Always`
asks from the first direct session on, and `Off` never does. A mapping never
holds a session up, is renewed while the tunnel runs and released when it
stops, and is ignored while the tunnel keeps to the relay; a router that maps
nothing leaves direct and the relay fallback as they were. It needs the
`port-mapping` Cargo feature, on by default: without it `Auto` never asks and
`Always` is refused. `diagnostics().port_mapping` says where it stands
(`standby` until a punch fails); `active` stays true while a renewal is pending
or has failed, until the lease expires. `cargo run -p pb-mapper --example
port_mapping -- connect web 127.0.0.1:9090` prints each step, through the
release when it stops.

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
phases, direct stages and attempts given up, sessions opening and closing
(each closing with its duration and bytes), a LAN found and a LAN missed,
and each change of the port mapping. `release_sent` is a PCP or NAT-PMP deletion the router does not
acknowledge; `released` is one UPnP confirmed. Each subscriber reads at its
own pace, and none keeps the tunnel alive. `TunnelEventKind` is
`#[non_exhaustive]`: a `match` needs a wildcard arm.

The figures that keep changing are `metrics()`: sessions open and in all on
each path, the application bytes each path carried (`traffic.relay`,
`traffic.direct`, of which `traffic.lan`; sent is what this end's local side
sent), the relay's health with the smoothed round trip of the tunnel's
control exchanges, and the newest direct link with QUIC's figures.
`subscribe_metrics(every)` samples them at once, then every period and
whenever a session opens or closes, never twice within 100 ms, and once more
after the tunnel stops:

```rust,ignore
let mut metrics = connection.subscribe_metrics(Duration::from_secs(1));
while let Some(sample) = metrics.recv().await {
    let relay = sample.traffic.relay;
    println!("{} open, relay {}/{} bytes", sample.sessions.active_relay, relay.sent, relay.received);
}
```

Nothing samples in the background and nothing queues: each sample is taken
when `recv()` returns it, so a slow reader gets fewer, fresher ones. One
second suits a panel on screen; lists and logs need five or more. Both ends
count every session, a share's relayed ones and a relay-only connection's
included. `LinkStats` bytes are QUIC's on the wire, more than `traffic`.

The relay carrier defaults to TCP. Select QUIC with
`client.with_relay_transport(RelayTransport::Quic)?` before registering or
connecting, and inspect `client.relay_diagnostics().await` for connection reuse,
RTT, loss, bandwidth and byte counters, or follow it with
`client.subscribe_relay_metrics(every)`; `control_rtt_ms` is the smoothed
round trip of every tunnel's control exchanges, over TCP as over QUIC. Clients of the same relay and carrier in one
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
