# pb-mapper User Guide

[English](user-guide.md) | [中文](user-guide.zh-CN.md)

## Overview

pb-mapper exposes local TCP/UDP services through a public server using a service key. It includes three CLI binaries and an optional Flutter GUI.

## Prerequisites

- Optional: Flutter SDK for the GUI (`ui/`)
- Optional: Docker/Compose for container deployment (see `DOCKER_README.md`)

## Install (recommended)

Download prebuilt binaries from GitHub Releases and extract them:

- Releases: https://github.com/acking-you/pb-mapper/releases

Each binary is packaged separately:

- `pb-mapper-server-<version>-<target>.tar.gz` / `.zip`
- `pb-mapper-server-cli-<version>-<target>.tar.gz` / `.zip`
- `pb-mapper-client-cli-<version>-<target>.tar.gz` / `.zip`

After extracting, add the binaries to your PATH or run them from the extracted folder.

## Build from source (optional)

### Rust binaries

Requires the Rust toolchain (see `rust-toolchain.toml` for the pinned version).

Build all Rust binaries:

```bash
cargo build --release
```

Build just the server with Make:

```bash
make build-pb-mapper-server
```

Cross-build a musl server binary:

```bash
make build-pb-mapper-server-x86_64_musl
```

Binaries are placed under `target/release/` (for example, `pb-mapper-server`).

### Flutter UI (optional)

```bash
cd ui
flutter run
```

## Run (CLI)

If you added the binaries to your PATH, use them directly. Otherwise, prefix with `./`.

### 1) Start the central server

```bash
pb-mapper-server --pb-mapper-port 7666
```

Optional flags:

- `--use-ipv6`: enable IPv6 listening
- `--keep-alive`: enable TCP keep-alive
- `--use-machine-msg-header-key`: derive `MSG_HEADER_KEY` from current machine hostname + MAC,
  and write it to `/var/lib/pb-mapper-server/msg_header_key`

### Machine-derived `MSG_HEADER_KEY` (optional)

When you want each deployed server to use a host-specific key (instead of the built-in default),
start server with:

```bash
pb-mapper-server --pb-mapper-port 7666 --use-machine-msg-header-key
```

This will:

- derive a stable 32-byte key from hostname + MAC addresses
- set server process `MSG_HEADER_KEY` automatically
- persist the key to `/var/lib/pb-mapper-server/msg_header_key`

Then use the same key for `pb-mapper-server-cli` or `pb-mapper-client-cli`:

```bash
export MSG_HEADER_KEY="$(cat /var/lib/pb-mapper-server/msg_header_key)"
pb-mapper-server-cli --pb-mapper-server "your-server:7666" tcp-server --key "my-service" --addr "127.0.0.1:8080"
```

### 2) Register a local service

Register a TCP service:

```bash
pb-mapper-server-cli --pb-mapper-server "your-server:7666" \
  tcp-server \
  --key "my-service" \
  --addr "127.0.0.1:8080"
```

Register a UDP service:

```bash
pb-mapper-server-cli --pb-mapper-server "your-server:7666" \
  udp-server \
  --key "my-udp" \
  --addr "127.0.0.1:8211"
```

To enable optional AES-256-GCM message encryption for forwarded traffic, add `--codec` before the subcommand when registering the service (for example, `pb-mapper-server-cli --codec tcp-server ...`).

### 3) Connect from a remote client

```bash
pb-mapper-client-cli --pb-mapper-server "your-server:7666" \
  tcp-server \
  --key "my-service" \
  --addr "127.0.0.1:9090"
```

After step 3, the remote machine can access the service at `127.0.0.1:9090`.

### Status commands

```bash
pb-mapper-server-cli --pb-mapper-server "your-server:7666" status remote-id
pb-mapper-server-cli --pb-mapper-server "your-server:7666" status keys
```

## Run (GUI)

The Flutter UI can start the server, register services, and connect clients through a graphical workflow. Start it from `ui/`:

```bash
cd ui
flutter run
```

## Environment variables

- `PB_MAPPER_SERVER`: default server address for the CLI
- `PB_MAPPER_KEEP_ALIVE`: enable TCP keep-alive (set to `ON`)
- `PB_MAPPER_LOG_FORMAT`: tracing output format, one of `pretty` (default), `compact`, or `json`
- `PB_MAPPER_CONTROL_IO_TIMEOUT`: close stalled control-plane handshakes after this duration, default `30s`
- `PB_MAPPER_STREAM_ACK_TIMEOUT`: wait for a registered server control connection to acknowledge a stream request before trying another connection, default `300ms`
- `PB_MAPPER_STREAM_READY_TIMEOUT`: wait after a stream ack for the server-side data stream to arrive before trying another connection, default `1s`
- `PB_MAPPER_STREAM_RECOVERY_TIMEOUT`: keep a client subscribe open while stale control connections are retired and replacement control connections register, default `2s`
- `PB_MAPPER_CONTROL_CONN_POOL_SIZE`: number of parallel server-side control connections per registered service, default `2`, maximum `16`
- `PB_MAPPER_CONTROL_HEARTBEAT_INTERVAL`: interval between server-cli control heartbeats, default `2s`
- `PB_MAPPER_CONTROL_HEARTBEAT_TOLERANCE`: how long a registered control connection may go without inbound control activity before it becomes suspect and is probed, default `6s`
- `PB_MAPPER_CONTROL_SUSPECT_GRACE`: additional grace after a failed remote registration probe before reconnecting, default `2s`
- `PB_MAPPER_REGISTRATION_PROBE_TIMEOUT`: timeout for each server-cli remote registration status probe, default `1s`
- `PB_MAPPER_SERVER_LEASE_TIMEOUT`: server-side idle lease timeout for V2 registered control connections, default `15s`
- `PB_MAPPER_CLIENT_HEALTH_CHECK_INTERVAL`: how often the client-side local listener rechecks that the remote service key is still registered, default `1s`
- `PB_MAPPER_CLIENT_HEALTH_CHECK_TIMEOUT`: timeout for each client-side remote key health check, default `1s`
- `PB_MAPPER_TUNNEL_IDLE_TIMEOUT`: close a fully idle TCP tunnel after this duration, default `1h`
- `PB_MAPPER_HALF_CLOSE_IDLE_TIMEOUT`: close a half-closed TCP tunnel after this idle duration, default `60s`
- `RUST_LOG`: logging level, for example `info` or `debug`

Timeout values accept plain seconds or `ms`/`s`/`m`/`h` suffixes, for example `500ms`, `60s`, `10m`, or `1h`.

## Docker deployment

For containerized deployment of the server, see [`DOCKER_README.md`](../DOCKER_README.md).
