# pb-mapper User Guide

[English](user-guide.md) | [中文](user-guide.zh-CN.md)

## Overview

pb-mapper exposes local TCP/UDP services through a public relay using a service key. One `pb-mapper` binary provides the `server`, `register`, `connect`, `status`, and `admin` commands, alongside an optional Flutter GUI.

## How it works

```mermaid
flowchart LR
    A["Local TCP or UDP service"] --> B["pb-mapper register"]
    B --> C["V2 control connection pool"]
    C --> D["pb-mapper server"]
    E["pb-mapper connect"] --> F["Local listener"]
    G["Remote user app"] --> F
    F --> E
    E --> D
    D --> C
    C --> B
    B --> A
```

The `server` role is the public rendezvous point. A `register` process registers one service key and keeps a small pool of long-lived control connections to the relay. A `connect` process first checks that the requested service key has a healthy registered control connection, then opens a local listener for downstream users.

When a user connects to the connect-side local listener, that process subscribes to the service key. The relay selects a healthy registered control connection, asks the matching register process to open a data stream, waits for an acknowledgement, and then forwards bytes between the client stream and the local service.

```mermaid
sequenceDiagram
    participant S as pb-mapper register
    participant R as pb-mapper server
    participant C as pb-mapper connect
    participant U as user app

    S->>R: Register V2 with service key
    R-->>S: conn id and generation
    loop Control lease
        S->>R: PingV2
        R-->>S: PongV2
    end
    C->>R: Status Service key
    R-->>C: healthy control connections
    U->>C: connect local listener
    C->>R: Subscribe service key
    R->>S: Stream request
    S->>R: Stream ack
    S->>R: Data stream
    R-->>C: subscribe ready
    C-->>U: forward service bytes
```

Control connections are leased rather than guessed from a single missing heartbeat. If a register process stops receiving control-plane activity for longer than the tolerance window, it opens a separate status probe and verifies that the exact `conn_id` and `generation` are still present on the relay. If that registration is missing, or if the probe keeps failing past the suspect grace window, the process reconnects and registers a fresh control connection. The relay also expires idle V2 control connections, and subscribe requests skip unhealthy or stale registrations.

## Prerequisites

- Optional: Flutter SDK for the GUI (`ui/`)
- Optional: Docker/Compose for container deployment (see `DOCKER_README.md`)

## Install (recommended)

Download prebuilt binaries from GitHub Releases and extract them:

- Releases: https://github.com/acking-you/pb-mapper/releases

Each target has one archive, named `pb-mapper-<target>.tar.gz` on Unix-like systems or `pb-mapper-<target>.zip` on Windows. After extracting, add `pb-mapper` to your PATH or run it from the extracted folder.

## Build from source (optional)

### Rust binaries

Requires the Rust toolchain (see `rust-toolchain.toml` for the pinned version).

Build the Rust CLI:

```bash
cargo build --release
```

Build the CLI with Make:

```bash
make build-pb-mapper
```

Cross-build a musl CLI binary:

```bash
make build-pb-mapper-x86_64_musl
```

The binary is placed at `target/release/pb-mapper`.

### Flutter UI (optional)

```bash
cd ui
flutter run
```

## Run (CLI)

If you added the binaries to your PATH, use them directly. Otherwise, prefix with `./`.

### 1) Start the central server

```bash
pb-mapper server --port 7666
```

Optional flags:

- `--ipv6`: enable IPv6 listening
- `--keep-alive`: enable TCP keep-alive
- `--auth-state-dir`: authentication state directory (default `/var/lib/pb-mapper/auth`)
- `--max-temporary-keys`: fixed temporary-key slot capacity (default `65536`)
- `--max-temporary-key-ttl`: maximum issued TTL (default `30d`)
- `--legacy-protocol allow|deny`: initial legacy-client policy
- `--use-machine-msg-header-key`: explicit legacy compatibility mode

### Administrator and temporary credentials

On first start, the relay creates a random administrator key at
`/var/lib/pb-mapper/auth/admin.key`. There is no built-in default credential.
Keep the administrator key on the relay host and use it to issue a temporary
credential for a workload:

```bash
export MSG_HEADER_KEY="$(sudo cat /var/lib/pb-mapper/auth/admin.key)"
pb-mapper admin --server "your-server:7666" \
  key issue --ttl 24h --label my-service
```

Export the printed `pbmt1_...` credential on both the register and connect
machines. The temporary key can see and use only its own namespace:

```bash
export MSG_HEADER_KEY='pbmt1_...'
pb-mapper register tcp --server "your-server:7666" --key "my-service" --addr "127.0.0.1:8080"
```

Renewing a key preserves the credential text. Revocation or expiry immediately
closes its active control and data connections. See
[`authentication-v2.md`](authentication-v2.md) for the full lifecycle,
namespace model, protocol framing, and migration procedure.

The machine-derived option remains available for an existing deployment, but
it is not recommended for new installations:

```bash
pb-mapper server --port 7666 --use-machine-msg-header-key
```

On upgrade, if no new administrator key or `MSG_HEADER_KEY` is present, the
relay automatically imports `/var/lib/pb-mapper-server/msg_header_key` so
legacy clients keep working.

### 2) Register a local service

Register a TCP service:

```bash
pb-mapper register tcp \
  --server "your-server:7666" \
  --key "my-service" \
  --addr "127.0.0.1:8080"
```

Register a UDP service:

```bash
pb-mapper register udp \
  --server "your-server:7666" \
  --key "my-udp" \
  --addr "127.0.0.1:8211"
```

To enable optional AES-256-GCM message encryption for forwarded traffic, add `--codec` to the register command.

### 3) Connect from a remote client

```bash
pb-mapper connect tcp \
  --server "your-server:7666" \
  --key "my-service" \
  --addr "127.0.0.1:9090"
```

After step 3, the remote machine can access the service at `127.0.0.1:9090`.

### Status commands

```bash
pb-mapper status remote-id --server "your-server:7666"
pb-mapper status keys --server "your-server:7666"
```

An administrator can explicitly inspect or connect to a temporary namespace:

```bash
pb-mapper status keys --server "your-server:7666" --namespace 4294967296
pb-mapper connect tcp --server "your-server:7666" --namespace 4294967296 \
  --key "my-service" --addr "127.0.0.1:9090"
```

Registering as administrator inside a temporary namespace additionally requires
`--force`.

### Administrator commands

```bash
pb-mapper admin --server "your-server:7666" status
pb-mapper admin --server "your-server:7666" key list
pb-mapper admin --server "your-server:7666" key reveal 4294967296
pb-mapper admin --server "your-server:7666" service list --all
pb-mapper admin --server "your-server:7666" connection list --all
```

Use `--output json` for one JSON document or `--output ndjson` for streaming
automation. Page size defaults to 100 and is capped at 1000.

## Run (GUI)

The Flutter UI can start the server, register services, and connect clients through a graphical workflow. Start it from `ui/`:

```bash
cd ui
flutter run
```

## Environment variables

- `PB_MAPPER_SERVER`: default server address for the CLI
- `MSG_HEADER_KEY`: 32-character administrator key or a `pbmt1_` temporary credential
- `PB_MAPPER_AUTH_STATE_DIR`: relay auth-state directory, default `/var/lib/pb-mapper/auth`
- `PB_MAPPER_AUTH_MAX_TEMP_KEYS`: fixed temporary-key capacity, default `65536`
- `PB_MAPPER_AUTH_MAX_TEMP_TTL_SECS`: maximum temporary-key TTL, default 30 days
- `PB_MAPPER_LEGACY_PROTOCOL`: `allow` or `deny`, default `allow`
- `PB_MAPPER_MAX_SERVICES_PER_NAMESPACE`: service names per namespace, default `256`
- `PB_MAPPER_MAX_REGISTER_CONNECTIONS_PER_SERVICE`: control connections per service, default `16`
- `PB_MAPPER_MAX_STREAMS_PER_NAMESPACE`: active streams per namespace, default `1024`
- `PB_MAPPER_NEW_STREAMS_PER_SECOND`: sustained new-stream rate per namespace, default `100`
- `PB_MAPPER_NEW_STREAMS_BURST`: new-stream burst per namespace, default `200`
- `PB_MAPPER_KEEP_ALIVE`: enable TCP keep-alive (set to `ON`)
- `PB_MAPPER_LOG_FORMAT`: tracing output format, one of `pretty` (default), `compact`, or `json`
- `PB_MAPPER_CONTROL_IO_TIMEOUT`: close stalled control-plane handshakes after this duration, default `30s`
- `PB_MAPPER_STREAM_ACK_TIMEOUT`: wait for a registered server control connection to acknowledge a stream request before trying another connection, default `300ms`
- `PB_MAPPER_STREAM_READY_TIMEOUT`: wait after a stream ack for the server-side data stream to arrive before trying another connection, default `1s`
- `PB_MAPPER_STREAM_RECOVERY_TIMEOUT`: keep a client subscribe open while stale control connections are retired and replacement control connections register, default `2s`
- `PB_MAPPER_CONTROL_CONN_POOL_SIZE`: number of parallel server-side control connections per registered service, default `2`, maximum `16`
- `PB_MAPPER_CONTROL_HEARTBEAT_INTERVAL`: interval between register-role control heartbeats, default `2s`
- `PB_MAPPER_CONTROL_HEARTBEAT_TOLERANCE`: how long a registered control connection may go without inbound control activity before it becomes suspect and is probed, default `6s`
- `PB_MAPPER_CONTROL_SUSPECT_GRACE`: additional grace after a failed remote registration probe before reconnecting, default `2s`
- `PB_MAPPER_REGISTRATION_PROBE_TIMEOUT`: timeout for each register-role remote registration status probe, default `1s`
- `PB_MAPPER_SERVER_LEASE_TIMEOUT`: server-side idle lease timeout for V2 registered control connections, default `15s`
- `PB_MAPPER_CLIENT_HEALTH_CHECK_INTERVAL`: how often the client-side local listener rechecks that the remote service key is still registered, default `15s`
- `PB_MAPPER_CLIENT_HEALTH_CHECK_TIMEOUT`: timeout for each client-side remote key health check, default `5s`
- `PB_MAPPER_CLIENT_HEALTH_FAILURE_THRESHOLD`: consecutive failed health checks required before restarting the client-side local listener, default `3`
- `PB_MAPPER_TUNNEL_IDLE_TIMEOUT`: close a fully idle TCP tunnel after this duration, default `1h`
- `PB_MAPPER_HALF_CLOSE_IDLE_TIMEOUT`: close a half-closed TCP tunnel after this idle duration, default `60s`
- `RUST_LOG`: logging level, for example `info` or `debug`

Timeout values accept plain seconds or `ms`/`s`/`m`/`h` suffixes, for example `500ms`, `60s`, `10m`, or `1h`.

## Docker deployment

For containerized deployment of the server, see [`DOCKER_README.md`](../DOCKER_README.md).
