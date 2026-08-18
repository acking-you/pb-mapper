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

**pb-mapper** exposes any number of local TCP/UDP services through a **single** public port. Instead of frp-style one-port-per-service mapping, services register under a key and anyone holding that key can reach them.

## Highlights

- **One binary, one public port** — `pb-mapper` provides every runtime role, while a service-key registry replaces per-service port planning.
- **Optional encryption** — AES-256-GCM (via `ring`) on forwarded traffic, enabled with `--codec` at registration.
- **Proven in production** — on real workloads (e.g. a Palworld UDP server), latency matches frp with a directly exposed port.

## Quick Start

### Recommended — AI agent deployment skill

With an AI coding agent (Claude Code, Cursor, Kiro), the built-in skills handle deployment interactively. The binary is downloaded locally and uploaded over SCP, so the remote host needs no GitHub access.

- `/pb-mapper-server-deploy` — deploys `pb-mapper server` as a systemd service.
- `/pb-mapper-connect-deploy` — deploys `pb-mapper connect` as a managed tunnel and validates it end to end.

### Alternative — one-liner install script

If the remote host can reach GitHub directly, this installs the unified `pb-mapper` binary and runs its `server` command as a systemd service on Linux (x86_64, musl) — port `7666`, `--use-machine-msg-header-key` on, key stored at `/var/lib/pb-mapper-server/msg_header_key`.

```bash
curl -fsSL https://raw.githubusercontent.com/acking-you/pb-mapper/master/scripts/install-server-github.sh | bash
```

After install, load the same key before running `pb-mapper register` or `pb-mapper connect`:

```bash
export MSG_HEADER_KEY="$(cat /var/lib/pb-mapper-server/msg_header_key)"
```

## Architecture

![pb-mapper architecture](docs/assets/architecture-flow.svg)

- **Local service side** (green) — `pb-mapper register` registers a local TCP/UDP service.
- **Public network** (blue) — `pb-mapper server` keeps the registry and forwards data bidirectionally.
- **Remote client side** (orange) — `pb-mapper connect` subscribes to a key and exposes it as a local port.

The register and connect workflows are also available in the Flutter UI.

### Example: reach a home web server from a coffee shop

Your web server runs on `localhost:8080` at home.

```
                  Home LAN                    Public Server                Coffee Shop
          ┌─────────────────────┐       ┌──────────────────┐       ┌──────────────────┐
          │  Web Server :8080   │       │ pb-mapper server │       │  Browser :3000   │
          │        ↑            │       │     :7666        │       │       ↑          │
          │ register ───────────┼──────►│  key='web' ──────┼◄──────┼── connect        │
          └─────────────────────┘       └──────────────────┘       └──────────────────┘
```

```bash
# 1. on the public server — start the central router
pb-mapper server --port 7666

# 2. at home — register the web server under key 'web'
pb-mapper register tcp --server <public-ip>:7666 --key web --addr 127.0.0.1:8080

# 3. at the coffee shop — subscribe and expose it locally
pb-mapper connect tcp --server <public-ip>:7666 --key web --addr 127.0.0.1:3000
```

Open `http://localhost:3000` in the coffee-shop browser — traffic flows through the public server back home.

## Components

| Command | Role |
| --- | --- |
| `pb-mapper server` | Central router (default port `7666`) |
| `pb-mapper register tcp\|udp` | Registers a local TCP/UDP service with the server |
| `pb-mapper connect tcp\|udp` | Subscribes to a registered service and exposes a local port |
| `pb-mapper status keys\|remote-id` | Queries the central router |
| **Flutter UI** (`ui/`) | GUI for server, register, connect, and status workflows |

## Developer view

- **Rust core** — the unified entry point is `src/bin/pb-mapper.rs`; shared protocol and networking live in `src/common` and `src/utils`; server / register / connect internals live in `src/pb_server`, `src/local/server`, and `src/local/client`.
- **Flutter UI** — views in `ui/lib/src/views`, FFI layers in `ui/lib/src/ffi`, Rust bridge in `ui/native/pb_mapper_ffi`. FFI calls run on a background isolate, and Rust returns JSON (`{success, message, data}`) to keep the C ABI stable.

## Documentation

- User guide (build / run / use): [`docs/user-guide.md`](docs/user-guide.md)
- Docker server guide: [`DOCKER_README.md`](DOCKER_README.md)
- 中文文档: [`README.zh-CN.md`](README.zh-CN.md), [`docs/user-guide.zh-CN.md`](docs/user-guide.zh-CN.md)

## Repository layout

- `src/` — Rust backend
- `ui/` — Flutter UI + native bridge
- `docs/` — documentation and assets
- `docker/`, `services/`, `scripts/`, `tests/` — deployment and tooling
- `skills/` — AI coding agent deployment skills (server and connect tunnel)

## License

Released under the [MIT License](LICENSE).
