# Open items from the 2026-11 refactor

What the November 2026 refactor and its defect fixes looked at and left on
purpose, so it is not found and argued again. Each entry says what a change
would take or what it trades. Fixes are in the [changelog](../CHANGELOG.md);
what turned out not to be a defect, or was already done, is not listed.
Remove an entry once it is decided.

## Compatibility

Each would change bytes on a wire, a stored file or a published shape.

### pb-mapper-core

- `derive_msg_header_key` maps each digest byte to its 62 characters with
  `% 62`, which favours `0`-`7` slightly. A fix changes every machine-derived
  key already handed out: it needs a new derivation version behind a flag.

### Manifests

- The `udp-timeout` feature does nothing: uni-stream's default features
  turn it on anyway. A fix is `default-features = false` on uni-stream in the
  root manifest with every shipping binary turning the feature on, or
  dropping the feature from `pb-mapper-protocol`, `pb-mapper-client`,
  `pb-mapper-server` and the CLI.

### pb-mapper-server

- The `RemoteId` status reply is a `Debug` dump. It is wire bytes; a stable
  shape changes them.
- A subscribe whose recovery or setup times out is closed with no answer, so
  the client counts a transport failure. A structured, retryable error would
  add a message on a path that has never sent one.

### pb-mapper-client

- Tunnel failures are text: `TunnelStatus::Failed`, `TunnelFailed.reason`
  and `RelayDiagnostics.last_error` reach the FFI's JSON and the Node addon
  as strings, with any code inside the text (the tests match on it). Typed
  reasons change those shapes in the SDK, the FFI and Node together.
- `pb_mapper::Error` keeps causes as text (`Protocol { message }` and the
  like; the missing causes were fixed in that text). Typed sources would add
  fields to the facade's published `Error`, which the FFI and Node read.
- The direct path's TLS binding context is `serde_json` of a tuple. Any
  change alters the binding bytes and breaks older peers, so it needs a
  `DIRECT_VERSION` bump, which stays 1.
- tokio-rustls keeps its `tls12` feature (root manifest). Dropping it takes
  the TLS 1.2 suites out of the ClientHello sent over TCP, which changes
  bytes on the wire.

### pb-mapper-desktop

- Desktop messages and `Invitation::service` are validated after decoding.
  Validating at decode changes the answer a peer gets on the desktop wire
  and `Invitation::service`'s public type: it needs a protocol version.
- `PrivilegedSupport.reason` is text in the FFI's JSON; a kind needs a new
  optional key.
- The background service's and the helper's answers carry no error kind.
  Adding one is a `HELPER_PROTOCOL` bump with new `.v<n>` pipe names.

### The app's FFI (`ui/native/pb_mapper_ffi`)

- A stricter service-name check when loading remembered grants and computers
  would refuse records 0.11.0 wrote and loads today.
- The control channel's `config-get` omits `lanTransport`; adding it is one
  more key in its machine-readable output.
- A relay without QUIC and an unreachable relay both report
  `SERVER_UNREACHABLE`. Telling them apart needs a new `ErrorCode` value
  that the Dart app learns.
- `updatedAtMs` holds the time of the read for a share and the creation
  time for a connection. Dart parses it and never reads it; a real update
  time needs a new persisted field in both stores.

### Node addon (`crates/pb-mapper-node`)

- SDK errors become `GenericFailure`; a stable kind tag changes JS
  `error.code` and the message, which are the addon's surface.
- Key ids (`generation << 32 | slot`) pass 2^53 after 2^21 reissues of one
  slot and lose precision as JS numbers, whatever the conversion. The fix is
  `BigInt` ids, a shape change.

## Large

### pb-mapper-auth

- Persistence runs on a tokio worker, and the async start blocks on file
  work. The change: run the actor on a dedicated `std::thread` fed by
  `blocking_recv` with its own one-second tick (or await each persistence
  step through `spawn_blocking`, in order), and do the start's file work in
  `spawn_blocking`.

### pb-mapper-client

- `connect_tcp`'s failure reason drops its causes: `run_owned_stream`
  reports `error.to_string()`, and `Subscription::with_retry` replaces the
  last real error with `ControlIoTimeout` at the setup deadline, so
  `TunnelFailed.reason` for an unreachable relay never says why. The change:
  keep the last attempt's error in `with_retry`, return it (or a timeout
  variant with it as `source`) at the deadline, and report it with its
  causes.

## Dependencies

- **uni-stream 0.3.5**: its default features enable hickory-resolver's
  `system-config`; turning that off needs a uni-stream release.
- **uni-stream 0.3.5**: it reads a `static mut` UDP timeout inside a `Lazy`
  on the first UDP stream anywhere in the process. pb-mapper-client writes
  it once, but cannot order that write before another thread's first UDP
  stream without writing it at every UDP entry point, which would turn a
  register-only process's 20 s into 120 s. The fix is an atomic or a
  `OnceLock` in uni-stream; `deps/uni-stream` is kept for maintenance only.
  The same global is why the register side's UDP sessions get 20 s unless a
  `connect` tunnel started first in that process; per-tunnel timeouts need
  an API uni-stream lacks.
- **enigo 0.6.1** (pb-mapper-desktop) pulls in windows 0.61 beside the
  workspace's 0.62; it needs an enigo release or a fork.
- **Windows' loopback TCP**: a send window left under one MSS until a
  five-second timer fires (Microsoft KB2020447) stalls loopback transfers
  now and then, `relay_transport_bench` among them. No workaround measured
  removed it. See "Stalls in relay_transport_bench" in
  [the refactor's benchmark record](benchmarks/2026-11-11-refactor/README.md).

## Trade-offs kept

Each works as designed; the line says what the other choice would cost.

### pb-mapper-core

- `RetryBackoff` has no jitter. Jitter spreads retry storms but changes
  retry timing everywhere.
- `resolve_addrs_async` asks public DNS servers before the system resolver,
  so names behind split-horizon DNS or a blocked UDP 53 reach the system
  resolver only after NXDOMAIN or a timeout, and every relay name is sent to
  public servers. Changing the order changes what names resolve to. (The
  blocking `resolve_addrs` asks only the system resolver, whatever its doc
  comment says.)
- `resolve_addrs_async` repeats the system lookup on failure: twice the
  latency of a failure, for one implicit retry.
- The config accessors read the environment on every call: documented
  re-read semantics against a few environment reads per control setup.

### pb-mapper-auth

- An out-of-range `AuthConfig` literal from an SDK caller is not checked: a
  capacity near `usize::MAX` aborts the start on allocation, and a maximum
  TTL past the wheel's span of about 34 years lets a key be issued already
  retired. The CLI, environment and FFI stay in range. A check at start
  would turn those into start errors; a validated type changes every
  literal.
- `IssuedTemporaryKey`'s `Debug` prints the credential. Nothing in the
  workspace formats it; redacting it protects SDK callers that log it but
  changes a public type's `Debug` output.
- A failed `chmod` in `append_wal` fails the store closed though the WAL is
  intact. Treating it as intact keeps live sessions while appending to a
  file whose mode could not be secured.
- The snapshot is rewritten every five minutes even when unchanged, about
  130 KB fsynced each time at the default capacity. A dirty flag would have
  to cover timer-driven changes (expiry, reap) that never reach the WAL.
- A key in a row above the configured capacity is never on the timing
  wheel: an active one shows `active` past its deadline, and a revoked one
  is not reaped until a restart or a GC. Such keys cannot authenticate, so
  this is display only.

### pb-mapper-protocol

- A torn last record of the first-flight replay log (`connection.replay`)
  fails closed across restarts, as documented and pinned by a test: the
  relay refuses every protocol-v2 connection until the file is removed.
  Dropping only an incomplete trailing record at load would lose nothing
  replay protection needs, since an admission is acknowledged only after
  `write_all` and `sync_data`.
- `CommonBuffer` keeps its largest frame's buffer, up to `MAX_PLAINTEXT_LEN`,
  until its stream ends. Shrinking trades that memory for a reallocation
  per large frame on the data path.
- The forward buffers grow to 8 MiB per direction, so codec frames reach
  8 MiB: memory per busy tunnel and store-and-forward latency per hop.
  Capping them at 64 KiB cost 16.9% and 7.7% of QUIC throughput, changed
  nothing on TCP and did not touch the loopback stall.

### pb-mapper-server

- Pre-authentication TCP connections are not capped. A failed accept no
  longer stops the listener, and idle sockets close after
  `control_io_timeout`; a cap would bound descriptors but also refuse real
  clients during a flood, and its size is policy.
- Registered control writes have no write timeout, and a full pong queue
  ends the registration. A wedged writer is already bounded by that queue
  and by stream-acknowledgement suspicion; a timeout changes when slow
  peers are dropped.
- A replacement must register within 200 ms of the stale connection's
  retirement (`REPLACEMENT_MARGIN`), less than a reconnecting provider's
  backoff plus two round trips. Widening it lengthens every failing
  subscribe and must stay under the client's 3 s minimum subscribe attempt.
- The re-subscribe poll is a fixed 50 ms inside the recovery window; backoff
  or jitter would pick up a replacement later.

### pb-mapper-client

- The network-change debounce sends one hint per burst, at once. A trailing
  hint would catch a burst's last change, but adds a second wake that
  cancels subscribe attempts begun after the first.
- A heartbeat that finds the control-write queue full ends the attempt and
  re-registers; waiting would keep a dead control link longer.
- The consumer's control setup takes no `control_permit`: taking one would
  queue direct setups behind registration recovery for up to the 3 s
  negotiation budget and send more of them to the relay.
- Administrator requests neither mark the relay failed nor clear that mark;
  letting them wake recovering tunnels would tie tunnel timing to admin
  calls.
- `with_relay_transport` copies the credential, so a carrier-switched client
  is its own session; sharing it would make one client's rotation move the
  other's credential.
- Unbiased `select!` loops cannot starve cancellation (tokio picks a branch
  at random); biasing them trades fairness between accepts, probes, control
  messages and session ends for cancellation priority.

### pb-mapper-desktop

- The engine's drop, on its own thread, blocks on the display-mode restore,
  and `RemoteBackend`'s and `PlatformBackend`'s drops restore too: the
  display-mode invariant's guaranteed restore, at the cost of a drop that
  can block.
- The elevated service's files reject unknown fields, so a same-version file
  it cannot read fails closed; loosening that trades it for downgrade
  tolerance.
- The background service's control pipe does not log the requests it
  refuses (an unverified caller, one silent past the timeout, pipe I/O).
  Logging them is a policy for a SYSTEM service's log.
- The helper pipe's drain polls every 1 ms; a waker-driven wait changes the
  helper's data-path timing.
- Data-path allocations not yet measured: `write_message`'s payload copy
  (it also sets TLS record boundaries), per-access-unit and per-header
  `Vec`s, and the video converter's views and samples per frame. Each needs
  the before and after measurement AGENTS.md asks for, some on hardware.

### The app's FFI and the CLI

- A panic in a JSON export aborts the app. Catching it would answer
  `INTERNAL` but keep running a process whose state the bug may have
  half-changed.
- A panicked blocking task is coded `IO` in one export and `INTERNAL` in
  another; `IO` makes the Dart viewer reconnect, `INTERNAL` ends the session.
- Keys the app does not know are dropped when a file is rewritten. A
  flattened map carried through each rewrite would keep a newer build's
  fields across a downgrade, if it ships before that build.
- Stopping the embedded relay stops every tunnel. Keeping the others needs a
  rule for which tunnels used it; theirs would otherwise retry forever.
- The control channel has no request timeouts; one on the CLI side must
  outlast the slowest command.
- The CLI has two duration grammars; unifying them changes what each
  accepts.
- The relay installs no SIGINT or SIGTERM handler. Nothing is lost (the auth
  state is written as it changes, and the state lock is an OS lock released
  at exit); a handler would only close connections politely.
- The FFI logs "Using caller-provided app directory" on every load and save:
  log volume against a trace of the directory each save used.

### Tests

- `reserve_addr` remains in `e2e/sdk.rs` (three ports),
  `e2e/key_namespaces.rs` (one) and `network_recovery.rs` (two), because
  SDK `connect` binds a `local_addr` string and reports no bound address.
  Port 0 needs an SDK accessor for the bound address, or a `connect` that
  takes a listener. `regression`'s `run_server_with_auth_config` case keeps
  one too: the published function reports its port only in its log.

### Published API, if 0.12.0 is to change it

Removing or narrowing these is a semver break with no behavior gained; the
main ones:

- public enums without `#[non_exhaustive]` (adding it is itself a break);
- unused published functions: `get_status`, `run_*_cli_with_shutdown`,
  `show_status_scoped` (which prints to stdout), `handle_status_cli_scoped`,
  `run_client_side_cli`, `run_server_side_cli`, `pb_mapper_server::run_server`;
- `pb-mapper-server`'s internal routing types and `pub mod direct` /
  `manager` (`ManagerTask`, `ConnTask`), and its entry points' shape
  (`io::Result`, an `UnboundedReceiver`, limits read only from the
  environment);
- `String` where a type would do: the credential APIs' `Result<_, String>`,
  `AuthFailure`'s code and message, `TemporaryKeyMetadata.state`, the
  status transport and state fields, and the desktop's
  `ConnectError::Protocol`, `InputError::Invalid`,
  `WorkerExit::CaptureFailed` and `HostOptions::service`;
- errors without a type of their own: `pb-mapper-protocol` has none, the
  desktop host returns `io::Result`, and core's `MsgCodec` and
  `MsgProtocol` keep their detail as a `String`.
