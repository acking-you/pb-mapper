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
- `RUST_LOG`: logging level, for example `info` or `debug`

## Docker deployment

For containerized deployment of the server, see [`DOCKER_README.md`](../DOCKER_README.md).
