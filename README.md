<div align="center">

<img src="docs/assets/poster.png" alt="pb-mapper — remote control infrastructure for the agent harness era" width="800" />

<p>
  <a href="https://www.rust-lang.org/"><img alt="Rust 2024" src="https://img.shields.io/badge/Rust-2024-000000?logo=rust&logoColor=white"></a>
  <a href="https://tokio.rs/"><img alt="Tokio" src="https://img.shields.io/badge/Async-Tokio-3873AD"></a>
  <a href="https://flutter.dev/"><img alt="Flutter" src="https://img.shields.io/badge/UI-Flutter-02569B?logo=flutter&logoColor=white"></a>
  <a href="LICENSE"><img alt="License: MIT" src="https://img.shields.io/badge/License-MIT-blue.svg"></a>
  <a href="https://github.com/acking-you/pb-mapper/releases"><img alt="Release" src="https://img.shields.io/github/v/release/acking-you/pb-mapper?logo=github&color=success"></a>
  <a href="https://github.com/acking-you/pb-mapper/actions/workflows/release.yml"><img alt="Build" src="https://github.com/acking-you/pb-mapper/actions/workflows/release.yml/badge.svg"></a>
  <a href="https://github.com/acking-you/pb-mapper/actions/workflows/docker-publish.yml"><img alt="Docker Image" src="https://github.com/acking-you/pb-mapper/actions/workflows/docker-publish.yml/badge.svg"></a>
  <a href="https://github.com/acking-you/pb-mapper/stargazers"><img alt="Stars" src="https://img.shields.io/github/stars/acking-you/pb-mapper?style=social"></a>
</p>

<p>
  <a href="README.md"><b>English</b></a> ·
  <a href="README.zh-CN.md">中文</a>
</p>

</div>

---

> **Remote control infrastructure for the agent harness era.**

Agent harnesses can write code, run tools, and coordinate long workflows. They
still need a narrow, durable path into private runtimes. **pb-mapper** provides
that network primitive: expose one relay port, register any number of keyed
TCP/UDP services behind it, and delegate access without publishing every
service or sharing the relay's root credential.

pb-mapper transports bytes. The service behind a tunnel still owns its
application-level authentication and authorization.

## Why it fits an agent harness

- **Compact and easy to embed** — one Rust binary contains the relay, register,
  connect, status, and administrator roles. Linux releases are distributed as a
  small, self-contained archive with no language runtime to install.
- **One public port, many control paths** — registration, subscription, status,
  and administration all enter through the same relay port. Adding another
  remote-control endpoint does not require another public listener.
- **Delegate access instead of the root key** — keep one 32-byte administrator
  key on the relay and issue renewable, expiring `pbmt1_` credentials to
  harnesses or workloads. Each credential receives an isolated namespace.
- **Revoke live access** — expiry, revocation, auth-state reset, and root-key
  rotation close affected live control and data connections.
- **Native performance, production experience** — the data path is implemented
  in Rust on Tokio, supports TCP and UDP, and has been exercised by long-running
  self-hosted products and real game-server traffic. The end-to-end suite covers
  the transport/encryption matrix and credential lifecycle.

## One relay, many private services

![pb-mapper architecture](docs/assets/architecture-flow.svg)

```text
 private runtime A ── register "app" ──┐
 private runtime B ── register "shell" ├──► pb-mapper relay :7666 ◄── agent harnesses
 private runtime C ── register "tools" ┘          one public port
```

- `pb-mapper register` runs beside a private TCP/UDP service and publishes a
  service name to the relay.
- `pb-mapper connect` runs beside an agent or operator and exposes that service
  on a local address.
- The relay matches both sides by namespace and service name, then forwards data
  bidirectionally.
- Different temporary credentials can reuse the same service names without
  seeing or colliding with each other.

This makes a relay a useful rendezvous layer for remote agent runtimes, coding
harnesses, private APIs, model gateways, browser-control endpoints, development
machines, and operational tools.

## Harness-oriented quick start

If your coding agent can load repository Skills, start with
[`pb-mapper-server-deploy`](skills/pb-mapper-server-deploy/SKILL.md) for the
relay and [`pb-mapper-connect-deploy`](skills/pb-mapper-connect-deploy/SKILL.md)
for a managed local endpoint. They build or download the artifact, upload it,
install systemd units, and verify the resulting path. The manual flow below
makes the same trust boundary explicit.

### 1. Deploy one public relay

On an x86_64 Linux host that can reach GitHub:

```bash
curl -fsSL https://raw.githubusercontent.com/acking-you/pb-mapper/master/scripts/install-server-github.sh | bash
```

The installer starts `pb-mapper server` on port `7666` and creates a random
administrator key at `/var/lib/pb-mapper/auth/admin.key`.

### 2. Issue a scoped credential

Keep the administrator key on the relay. Use it to mint a credential for one
harness or workload:

```bash
export MSG_HEADER_KEY="$(sudo cat /var/lib/pb-mapper/auth/admin.key)"
pb-mapper admin --server <relay>:7666 key issue --ttl 24h --label coding-harness
```

Distribute the printed `pbmt1_...` credential to only the corresponding target
and harness. It can register, connect, and inspect services in its own namespace;
it cannot perform administrator operations.

### 3. Register a private control endpoint

On the target machine:

```bash
export MSG_HEADER_KEY='<pbmt1_credential>'
pb-mapper register tcp \
  --server <relay>:7666 \
  --key agent-control \
  --addr 127.0.0.1:10999
```

### 4. Attach the harness

On the machine running the harness:

```bash
export MSG_HEADER_KEY='<pbmt1_credential>'
pb-mapper connect tcp \
  --server <relay>:7666 \
  --key agent-control \
  --addr 127.0.0.1:11999
```

The harness can now reach the private endpoint at `127.0.0.1:11999`. Only the
relay's `7666` port is public.

## Credential model for automation

| Credential | Intended holder | Authority |
| --- | --- | --- |
| Administrator key | Relay operator or trusted provisioning automation | Issue, reveal, renew, revoke, rotate, inspect all namespaces |
| Temporary `pbmt1_` credential | One harness, tenant, device, or workload | Register, connect, and inspect only its own namespace |

Protocol v2 authenticates the encrypted first request without adding a separate
handshake round trip. Temporary credentials are derived from the administrator
key, persistent server instance ID, and key ID; the relay stores lifecycle
metadata rather than a copy of each temporary secret. Optional AES-256-GCM data
encryption is enabled with `--codec` when registering a service.

pb-mapper uses pre-shared credentials, not public-key identity. Use TLS or
another application protocol when you also need certificate-based endpoint
identity or protection against traffic analysis. See the
[authentication design](docs/authentication-v2.md) for the exact boundary.

## Current integration surface

| Surface | What is available today |
| --- | --- |
| Unified CLI | `server`, `register`, `connect`, `status`, and `admin` roles |
| Deployment Skills | Agent-readable server deployment, connect deployment, and release workflows under `skills/` |
| Operations | Linux systemd units, install scripts, Docker image, status and administrator inventory |
| Native embedding | Rust crates plus a C ABI used by the Flutter desktop/mobile UI |
| Networking | TCP and UDP, per-tunnel keep-alive, optional forwarded-data encryption |

## Roadmap: harness-native remote control

The current release provides the secure network and credential foundation. The
next layer will make that foundation directly consumable by agent harnesses:

- one-click Skills for relay deployment, target registration, harness
  attachment, credential issuance, distribution, renewal, and revocation;
- stable Rust and language-level client SDKs for embedding tunnels without
  shelling out to the CLI;
- a TypeScript package backed by Node-API (N-API);
- a separate client-only build, targeting a packaged size below **5 MB** on
  supported platforms;
- harness adapters and examples for remote model runtimes, tool servers,
  private APIs, development machines, and browser-control endpoints.

These are roadmap items, not yet part of the published compatibility contract.

## Commands

| Command | Role |
| --- | --- |
| `pb-mapper server` | Run the central relay (default port `7666`) |
| `pb-mapper register tcp\|udp` | Register a private TCP/UDP service |
| `pb-mapper connect tcp\|udp` | Expose a registered service on a local address |
| `pb-mapper status keys\|remote-id` | Inspect the caller's namespace |
| `pb-mapper admin ...` | Manage credentials, services, connections, auth state, and legacy migration |

The same server, register, connect, and status workflows are available in the
Flutter UI.

## Build and documentation

```bash
make build-pb-mapper
cargo test
```

- User guide: [`docs/user-guide.md`](docs/user-guide.md)
- Authentication and protocol v2: [`docs/authentication-v2.md`](docs/authentication-v2.md)
- Docker server guide: [`DOCKER_README.md`](DOCKER_README.md)
- Chinese documentation: [`README.zh-CN.md`](README.zh-CN.md), [`docs/user-guide.zh-CN.md`](docs/user-guide.zh-CN.md)

Repository layout:

- `crates/` — Rust workspace: core, auth, protocol, server, client, CLI, and testkit
- `ui/` — Flutter UI and native C ABI bridge
- `skills/` — agent-readable deployment and release workflows
- `docs/` — architecture, authentication, user guides, and project assets
- `docker/`, `services/`, `scripts/` — packaging and operations

## License

Released under the [MIT License](LICENSE).
