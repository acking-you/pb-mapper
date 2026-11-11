# Open items from the 2026-11 refactor

What is left after the November 2026 refactor, its defect fixes and two
passes over this list, one for correctness and one for measured performance,
so it is not found and argued again. Each entry says why it was not simply
done: the wire, file or published shape a change would break, the dependency
that owns it, or the two behaviours it chooses between, and what a change
would take. Fixes are in the [changelog](../CHANGELOG.md) and the
measurements in [the refactor's benchmark records](benchmarks/2026-11-11-refactor/README.md);
what turned out not to be a defect, or is done, is not listed. Remove an
entry once it is decided.

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
  `pb-mapper-server` and the CLI. Either changes what a published crate's
  features are or do.

### pb-mapper-server

- The `RemoteId` status reply is a `Debug` dump. It is wire bytes; a stable
  shape changes them.
- A retire by connection ID (`ConnectionRetire`) carries no generation. An
  ended registration's ID rests 10 s before another connection takes it;
  past that the ID can name a newer connection of the same service, which a
  retire from an older listing then reaches. An optional `generation`,
  omitted when unset and matched when present, would close it, with the
  CLI, SDK, Node and app calls taking it. But a field on
  `AdminRequest::ConnectionRetire` breaks SDK request literals, which
  AGENTS.md forbids, and a new request kind is refused by a 0.11.0 relay as
  undecodable, so the shape is the owner's call.

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
- A value a newer build adds to one of the SDK enums the app saves
  (`RelayTransport`, `LanTransport`, `PortMapping`) fails the whole parse:
  `config.json` then loads as the defaults, which the window's next settings
  save writes over the file, credential included, and a tunnel list is
  refused until it is fixed. A `#[serde(other)]` catch-all on those
  published enums, or the FFI keeping the raw value, would keep them.

### Node addon (`crates/pb-mapper-node`)

- SDK errors become `GenericFailure`; a stable kind tag changes JS
  `error.code` and the message, which are the addon's surface.
- Key ids (`generation << 32 | slot`) pass 2^53 after 2^21 reissues of one
  slot. The addon reports such an id as 2^53 - 1, which names no key, so JS
  cannot act on that key by id; `BigInt` ids, a shape change, would let it.

## Large

Each needs more than a fix, or a test bed this repository's tests lack.

### pb-mapper-desktop

- The helper's drain polls every 1 ms, and thread C every 2 ms
  (`DRAIN_POLL`), while the Windows encoder holds a unit: the encoder reads
  its event queue with `MF_EVENT_FLAG_NO_WAIT`, so there is nothing to wait
  on. The change: take the events through `BeginGetEvent` with an
  `IMFAsyncCallback` that signals, let `drain_video(wait)` wait on it, and
  make the Windows source a push source. Only hardware and an installed
  helper exercise it. See
  [open-items-desktop.md](benchmarks/2026-11-11-refactor/open-items-desktop.md).

## Dependencies

- **uni-stream 0.3.5**: its default features enable hickory-resolver's
  `system-config`; turning that off needs a uni-stream release.
- **uni-stream 0.3.5**: it reads a `static mut` UDP timeout inside a `Lazy`
  on the first UDP stream anywhere in the process. pb-mapper-client orders
  its write before every UDP stream it makes (`udp_timeout.rs`: one `Once`
  for a connect tunnel's start and a publisher's UDP dial, whichever comes
  first deciding), so its own streams no longer race it; a UDP stream made
  elsewhere in the process can, such as the app's preflight bind of a UDP
  connection's port if a datagram reaches it in that moment. Which timeout a
  publisher's UDP sessions get, 20 s or 120 s, still depends on whether a
  connect tunnel started first in that process; per-tunnel timeouts need an
  API uni-stream lacks, and the fix is an atomic or a `OnceLock` there;
  `deps/uni-stream` is kept for maintenance only.
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

- `resolve_addrs_async` asks the built-in public DNS servers (Alibaba,
  Tencent, Google) before the system resolver. It resolves the relay for
  every SDK administrator request, every app tunnel start and
  relay-settings change and `pb-mapper status`, and an SDK tunnel's local
  address; the blocking `resolve_addrs`, and the SDK's tunnels for their
  relay, ask only the system resolver. Public servers first was a choice,
  and which order to keep is the owner's call. On Linux both honour the
  hosts file. Public first sends those names to the public servers,
  resolves a split-horizon or VPN name that also has a public record to its
  public address, and, on a network that drops outbound DNS except to its
  own resolver, makes each lookup wait out the public servers' timeouts.
  System first gives a name the answer every other program gets: a device
  whose own resolver answers wrongly but successfully loses the public
  answer, and one whose resolver fails slowly pays that before the public
  lookup. A change to system first is written and held for this decision;
  no recorded measurement compares the two orders.
- The config accessors read the environment on every call: documented
  re-read semantics against a few environment reads per control setup.
- `duration_from_env` takes the default without a word when a variable does
  not parse (`PB_MAPPER_CONTROL_IO_TIMEOUT=1d`, a unit the CLI's flags
  take), so an operator's setting can silently do nothing. The accessors
  read it on every call, so a warning would repeat with each control setup,
  and refusing it changes start-up for configurations that run today.
- A crash during `durable_file::replace_contents`, such as the replay log's
  compaction or its rewrite at start, can leave the
  `.<name>.tmp-<pid>-<hex>` file beside the target, never removed. It costs
  disk only, up to the file's size per such crash. A sweep of stale
  temporary files at start belongs in `durable_file`, for every caller, and
  has to leave alone one that another running process is writing beside
  the same file.

### pb-mapper-auth

- Each administrator command and each compaction runs on tokio's blocking
  pool. That costs one thread hand-off per command (6 to 15 µs in
  `auth_bench` on Windows 11 and a Ryzen 9 9950X, see
  [open-items-auth.md](benchmarks/2026-11-11-refactor/open-items-auth.md)),
  and steps queue when every blocking-pool thread is busy. A thread of the
  actor's own would not queue, but it would outlive the caller's runtime: a
  program returning from `main` could then cut a WAL append short, and a
  torn last record fails the store closed at the next start.
- The five-minute compaction counts wall-clock seconds, so a backward clock
  step postpones it by the step's length; the WAL keeps every change
  meanwhile. A monotonic clock would compact on schedule, but it changes
  when compaction runs after a step.
- A failed compaction, the five-minute one or a GC's, fails the store
  closed, as [authentication-v2.md](authentication-v2.md) states, though
  what is on disk still restores the state: a failure up to the snapshot's
  rename leaves the old snapshot and the WAL, and one after it the new
  snapshot beside a WAL whose records it already holds, which replay
  tolerates. A disk full for a moment at the five-minute mark so ends every
  temporary-key session until a restart or reset. Logging and retrying at
  the next interval, as a rolled-back append does, would keep them; it
  changes documented behaviour.

### pb-mapper-protocol

- Every forwarding reader keeps the buffer of its largest read or frame
  until its stream ends (`BufferReader`, and `CommonBuffer` in
  `NormalMessageReader`). Reads, and so an encrypting forwarder's frames,
  stop at 1 MiB, so between peers of this version a direction holds 1 MiB
  at most; a reader of frames from a 0.11.0 peer, which sends up to
  `MAX_PLAINTEXT_LEN` (8 MiB) and must still be read, keeps up to 8 MiB.
  Giving a grown buffer back once small reads return held no less in
  `buffer_retention_bench` and costs a reallocation per large read; a
  64 KiB cap cost QUIC throughput. After a burst, an idle tunnel, its relay
  and both ends together, still holds 12 to 24 MiB more than before it
  (`buffer_retention_bench`, Windows 11, Ryzen 9 9950X), most of it not the
  readers' buffers; what it is (the allocator keeping freed frame buffers,
  QUIC's own buffers) is not measured. See
  [open-items-relay.md](benchmarks/2026-11-11-refactor/open-items-relay.md).

### pb-mapper-server

- Pre-authentication TCP connections are not capped. A failed accept no
  longer stops the listener, and idle sockets close after
  `control_io_timeout`; a cap would bound descriptors but also refuse real
  clients during a flood, and its size is policy. The pool of returned
  connection IDs keeps the size of the most connections ever open at once,
  a flood's included, until the relay stops.
- Registered control writes have no write timeout, and a full pong queue
  ends the registration. A wedged writer is already bounded by that queue,
  by stream-acknowledgement suspicion, and by a pb-mapper provider
  re-registering once it has heard nothing for its heartbeat tolerance and
  grace. A timeout would only change which of these ends it first.
- A replacement must register within 200 ms of the stale connection's
  retirement (`REPLACEMENT_MARGIN`), which is less than a reconnecting
  provider's backoff plus two round trips. A replacement that registers
  later serves the subscriber's next attempt: the waiting subscriber is
  refused with the retryable `service_not_available`, which a client
  retries within its 5 s budget. Widening the margin would lengthen every
  failing subscribe and still lose to the client's 3 s attempt. The 5 s
  setup bound still ends the wait before the retirement for an
  acknowledgement budget above about 1.6 s.

### pb-mapper-client

- `Client::notify_network_change` (Node's `notifyNetworkChange`) raises one
  hint per 250 ms, at once, and drops a second call inside that window. It
  is a synchronous call, which Node makes from its JavaScript thread outside
  any runtime, with no task to send a trailing hint from; the address
  watcher (Linux, Windows), which also hints at the end of a burst, and the
  two-second address poll elsewhere raise their own hints for the same
  change.
- The consumer's control setup takes no `control_permit`: taking one would
  queue direct setups behind registration recovery for up to the 3 s
  negotiation budget and send more of them to the relay.
- Administrator requests neither mark the relay failed nor clear that mark;
  letting them wake recovering tunnels would tie tunnel timing to admin
  calls.
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
- A trust revoke whose folder cannot be synced is logged
  (`desktop_trust_revoke_unsynced`) and succeeds, so a power loss soon after
  it can still bring the grant back. Failing it would stop the app from
  going on to revoke the background service's copy of the grant.
- The background service's control pipe does not log the requests it
  refuses (an unverified caller, one silent past the timeout, pipe I/O).
  Logging them is a policy for a SYSTEM service's log.
- serde buffers every field of the internally tagged desktop messages before
  it picks the variant: 4.36 allocations per video unit and about 3 per
  input event in `host_send_bench` (Windows 11, Ryzen 9 9950X). Removing
  them needs hand-written decoding of the hot messages that accepts and
  refuses exactly what the derived decoder does, with a differential test;
  the wire would not change. The helper pipe's framing (`oop::ipc`)
  allocates per record as the wire did, unmeasured: nothing outside the
  crate reaches it. See
  [open-items-desktop.md](benchmarks/2026-11-11-refactor/open-items-desktop.md).

### The app's FFI and the CLI

- A panic in a JSON export aborts the app. Catching it would answer
  `INTERNAL` but keep running a process whose state the bug may have
  half-changed.
- The remembered computers and the remembered sharing grant reject unknown
  fields, and the computers a newer vault version: after a downgrade the
  vault reads as unreadable and the window offers to clear it. Keeping a
  newer build's data there needs the flattened map the settings and tunnel
  lists now have, and a rule for versions.
- On Windows a settings or tunnel file replaced in one step takes this user
  as owner and its directory's inherited permissions: stable Rust reads
  neither a file's owner nor its ACL. Keeping them takes the security API
  (`GetSecurityInfo`, the token's user, a walk of the DACL) in hand-declared
  Win32, for a file not this user's in the app's own `%APPDATA%` (left by a
  run as administrator, which the replacement hands back to the user) or
  one given an explicit ACL by hand. A hard-linked file, and on unix another
  user's file or one in another group, is written in place, as 0.11.0 wrote
  it.
- The control channel's command line waits for the app's answer without a
  bound; Ctrl-C ends it. No bound outlasts the slowest command: `register`
  runs DNS lookups whose time the resolvers decide and a preflight bounded
  by `PB_MAPPER_CONTROL_IO_TIMEOUT`, which the app's environment sets, so a
  fixed one could report as failed a registration that then succeeds. (The
  app lets go of a peer that sends or reads nothing after 10 s.)
- The CLI's duration flags take s, m, h and d, the environment's timeouts
  ms, s, m and h: they set different things, and the CLI refuses with its
  units what its grammar does not take. Unifying them changes what each
  accepts.

### Tests

- `reserve_addr` remains in the testkit's `subscribe` and tunnel helpers and
  in `regression`'s `run_server_with_auth_config` case. The tunnel helper
  drives the public relay-only entry points, which report no address, and
  the published `run_server_with_auth_config` reports its port only in its
  log. `subscribe` drives the SDK and could bind port 0 now that
  `Connection::local_addr` says where a connection listens: a change to
  shared test support with nothing against it, not yet made.

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
