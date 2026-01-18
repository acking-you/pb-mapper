# pb-mapper

[English](README.md) | [中文](README.zh-CN.md)

pb-mapper is a Rust-based service mapping system that exposes multiple local TCP/UDP services through a single public server port. Unlike frp-style per-service port mapping, pb-mapper uses a service key registry so many local services can be published behind one public port and accessed by others through the same server.

## Highlights

- **Easy to use by design**: a single public server port plus a service-key registry removes per-service port planning; the same flow is shared by CLI and GUI.
- **Extensible architecture**: clear server/client split (`pb-mapper-server`, `pb-mapper-server-cli`, `pb-mapper-client-cli`) with shared protocol and utilities in `src/common` and `src/utils`, making new features and transports easier to add.
- **Encryption support**: optional message encryption for forwarded traffic using AES-256-GCM (via `ring`), enabled with the `--codec` flag when registering services.
- **Performance**: in real use (e.g., running a Palworld UDP server), latency is comparable to using frp with a directly exposed remote port.

## Docs

- User Guide (build/run/use): [`docs/user-guide.md`](docs/user-guide.md)
- Docker Server Guide: [`DOCKER_README.md`](DOCKER_README.md)
- Chinese docs: [`README.zh-CN.md`](README.zh-CN.md), [`docs/user-guide.zh-CN.md`](docs/user-guide.zh-CN.md)

## Components

- **pb-mapper-server**: central router (default port `7666`)
- **pb-mapper-server-cli**: register local TCP/UDP services to the server
- **pb-mapper-client-cli**: connect to a registered service and expose a local port
- **Flutter UI** (`ui/`): GUI for server and client management

## Architecture (developer view)

### Rust core
- **Binaries** live in `src/bin/` and share protocol + networking helpers in
  `src/common` and `src/utils`.
- **Server/Client internals** are split across `src/pb_server`,
  `src/local/server`, and `src/local/client`.

### Flutter UI
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

## How it works (high level)

1. Run `pb-mapper-server` on a public host (single public port).
2. Run `pb-mapper-server-cli` next to each local service to register it with a unique service key.
3. Run `pb-mapper-client-cli` on a remote machine to connect using a service key and expose a local port.

## Repository layout

- `src/`: Rust backend
- `ui/`: Flutter UI + native bridge
- `docs/`: documentation
- `docker/`, `services/`, `scripts/`, `tests/`: deployment and tooling

## Development

For build/run/usage instructions, see the User Guide.
