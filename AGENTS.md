# Repository Guidelines

## Architecture Overview
- One `pb-mapper` binary in `src/bin/` with four role commands:
  - `server`: central router (default port 7666)
  - `register`: registers local TCP/UDP services with the router
  - `connect`: connects to a registered service and exposes a local port
  - `status`: queries router IDs and registered keys
- Core crates: `src/pb_server`, `src/local/{server,client}`, `src/common` (protocol, streams, listeners), `src/utils`.

## Project Structure & Modules
- `src/`: Rust backend and CLI
  - `src/bin/pb-mapper.rs`: unified CLI entry point
  - `src/pb_server`, `src/local`, `src/common`, `src/utils`
- `ui/`: Flutter UI; Rust bridge under `ui/native/*`
- `tests/`: integration tests; loads env from `tests/.env`
- `docker/`, `services/`, `scripts/`: container, systemd, build/release

## Build, Test, and Development Commands
- Build (release): `make build-pb-mapper`
- Cross-build (musl): `make build-pb-mapper-x86_64_musl`
- Run server: `cargo run --bin pb-mapper -- server --port 7666`
- Register service: `cargo run --bin pb-mapper -- register tcp --key k --addr 127.0.0.1:8080 --server host:7666`
- Connect client: `cargo run --bin pb-mapper -- connect tcp --key k --addr 127.0.0.1:9090 --server host:7666`
- Tests: `cargo test` (see Testing for env)
- Docker (server): `make release-pb-mapper-docker-image`
- UI (optional): `cd ui && flutter run`
Notes: CI builds release artifacts on tags `vX.Y.Z` (see `.github/workflows/release.yml`).

## Coding Style & Naming Conventions
- Rust 2021; toolchain pinned via `rust-toolchain.toml` (CI uses 1.88.0)
- Format: `cargo fmt --all` (4 spaces; import grouping per `rustfmt.toml`)
- Lint: `cargo clippy --all-targets -- -D warnings`
- Naming: modules/functions `snake_case`, types/traits `PascalCase`, consts `SCREAMING_SNAKE_CASE`

## Testing Guidelines
- Framework: `tokio` async + integration tests under `tests/`
- Env vars (see `tests/.env`): `PB_MAPPER_TEST_SERVER`, `LOCAL_TEST_SERVER`, `ECHO_TEST_SERVER`, `SERVER_TEST_KEY`, `SERVER_TEST_TYPE` (`TCP`/`UDP`)
- Run ignored tests: `cargo test -- --ignored`
- Prefer new integration tests in `tests/` with reproducible env defaults

## Commit & Pull Request Guidelines
- Commits: short, imperative (e.g., "Fix localhost resolution panic", "add network perms", "change to StreamBuilder")
- Before committing code changes, always run:
  - `cargo fmt --all`
  - `cargo clippy --all-targets -- -D warnings`
  - If `deps/uni-stream` was touched, run the same two commands inside that submodule as well.
- PRs include: summary, rationale, test steps/coverage, and doc/config updates when behavior changes
- Link issues; attach screenshots/logs for UI or networking changes

## Security & Configuration Tips
- Never commit secrets; use `.env` and document required variables
- Helpful envs: `RUST_LOG=info`, `PB_MAPPER_SERVER=host:7666`, `PB_MAPPER_KEEP_ALIVE=ON`
- Systemd: install the unified binary at `/usr/local/bin/pb-mapper`; role-specific units live in `services/`
