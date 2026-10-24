<div align="center">

<img src="docs/assets/poster.png" alt="pb-mapper — P2P first, relay always: the desktop app's galaxy of this computer, its relay and every tunnel between them, the Tunnels page, and a phone connecting to a service on the relay" width="800" />

<p>
  <a href="https://www.rust-lang.org/"><img alt="Rust 2024" src="https://img.shields.io/badge/Rust-2024-000000?logo=rust&logoColor=white"></a>
  <a href="https://tokio.rs/"><img alt="Tokio" src="https://img.shields.io/badge/Async-Tokio-3873AD"></a>
  <a href="https://flutter.dev/"><img alt="Flutter" src="https://img.shields.io/badge/UI-Flutter-02569B?logo=flutter&logoColor=white"></a>
  <a href="LICENSE"><img alt="License: MIT" src="https://img.shields.io/badge/License-MIT-blue.svg"></a>
  <a href="https://github.com/acking-you/pb-mapper/releases"><img alt="Release" src="https://img.shields.io/github/v/release/acking-you/pb-mapper?logo=github&color=success"></a>
  <a href="https://crates.io/crates/pb-mapper"><img alt="crates.io" src="https://img.shields.io/crates/v/pb-mapper?logo=rust"></a>
  <a href="https://www.npmjs.com/package/pb-mapper"><img alt="npm" src="https://img.shields.io/npm/v/pb-mapper?logo=npm"></a>
  <a href="https://github.com/acking-you/pb-mapper/actions/workflows/release.yml"><img alt="Build" src="https://github.com/acking-you/pb-mapper/actions/workflows/release.yml/badge.svg"></a>
  <a href="https://github.com/acking-you/pb-mapper/stargazers"><img alt="Stars" src="https://img.shields.io/github/stars/acking-you/pb-mapper?style=social"></a>
</p>

<p>
  <a href="README.md"><b>English</b></a> ·
  <a href="README.zh-CN.md">中文</a>
</p>

</div>

---

**pb-mapper** reaches private TCP and UDP services behind NAT. Run `register`
next to a service and `connect` wherever you need it, and the service appears on
a local port. Each connection tries a **peer-to-peer QUIC path** first. If the
network will not allow one, it falls back to the **relay** before a single
byte is sent. Nobody picks a path by hand: the CLI and the SDKs decide per
connection, and keep the tunnel up through relay restarts, outages and network
changes.

```text
                 ┌──── direct: UDP hole punching + QUIC ────┐
 your app ─► connect                                         register ─► private service
   :9090         └──── fallback: relay.example.com:7666 ─────┘
```

## Why pb-mapper

**🧩 An SDK that handles the network for you.** `register` and `connect` are
the whole API. Hole punching, mutually authenticated QUIC, relay fallback,
reconnection, DNS refresh and recovery after a network change all happen inside
them, for TCP and UDP alike. One process can hold any number of tunnels, each
with its own credential.

**🔌 One TCP port relays everything; one UDP port punches for everything.**
`7666/tcp` carries registration, subscription, status, administration and
relayed bytes for every service. `7666/udp` coordinates hole punching for all of
them. Adding a service never opens another port.

**🔑 Keys, isolation and events built in.** One administrator key issues
renewable, revocable `pbmt1_` credentials, each scoped to its own namespace, so
a tenant, device or agent session sees only its own services. Every tunnel
exposes a status stream and an event history: direct established, fallback,
relay lost, network changed. That makes it straightforward to embed in any app;
remote control for an agent harness is the use case it was built for.

**⚡ Fast on either path.** In the author's field tests, direct sessions keep
interactive latency under **20 ms**, and the relay fallback matches frp's
one-port-per-mapping setup in latency and stability. How fast your own setup
is depends on the two networks and where the relay sits.

## Quick start

**1. Run a relay** on any host with a public IP:

```bash
cargo install pb-mapper-cli --locked   # or a binary from Releases
pb-mapper server                       # relay on 7666/tcp, hole punching on 7666/udp
```

Open **7666/tcp and 7666/udp** in the firewall and security group. On first
start the relay creates a random administrator key, `admin.key`, in its auth
state directory (`/var/lib/pb-mapper/auth` for Linux services).

**2. Issue a credential** for the two ends:

```bash
export MSG_HEADER_KEY="$(sudo cat /var/lib/pb-mapper/auth/admin.key)"
pb-mapper admin --server relay.example.com:7666 key issue --ttl 24h --label laptop
```

**3. Publish a service and reach it**, with the printed `pbmt1_…` credential:

```bash
# next to the service
MSG_HEADER_KEY='pbmt1_…' pb-mapper register tcp --key web --addr 127.0.0.1:8080 --server relay.example.com:7666
# wherever you need it
MSG_HEADER_KEY='pbmt1_…' pb-mapper connect tcp --key web --addr 127.0.0.1:9090 --server relay.example.com:7666
```

`127.0.0.1:9090` now reaches the service. `pb-mapper status sessions` shows
which path each connection took.

## How a connection picks its path

A connection goes direct when all of the following hold. The first three are
the defaults:

1. the relay serves hole punching (on by default, same port number over UDP);
2. the publisher allows direct (`register` does by default);
3. the consumer asks for it (`connect` uses `--path auto` by default);
4. the two networks can actually be punched through.

When any of them fails, the connection uses the relay. The switch happens
**before the publisher opens its local service and before any application byte
is sent**, so nothing is ever replayed and the backend is dialed exactly once.
After a fallback the tunnel sends its next connections straight to the relay
for a while (10 s, doubling to 5 min, cleared by a direct success or a network
change), so a network that cannot be punched costs one attempt, not one per
connection.

Turn direct off where you need to, on any side:

| Where | Flag | Effect |
| --- | --- | --- |
| Relay | `pb-mapper server --relay-only` | no UDP port; every connection is relayed |
| Publisher | `pb-mapper register --relay-only …` | this service is only reachable through the relay |
| Consumer | `pb-mapper connect --relay-only …` | this tunnel always uses the relay |

The relay announces only its UDP port; clients pair it with the relay address
they dialed, so 1:1 NAT and port forwarding need no extra setup. Behind a
setup where the UDP port maps elsewhere, pass
`--direct-advertise PUBLIC_IP:PORT`, and use `--direct-listen` to bind a
different local address.

Worth knowing:

- **Direct sessions belong to the two endpoints.** A relay restart or a revoked
  credential stops new setups but does not cut a running direct session; stopping
  the local tunnel does. Relayed connections close on revocation at once.
- **Not every network can be punched.** Symmetric NATs and blocked UDP use the
  relay. The [design](docs/issue-13-direct-connect-design.zh-CN.md) and the
  [validation record](docs/issue-13-validation.zh-CN.md) describe what has been
  tested.
- **pb-mapper moves bytes.** The service behind a tunnel still owns its own
  authentication and authorization.

## SDKs

### Rust

```toml
[dependencies]
pb-mapper = "0.8"
tokio = { version = "1", features = ["full"] }
```

```rust
use pb_mapper::{Client, ClientConfig, ConnectRequest, RegisterRequest, Transport};

let client = Client::new(ClientConfig {
    server: "relay.example.com:7666".into(),
    credential: std::env::var("MSG_HEADER_KEY")?,
    keep_alive: true,
    namespace: None,
})?;

// Device side: publish a local service.
let registration = client.register(RegisterRequest {
    key: "web".into(),
    local_addr: "127.0.0.1:8080".into(),
    transport: Transport::Tcp,
    codec: false,
    force_namespace: false,
}).await?;
registration.wait_ready().await?;

// Anywhere else: reach it on a local port, direct first.
let connection = client.connect(ConnectRequest {
    key: "web".into(),
    local_addr: "127.0.0.1:9090".into(),
    transport: Transport::Tcp,
}).await?;
connection.wait_ready().await?;

// Listen for changes: Starting → Connected → Retrying → …
let mut status = connection.subscribe();
while status.changed().await.is_ok() {
    println!("{:?}", *status.borrow());
}
```

Every capability has a runnable example in
[`crates/pb-mapper/examples/`](crates/pb-mapper/examples). Start with
`cargo run -p pb-mapper --example quickstart`, which needs no relay of your own:

| Example | Shows |
| --- | --- |
| [`quickstart`](crates/pb-mapper/examples/quickstart.rs) | An embedded relay, a published echo service, a direct round trip, and the path it took |
| [`register_tcp`](crates/pb-mapper/examples/register_tcp.rs) / [`connect_tcp`](crates/pb-mapper/examples/connect_tcp.rs) | The two ends of a TCP tunnel, with the live direct/relay split |
| [`udp`](crates/pb-mapper/examples/udp.rs) | Both ends of a UDP tunnel, such as a game server |
| [`path_options`](crates/pb-mapper/examples/path_options.rs) | `allow_direct: false`, `PathPolicy::RelayOnly`, idle deadlines |
| [`watch_events`](crates/pb-mapper/examples/watch_events.rs) | Status stream, event history, live direct stage, JSON diagnostics |
| [`status`](crates/pb-mapper/examples/status.rs) | Services, control connections and sessions in a namespace |
| [`admin_credentials`](crates/pb-mapper/examples/admin_credentials.rs) | Issue, show, reveal, renew, list, revoke and collect credentials |
| [`admin_inventory`](crates/pb-mapper/examples/admin_inventory.rs) | Every tenant's services and connections, retiring stuck ones, root-key rotation |
| [`remote_control`](crates/pb-mapper/examples/remote_control.rs) | The agent-harness pattern: one credential per session, isolated, revoked at the end |

Build without the `p2p` feature (`default-features = false`) for a relay-only
client without QUIC.

### TypeScript (Node-API)

```bash
npm install pb-mapper
```

```ts
import { Client } from "pb-mapper";

const client = new Client({
  server: "relay.example.com:7666",
  credential: process.env.MSG_HEADER_KEY!,
});

const conn = await client.connect({
  key: "web", localAddr: "127.0.0.1:9090", transport: "tcp",
});
await conn.waitReady();

// Event listener: resolves on each change, ends once the tunnel settles.
for (let s = conn.status(); s !== "stopped" && !s.startsWith("failed");) {
  s = await conn.nextStatus();
  console.log(s, conn.diagnostics().events.at(-1));
}
```

Prebuilt for Linux (x64, arm64, glibc), macOS (x64, arm64) and Windows x64, on
Node.js 18+ and Bun 1.1+. See [`js/README.md`](js/README.md) for the full API.

### Other surfaces

| Surface | What it offers |
| --- | --- |
| CLI `pb-mapper` | `server`, `register`, `connect`, `status`, `admin` in one binary |
| Agent Skill | Installs and verifies a relay, register and connect end to end (below) |
| Flutter UI | Desktop and mobile GUI over a C ABI; uses the relay path for now |
| Relay library | `pb-mapper-server`, for embedding a relay in your own process |

## Credentials and security

| Credential | Holder | Can |
| --- | --- | --- |
| Administrator key (32 bytes) | Relay operator or provisioning automation | Issue, reveal, renew, revoke, rotate; inspect every namespace |
| Temporary `pbmt1_` credential | One tenant, device, harness or session | Register, connect and inspect inside its own namespace |

- Temporary credentials are derived from the administrator key, a persistent
  relay instance ID and a key ID. The relay stores lifecycle metadata, not the
  secrets. Expiry, revocation and root-key rotation take effect on live relay
  connections at once.
- Protocol v2 authenticates the encrypted first request without an extra round
  trip. Direct sessions run over QUIC with mutually pinned certificates bound to
  the session grant. `register --codec` adds AES-256-GCM to relayed data.
- Credentials are pre-shared, not a public-key identity system. Use TLS or
  another application protocol on top when you need certificate identity or
  protection against traffic analysis. See the
  [authentication design](docs/authentication-v2.md) and
  [data encryption compatibility](docs/data-encryption-compatibility.md).

## Deploy

- **Agent Skill**: tell your coding agent
  `Fetch and follow instructions from https://raw.githubusercontent.com/acking-you/pb-mapper/master/skills/pb-mapper-suite/INSTALL.md`
  and it installs the CLI, deploys the relay, sets up register/connect and
  verifies the tunnel.
- **Docker**: `docker compose -f docker/docker-compose.yml up -d` publishes
  `7666/tcp` and `7666/udp`; see [`DOCKER_README.md`](DOCKER_README.md).
- **systemd**: units for the relay, register and connect live in
  [`services/`](services/readme.md).
- **Release binaries** for Linux, macOS and Windows, plus the UI, are on
  [Releases](https://github.com/acking-you/pb-mapper/releases).

## Upgrading from 0.7

Direct with relay fallback is now the default everywhere. Existing commands keep
working: `--enable-direct`, `--allow-direct` and `--path auto` are still
accepted, and clients against an older relay simply use the relay. To keep the
0.7 behavior, add `--relay-only` to `server`, `register` or `connect`. In the
SDKs, set `allowDirect: false` / `path: "relay-only"` in Node, or
`RegisterOptions { allow_direct: false, .. }` / `PathPolicy::RelayOnly` in Rust.
0.7 clients that opted into direct do not know how to pair the announced port
with the relay address, so against a default 0.8 relay they fall back to the
relay; upgrade them, or start the relay with `--direct-advertise PUBLIC_IP:7666`.

## Documentation

- [User guide](docs/user-guide.md): every command and flag
- [Weak-network recovery](docs/weak-network-recovery.md): deadlines, resource bounds, fault tests
- [Authentication and protocol v2](docs/authentication-v2.md)
- [Direct connection design](docs/issue-13-direct-connect-design.zh-CN.md) (Chinese)
- [Changelog](CHANGELOG.md)

Repository layout: `crates/` holds the Rust workspace (core, auth, protocol,
server, client, the `pb-mapper` SDK facade, Node-API binding, CLI and testkit);
`js/` the npm package; `ui/` the Flutter app and its C ABI; `skills/` the agent
workflows; `docker/`, `services/` and `scripts/` packaging and operations.

```bash
make build-pb-mapper
cargo test --workspace --all-features
```

## License

[MIT](LICENSE)
