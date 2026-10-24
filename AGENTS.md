# Repository Guidelines

## Architecture
- One `pb-mapper` binary (`crates/pb-mapper-cli/src/bin/pb-mapper.rs`) with the
  roles `server`, `register`, `connect`, `status` and `admin`.
- Crates form a DAG, bottom-up: `pb-mapper-core` → `pb-mapper-auth` →
  `pb-mapper-protocol` → `pb-mapper-server` / `pb-mapper-client` (peers, no
  reference either way) → `pb-mapper-cli`, the `pb-mapper` SDK facade and
  `pb-mapper-node`. `ui/native/pb_mapper_ffi` is the Flutter C ABI.
  `pb-mapper-testkit` is test support only.
- Binary, package and library file names are hardcoded by release workflows,
  Dockerfiles, install scripts and the Dart loader; do not rename them.

## Compatibility invariants
- Defaults are direct first with relay fallback: the relay serves direct
  coordination on its TCP port number over UDP and announces only that port
  (clients pair it with the relay address they dialed), `register` allows
  direct, `connect` uses `--path auto`. `--relay-only` on any side restores the
  pre-direct protocol for it, and the 0.7 opt-in flags still parse. New wire
  fields are optional and omitted when unset; old SDK request literals must
  keep compiling.
- Direct (`crates/*/src/direct`, `pb-mapper-protocol/src/direct.rs`) is tried
  only when the relay, the publisher and that connect all allow it. After a
  pre-commit fallback the consumer pauses direct (10 s doubling to 5 min,
  cleared by a direct success or a network change) so an unpunchable path
  costs one attempt, not one per connection. pb-mapper owns coordination;
  reuse mature crates, not a whole foreign runtime.
- The commit point is StartDirect: the publisher dials its backend only after
  it, and the consumer marks the attempt committed before sending it. Fallback
  to relay happens only before commit, pins the selected publisher and
  generation, and consumes its grant once. After commit nothing is replayed.
- Established direct sessions belong to the endpoints. Coordinator loss and
  credential revocation must not close them; local stop cancels their tasks and
  closes the endpoint. Session reports are observations, never authorization.
- Keep TLS exporter binding, exact certificate pinning, namespace isolation,
  bounded control messages and bounded observation tables intact.
- Never write the process environment after startup; the credential lives in
  process state (`set_process_msg_header_key`).

## Performance
- Zero cost by default: an unused feature must not add work, allocation or size
  to the relay path. Box rare, large enum variants carried by hot channels.
- Do not copy, queue or spawn per packet on a data path; filter in place.
- Measure before and after a data-path change, and report the environment with
  any number. Never put an unmeasured figure in docs or images.

## Dependencies
- Direct dependencies that need patches are forks under `acking-you`, added as
  submodules in `deps/` on the maintenance branch documented in
  `deps/README.md`; push a fork commit before updating the gitlink. Transitive
  dependencies stay with Cargo. rustls is used through `quinn::rustls`.
- Pin versions once in `[workspace.dependencies]`.

## Verification
- `cargo fmt --all`
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- `cargo test --workspace --all-features`, or the narrowest relevant crates and
  `crates/pb-mapper-cli/tests/*` suites for a focused change.
- Node: `cd js && bun install && bun run build:release && bun test` (end-to-end
  cases need `cargo build --bin pb-mapper`).
- If a submodule under `deps/` changed, run fmt and clippy inside it too.

## Testing
- Cover core capabilities and real failure, state and security boundaries;
  avoid wording, glue and repetitive input matrices.
- Integration tests build their own relay and tunnels with `pb-mapper-testkit`
  and need no environment setup. Prefer keeping a bound socket over
  `reserve_addr`. Wait on readiness, not sleeps.
- Tests share a process with parallel threads (and DNS lookups): do not mutate
  the environment in them. Test environment-reading code in a
  `harness = false` binary such as
  `crates/pb-mapper-core/tests/process_environment.rs`. Tests that set the
  process credential hold `PROCESS_CREDENTIAL_TEST_LOCK`.

## Commits, PRs and releases
- Commits: short and imperative. PRs: summary, rationale, verification, and doc
  updates when behavior changes; attach logs for networking changes.
- A `vX.Y.Z` tag publishes everything: CLI archives, Docker images, the UI, and
  the Rust crates and npm packages (`release-sdk.yml`). Tag, workspace version,
  `js/package.json` and its platform packages must match; never reuse a tag or
  republish a version.
- Never commit secrets. Test relays use dedicated credentials and ports.
