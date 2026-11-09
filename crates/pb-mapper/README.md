# pb-mapper

Client SDK for [pb-mapper](https://github.com/acking-you/pb-mapper): publish a
private TCP/UDP service with `register` and reach it on a local port with
`connect`. Every connection goes peer to peer over QUIC when the networks allow
it and falls back to the relay before any byte is sent when they do not.
Reconnection, DNS refresh and recovery after network changes are automatic.
With the `desktop` feature it also shares and views a remote desktop over the
same relay ([Remote desktop](#remote-desktop)).

```toml
[dependencies]
pb-mapper = "0.11"
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
| Carry a LAN path over TCP | `ConnectOptions { lan_transport: LanTransport::Tcp }`, `LanTransport::Quic` by default; the publisher serves it unless `RegisterOptions { lan_tcp: false }`; a path across the internet is always QUIC |
| Reclaim abandoned TCP data connections | `register_with_idle_timeout`, `RegisterOptions { idle_timeout }` |
| Listen to a tunnel | `subscribe()` for status, `subscribe_events()` for every event as it happens, `subscribe_metrics(every)` for sessions, bytes per path and round trips as they change, `diagnostics()` for a snapshot |
| Watch the relay carrier | `Client::relay_diagnostics`, `Client::subscribe_relay_metrics(every)` |
| Wake recovery after a network change or resume | `Client::notify_network_change` |
| Inspect a namespace | `list_keys`, `service_status`, `sessions`, `remote_id` |
| Manage credentials (administrator key) | `admin()`: `issue_key`, `show_key`, `reveal_key`, `renew_key`, `revoke_key`, `list_keys_all`, `gc_keys`, `auth_status` |
| Operate the relay | `admin()`: `list_services_all`, `list_connections_all`, `retire_connections`, `set_legacy_protocol`, `rotate_root_key_to_file` |
| Share or view a remote desktop | the `desktop` feature: [Remote desktop](#remote-desktop) |

Each row has a runnable example in
[`examples/`](https://github.com/acking-you/pb-mapper/tree/master/crates/pb-mapper/examples);
`cargo run -p pb-mapper --example quickstart` runs a relay in-process, so it
needs no setup.

| Example | Shows |
| --- | --- |
| [`quickstart`](https://github.com/acking-you/pb-mapper/blob/master/crates/pb-mapper/examples/quickstart.rs) | An embedded relay, a published echo service, a direct round trip, and the path it took |
| [`register_tcp`](https://github.com/acking-you/pb-mapper/blob/master/crates/pb-mapper/examples/register_tcp.rs) / [`connect_tcp`](https://github.com/acking-you/pb-mapper/blob/master/crates/pb-mapper/examples/connect_tcp.rs) | The two ends of a TCP tunnel, with the live direct/relay split |
| [`udp`](https://github.com/acking-you/pb-mapper/blob/master/crates/pb-mapper/examples/udp.rs) | Both ends of a UDP tunnel, such as a game server |
| [`path_options`](https://github.com/acking-you/pb-mapper/blob/master/crates/pb-mapper/examples/path_options.rs) | `allow_direct: false`, `PathPolicy::RelayOnly`, idle deadlines |
| [`port_mapping`](https://github.com/acking-you/pb-mapper/blob/master/crates/pb-mapper/examples/port_mapping.rs) | `PortMapping::{Auto, Always, Off}`: the punch that failed, the router found, the lease and its renewals |
| [`watch_events`](https://github.com/acking-you/pb-mapper/blob/master/crates/pb-mapper/examples/watch_events.rs) | Status stream, event history, live direct stage, JSON diagnostics |
| [`status`](https://github.com/acking-you/pb-mapper/blob/master/crates/pb-mapper/examples/status.rs) | Services, control connections and sessions in a namespace |
| [`admin_credentials`](https://github.com/acking-you/pb-mapper/blob/master/crates/pb-mapper/examples/admin_credentials.rs) | Issue, show, reveal, renew, list, revoke and collect credentials |
| [`admin_inventory`](https://github.com/acking-you/pb-mapper/blob/master/crates/pb-mapper/examples/admin_inventory.rs) | Every tenant's services and connections, retiring stuck ones, root-key rotation |
| [`remote_control`](https://github.com/acking-you/pb-mapper/blob/master/crates/pb-mapper/examples/remote_control.rs) | The agent-harness pattern: one credential per session, isolated, revoked at the end |

The remote desktop's examples are [below](#remote-desktop).

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

## Remote desktop

```toml
pb-mapper = { version = "0.11", features = ["desktop"] }
```

The `desktop` feature exposes `pb_mapper::desktop`: a host shares a display,
and a viewer watches it and, when allowed, controls it, over the `Client` both
already have. A host grants viewing through a secret `pbd1.` invitation, which
names its service, pins its certificate's fingerprint for TLS 1.3 and carries
a bearer token; control is a separate permission the host allows and the viewer
asks for. Hosting is Windows today, with GPU H.264 and H.265 and an opt-in
privileged helper for UAC and the lock screen; viewing runs anywhere. Both ends
take the tunnel defaults: LAN first, then direct, then the relay. Nothing
captures or injects until a host is started and an authorized viewer connects,
and an SDK built without the feature carries none of it. The helper's real UAC
acceptance and the 60+ FPS end-to-end UI target are still unverified. See the
[setup, measurements and support matrix](../../docs/remote-desktop.md).

| Need | API |
| --- | --- |
| Host this computer | `Host::builder(HostOptions::new("desktop:office")).start(&client)`; refused before registering where `host_support().host` is false (off Windows, or Windows without Media Foundation), `host_support().reason` saying why |
| Choose what is shared | `HostOptions { display: DisplaySelection::{Primary, Id, Device}, access: Access::{Ordinary, Privileged}, allow_control }`, `displays()` |
| Capture with your own code | `HostBuilder::backend(\|\| Ok(backend))` with a `Backend`, built and used on its session's thread, so it need not be `Send` |
| Keep an invitation valid across restarts | `Host::identity()` and `HostBuilder::identity`; `trust::TrustFile` seals one with a `trust::Sealer` (`trust::CurrentUser` DPAPI on Windows); `restart`, `switch_display` and `rename` keep it |
| Run a host | `status()`, `watch(every)`, `end_session()`, `set_control_allowed()`, `stop()`, `stop_with(StopReason)`, `metrics()` |
| View | `Viewer::connect(&client, &invitation, ConnectOptions::default(), config)`, a typed `ConnectError`; `config` from `ViewerConfig::for_decoders(decoders, resolution, refresh_hz, max_fps)`, the display's own size by default, or `ViewerConfig::new(codecs)` |
| Take the picture | `viewer.video_sink().attach()` → `VideoConsumer`: see [Decoding](#decoding) |
| Control | `request_control(true)`, then `input(generation, &[Input])` in the host display's pixels and a typed `InputError`; `view().display_point(nx, ny)`, `Input::text_chunks(text)`, `release_input()` |
| Follow a viewer | `status()`, `watch(every)`, the allocation-free `view()` per frame, `metrics()`, `subscribe_metrics(every)` |
| Change a session live | `set_quality(QualityPreset)`, `pause()`, `resume()`, `request_keyframe(reason)` |
| Change the host display's mode | `Viewer::set_display_mode(ModeTarget::{FitLocal(Mode), Size(Mode), Original})` while controlling, a typed `DisplayModeError`; `ViewerStatus.modes` (`ModeList::fit_local`), `HostStatus.display_mode`, `Host::restore_display_mode()`. The host puts the mode back when its viewer leaves or loses control, and `recover_display_modes(state_dir)` after a crash; an own `Backend` implements `display_modes`, `set_display_mode` and `restore_display_mode` |
| Stay connected | `supervisor::Supervisor`, the app's policy: backoff of 1 to 15 s within 120 s, a stall after 5 s without video, H.265 → H.264 after a decoder failure, keeping the preset and the app's control choice; or the pure `supervisor::Reconnect` under your own timers |
| Share control with your own input | `HostBuilder::admission` with a `ControlAdmission`, whose lease is held only while a viewer controls |
| Drive the privileged helper (Windows) | `privileged::{support, status, request_installation}`, `privileged::background` |

| Example | Runs where | Shows |
| --- | --- | --- |
| `desktop_loopback` | any OS, no setup | A relay, a host of an example-defined `Backend` and a viewer in one process: control, a move, a click and chunked text the backend records, a preset change, pause and resume, a display fitted to the viewer and put back when it leaves, status and metrics. `--serve` hosts the synthetic desktop for the examples below |
| `desktop_host` | Windows; elsewhere it prints `host_support().reason` | `HostOptions::new`, `--display`, `--allow-control`, `--relay-only`, `--privileged`, `--identity DIR` (a `TrustFile`); the invitation; a `watch()` loop; Ctrl+C stops |
| `desktop_viewer` | any OS | `ViewerConfig::for_decoders` for an H.264 decoder; every `ConnectError`; a thread recording Annex B to `remote.h264` for `ffplay`; status and metrics; `--resolution`, `--quality`, `--relay-only`, `--lan-transport` |
| `desktop_control` | any OS, against a host you own | Taking control, `display_point`, batches, `text_chunks`, every `InputError`, `release_input` on the way out |
| `desktop_supervised` | any OS, no setup | `Supervisor` through an H.265 decoder failure, a host restart, a stall and a stop |
| `desktop_display_mode` | any OS, against a host you own; `desktop_loopback --serve` is the safe one | The host display's `ModeList`; `--fit WxH`, `--mode WxH`, `--original`; a `DisplayModeError` the viewer refuses with, and the host's refusal notice; the mode put back at the disconnect |

`cargo run -p pb-mapper --features desktop --example desktop_loopback` needs no
relay of your own and runs in CI.

### Decoding

The SDK carries video and ships no decoder. Each `VideoUnit` is one Annex B
access unit of H.264 or H.265, or one JPEG picture, with its codec, size,
`keyframe`, `discontinuity`, `generation`, `frame_id` and `pts_us`. Offer only
what you decode: `ViewerConfig::for_decoders(Decoders { h264, h265 }, ..)`
makes the offer and fits the picture inside each decoder's largest size; by
hand, `ViewerConfig.codecs` lists H.264 or JPEG or both, and H.265 beside them
when you decode it too. The host picks one for the session.
Decode with what the platform has (Media Foundation, VideoToolbox, MediaCodec,
openh264, FFmpeg) on the consumer's own thread: `VideoConsumer::next` blocks,
and never on Tokio. The sink keeps only the newest decodable units: when the
consumer falls behind it drops to the next IDR and caps the host's frame rate
to what the consumer presents.

- After a `discontinuity`, drop what the decoder holds and start again at the
  keyframe the sink hands out next.
- Call `presented(generation, frame_id)` for each frame actually shown. Input
  is authorised only by a recent presentation, never by transport receipts:
  a viewer that never presents cannot control, by design.
- Report a decoder error with `failed(message)`, which also asks for an IDR,
  and ask for one alone with `keyframe(reason)`. Requests are rate-limited;
  five errors in 30 s with nothing shown end the session with
  `decoder_failed`, which `Supervisor` answers by connecting again without
  H.265.
- `Pull::NeedCapacity(n)` asks for a buffer of `n` bytes, `Pull::Paused` means
  the viewer is paused (stop counting stalls), and `Pull::Closed` ends the
  consumer.

The host's identity is a private key and a bearer grant: whoever holds it can
be the host its invitations name. Storing and revoking it belong to the
embedding application; `trust::TrustFile` keeps one sealed per directory,
bound to the relay and namespace it was made for, and deleting the file
revokes it.
