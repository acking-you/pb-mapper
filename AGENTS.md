# Repository Guidelines

Rules for changing this repository. [CLAUDE.md](CLAUDE.md) maps the code;
[docs/README.md](docs/README.md) indexes the design docs.

## Subagents
- Run subagents, workflow agents and delegated sessions on a cheap, practical
  model (Claude Opus 5.5 rather than Claude Fable 5.2, for example) unless the
  user asks for another.
- Name the model when spawning; left unset, an agent inherits the session's.

## Architecture
- One `pb-mapper` binary (`crates/pb-mapper-cli/src/bin/pb-mapper.rs`) with the
  roles `server`, `register`, `connect`, `status` and `admin`.
- Dependencies point one way: `pb-mapper-cli`, the `pb-mapper` SDK facade and
  `pb-mapper-node` → `pb-mapper-server` / `pb-mapper-client` →
  `pb-mapper-protocol` → `pb-mapper-auth` → `pb-mapper-core`. Server and
  client never depend on each other outside tests. `pb-mapper-testkit` is test
  support only.
- `p2p` is a feature that `pb-mapper-server` and `pb-mapper-client` leave off,
  so alone they are relay-only; the CLI, the facade, the Node addon, the FFI
  and `pb-mapper-desktop` turn it on. Never make it a default of those two
  crates.
- `pb-mapper-desktop` (the remote desktop) sits above `pb-mapper-client`. The
  facade's `desktop` feature re-exports it, and of what ships only the FFI
  enables it.
- The desktop names an operating system's API only under `src/platform/<os>/`,
  chosen at compile time (Windows today): capture, encoding, input, cursor and
  display modes all go there. The host's desktop thread (thread C) calls its
  `Backend` statically (`Engine<B: Backend>`); add no dynamic dispatch per
  frame or per input.
- The app calls Rust through `ui/native/pb_mapper_ffi`, the Flutter C ABI. A
  call changes on every side at once: the Rust export, the Dart lookup, the
  client interface, its implementation and the test fake. A new relay or
  desktop operation is an `op` of an existing `*_call_json` export, not a new
  export.
- Binary, package and library file names are hardcoded by release workflows,
  Dockerfiles, install scripts and the Dart loader; do not rename them.

## Compatibility invariants
- Every entry point defaults to LAN first, then direct, then the relay. That
  default is written separately, and changes together, in the SDK option
  structs (pinned by `crates/pb-mapper/tests/default_path.rs`), the CLI flags
  (pinned by the tests in `pb-mapper.rs`), the Node addon, and the app: the
  FFI's `AppConfig` for LAN first, the Dart tunnel and desktop forms for
  direct.
- The relay serves direct coordination on its TCP port number over UDP and
  announces only that port (clients pair it with the relay address they
  dialed); given no direct flag it still starts, relay-only, when that UDP port
  is taken. `--relay-only` on any side restores the pre-direct protocol for it,
  and the 0.7 opt-in flags still parse.
- Relay wire: new fields are optional and omitted when unset; old SDK request
  literals must keep compiling.
- Direct (`crates/*/src/direct`, `pb-mapper-protocol/src/direct.rs`) is tried
  only when the relay, the publisher and that connect all allow it. After a
  pre-commit fallback the consumer pauses direct (10 s doubling to 5 min,
  cleared by a direct success or a network change), so an unpunchable path
  costs one attempt, not one per connection. pb-mapper owns coordination;
  reuse mature crates, not a whole foreign runtime.
- The commit point is StartDirect: the publisher dials its backend only after
  it, and the consumer marks the attempt committed before sending it. Fallback
  to relay happens only before commit, pins the selected publisher and
  generation, and consumes its grant once. After commit nothing is replayed.
- Established direct sessions belong to the endpoints. Coordinator loss,
  credential revocation and a relay rejection must not close them or stop the
  worker that holds them; local stop cancels their tasks and closes the
  endpoint. Session reports are observations, never authorization.
- A path across the internet, punched or not, is QUIC only. Within a LAN the
  dialing end may ask for TCP (`lan_transport`, QUIC by default); the
  publisher serves it on demand (`lan_tcp`, on by default) on LAN addresses
  only, and an older peer or relay, or `lan_tcp = false`, leaves the session
  on QUIC, byte-identical. `DIRECT_VERSION` stays 1.
- Remote desktop: its messages reject unknown fields, so nothing new (a field,
  message or value) reaches a peer that did not list the matching capability,
  and ALPN stays `pb-mapper/desktop/2`. `HELPER_PROTOCOL` covers every request
  and response on both helper pipes (`oop/proto.rs`, and the background host's
  commands with their options and settings) and changes together with both
  `.v<n>` pipe names. Both ends of the helper pipe validate input.
- Display modes change only dynamically, never with `CDS_UPDATEREGISTRY` or
  `CDS_GLOBAL`, journalled before the change. The host restores at session
  end, on revoke, on the owner's Restore and, after a crash, at its next
  start; never over a mode the owner set meanwhile.
- Keep TLS exporter binding, exact certificate pinning, namespace isolation,
  bounded control messages and bounded observation tables intact.
- Never write the process environment after startup; the credential lives in
  process state (`set_process_msg_header_key`).

## Rust
- Read [docs/rust-engineering-guide.md](docs/rust-engineering-guide.md) before
  writing or changing Rust; where it differs from this file or the
  repository's lints, they win.
- Lints: `unwrap_used` and `expect_used` are denied workspace-wide, and a new
  crate takes `[lints] workspace = true`. `#[cfg(test)]` code is exempt through
  `clippy.toml` and needs no attribute; `pb-mapper-testkit` and each `tests/`
  or `examples/` target that unwraps carry a file-level
  `#![allow(clippy::unwrap_used, clippy::expect_used)]`. A production `unwrap`
  or `expect` records its reason at the site.
- Errors: snafu in core, protocol, server and client, wrapping the layer below
  as `source`; auth, the desktop and the FFI keep hand-written types. No
  `thiserror` or `anyhow`. Callers branch on kinds and codes (`AuthFailure`'s
  `code` and `retryable`, the desktop's `FailureKind` read with
  `failure_kind`, the FFI's `ErrorCode`), never on message text.
- Use what exists: `pb-mapper-testkit` and `pb_mapper_core::test_support` for
  shared test support (not a `test-support` feature),
  `pb_mapper_core::durable_file` for atomic state files (not `tempfile`),
  `pb_mapper_core::timeout::RetryBackoff` for retries.
- Edition 2024 on Rust 1.99.1: `rust-toolchain.toml`, `rust-version` and the
  workflows change together.

## Performance
- Zero cost by default: an unused feature must not add work, allocation or size
  to the relay path. Box rare, large enum variants carried by hot channels.
- Do not copy, queue or spawn per packet on a data path; filter in place.
- Measure before and after a data-path change with the release-mode `*_bench`
  examples, and report the environment with any number; recorded runs go under
  `docs/benchmarks/`. Never put an unmeasured figure in docs or images.

## Dependencies
- A direct dependency that needs a patch is a fork under `acking-you`, a
  submodule in `deps/` on the maintenance branch `deps/README.md` documents.
  Cargo takes it only as `path` + `version` in `[workspace.dependencies]`,
  with the submodule in `[workspace].exclude`; never `[patch.crates-io]` or a
  git dependency. Push the fork's commit before updating the gitlink, and bump
  a published fork's version, there and here, when its source changes.
  Transitive dependencies stay with Cargo.
- A fork the published SDK uses joins `publish-forks` in `release-sdk.yml`
  under its own package name before it is patched: `deps/stun-proto` still
  matches upstream 2.0.4 and is not published.
- `deps/kanal` and `deps/uni-stream` are kept for maintenance only; builds use
  the registry's `kanal-plus` and `uni-stream`.
- TLS uses ring only, through `quinn::rustls`; never pull in `aws-lc-rs`.

## Verification
CI's gates are `syntax-check.yml` and `nat-traversal.yml` under
`.github/workflows/`. Run what your change touches and say what you could not
run.
- Rust: `cargo fmt --all`,
  `cargo clippy --workspace --all-targets --all-features -- -D warnings`, and
  `cargo test --workspace --all-features` or, for a focused change, the
  narrowest relevant crates and `crates/pb-mapper-cli/tests/*` suites. CI also
  runs `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps` and
  `cargo run -p pb-mapper --features desktop --example desktop_loopback`.
- NAT traversal, for changes under `crates/`, `deps/` or `Cargo.*`, on Linux:
  `cargo build -p pb-mapper-cli --features port-mapping`, then
  `sudo python3 scripts/test_nat_traversal.py --binary target/debug/pb-mapper`.
- Platform code: the Windows host and helper compile, lint and test only on
  Windows (`-p pb-mapper-desktop -p pb-mapper-ffi --all-features`). The
  Android JNI is linted only by
  `cargo ndk -t arm64-v8a clippy -p pb-mapper-ffi --locked -- -D warnings`;
  its Kotlin side is tested by `./gradlew :app:testDebugUnitTest` in
  `ui/android`, once `flutter build apk --config-only` in `ui/` has written
  the git-ignored `gradlew`.
- Node: `cd js && bun install --frozen-lockfile && bun run build:release &&
  bun test`. The end-to-end cases need `cargo build --bin pb-mapper` and skip
  silently without it; on Windows point `PB_MAPPER_BIN` at the `.exe`. The
  addon stays under 7 MiB.
- App, from `ui/`: `flutter gen-l10n` leaving `l10n_untranslated.json` as
  `{}`, `dart format --output=none --set-exit-if-changed lib test tool`,
  `flutter analyze --no-fatal-infos` and `flutter test`. Icons come from
  `AppIcons`, never Material `Icons.*`.
- If a submodule under `deps/` changed, run fmt and clippy inside it too.
- `scripts/release`:
  `python -m unittest discover -s scripts/release -p 'test_*.py'`.

## Testing
- Cover core capabilities and real failure, state and security boundaries;
  avoid wording, glue and repetitive input matrices.
- Integration tests build their own relay and tunnels with `pb-mapper-testkit`
  and need no environment setup. Prefer keeping a bound socket over
  `reserve_addr`. Wait on readiness, not sleeps.
- Tests share a process with parallel threads, so unit tests never write the
  environment. An integration test that needs a process-wide variable sets it
  once, before anything starts, in its own `tests/*.rs` binary; code that
  reads the environment is tested in a single-threaded `harness = false`
  binary such as `crates/pb-mapper-core/tests/process_environment.rs`.
  `regression.rs`'s per-test `EnvOverrides` predates this; do not extend it.
- The testkit's `Relay` sets the process credential to `TEST_ADMIN_KEY` once
  per binary; a case that needs another credential passes it to
  `TunnelSpec::credential`. A test that sets or clears the process credential
  holds `PROCESS_CREDENTIAL_TEST_LOCK`, as does every case in its binary that
  relies on it.

## Commits, PRs and releases
- Commits: short and imperative. PRs: summary, rationale, verification, and doc
  updates when behavior changes; attach logs for networking changes.
- A user-visible change adds a bullet under `## [Unreleased]` at the top of
  `CHANGELOG.md`; a release renames that heading. The English and Chinese
  pages of a pair change together, and a new, renamed or retired page updates
  `docs/README.md`.
- A `vX.Y.Z` tag publishes everything: CLI archives, Docker images, the UI, and
  the Rust crates and npm packages (`release-sdk.yml`). The tag, the workspace
  version with its internal crate pins, `js/package.json` with its platform
  packages, and `ui/pubspec.yaml` must match, and the pubspec's build number
  only rises (`docs/ui-updates.md` records the next). Never reuse a tag or
  republish a version. The procedure is
  `skills/pb-mapper-release-pipeline/SKILL.md`.
- `version` stays the first key of `[workspace.package]`, which the release
  workflows read by position; a new published crate joins the publish order in
  `release-sdk.yml`.
- Never commit secrets. Test relays use dedicated credentials and ports.
