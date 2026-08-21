# pb-mapper UI as a CLI — design spec

> Historical design document: this reflects the implementation at some point in
> 2025. Its line numbers and code references may have drifted — the crate split
> moved every path under `crates/` — and it is kept for design intent only. For
> current behaviour, read the code.

> Historical design note: the standalone CLI was consolidated in v0.3.0. In
> current commands, `pb-mapper-server` maps to `pb-mapper server`,
> `pb-mapper-server-cli` maps to `pb-mapper register`, and
> `pb-mapper-client-cli` maps to `pb-mapper connect`. The older executable
> names below are retained only where they describe the design context that
> preceded the unified binary.

## Goal

`pb_mapper_ui` should be usable as a command-line tool as well as a window:

```console
$ pb_mapper_ui register --key home-ubuntu --addr 127.0.0.1:8080
$ pb_mapper_ui status --json
```

Two things are wanted from that, and they pull in different directions:

1. **When the UI is running**, a command should be executed *by that process*, so
   the window reflects it immediately — a service registered from a terminal
   appears in the list without a manual refresh.
2. **When it is not**, the command should still work, doing the job in its own
   process the way `pb-mapper-client-cli` does today.

Both are in scope. They are different enough that the spec keeps them as two
named modes rather than one behaviour that quietly changes.

## The two modes

| | Attached | Headless |
|---|---|---|
| Who does the work | The running UI process | The CLI process itself |
| Lifetime of a tunnel | Outlives the command | Ends when the command ends |
| Command returns | Immediately | Blocks until interrupted |
| UI sees it | Yes | No — there is no UI |
| Needs a running UI | Yes | No |
| Config file | Read and written | Read only |

The lifetime row is the important one. `register` attached is fire-and-forget:
the UI holds the tunnel and the terminal returns. `register` headless *is* the
tunnel: the process stays in the foreground until Ctrl-C, exactly like
`pb-mapper-client-cli`.

The config row is the second-order consequence. Attached `register` persists the
service, because the UI's list is a list of persisted services and a CLI-created
one must survive a restart like any other. Headless `register` does not: it is a
one-shot tunnel, and silently seeding the UI's autostart list from a throwaway
command on a server is a surprise nobody asked for.

### Commands that exist in only one mode

Some verbs have no meaning without a process to talk to:

| Command | Headless | Why |
|---|---|---|
| `unregister`, `disconnect` | **Rejected** | They stop a task in another process. Headless there is nothing to stop — the tunnel is the CLI's own lifetime, so Ctrl-C is the stop. |
| `server stop` | **Rejected** | Same. |
| `watch`, `focus` | **Rejected** | Both are about the UI itself. |
| `server start` | Allowed, blocks | Equivalent to running `pb-mapper-server`. |

Rejected means exit code 2 with a message naming Ctrl-C or the missing UI — not
a silent success.

### Mode selection

Because those semantics differ, mode must not be decided by whether a UI
happens to be open. A script that expects `register` to block would return
immediately on a developer's machine and hang on a server.

- **Mutating commands** (`register`, `connect`, `unregister`, `disconnect`,
  `server start|stop`, `config set`) default to **attached**, and fail with a
  clear message if no UI is running:

  ```
  No running pb-mapper UI to attach to.
  Start the UI, or run this command with --headless to do the work here.
  ```

  `--headless` forces the second mode. `--attach` forces the first, so a script
  can insist on it rather than silently falling back.

- **Query commands** (`status`, `services`, `connections`, `config get`) fall
  back automatically. They are read-only and short-lived, and the fallback is
  not a different behaviour: attached reads the UI's live in-process state,
  headless makes the same stateless network query to the pb-mapper server that
  the FFI makes anyway. Either way the answer describes the same server.

## Prerequisites: what has to be cleared first

Building on the current state would carry four known problems into two new
callers. They are listed here in the order they should be fixed, and phase −1
below does all of it before any CLI code is written.

### P0. Per-service keep-alive does not work — a live bug, not debt

```rust
// src/common/config.rs:329
pub static IS_KEEPALIVE: LazyLock<bool> = LazyLock::new(|| {
    if std::env::var(PB_MAPPER_KEEP_ALIVE).is_ok() { /* … */ true } else { false }
});
```

Read at `local/client/stream.rs:37`, `local/server/stream.rs:62`,
`local/server/mod.rs:418` and `pb_server/mod.rs:932`. The UI sets the variable
that feeds it from three places — `start_server`, `register_service`,
`connect_service` (`state.rs:688,813,963`).

`LazyLock` freezes on first read. In a one-tunnel-per-process CLI that is
harmless: the binary sets the variable at startup, before anything reads it. In
the UI it means:

- The value is decided by whichever tunnel happens to start first, and every
  later tunnel inherits it regardless of its own checkbox.
- Once on, it can never be turned off — nothing calls `remove_var`, and the
  `LazyLock` would ignore it if it did.
- `.is_ok()` tests presence, not value, so `PB_MAPPER_KEEP_ALIVE=OFF` enables
  keep-alive.

So the per-service toggle shipped in 0.2.19 is largely inert. This is worth a
patch release on its own merits, independent of the CLI.

**Fix (done):** make it a parameter instead of an ambient global. A plain value
survives the per-connection `tokio::spawn`s inside the tunnel, which a
task-local would not.

```rust
// src/local/server/mod.rs — the server side already threaded two flags, so the
// third joins them in a struct rather than becoming a third adjacent bool.
#[derive(Clone, Copy, Debug)]
pub struct ServerTunnelOptions {
    pub need_codec: bool,
    pub is_datagram: bool,
    pub keep_alive: bool,
}
```

The client side takes a plain `keep_alive: bool` — it has no other flag to group
with, and a one-field struct would be ceremony. `run_server_with_shutdown` takes
one too, so the UI's embedded central server stops sharing a setting with the
tunnels running beside it.

`IS_KEEPALIVE` is replaced by `keep_alive_from_env()`, which the binaries read
once at startup as the documented fallback (`--keep-alive || env`). Nothing on a
tunnel path reads the environment any more.

`pb_server/mod.rs:932` keeps reading the global: that one is the central server,
where process-wide really is the right scope.

The existing binaries keep their contract — they build `TunnelOptions` from
`--keep-alive || env present` once at startup, which is where reading the
environment belongs.

### P1. The lock is held across network and file I/O

`register_service` holds the `PbMapperState` mutex across two DNS resolutions, a
`TcpStream::connect` preflight and a blocking `save_service_config`.
`connect_service` holds it across two resolutions and a listener bind. A slow or
blackholed server therefore stalls every other caller for the connect timeout.

**Fix:** three phases instead of one long critical section.

```
1. lock (µs)     claim the key in an in-flight set
2. no lock       resolve, preflight, persist
3. lock (µs)     spawn, insert handle, seed cache, release the claim
```

Step 1 is not optional. Narrowing the critical section alone would introduce a
race the current code accidentally prevents: two concurrent `register --key home`
— one from the window, one from a terminal — would both preflight, then both
insert, and the second would overwrite the first's `JoinHandle`, leaking a
running task with nothing left to abort it. The in-flight set makes the second
caller fail with `ALREADY_IN_PROGRESS` instead.

**Why not an actor.** Replacing the mutex with a command actor was the obvious
suggestion and it is the wrong tool. An actor that awaits a preflight inside its
handler is blocked exactly as the mutex was; to avoid that it would have to spawn
the slow work and reply from the spawned task, which needs the same per-key
in-flight bookkeeping — at which point it is a lock and a guard with extra
indirection. The three-phase shape fixes the actual defect, and it does not
rewrite 1,450 lines of working code to do it.

### P2. Errors are free-form strings

`Result<_, String>` throughout `state.rs`. It is why the first draft of this spec
made the protocol's `code` field optional — a machine-readable code cannot be
derived from prose without string matching.

**Fix (done):** `CtlError` in `ui/native/pb_mapper_ffi/src/error.rs`, carrying an
`ErrorCode` beside the sentence. `Display` returns the sentence alone, so no UI
text changes.

There is deliberately **no `From<String>`**. It was the obvious shortcut — every
existing `format!` site would have compiled untouched by defaulting to
`Internal` — and it is exactly what would have left the classification undone
forever. Without it the compiler names every site, and every site picked a code.

`code` is therefore **required whenever `success` is false**, with no exceptions:
`parse_c_string` and the null-handle check return `CtlError` too, so failures
raised at the FFI boundary before any state is touched are coded like the rest.
`response.rs` has no way left to emit an uncoded error.

### P3. Two callback slots, hand-rolled twice

`pb_mapper_set_log_callback` is an `AtomicPtr` plus a `transmute`. Adding the
change callback the same way duplicates it.

**Fix:** one `CallbackSlot<F>` used by both, with the `transmute` written once.

### Deliberately not in scope

`PbMapperService._runJsonOnWorker` spawns a fresh isolate through `compute()`
for **every** FFI call, which re-opens the dynamic library each time. That is
real waste, but it is pre-existing, it is on the UI path rather than the CLI
path, and fixing it properly means a long-lived worker isolate with a `SendPort`
— its own piece of work with its own testing. Naming it here so it is a decision
rather than an oversight. CLI mode sidesteps it by calling the FFI directly on
the main isolate.

## The unifying idea

Everything below follows from one decision: **there is a single `Command` enum,
and it is simultaneously the clap subcommand tree, the wire format, and the
dispatch input.**

```rust
#[derive(clap::Subcommand, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "kebab-case")]
pub enum Command { Register(RegisterArgs), Status, /* … */ }

pub async fn dispatch(
    state: &Arc<Mutex<PbMapperState>>,
    command: Command,
    origin: Origin,
) -> Response;
```

- The **CLI** parses argv into `Command`.
- **Attached mode** serialises that same value to JSON and sends it.
- The **control server** deserialises it and calls `dispatch`.
- **Headless mode** calls `dispatch` directly against a state it owns.

So a new subcommand extends the protocol by construction — there is no second
place to add it, and no marshalling code to keep in step. The "the two paths
drift" risk shrinks to one exhaustive `match` the compiler enforces.

Origin is not part of the wire format. The control server stamps `Origin::Cli`
on what arrives over the socket; the FFI stamps `Origin::Ui`; the background
status refreshers stamp `Origin::Internal`. Nothing has to be threaded through
`PbMapperState`.

## Where the CLI lives

**All of it in Rust, in `pb-mapper-ffi`, exported as one entry point.**

```rust
#[no_mangle]
pub unsafe extern "C" fn pb_mapper_cli_main(argc: c_int, argv: *const *const c_char) -> c_int;
```

Three reasons, in order of weight:

1. **Dart cannot talk to a Windows named pipe.** `dart:io` sockets cover unix
   domain sockets but not named pipes. A Dart-side IPC client would have to fall
   back to a `127.0.0.1` TCP port on Windows, which any local process can reach —
   turning tunnel control into something that needs its own token scheme. In
   Rust the transport is a named pipe with an ACL, and the problem does not
   exist.
2. **Headless mode is Rust work regardless.** It runs
   `run_server_side_cli_with_callback` and friends — the same functions the FFI
   already calls and the same ones `pb-mapper-client-cli` is built from.
   Splitting the CLI so that argument parsing lives in Dart and execution in
   Rust buys nothing and costs a marshalling layer.
3. **One implementation covers every platform.** Rust is natively
   cross-platform here; the alternative is per-platform runner code in C++,
   Swift and C.

Dart's role shrinks to: read `main(List<String> args)`, notice it is a command
rather than a launch, hand the arguments to the FFI, exit with the code it
returns. No verb table on the Dart side beyond the one-word check described
below, so there is nothing there to fall out of date.

### Byproduct: a standalone binary

The same module compiles as a normal `[[bin]]` target, `pb-mapper-ctl`, for
anyone who wants the tool without the GUI bundle. It costs one section in
`Cargo.toml`, plus adding `"rlib"` to the crate's `crate-type` — a `bin` target
cannot link a crate that only produces `cdylib` and `staticlib`.

## Module layout

New files, all under `ui/native/pb_mapper_ffi/src/`:

```
ctl/
  mod.rs        Command, Origin, dispatch()
  proto.rs      Request/Response envelopes, length-prefix framing
  endpoint.rs   endpoint name/path, one function shared by both ends
  server.rs     ControlServer — accept loop, lives in the UI process
  client.rs     connect + one round trip, lives in the CLI process
cli/
  mod.rs        pb_mapper_cli_main, mode selection, exit codes
  args.rs       the clap tree (re-exports ctl::Command)
  render.rs     human-readable output; --json bypasses it
  headless.rs   the in-process execution path
events.rs       StateChange, the change callback, watch subscribers
tunnel.rs       spawn_server_side / spawn_client_side, shared by state.rs
                and headless.rs so both raise a tunnel the same way
```

New dependencies: `clap` (already a workspace dependency), `interprocess`.

## Architecture

```
pb_mapper_ui register --key home --addr 127.0.0.1:8080
        │
        │  Dart main(args): first non-flag argument is a known verb → CLI mode.
        │  Skip windowManager, initLogging, createActors, runApp.
        ▼
  pb_mapper_cli_main(argv) ────────────────► Rust
        │
        ▼
   clap → Command ────────────┬───────────────────────┐
                      attached│                       │headless
                              ▼                       ▼
                     serialise to JSON,        own runtime + own
                     write to the socket       PbMapperState, no socket
                              │                       │
              ┌───────────────┴──────────┐            │
              │  Running UI process      │            │
              │    ControlServer         │            │
              │        ↓                 │            │
              │    dispatch(cmd, Cli) ───┼────────────┤ dispatch(cmd, Cli)
              │        ↓                 │            │
              │    PbMapperState         │            ▼
              │        ↓                 │     tunnel::spawn_*,
              │    events::emit          │     then block on ctrl_c
              │        ↓ NativeCallable  │            │
              │    Dart: changeStream    │            │
              │        ↓ (debounced)     │            │
              │    views reload          │            │
              └───────────────┬──────────┘            │
                              │ Response JSON         │
        ┌─────────────────────┘                       │
        ▼                                             ▼
   render, exit(code)                    SIGINT/SIGTERM → abort, exit 130
```

## 1. Wire protocol

### Framing

`u32` big-endian byte length, then that many bytes of UTF-8 JSON. One request
per connection, one response, then close — except `watch`, below. Bodies over
4 MiB are refused; the largest realistic payload is a server map dump.

Length prefixing rather than newline delimiting: it matches how pb-mapper frames
its own messages, and it lets the reader allocate once instead of growing a
buffer against untrusted input.

### Request

```jsonc
{
  "v": 1,                       // protocol version, not app version
  "id": "3f2a",                 // optional, echoed back; for watch correlation
  "cmd": "register",            // serde tag from Command
  "key": "home-ubuntu",         // …flattened Command payload
  "addr": "127.0.0.1:8080",
  "protocol": "tcp",
  "encrypt": true,
  "keepAlive": false
}
```

```rust
#[derive(Serialize, Deserialize)]
pub struct Request {
    pub v: u32,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(flatten)]
    pub command: Command,
}
```

### Response

The same envelope the FFI already returns, plus `v`, `id` and an optional
machine-readable `code`. `--json` prints this verbatim, so there is no second
serialisation path to keep consistent with the UI's.

```rust
#[derive(Serialize, Deserialize)]
pub struct Response {
    pub v: u32,
    #[serde(skip_serializing_if = "Option::is_none")] pub id: Option<String>,
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")] pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")] pub data: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")] pub code: Option<ErrorCode>,
}
```

`code` is **required whenever `success` is false**, which P2 above is what makes
possible. A script branches on it without parsing prose; `message` stays the
human sentence and keeps the wording the UI already shows.

```
raised by the control server:
  BAD_REQUEST  UNKNOWN_COMMAND  VERSION_MISMATCH  NOT_SUPPORTED_HEADLESS

from CtlError (implemented):
  NOT_FOUND  ALREADY_EXISTS  ALREADY_IN_PROGRESS  INVALID_ADDRESS
  INVALID_ARGUMENT  SERVER_UNREACHABLE  ADDRESS_IN_USE  TIMEOUT
  PROTOCOL  IO  INTERNAL
```

`INVALID_ARGUMENT`, `TIMEOUT` and `PROTOCOL` were not in the first draft. They
earned their place while classifying: a rejected `MSG_HEADER_KEY` is not an
address problem, a status query that ran out of time is not an unreachable
server, and a server answering with a message this version did not expect is
neither. Collapsing them into `INTERNAL` would have been the debt this exercise
was meant to avoid.

Adding a variant is additive — clients are required to treat an unrecognised
code as `INTERNAL`.

### Version negotiation

`v` is checked before `cmd` is looked at.

- CLI `v` > server `v`: refuse with `VERSION_MISMATCH` and a message saying the
  running UI is older than this CLI, naming both versions.
- CLI `v` < server `v`: the server accepts it and answers in the client's
  version, for as long as the older version is supported.

This is not theoretical even though the UI and the CLI are usually the same
binary: `pb-mapper-ctl` can be installed independently, and a UI updated in
place keeps running the old code until it is restarted.

A `hello` command returns `{v, appVersion, pid}` so a client can check without
doing anything.

### `watch`

The one exception to one-request-one-response. After the initial `Response`, the
server keeps the connection open and writes one framed `StateChange` per event
until the client disconnects. `pb_mapper_ui watch --json` is then a plain event
stream for scripting, and it shares its implementation with the Dart change
stream — both are subscribers to the same broadcast channel.

## 2. Endpoint and discovery

One function, used by both ends, so they cannot disagree:

```rust
pub fn endpoint() -> Endpoint;   // ctl/endpoint.rs
```

| Platform | Endpoint |
|---|---|
| Windows | `\\.\pipe\pb-mapper-ui.{hash of USERNAME}` |
| Linux | `$XDG_RUNTIME_DIR/pb-mapper-ui/ctl.sock`, dir `0700` |
| macOS | `$TMPDIR/pb-mapper-ui/ctl.sock` — `TMPDIR` is already per-user |
| fallback | `$HOME/.cache/pb-mapper-ui/ctl.sock`, dir `0700` |

`PB_MAPPER_UI_SOCK` overrides all of it — needed for tests, and useful for
running two profiles side by side.

**Access control is the OS boundary and nothing else.** The socket is `0600`
inside a `0700` directory the user owns. No token file, no TCP port, no shared
secret to leak into a log.

**On Windows an explicit DACL is required, not optional.** Measured: a pipe
created with a default security descriptor comes out as

```
D:(A;;FA;;;SY)(A;;FA;;;BA)(A;;FA;;;S-1-5-21-…-1001)(A;;FR;;;WD)(A;;FR;;;AN)
                                                    ^^^^^^^^^^^^^^^^^^^^^^^
```

— `WD` is Everyone and `AN` is Anonymous, both granted `FILE_GENERIC_READ`. Any
local process, under any account, could open the control pipe and read what the
server writes. That is not the "OS boundary is the boundary" story this design
rests on.

Supplying `D:P(A;;FA;;;OW)(A;;FA;;;SY)` produces exactly those two entries and
nothing else — `P` blocks inheritance, so no parent ACL can widen it again. The
implementation should name the current user's SID rather than `OW`, but the
mechanism is confirmed.

This has to be checked against `interprocess` when the dependency is actually
added: a plain listener almost certainly passes a null `lpSecurityAttributes`,
which is the bad case above. If the crate offers no way to supply a descriptor,
the control server creates the pipe itself.

**Discovery is a connect attempt.** Not a PID file, not a lock file — those go
stale and produce the worst failure mode, which is a CLI that reports "no UI"
while one is running, or blocks waiting for one that died. Connect succeeds →
attached mode is available. `NotFound` / `ConnectionRefused` → it is not. Any
other error is reported as-is rather than being read as "no UI".

**Stale sockets** only exist on unix, because a Windows pipe object dies with
its process. On `bind` failing with `AddrInUse`, try to connect once:

- connect fails → nobody is home, unlink and rebind (once; a second failure is
  a real error).
- connect succeeds → **another UI is already running.** That is the
  single-instance signal, and it falls out for free: the second instance sends
  `focus` and exits, instead of opening a second window with a second copy of
  every tunnel.

## 3. `ControlServer`

Spawned onto the runtime that `PbMapperHandle` already owns, holding a clone of
the same `Arc<Mutex<PbMapperState>>` the FFI uses. It is started by a new
`pb_mapper_start_control_server(handle)` called from Dart during startup, and
stopped by a `CancellationToken` on `pb_mapper_destroy` — the same shape
`start_server`/`stop_server` already use for the pb-mapper server itself.

Per connection: read one framed request, `dispatch(state, command, Origin::Cli)`,
write one framed response, close. Connections are handled concurrently, but
`PbMapperState` is one mutex, so execution is serialised.

Serialised, but only for the microseconds of map manipulation that P1 leaves
inside the critical section. The slow parts of `register` — resolution,
preflight, persistence — run with the lock released, and the in-flight set is
what keeps two concurrent registrations of the same key from racing. Without P1
this section would not be safe to write: a terminal blocked behind another
process's connect timeout is the kind of failure that gets blamed on the CLI.

Refusing to start is not fatal: if the endpoint cannot be bound, the UI logs a
warning and runs as a window. A GUI that will not open because a pipe is
unavailable would be a poor trade.

## 4. `pb_mapper_cli_main` and the clap tree

```rust
#[derive(clap::Parser)]
#[command(name = "pb-mapper", version,
          about = "Control a running pb-mapper UI, or run a tunnel here")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,

    /// Do the work in this process instead of handing it to a running UI.
    #[arg(long, global = true, conflicts_with = "attach")]
    pub headless: bool,

    /// Require a running UI; fail rather than doing the work here.
    #[arg(long, global = true)]
    pub attach: bool,

    /// Print the raw JSON envelope instead of a human summary.
    #[arg(long, global = true)]
    pub json: bool,

    /// Override the configured pb-mapper server for this invocation.
    #[arg(short, long, global = true, value_name = "PB_MAPPER_SERVER")]
    pub pb_mapper_server: Option<String>,

    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    pub verbose: u8,
}

#[derive(clap::Subcommand, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "kebab-case")]
pub enum Command {
    /// Expose a local service through the pb-mapper server.
    Register(RegisterArgs),
    /// Stop a registered service. Attached only.
    Unregister { key: String },
    /// Open a local port that forwards to a registered service.
    Connect(ConnectArgs),
    /// Close a client connection. Attached only.
    Disconnect { key: String },
    /// The local pb-mapper server.
    #[command(subcommand)]
    Server(ServerCommand),
    /// What the pb-mapper server knows.
    Status,
    /// Registered service keys.
    Services,
    /// Live connections for one service.
    Connections { key: String },
    /// Stored settings.
    #[command(subcommand)]
    Config(ConfigCommand),
    /// Stream state changes until interrupted. Attached only.
    Watch,
    /// Bring the running UI to the front. Attached only.
    Focus,
    /// Protocol and version handshake.
    Hello,
}

#[derive(clap::Args, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegisterArgs {
    #[arg(long)] pub key: String,
    #[arg(long)] pub addr: String,
    /// UDP instead of TCP.
    #[arg(long, default_value_t = false)] pub udp: bool,
    /// Forward without the encryption codec.
    #[arg(long, default_value_t = false)] pub no_encrypt: bool,
    #[arg(long, default_value_t = false)] pub keep_alive: bool,
}
```

`ConnectArgs` is the same minus `no_encrypt`, matching `connect_service`, which
takes no encryption flag. The names follow the existing binaries
(`--pb-mapper-server`, `--keep-alive`) so muscle memory carries over.

Exit codes:

| Code | Meaning |
|---|---|
| 0 | Success |
| 1 | The operation failed (server unreachable, key already registered, …) |
| 2 | Usage error — unknown verb, missing argument, command not valid in this mode |
| 3 | Attached mode requested or required, and no UI is running |
| 130 | Interrupted (a headless tunnel stopped by Ctrl-C) |

## 5. Change notification

### Rust side

```rust
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct StateChange {
    pub kind: ChangeKind,     // services | clients | server | config
    pub key: Option<String>,  // set when the change concerns one service
    pub origin: Origin,       // ui | cli | internal
    pub seq: u64,             // monotonic
}
```

`pb_mapper_set_change_callback` mirrors `pb_mapper_set_log_callback` exactly: a
function pointer in an `AtomicPtr`, invoked with a leaked `CString` the caller
frees with `pb_mapper_free_string`. Reusing the shape means reusing its
correctness — including that Dart must bind it with `NativeCallable.listener`,
since it is invoked from tokio worker threads and `.isolateLocal` would be
undefined behaviour there.

**Events are invalidation hints, not state.** They carry no payload beyond
identity. The receiver re-reads through the normal API. This is deliberate: a
dropped or coalesced event costs nothing, because the next one — or the next
user action — re-reads everything anyway. Shipping the new state inside the
event would mean taking the mutex on the emit path and guessing which projection
the receiver wants.

`seq` exists so a `watch` client can detect a gap and Dart can drop an event
that arrives out of order behind a slow reload.

`origin` exists so the UI can tell *someone else did this*. A change the window
made needs no announcement; a change a terminal made should say so. That is one
toast, and it is the difference between the two processes feeling joined and
feeling like they are fighting over the same files.

### Emission points

Only three live inside `PbMapperState`, and all three are transitions rather
than refreshes:

| Location | Condition | Emits |
|---|---|---|
| `schedule_service_status_refresh` | `(status, message)` differs from the cached entry | `services{key}`, `Internal` |
| `schedule_client_status_refresh` | same | `clients{key}`, `Internal` |
| `schedule_local_server_status_refresh` | `is_running` changed | `server`, `Internal` |

Comparing before emitting is what keeps this from becoming a firehose — those
refreshers run on a timer for every configured service.

Everything else is emitted at the boundary, where the origin is known:

| Boundary | After | Emits |
|---|---|---|
| `dispatch` | any successful mutating `Command` | derived from the variant |
| FFI mutators (`pb_mapper_register_service`, …) | `Ok(_)` | `Origin::Ui` |

Ten FFI functions each gain one line. In exchange, `PbMapperState` learns
nothing about who is calling it.

### Dart side

```dart
enum StateChangeKind { services, clients, serverStatus, config }
```

`PbMapperService.changeStream`, beside the existing `logStream`. Views subscribe
in `initState`, filter by kind, debounce 150 ms so a burst of three services
starting is one reload, and cancel in `dispose`.

**This is worth doing on its own merits.** The lists currently refresh only when
the user acts or a poll happens to land. Event-driven refresh is what makes a
CLI-driven change appear, and it removes polling from the common case — which is
also the answer to the battery cost of a tray app that never sleeps.

## 6. The Dart entry point

```dart
Future<void> main(List<String> args) async {
  if (CliEntry.looksLikeCommand(args)) {
    exit(CliEntry.run(args));      // direct FFI, no isolate, no engine services
  }
  // …existing startup…
}
```

Two details decide whether this is safe:

**No verb list in Dart.** An earlier draft had `looksLikeCommand` hold a
`Set<String>` of verbs, which is a second place to update every time a
subcommand is added — the exact duplication the `Command` enum exists to
prevent. Instead `pb_mapper_cli_main` returns a sentinel:

```rust
pub const PB_MAPPER_CLI_NOT_A_COMMAND: c_int = -1;
```

It compares the first non-flag argument against the subcommand names clap
already knows (`Cli::command().get_subcommands()`), and returns the sentinel
without printing anything if there is no match. Dart runs the GUI on `-1` and
exits with the value otherwise. The verb set therefore has exactly one
definition, in the enum.

Everything Flutter, the OS, and the debugger pass begins with `-`
(`--observatory-port=…`, macOS's `-psn_0_123456`), so a normal launch reaches
the sentinel path.

**Not going through `PbMapperApi`.** The existing Dart API wraps every FFI call
in `compute()`, which spawns an isolate and re-opens the library. That is right
for a UI that must not block a frame and wrong for a one-shot command. `CliEntry`
calls `pb_mapper_cli_main` directly on the main isolate.

## 7. Headless execution

`dispatch` with a `PbMapperState` the CLI process constructs itself. Because the
config directory is resolved the same way (`dirs::config_dir()/pb-mapper-ui`),
headless picks up the same `server_address`, `keep_alive` and `msg_header_key`
the UI uses — including `apply_msg_header_key_env`, which happens in
`PbMapperState::new`. The config file is the contract between the two modes, and
it already exists.

| Command | Backed by | Blocks |
|---|---|---|
| `register` | `tunnel::spawn_server_side` → `run_server_side_cli_with_callback::<Tcp\|UdpStreamProvider>` | yes |
| `connect` | `tunnel::spawn_client_side` → `run_client_side_cli_with_callback::<Tcp\|UdpListenerProvider>` | yes |
| `server start` | `PbMapperState::start_server` | yes |
| `status`, `services`, `connections` | `get_server_status_detail`, `get_service_conns` (`PbConnStatusReq`) | no |
| `config get\|set` | `load_config` / `save_config` | no |
| `unregister`, `disconnect`, `server stop`, `watch`, `focus` | — | rejected, exit 2 |

`tunnel.rs` is the piece that stops the two modes drifting. Today the spawn
logic — resolve addresses, pick TCP or UDP, build the status callback, spawn — is
inline in `PbMapperState::register_service` and `connect_service`. It moves out
to:

```rust
pub struct TunnelSpec {
    pub key: String,
    pub local: SocketAddr,
    pub remote: SocketAddr,
    pub udp: bool,
    pub options: TunnelOptions,   // keep_alive, encrypt — see P0
}
pub fn spawn_server_side(spec: TunnelSpec, status: StatusCallback) -> JoinHandle<()>;
pub fn spawn_client_side(spec: TunnelSpec, status: StatusCallback) -> JoinHandle<()>;
```

`PbMapperState` calls it and keeps the handle; headless calls it and awaits the
handle. One implementation, two lifetimes.

`TunnelOptions` travelling in the spec is what makes `--keep-alive` mean
something per command rather than per process — P0 is a prerequisite for this
signature, not a separate cleanup.

**Shutdown**: `tokio::signal::ctrl_c()` everywhere, plus `SIGTERM` via
`tokio::signal::unix` where it exists — needed for systemd and `docker stop`,
which do not send SIGINT. On either: abort the tunnel task, let the runtime
drain, exit 130.

## Command surface

Derived from `PbMapperApiClient`, which is already the complete list of
operations the UI can perform. No second vocabulary.

```
pb_mapper_ui register    --key K --addr A [--udp] [--no-encrypt] [--keep-alive]
pb_mapper_ui unregister  --key K
pb_mapper_ui connect     --key K --addr A [--udp] [--keep-alive]
pb_mapper_ui disconnect  --key K
pb_mapper_ui server      start [--port P] | stop
pb_mapper_ui status      [--json]
pb_mapper_ui services    [--json]
pb_mapper_ui connections --key K [--json]
pb_mapper_ui config      get [--json] | set --server A [--header-key K] [--keep-alive]
pb_mapper_ui watch       [--json]
pb_mapper_ui focus
pb_mapper_ui hello       [--json]

Global: --headless | --attach, --pb-mapper-server A, --json, -v
```

`connections --key K` is the structured per-service query added in 0.2.19; it
has no equivalent in the older CLIs. `deleteServiceConfig` and
`deleteClientConfig` are deliberately absent for now — deleting a stored config
from a terminal is easy to do by accident and the UI has a confirmation step. If
they are added, they take `--force`.

## Platform notes

Rust makes headless mode uniform across desktop targets. The platform-specific
work is confined to four points, three of which are now understood well enough
to plan around.

### Windows: stdout on a GUI-subsystem binary — **measured; the runner needs a fix**

`ui/windows/runner/main.cpp:12` calls `AttachConsole(ATTACH_PARENT_PROCESS)`,
but the `freopen_s` that rebinds `stdout`/`stderr` to `CONOUT$` lives only in
the `CreateAndAttachConsole()` branch in `utils.cpp`. Two probes — a
GUI-subsystem Rust binary reproducing the runner, and the real app with a
temporary Dart branch — say what that costs:

| | Rust `println!` | C runtime / Dart |
|---|---|---|
| before `AttachConsole` | handle is `0x0` | `_fileno(stdout) = -2` |
| after `AttachConsole` | handle is a **console** | `_fileno(stdout) = -2` — **still nothing** |
| after `freopen("CONOUT$")` | console | `_fileno(stdout) = 3`, flush ok |

Rust's `println!` goes to `GetStdHandle(STD_OUTPUT_HANDLE)`, which
`AttachConsole` populates, so it works. Dart's `stdout` resolves through the C
runtime's descriptor, which `AttachConsole` does not touch. Run unredirected
from a terminal, Dart fails outright:

```
stdout write+flush: FAILED FileSystemException: writeFrom failed,
                    path = '' (OS Error: 句柄无效。, errno = 6)   // ERROR_INVALID_HANDLE
```

Redirect it (`pb_mapper_ui status > out.txt`) and it works, because the shell
supplies a real descriptor — so this breaks in exactly the interactive case and
looks fine in a pipe.

**Two silent-failure traps found along the way.** Rust's `println!` returns
`Ok` when the handle is null, and Dart's `stdout.hasTerminal` returns **`true`**
in the very run where every write fails. Neither writer reports the problem;
output simply disappears.

**And `AttachConsole` itself is harmful when the shell redirects.** Building the
CLI turned this up: the first version of the fix produced a working exit code
and no output at all. Measured with a redirect in place:

```
before AttachConsole:  GetStdHandle = 0x6c   DISK (the file)    <- already fine
after  AttachConsole:  GetStdHandle = 0xb8   CHAR (a console)   <- clobbered
```

`AttachConsole` replaces the process std handles, so `pb_mapper_ui status >
out.txt` had its output diverted to a console window and the file stayed empty.
Attaching is only correct when there is nothing there to begin with.

**The fix, both halves together** (`ui/windows/runner/main.cpp`):

```cpp
HANDLE existing_stdout = ::GetStdHandle(STD_OUTPUT_HANDLE);
bool stdout_already_usable =
    existing_stdout != nullptr && existing_stdout != INVALID_HANDLE_VALUE &&
    ::GetFileType(existing_stdout) != FILE_TYPE_UNKNOWN;

if (!stdout_already_usable) {
  if (::AttachConsole(ATTACH_PARENT_PROCESS)) {
    FILE *unused;
    freopen_s(&unused, "CONOUT$", "w", stdout);
    freopen_s(&unused, "CONOUT$", "w", stderr);
  } else if (::IsDebuggerPresent()) {
    CreateAndAttachConsole();
  }
}
```

- redirected: leave everything alone; both writers already work
- unredirected from a terminal: attach for Rust, `freopen_s` for the C runtime
- no console at all: the attach fails and nothing happens

### Windows: window flash — **measured; there is none**

Reading the runner predicted this and a build confirmed it:

- `win32_window.cpp:137` creates the window with `WS_OVERLAPPEDWINDOW` and
  **not** `WS_VISIBLE`.
- `Win32Window::Show()` (`:152`) is the only `ShowWindow` call, and
  `flutter_window.cpp:31` invokes it from `SetNextFrameCallback` — i.e. **when
  Flutter renders its first frame.**
- CLI mode never calls `runApp`, so no frame is ever produced.

A build whose `main(List<String> args)` exits before `runApp` was polled for a
window for its whole life: `MainWindowHandle` stayed `0` across 4,097 polls,
while the same binary launched normally showed one immediately. `window.Create()`
running unconditionally in `wWinMain` costs nothing visible.

No `argv` check in `wWinMain` is needed, which is what kept the CLI logic out of
C++ in the first place.

### macOS: the runner does not forward arguments — **confirmed gap**

`ui/macos/Runner/MainFlutterWindow.swift` constructs `FlutterViewController()`
with no project, so there is no equivalent of Windows'
`set_dart_entrypoint_arguments` or Linux's
`fl_dart_project_set_dart_entrypoint_arguments`
(`linux/runner/my_application.cc:59,83`). `main(List<String> args)` on macOS
therefore receives nothing today.

Fix: build a `FlutterDartProject`, set `dartEntrypointArguments` from
`CommandLine.arguments.dropFirst()`, pass it to the view controller. Roughly
five lines, and the only runner change the plan currently requires.

Alternatively macOS ships `pb-mapper-ctl` and the `.app` bundle stays
GUI-only — defensible, since `Foo.app/Contents/MacOS/Foo` is not a path anyone
types. The five lines are cheap enough to do anyway.

### Startup latency — **measured at ~254 ms**

Attached mode boots the Flutter engine and the Dart VM before Dart can dispatch
to the FFI. Five runs of a build that exits immediately in CLI mode: 247, 253,
254, 257, 275 ms — median **254 ms**, and that is the floor, before the socket
round trip or any work.

Fine interactively. Poor in a loop, and noticeable in a shell prompt or a
`watch`-style script.

Mitigation, in order of preference:

1. Ship `pb-mapper-ctl` for scripting; no engine, so this cost disappears.
2. Move the argv check into the runner so the engine never starts for a command.
   That is per-platform C++/Swift/C — the cost this design is arranged to avoid.

254 ms is not bad enough to justify (2) on its own; (1) covers the case that
cares. Revisit only if `pb_mapper_ui` itself ends up in a hot loop.

### Android and iOS

Out of scope: there is no command line to invoke. `ControlServer` is not started
there, and the FFI export is present but unused. No behaviour changes.

## Phasing

Each phase ends somewhere demonstrable.

| Phase | Contents | Done when |
|---|---|---|
| −1 | **P0** `TunnelOptions`; **P1** three-phase locking + in-flight set; **P2** `CtlError`; **P3** `CallbackSlot` | The keep-alive toggle works per service; no lock is held across I/O. Ships on its own |
| 0 | **Done.** Windows stdout, window flash, engine startup, pipe DACL — all measured; see the platform notes | Runner needs a 4-line `freopen_s`; the pipe needs an explicit DACL; no flash; 254 ms |
| 1 | **Done.** `ctl` module, `Command`, `dispatch`, `ControlServer`, CLI entry, runner fix — and the whole command surface rather than just `hello`/`status`, since dispatch is one arm per existing state method | `pb_mapper_ui status` answers from a running UI |
| 2 | `events.rs`, change callback, Dart `changeStream`, view subscriptions; **delete `polling.dart`** and the card timeouts it stands in for | Registering from a terminal updates the open window by itself, and nothing polls |
| 3 | `tunnel.rs` extraction, headless mode, signal handling | The same commands work with no UI running |
| 4 | Full command surface, `--json`, exit codes, `watch`, `pb-mapper-ctl` target | The tool is complete |
| 5 | Single-instance + `focus`, macOS argv, error messages, docs | Shippable |

Phase −1 is a bug fix and a release in its own right; nothing below depends on
the CLI being wanted. Do it first regardless.

Phase 2 is where the requirement is actually met and should not be deferred; it
also improves the existing UI whether or not the CLI is ever used.

It was supposed to *delete* the polling. Reading it changed that: the loop in
`common/polling.dart` is not only keeping the list fresh, it is also what
notices a service that came up **failed** and raises the red toast for it.
Deleting it would have taken that with it. So the wait was converted rather
than removed — `waitUntilSettled` now blocks on the change stream instead of a
one-second timer, which reacts sooner and asks the native side nothing in the
meantime, with a slow tick left as a backstop rather than the mechanism.

The 10-second `Future.delayed` in `service_card.dart` and `client_card.dart`
stays for the same reason: it is labelled "clear operating state if no status
update received", which is a fallback for the event never arriving, not a second
copy of the mechanism.

Phase 3's `tunnel.rs` extraction is the one remaining refactor of existing code,
and doing it before the headless path exists means the shared implementation is
proven by the UI before anything else depends on it.

## Test plan

The point of these is that the two execution paths cannot silently diverge.

1. **Surface completeness** — a Rust test whose `match` over `Command` is
   exhaustive and asserts each variant maps to a `PbMapperState` method or an
   explicit "not applicable". Adding a variant without wiring it fails to
   compile. Nothing on the Dart side to check, because Dart forwards argv rather
   than reimplementing the tree.
2. **Round trip** — `#[tokio::test]` starting a `ControlServer` on a
   `PB_MAPPER_UI_SOCK` in a temp directory, then running each read-only command
   through `ctl::client` and asserting the envelope shape. Covers framing,
   version check, and unknown-command handling.
3. **Attached/headless parity** — the same `status` command both ways against a
   test pb-mapper server; the `data` objects must be equal. This is the direct
   test for the drift risk.
4. **Stale socket** (unix) — write a dangling socket file, assert bind reclaims
   it; hold a live listener, assert bind reports "already running" instead.
5. **Mode rules** — `unregister --headless` exits 2 with the Ctrl-C message;
   `status` with no UI falls back silently; `status --attach` with no UI exits 3.
6. **Change events** — register through the control server, assert an event with
   `kind: services`, `origin: cli` arrives on a subscriber, and that a status
   refresh producing an unchanged `(status, message)` emits nothing.
7. **Dart** — the sentinel path: verbs, `--observatory-port=…`, `-psn_0_123456`,
   an empty list, and an unknown first word all route to GUI or CLI correctly.
8. **Per-service keep-alive** (phase −1) — register two tunnels in one process,
   one with `keep_alive` and one without, and assert the socket option differs.
   This is the test the current design cannot pass, and the reason P0 exists.
9. **No lock across I/O** (phase −1) — with a blackholed server address, a
   `register` that is stuck in preflight must not delay a concurrent `status`.
   Assert on elapsed time with a generous bound; the failure mode is seconds,
   not milliseconds.
10. **Concurrent same-key register** — two at once, one wins, the other gets
    `ALREADY_IN_PROGRESS`, and exactly one `JoinHandle` is left behind.

## Risks

- **Phase −1 touches shipped tunnel code.** `TunnelOptions` changes signatures
  in `src/local/{client,server}/`, which is the path every existing tunnel runs
  through — a regression there breaks the product, not just the CLI. It is
  mechanical and the integration tests in `tests/` cover the paths, but it wants
  its own review rather than riding along with a feature.
- **A blocking headless command inside the GUI binary.** `pb_mapper_ui register
  --headless` holds a terminal open on a process that is nominally a GUI app.
  Correct, but surprising; the help text should say so, and `pb-mapper-ctl` is
  the better answer for that use.
- **Windows named pipe permissions.** Confirmed in phase 0: the default DACL
  grants Everyone and Anonymous read. Phase 1 must set an explicit one and have
  a test that reads the descriptor back, because getting it wrong is silent —
  the pipe works exactly the same either way.
- **Two execution paths for one operation.** Mitigated structurally by
  `tunnel.rs` and one `Command` enum, and tested by parity test 3 — but it stays
  on this list, because it is the failure this design exists to prevent.

## Relationship to the existing CLIs

`pb-mapper-client-cli` and `pb-mapper-server-cli` stay as they are. Headless
mode overlaps them deliberately — it is the same library doing the same work —
but the entry point differs, the config file is shared with the UI, and attached
mode has no equivalent at all.

The documentation should be explicit that these are not interchangeable: the
existing binaries always run their own tunnel in their own process, take no
account of the UI's stored configuration, and a UI running elsewhere will not
know about them.
