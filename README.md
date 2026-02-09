# pb-mapper

[English](README.md) | [中文](README.zh-CN.md)

pb-mapper is a Rust-based service mapping system that exposes multiple local TCP/UDP services through a single public server port. Unlike frp-style per-service port mapping, pb-mapper uses a service key registry so many local services can be published behind one public port and accessed by others through the same server.

## Highlights

- **Easy to use by design**: a single public server port plus a service-key registry removes per-service port planning; the same flow is shared by CLI and GUI.
- **Extensible architecture**: clear server/client split (`pb-mapper-server`, `pb-mapper-server-cli`, `pb-mapper-client-cli`) with shared protocol and utilities in `src/common` and `src/utils`, making new features and transports easier to add.
- **Encryption support**: optional message encryption for forwarded traffic using AES-256-GCM (via `ring`), enabled with the `--codec` flag when registering services.
- **Performance**: in real use (e.g., running a Palworld UDP server), latency is comparable to using frp with a directly exposed remote port.

## Quick Start (server install)

One command installs `pb-mapper-server` as a systemd service on Linux (x86_64, musl build). Defaults: port `7666`, `MSG_HEADER_KEY=abcdefghijklmnopqlsn123456789j01`.

```bash
curl -fsSL https://raw.githubusercontent.com/acking-you/pb-mapper/master/scripts/install-server-github.sh | bash
```

## Docs

- User Guide (build/run/use): [`docs/user-guide.md`](docs/user-guide.md)
- Docker Server Guide: [`DOCKER_README.md`](DOCKER_README.md)
- Chinese docs: [`README.zh-CN.md`](README.zh-CN.md), [`docs/user-guide.zh-CN.md`](docs/user-guide.zh-CN.md)

## Components

- **pb-mapper-server**: central router (default port `7666`)
- **pb-mapper-server-cli**: register local TCP/UDP services to the server
- **pb-mapper-client-cli**: connect to a registered service and expose a local port
- **Flutter UI** (`ui/`): GUI for server and client management

## Architecture

![pb-mapper architecture](docs/assets/architecture.svg)

The diagram above shows the three-zone architecture:

- **Local Service Side** (green): `pb-mapper-server-cli` (or Flutter UI) registers a local TCP/UDP service with the public server.
- **Public Network** (blue): `pb-mapper-server` maintains a service registry, manages connections, and forwards data bidirectionally.
- **Remote Client Side** (orange): `pb-mapper-client-cli` (or Flutter UI) subscribes to a service key and exposes it as a local port.

### Concrete example: access a home web server remotely

Suppose you run a web server on `localhost:8080` at home, and want to access it from a coffee shop.

```
                  Home LAN                    Public Server                Coffee Shop
          ┌─────────────────────┐       ┌──────────────────┐       ┌──────────────────┐
          │  Web Server :8080   │       │  pb-mapper-server│       │  Browser :3000   │
          │        ↑            │       │     :7666        │       │       ↑          │
          │  server-cli ────────┼──────►│  key='web' ──────┼◄──────┼── client-cli     │
          └─────────────────────┘       └──────────────────┘       └──────────────────┘
```

**Step 1** – On the public server, start the central router:
```bash
pb-mapper-server --port 7666
```

**Step 2** – At home, register your web server:
```bash
pb-mapper-server-cli --server <public-ip>:7666 --key web --local 127.0.0.1:8080
```

**Step 3** – At the coffee shop, subscribe and expose locally:
```bash
pb-mapper-client-cli --server <public-ip>:7666 --key web --local 127.0.0.1:3000
```

Now open `http://localhost:3000` in the coffee-shop browser — traffic flows through the public server back to your home web server.

### Developer view

#### Rust core
- **Binaries** live in `src/bin/` and share protocol + networking helpers in
  `src/common` and `src/utils`.
- **Server/Client internals** are split across `src/pb_server`,
  `src/local/server`, and `src/local/client`.

#### Flutter UI
- **Layering**:
  - UI screens/widgets: `ui/lib/src/views`, `ui/lib/src/widgets`.
  - Typed UI API: `ui/lib/src/ffi/pb_mapper_api.dart`.
  - FFI transport + isolate dispatch: `ui/lib/src/ffi/pb_mapper_service.dart`.
  - Raw FFI bindings: `ui/lib/src/ffi/pb_mapper_ffi.dart`.
  - Rust FFI crate: `ui/native/pb_mapper_ffi` (C ABI + JSON response envelope).
- **Threading**: All FFI calls are executed on a background isolate to avoid
  blocking Flutter's UI thread.
- **Native responses**: Rust returns JSON strings (`{success, message, data}`)
  to avoid generated bindings and keep the ABI stable during iteration.

## Repository layout

- `src/`: Rust backend
- `ui/`: Flutter UI + native bridge
- `docs/`: documentation
- `docker/`, `services/`, `scripts/`, `tests/`: deployment and tooling

## Development

For build/run/usage instructions, see the User Guide.
