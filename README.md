<div align="center">

<img src="docs/assets/poster.png" alt="pb-mapper" width="800" />

<p>
  <a href="https://www.rust-lang.org/"><img alt="Rust 2021" src="https://img.shields.io/badge/Rust-2021-000000?logo=rust&logoColor=white"></a>
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

**pb-mapper** is a Rust-based service mapping system that exposes multiple local TCP/UDP services through a **single** public server port. Unlike frp-style per-service port mapping, it uses a service-key registry so many local services can share one public entrypoint and be reached by anyone holding the key.

## Highlights

- **Single-port simplicity** — one public port plus a service-key registry; no per-service port planning, and CLI and GUI share the same workflow.
- **Extensible architecture** — clean split between `pb-mapper-server`, `pb-mapper-server-cli`, and `pb-mapper-client-cli`, with shared protocol and helpers in `src/common` and `src/utils` to make new transports and features easy to add.
- **Optional encryption** — AES-256-GCM (via `ring`) on forwarded traffic, opted in with `--codec` when registering a service.
- **Production-grade performance** — in real workloads (e.g., a Palworld UDP server), latency is on par with frp using a directly exposed remote port.

## Quick Start

### Recommended — AI agent + deployment skill

If you use an AI coding agent (Claude Code, Cursor, Kiro), invoke the built-in deployment skill for a fully interactive, one-command deploy. The remote host doesn't need GitHub access — the binary is downloaded locally and uploaded over SCP.

- **Server**: `/pb-mapper-server-deploy` — downloads the binary locally, uploads via SCP, and configures a systemd service on the remote host.
- **Client tunnel**: `/pb-mapper-client-cli-deploy` — same local-download-then-upload flow for `pb-mapper-client-cli`, with systemd service and end-to-end validation.

The skills collect SSH credentials, ports, and encryption keys interactively, and fall back to a proxy if GitHub downloads fail on your local network.

### Alternative — one-liner install script

If the remote host has direct GitHub access, a single command installs `pb-mapper-server` as a systemd service on Linux (x86_64, musl). Defaults: port `7666`, `--use-machine-msg-header-key` enabled, key persisted at `/var/lib/pb-mapper-server/msg_header_key`.

```bash
curl -fsSL https://raw.githubusercontent.com/acking-you/pb-mapper/master/scripts/install-server-github.sh | bash
```

After install, load the same key for `pb-mapper-server-cli` and `pb-mapper-client-cli`:

```bash
export MSG_HEADER_KEY="$(cat /var/lib/pb-mapper-server/msg_header_key)"
```

## Architecture

![pb-mapper architecture](docs/assets/architecture.svg)

Three zones:

- **Local service side** (green) — `pb-mapper-server-cli` (or the Flutter UI) registers a local TCP/UDP service with the public server.
- **Public network** (blue) — `pb-mapper-server` maintains a service registry, manages connections, and forwards data bidirectionally.
- **Remote client side** (orange) — `pb-mapper-client-cli` (or the Flutter UI) subscribes to a service key and exposes it as a local port.

### Concrete example: access a home web server remotely

You run a web server on `localhost:8080` at home and want to reach it from a coffee shop.

```
                  Home LAN                    Public Server                Coffee Shop
          ┌─────────────────────┐       ┌──────────────────┐       ┌──────────────────┐
          │  Web Server :8080   │       │  pb-mapper-server│       │  Browser :3000   │
          │        ↑            │       │     :7666        │       │       ↑          │
          │  server-cli ────────┼──────►│  key='web' ──────┼◄──────┼── client-cli     │
          └─────────────────────┘       └──────────────────┘       └──────────────────┘
```

**1.** On the public server, start the central router:

```bash
pb-mapper-server --port 7666
```

**2.** At home, register your web server:

```bash
pb-mapper-server-cli --server <public-ip>:7666 --key web --local 127.0.0.1:8080
```

**3.** At the coffee shop, subscribe and expose locally:

```bash
pb-mapper-client-cli --server <public-ip>:7666 --key web --local 127.0.0.1:3000
```

Open `http://localhost:3000` in the coffee-shop browser — traffic flows through the public server back to the home web server.

## Components

| Component | Role |
| --- | --- |
| `pb-mapper-server` | Central router (default port `7666`) |
| `pb-mapper-server-cli` | Registers a local TCP/UDP service with the server |
| `pb-mapper-client-cli` | Subscribes to a registered service and exposes a local port |
| **Flutter UI** (`ui/`) | GUI replacement for both CLIs |

## Developer view

### Rust core

- Binaries live in `src/bin/`; protocol and networking helpers are shared via `src/common` and `src/utils`.
- Server / client internals are split across `src/pb_server`, `src/local/server`, and `src/local/client`.

### Flutter UI

- **Layering**
  - UI screens / widgets: `ui/lib/src/views`, `ui/lib/src/widgets`
  - Typed UI API: `ui/lib/src/ffi/pb_mapper_api.dart`
  - FFI transport + isolate dispatch: `ui/lib/src/ffi/pb_mapper_service.dart`
  - Raw FFI bindings: `ui/lib/src/ffi/pb_mapper_ffi.dart`
  - Rust FFI crate: `ui/native/pb_mapper_ffi` (C ABI + JSON response envelope)
- **Threading** — all FFI calls run on a background isolate so Flutter's UI thread is never blocked.
- **Native responses** — Rust returns JSON strings (`{success, message, data}`) to skip generated bindings and keep the ABI stable during iteration.

## Documentation

- User guide (build / run / use): [`docs/user-guide.md`](docs/user-guide.md)
- Docker server guide: [`DOCKER_README.md`](DOCKER_README.md)
- 中文文档: [`README.zh-CN.md`](README.zh-CN.md), [`docs/user-guide.zh-CN.md`](docs/user-guide.zh-CN.md)

## Repository layout

- `src/` — Rust backend
- `ui/` — Flutter UI + native bridge
- `docs/` — documentation and assets
- `docker/`, `services/`, `scripts/`, `tests/` — deployment and tooling
- `skills/` — AI coding agent deployment skills (server, client-cli)

## License

Released under the [MIT License](LICENSE).
