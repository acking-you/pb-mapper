# Architecture

This page maps the code. The rules for changing it — subagent models,
invariants, the Rust guide, verification — are in [AGENTS.md](../AGENTS.md)
and apply to every task.

## Project Overview

This is a Rust-based network tunneling/proxy system called `pb-mapper` that allows exposing local services to clients over a public network. The project enables users to access their home services (like file transfer servers) from anywhere by creating secure tunnels through a public server.

The system uses one **pb-mapper** binary (`crates/pb-mapper-cli/src/bin/pb-mapper.rs`) with explicit role commands:

1. **`pb-mapper server`**: Central server that manages connections between local services and clients
   - Runs on port 7666 by default
   - Supports IPv4/IPv6 configuration
   - Manages service registration and client subscription mappings
   - Handles connection forwarding and keep-alive mechanisms

2. **`pb-mapper register`**: Registers local services with the central server
   - Exposes local TCP/UDP services to the public server
   - Supports encryption codec for secure communication
   - Configurable via environment variables and command-line arguments

3. **`pb-mapper connect`**: Connects to registered services through the central server
   - Subscribes to remote services and creates local listening endpoints
   - Supports both TCP and UDP protocols

4. **`pb-mapper status`**: Queries remote IDs and registered service keys

5. **`pb-mapper admin`**: Administrator operations against a running server —
   issuing, listing, and revoking temporary credentials, rotating the
   administrator key, and listing services and connections

6. **UI Module** (`ui/`): Flutter graphical interface
   - Replaces all CLI functionality with a user-friendly GUI
   - Calls into Rust through raw `dart:ffi` against the `pb-mapper-ffi` crate
   - Provides comprehensive service management interface

The system works by creating a bridge between local services and remote clients through a public server, enabling access to services behind NAT/firewalls.

## Code Architecture

### Project Structure

The root `Cargo.toml` is a virtual manifest; every crate lives under `crates/`,
except the FFI cdylib, which sits next to the Flutter code that loads it.

```
pb-mapper/
├── crates/
│   ├── pb-mapper-core/     # Bottom layer: checksum, config, conn_id, error,
│   │                       # addr, codec, timeout, time, durable_file, listener,
│   │                       # paging, DataLenType
│   ├── pb-mapper-auth/     # Credential lifecycle, persistence, timing wheel
│   ├── pb-mapper-protocol/ # Message framing, v2 secure sessions, forwarding
│   ├── pb-mapper-server/   # Central relay server, plus the task manager
│   ├── pb-mapper-client/   # Both tunnel ends: `register` and `connect`
│   ├── pb-mapper-desktop/  # Remote desktop: host, viewer, credit, QoS, display
│   │                       # modes; platforms behind platform/ (Windows today)
│   ├── pb-mapper/          # The published SDK facade over pb-mapper-client;
│   │                       # feature `desktop` re-exports pb-mapper-desktop
│   ├── pb-mapper-node/     # Node-API bindings for the client SDK, built by js/
│   ├── pb-mapper-testkit/  # Test support: a complete e2e tunnel, for any test file
│   └── pb-mapper-cli/      # The `pb-mapper` binary, integration tests, examples
├── ui/                    # Flutter UI, talking to Rust over dart:ffi
│   ├── lib/               # Flutter application code
│   │   ├── l10n/          # ARB sources and generated AppLocalizations
│   │   └── src/ffi/       # The Dart side of the FFI boundary
│   ├── native/pb_mapper_ffi/     # C ABI crate (a workspace member)
│   ├── native/pb_mapper_galaxy/  # Galaxy view geometry; no dependencies
│   ├── shaders/           # The galaxy's nebula backdrop
│   └── test/              # Widget tests
├── docker/                # Docker deployment configuration
└── services/              # Systemd service files
```

The dependency graph is a DAG, and the layering is what the crate split
encodes:

```
pb-mapper-cli          pb-mapper-ffi
      │                      │
      └────┬─────────────────┤
           ▼                 ▼
    pb-mapper-server   pb-mapper-client   (peers: no reference either way)
           └──────┬──────────┘
                  ▼
          pb-mapper-protocol
                  ▼
            pb-mapper-auth
                  ▼
            pb-mapper-core
```

The remote desktop sits above `pb-mapper-client` and reaches the FFI through
the SDK facade:

```
pb-mapper-ffi ─▶ pb-mapper (feature desktop) ─▶ pb-mapper-desktop ─▶ pb-mapper-client
```

`pb-mapper` is the published SDK facade over `pb-mapper-client`. Its optional
`desktop` feature re-exports `pb-mapper-desktop` as `pb_mapper::desktop`, and
only the FFI enables it, so no CLI, relay or plain SDK build links capture,
codec or input code. `pb-mapper-desktop` also uses `pb-mapper-core` on every
target, for `durable_file` (the trust file and the display-mode journal),
`time`, `RetryBackoff` and `checksum`'s temporary-credential prefix, which
`Invitation::decode` uses to recognise a pasted setup string.

Note that the binary is still named `pb-mapper`, discovered from
`src/bin/pb-mapper.rs` inside `pb-mapper-cli`. The release workflows, both
Dockerfiles, and the install scripts hardcode that name, and `cargo build --bin
pb-mapper` resolves it from the workspace root regardless of the crate name.
Likewise `pb-mapper-ffi` keeps its package name, because it determines the
`libpb_mapper_ffi.{so,dylib,a}` / `pb_mapper_ffi.dll` filenames that the Dart
loader, two CMakeLists, four xcconfigs, and the release-ui hash checks expect.

`pb-mapper-galaxy` sits outside that graph: a leaf with no dependencies that
only `pb-mapper-ffi` uses, for the UI's galaxy view (see `docs/ui-design.md`).
Nothing in the SDK crates knows it exists.

### Core Modules

#### Rust Backend (`crates/`)
- **`pb-mapper-core/`**: The bottom layer; depends on no other crate here
  - `checksum.rs` and `checksum/`: Credentials and the frame checksum they
    key, all named as `checksum::…` — `credential.rs` parses and encodes an
    administrator key or a temporary credential, `process.rs` holds the
    process credential (read once from `MSG_HEADER_KEY`, replaceable at
    runtime) and the checksum over `datalen`, `machine_key.rs` the legacy
    machine-derived key
  - `config.rs`: Environment configuration — each `PB_MAPPER_*` name and the
    accessor that reads it — and address resolution entry points
  - `conn_id.rs`: Connection ID types
  - `error.rs`: The shared error type, plus the `snafu_error_*` macros
  - `addr.rs`: Address resolution; custom DNS servers on the async path
  - `codec.rs`: AES-256-GCM encrypt/decrypt
  - `timeout.rs`: `RetryBackoff`
  - `time.rs`: Wall-clock time and durations in the whole units the wire,
    state files and diagnostics carry, saturating rather than wrapping
  - `durable_file.rs`: Atomic replace of a state file (`replace_contents`: a
    temporary beside the target, its mode, write, sync, rename, directory
    sync) and its last two steps; the auth state, the protocol's replay log,
    the desktop's trust file and display-mode journal, and the app's config
    files all write through it
  - `listener.rs`: The TCP listeners every role binds for its peers (relay,
    connect tunnel, direct LAN acceptor, desktop host): address reuse as
    Tokio sets it, and `ACCEPT_BACKLOG`, the accept queue 0.11.0 had under
    mio 1.1 rather than whatever the locked mio asks for
  - `paging.rs`: One page out of a fully materialised listing, and the
    page-size ceiling the relay, the credential actor and the SDK share
  - `test_support.rs`: What the tests of this crate and the crates above it
    share: `TEST_ADMIN_KEY`, `TempPath`, `PROCESS_CREDENTIAL_TEST_LOCK`
  - `lib.rs`: `DataLenType`, which lives here so `checksum` and `error` can name it

- **`pb-mapper-auth/`**: The credential subsystem, and the largest one
  - `lib.rs`: The constants and the module map; `codes.rs`: every
    `AuthFailure::code`, each spelled once
  - `types.rs`: `AuthConfig`, `AuthFailure` and the administrator wire types;
    `lease.rs`: `AuthLease`, `AuthContext`; `ids.rs`: `KeyId`, its generation
    and its slot
  - `state.rs`: The shared state — the slot table and what hangs off it — and
    its restore from persisted state
  - `runtime.rs`: `AuthRuntime`, the facade: start-up, and the hot-path
    authentication of a presented key
  - `actor/`: The lifecycle actor, the one writer — `command.rs` its
    requests, `lifecycle.rs` issue, renew, revoke and collect, `epoch.rs` root
    rotation and reset, `replay.rs` the administrator replay window
  - `persistence/`: `format.rs`, `snapshot.rs`, `wal.rs`, `blob.rs`,
    `admin_key.rs`, `instance_id.rs`, `fs.rs`, `namespaces.rs`
  - `leases.rs`, `timing_wheel.rs`: Temporary-key lifetimes, scheduled on a
    hierarchical wheel that drives credential expiry
  - `keys.rs`, `config.rs`: Which administrator credential a relay starts
    with, and temporary-key derivation; configuration and platform dirs
  - `examples/`: `auth_bench` (derivation, authentication, and how long each
    administrator command, compaction and start takes and holds up the
    runtime's other tasks) and `snapshot_bench` (what an idle store writes)

- **`pb-mapper-protocol/`**: Framing and the authenticated session
  - `lib.rs`: The module list, the crate-wide frame limits and the root re-exports
  - `framing.rs`: The checksum + length framing, and the reader/writer traits
  - `frame_read.rs`: Frame progress kept by the reader, not by a cancellable
    read future
  - `command.rs`: Request/response types (`PbConnRequest`, `LocalServer`,
    `AdminRequest`, …), and the refusal codes and audit actions both ends name
  - `secure.rs`: Protocol-v2 single-flight sessions, client and server
  - `secure/`: `frame.rs`, `first_flight.rs`, `replay.rs` (the first-flight
    replay log, and the one writer thread per relay, `pb-mapper-replay-log`,
    that appends to it), `limiter.rs`
  - `data.rs`: Optional data encryption, negotiated on each relay leg
  - `forward.rs`: Stream and datagram forwarding
  - `buffer.rs`: Read buffers for the framing
  - `transport.rs`, `quic.rs`: The relay carriers, TCP or QUIC, and the pinned
    TLS QUIC primitives relay clients and servers share
  - `direct.rs`: Direct coordination messages, and the control timings both
    ends share
  - `examples/`: `data_codec_bench`, `framing_bench` and `handshake_bench`
    (first flights admitted against the replay log, one to 256 at once)

- **`pb-mapper-server/`**: The central relay
  - `lib.rs`: Module wiring, the public configuration types and re-exports
  - `task.rs`: `ManagerTask` / `ConnTask`, the work between the connections
    and the routing manager
  - `routing.rs`, `service_key.rs`: The routing domain model (registered
    control connections by service, pending streams, namespace quotas and
    rate limits, registration leases), and namespaced service keys
  - `runtime/`: Relay orchestration (`mod.rs`) and the `Router` that
    serialises the global routing maps and quotas, one method per task:
    `router.rs` dispatch, `lease.rs` registration leases, `subscribe.rs`
    subscriptions and their streams, `inventory.rs` the status and
    administrator views
  - `manager.rs`: The routing manager's work queue, and the registry of
    connection IDs and the mailboxes they are reached through
  - `connection.rs`: Per-socket authentication and dispatch
  - `server.rs`, `client.rs`: The service-side and subscriber-side loops
  - `guard.rs`: The one deregistration every connection kind owes the routing
    manager, sent exactly once
  - `direct/`: Direct coordination — `mod.rs` its configuration
    (`DirectServerConfig`), its bind and `DirectTask`, `state.rs` the
    setup state machine the manager drives (`state/observe.rs`: session
    observations, never authorization), `control.rs` one coordination
    connection's socket loop, `stun.rs` the STUN Binding service (`p2p`)
  - `quic/` (feature `relay-quic`): The optional QUIC listener — `mod.rs` the
    accept loop, `admission.rs` its handshake and connection limits
  - `admin.rs`: Administrator request handling
  - `status.rs`, `error.rs`: Status replies, errors

- **`pb-mapper-client/`**: Both ends of a tunnel
  - `sdk/`: The library API the `pb-mapper` facade re-exports — `client.rs`
    (`Client`), `handle.rs` (`Registration`, `Connection`), `worker.rs` (the
    lifecycle every tunnel worker shares), `types.rs`, `error.rs`, and
    `admin/` (the administrator RPCs, their owned response types, and the
    one-shot session each request runs over)
  - `server/`: `register` — publishes a local service: `mod.rs` the entry
    points, `worker.rs` the control pool and each worker's reconnect loop,
    `attempt.rs` one registration attempt and holding it (its deadlines and
    the control watchdog), `control.rs` a registered control connection's
    messages and lease, `probe.rs`, `status.rs` the pool's one status,
    `stream.rs` one forwarded session, `error.rs`
  - `client/`: `connect` — subscribes and listens locally (`mod.rs`,
    `health.rs` the remote probes, `stream.rs`, `listener.rs` the tunnel's
    local listeners, TCP bound through core's `listener` and UDP, each able to
    say where it is bound), plus `status.rs` and `fetch_status_scoped`
  - `worker_status.rs`: What a tunnel worker reports about itself, and the
    one place the text of the public status callbacks is written
  - `relay.rs`, `relay/quic.rs`, `endpoint.rs`: The relay carrier and its
    pooled QUIC connections; relay resolution and bounded setup admission.
    `addr.rs` resolves a tunnel endpoint to every address it names,
    `network.rs` holds the process-wide network hints, `udp_timeout.rs`
    orders uni-stream's process-wide UDP session timeout before any UDP
    stream reads it, and `recovery.rs` the short, bounded setup attempts
  - `test_relay.rs`, `status_text_tests.rs`: This crate's own loopback relay
    (it cannot use the testkit, which depends on it), and the pin on the
    status callbacks' text
  - `direct/` (feature `p2p`): direct sessions. `lan.rs` ranks interfaces
    (netdev, read in the background) and judges from a peer's candidates
    whether it shares a LAN; `candidates.rs` orders what is offered, LAN
    first; `socket.rs` and `endpoint/` (STUN discovery in `discovery.rs`,
    the dialing race in `dial.rs`) probe and dial each path as it answers;
    `consumer.rs` is the dialing end, up to the commit point or the relay
    fallback, over `control.rs`'s control connection, and `provider.rs` is
    the publishing end. `identity.rs` carries the exact certificates,
    `mapping.rs` the gateway port mapping (feature `port-mapping`),
    `reports.rs` the coalesced observations, and `link.rs` what a committed
    session's path is. LAN-first (on by default, `lan_first` in the options,
    carried as the crate-private `DirectPolicy` of `direct_policy.rs`)
    only orders probes and holds other paths back while a LAN is known to
    be there (`Assessment::holds`); off, the LAN is still detected and
    reported. A committed session's `DirectLink` lives in
    `diagnostics/link.rs`. `tcp.rs` is the TCP half of a LAN path, which the
    dialing end chooses (`ConnectOptions::lan_transport`, QUIC by default)
    and the publisher permits (`RegisterOptions::lan_tcp`, on by default):
    the provider's lazily opened acceptors (on every interface for a
    consumer with LAN evidence, wherever the relay is; on loopback alone for
    one on this computer under a loopback relay; admitting only LAN or
    loopback sources while a TCP-asking session is pending), the consumer's
    dialer and the head-start race (`TCP_HEAD_START`, 300 ms, while LAN-first
    is on), and `WriteThrough` (tokio-rustls leaves records buffered when the
    socket is full; the forward loop never flushes). `DirectConn` in
    `conn.rs` is the committed connection on either carrier, and the
    forwarding both ends run over it; `binding.rs`'s `Exporter` gives each
    carrier its own binding label. A path across the internet, punched or
    not, is always QUIC
  - `diagnostics.rs` (the `Diagnostics` handle) and `diagnostics/` —
    `events.rs` (event kinds, their history and stream), `report.rs`
    (`TunnelDiagnostics`, `PathDiagnostics`), `state.rs` (the recording
    state), `guards.rs` (`SessionGuard` and the other drop guards),
    `metrics.rs`, `link.rs`, `port_mapping.rs`: a tunnel's diagnostics, its
    event stream and its `TunnelMetrics` (sessions and bytes per path, the
    relay round trip, the live link), all read from one state; each session
    holds a `SessionGuard` whose `ByteMeter` the forwarding fills

- **`pb-mapper-desktop/`**: Endpoint-owned remote desktop, reached as
  `pb_mapper::desktop`; how it works is in `docs/remote-desktop.md`
  - `host.rs` (`host/builder.rs` its options and builder, `host/acceptor.rs`
    each connection's handshake and session), `session.rs` (B-recv;
    `session/sender.rs` B-send), `shared.rs`: The host, and one session's
    network side, B-recv and B-send over TLS; `shared.rs` states the lock
    discipline
  - `worker.rs` (`worker/`: capture, the input lane and control grant, the
    pointer reports, the drag-start hold, display modes, the one-second
    statistics window), `pacer.rs`, `scheduler.rs`, `lane.rs`: Thread C, the
    one desktop-bound thread (capture, encode, input), its pacing, its one
    wait, and the input lane from B-recv
  - `timer.rs`: Fine process timers, held while a session captures; on
    Windows `platform/windows/timer.rs` (the power-throttling opt-out and
    `timeBeginPeriod`)
  - `credit.rs`, `qos.rs`, `idr.rs`, `outgoing.rs`: Receipt credit, delay-based
    QoS, on-demand IDRs, and the hand-off from thread C to B-send
  - `protocol.rs`, `wire.rs`, `invite.rs`, `security.rs`, `auth.rs`: Protocol v2
    (ALPN `pb-mapper/desktop/2`), invitations, certificate pinning, and the
    input authorisation ledger
  - `video.rs`, `status.rs`: What a session's video is (`VideoCodec`,
    `QualityPreset`, `VideoLimits`, `Resolution`, the encoded units) and the
    bounds they share; `HostStatus` and what it holds. Both are re-exported
    at the crate root
  - `viewer/`: The viewer — `mod.rs` connects, `session.rs` runs its network
    session, `config.rs` is what it offers, `outbound.rs` what it sends in
    priority order, `recovery.rs` and `fps_cap.rs` its keyframe recovery and
    frame-rate cap, `stats.rs` and `status.rs` what it reports — and in
    `sink.rs` its one hand-off to a native consumer, which is also where a
    presentation is validated. A direct path's `ViewerLink` carries its
    `transport`; over TCP its round trips are only the handshake's
  - `backend.rs`: The public seam between thread C and what captures and
    injects for it: `Backend` (object-safe, not `Send`; `Box<B>` forwards
    every method under `#[deny(clippy::missing_trait_methods)]`, as every
    forwarding impl does), `Description`, `BackendCaps`, `HostOs`,
    `CursorMode`, `Waker`, and typed failures — `BackendError` carries a
    `FailureKind` inside the `io::Error`, and `failure_kind` reads it back;
    match kinds, never text
  - `launch.rs`: `Launch` and `Launcher<B, F>`. One `Launch::prepare` per
    admitted session is the only dynamic call on a host's session path;
    thread C is `Engine<B: Backend>` on a concrete backend
  - `native.rs`: `PlatformBackend<P>`, the neutral glue that drives any
    platform's parts with static calls (desktop checks, held-input release,
    input validation, the video source's lifetime, the display-mode guard),
    and the opaque public `NativeBackend` / `native_backend()` that benches
    and decorators compose
  - `platform/`: `Platform` and its parts (`pub(crate)`), chosen once at
    compile time: `Current` is `windows::Windows` on Windows and the
    uninhabited `unsupported::Unsupported` elsewhere. No platform `cfg`
    above this module, except the viewer's Android 60 fps rule and the
    `windows` re-export. `host_support()` replaces the old `capabilities()`;
    `displays()` lists displays, chosen by `DisplaySelection` with the
    device name (`\\.\DISPLAYn`) as the stable key. `synthetic.rs` is the
    test platform (pulled or pushed encoder). How to add Linux, macOS or
    Android is "Platforms" in `docs/remote-desktop.md`
  - `platform/windows/`: the ordinary desktop, `video/` (Desktop
    Duplication → D3D11 → Media Foundation H.265 or H.264, the fused
    `VideoSource`: the `Encoder` in `mod.rs`, the Media Foundation encoder in
    `codec.rs`, the D3D11 scaler in `converter.rs`, the per-adapter H.265
    probe in `probe.rs`; H.265 only for a viewer that lists `h265`, on a GPU
    whose hardware HEVC encoder passes a probe kept per adapter, and one
    encoder failure turns it off for the process), `input`, `cursor`,
    `still`, `waiter`, `timer`, `mf`, `dpapi` (the one DPAPI call every
    Windows store uses), `win32` (the Win32 glue the host and the helper
    share), `modes.rs` and `helper/`
  - `platform/windows/helper/`: The opt-in UAC/Winlogon service —
    `service.rs`; the workers it starts (`worker.rs`, `child.rs`) and a
    worker's binding to the input desktop (`binding.rs`); `pipe.rs`;
    `install.rs` and `readiness.rs` (whether the installed helper can be used
    or is offered Update); `background.rs` and `background/`, its own host
    and that host's grant at rest; `os/`, its Win32 glue by concern. The
    service side and the `pb-mapper-desktop-helper` binary need `--features
    windows-helper`. `HELPER_PROTOCOL` (now 7) and the `.v<n>` pipe names
    change together; an installed helper of another protocol, or of another
    build (its executable differs from the one bundled with the app), is
    offered Update. Protocol 7: `Start`/`Attach` carry `display_device`;
    `DisplayModes`, `SetDisplayMode`, `RestoreDisplayMode`; responses carry
    `failure` (a `FailureKind`), `caps`, `modes`, `restored`. A worker exits 3
    with modes outstanding, and the service then runs `helper restore
    <session>` (10 min), as it does at its start for any journal left; it
    ends that run only once a new worker in the session has attached.
    `Start` and the display-mode calls are bounded by 20 s
    (`MODE_TIMEOUT`), every other call by 3 s
  - `oop/`: the out-of-process backend over any byte transport: `proto`,
    `ipc` framing, `RemoteBackend<T>` (client) and `serve<B>` (worker),
    tested on every target over `tokio::io::duplex`. Both ends validate
    input; neither check may go
  - `privileged/`: the public helper facade on every target —
    `support()`, and on Windows `status`, `request_installation`,
    `background::{configure, adopt, Options::resume, …}` (the app's
    hand-off to the service)
  - `codec/annexb.rs`: neutral Annex B scanning and keyframe detection
  - `input/`: the input vocabulary a controlling viewer sends (`Input`,
    `Button`, `Key`, `InputEvent`, `DoubleClickGuard`) and the neutral rules
    every platform's injector follows: held-input release (`held.rs`) and
    cursor confinement (`cursor.rs`)
  - `modes.rs`: display modes, platform-neutral — `Mode`, `ModeList`
    (`fit_local`: the local size if listed, else the largest inside it
    within 2 % of its shape), `ModeTarget`, `DisplayModeError`; the journal
    (`display-modes.json` in the host's state directory, written durably
    before any change), the per-display claims of one process (the first
    original and the latest mode set; the last holder restores), `recover`,
    which replays an entry only in the scope it was made in
    (`DisplayModes::scope`: the login and the monitor) and drops one for a
    display that is gone, and `SETTLE` (3 s after a change,
    `display_changing`)
  - `platform/windows/modes.rs`: `WinModes` over `EnumDisplaySettingsExW` /
    `ChangeDisplaySettingsExW` — `CDS_TEST`, then dynamic (`dwFlags` 0),
    never `CDS_UPDATEREGISTRY` or `CDS_GLOBAL`; a NULL `DEVMODE` puts the
    registry mode back. `filter_modes` is the pure filter (depth,
    orientation, ≥ 800×600, 64 at most)
  - A mode change (远程分辨率, Remote resolution) needs the `display_mode`
    capability on both ends and control; the owner's
    `Host::restore_display_mode` needs neither. The host restores at session
    end, on revoke and on the owner's Restore — never on release, pause or a
    fresh grant — and never fights a mode the owner set meanwhile.
    `recover_display_modes(state_dir)` replays a journal a crashed host
    left; every `Host` start does it too, and after each session, backing
    off from 2 s to a minute
  - `Resolution::default()` is `Native` (the display's own size, up to
    3840×2160); `ViewerConfig::for_decoders(Decoders, …)` makes the offer —
    H.265 wherever its decoder exists, then H.264, else JPEG — and
    `VideoLimits::clamped` fits the limits inside the first decoder's largest
    picture, long edge against long edge
  - Hosts start with `Host::builder` / `Host::start(client, HostOptions::new
    (service))`, whose defaults are the tunnel's: LAN first, then direct,
    then the relay, and `lan_tcp` on. `Host::{identity, restart_on,
    switch_display, rename}` and `privileged::background::adopt` hold what
    the FFI used to
  - `supervisor.rs`: `RetryPolicy`, the pure `Reconnect` (the app's Dart
    policy: backoff 1, 2, 4, 8, 15 s within 120 s; `protocol` at most 3 in a
    row; a 5 s stall; one H.265 → H.264 fallback) and `Supervisor`, which
    connects, waits on each viewer and keeps the control choice and preset.
    The app keeps its own Dart policy; this serves SDK integrators
  - `trust.rs`: `Sealer`, `CurrentUser` (current-user DPAPI under the app's
    constants), `RelayScope`, `Remembered<T>`, `TrustFile<S, T>` — the app's
    `desktop-trust.dpapi` format plus `display_device` — and
    `write_durably`, which the display-mode journal also writes through;
    `F/desktop/trust.rs` is a thin wrapper over `TrustFile<CurrentUser,
    Settings>`
  - `watch.rs`: `Host::watch`, `Viewer::watch` — status as a
    `tokio::sync::watch`, sent when it changes, per-frame fields aside,
    ending with the host or viewer
  - `mapping.rs`: `DesktopView::display_point`, `Input::text_chunks`
  - `tests.rs`, `tests/session.rs`: Tests over a real relay and TLS, the second
    end to end; `slow_link.rs`: the ignored acceptance cases; `test_util.rs`:
    what the crate's own tests share (`until`, `Presenting`, the `Pixel`
    backend). `host/`, `session/`, `viewer/` and `worker/` keep their own
    `tests.rs`
  - `examples/`: Capture, transport and viewer probes, `video_bench` (the
    Windows capture and encode benchmark, which also counts the encoder's
    calls: allocations, time and thread cycles per unit), `host_send_bench`
    (a relay, a host with a synthetic encoder and a viewer in one process,
    counting allocations per video unit), `deployed` (the selftest, native
    size by default), `input_acceptance/` (native input into a window of its
    own, on Windows) and `display_mode_probe/`, the hardware acceptance of
    display modes, which changes the display it runs on; `probe_support/`
    holds what the probes share (their relay configuration, `--name value`
    options and SDK clients)

- **`pb-mapper/`**: The published SDK facade; `desktop` re-exports the
  remote desktop. `examples/desktop_{loopback,host,viewer,control,
  supervised,display_mode}.rs` with `examples/desktop_support/` (in-process
  relay, synthetic `Backend`, renderer thread); `desktop_loopback` runs on
  any OS and in the Linux CI job. `tests/default_path.rs` pins LAN → direct
  → relay as every entry point's default; `pb-mapper-client` alone is
  relay-only, as its crate docs say

- **`pb-mapper-node/`**: The Node-API bindings over the client SDK, which
  `js/` builds. `lib.rs` states the boundary's three rules (numbers narrow,
  enums cross as strings, errors flatten); `client.rs`, `tunnel.rs` and
  `admin.rs` are the `Client`, `Registration` / `Connection` and `Admin`
  classes, `readers.rs` the events and metrics JS pulls one at a time,
  `diagnostics.rs` the SDK's diagnostics as JS objects, and `convert.rs` the
  conversions at the boundary

- **`pb-mapper-testkit/`**: Test support only; nothing shipped depends on it
  - `relay.rs`: `Relay` — a live server on a loopback port with its own auth
    state, which retains its `AuthRuntime`, so a case can issue, renew, and
    revoke credentials without the admin wire protocol; `loopback_direct` /
    `loopback_quic` put direct coordination and the QUIC carrier on loopback
  - `tunnel.rs`: `TunnelSpec` / `Tunnel` / `TunnelHarness` — echo server plus
    `register` plus `connect`, on loopback ports
  - `sdk.rs`: `admin_client` / `publish` / `subscribe` — the SDK side of a
    case, each ready before it is handed back
  - `echo.rs`, `traffic.rs`: `tcp_echo` / `udp_echo`, each stopped by
    dropping the `Echo` it returns, and the framed and raw traffic drivers
  - `control.rs`: A protocol-v2 control connection registered by hand
  - `shaped.rs`: `Shape` / `shaped_link` — a TCP hop with delay and rate
    adjustable while connections run, owned by the `ShapedLink` it returns,
    shared by the remote desktop's tests
  - `poll.rs`: The one readiness loop every probe polls with
  - Tests log as the binary does: the crate includes the CLI's
    `src/bin/pb-mapper/logging.rs` by path. `TEST_ADMIN_KEY` is
    `pb_mapper_core::test_support`'s
  - A crate rather than `tests/common/mod.rs`: that module is compiled separately
    into every test binary, and whatever a binary does not use is reported as
    dead code — fatal under `-D warnings`

- **`pb-mapper-cli/`**: The binary, integration tests, and examples
  - `src/bin/pb-mapper.rs`: Argument parsing, dispatch, and `status`
  - `src/bin/pb-mapper/server.rs`: The `server` role — its flags, its
    authentication settings and where direct coordination listens
  - `src/bin/pb-mapper/tunnel.rs`: The `register` and `connect` roles, and the
    log kept of a running tunnel; its tests pin the flags' defaults
  - `src/bin/pb-mapper/admin.rs`: The `admin` subcommand
  - `src/bin/pb-mapper/logging.rs`: Where `tracing` goes (`RUST_LOG`,
    `PB_MAPPER_LOG_FORMAT`); `pb-mapper-testkit` includes it by path, so it
    names nothing else in this crate
  - `tests/e2e/`: One binary for the cases that can share a process —
    `tunnel_matrix.rs` (the transport/codec matrix over the whole tunnel),
    `temporary_credential.rs` (the credential lifecycle over the whole
    tunnel: namespace isolation, renew, expiry, revoke), `key_namespaces.rs`
    (several keys in one allocated namespace, over TCP, QUIC and direct
    setup), `sdk.rs` (the client SDK's tunnels and administrator RPCs),
    `direct.rs` (direct sessions end to end), `data_compatibility.rs`
    (mixed data formats on a real relay) and `bound_listener.rs` (hands a
    `connect` tunnel a listener the case already bound, through the
    listener-provider seam; `regression` and `regression_client` include
    it by path)
  - `tests/regression/`, `tests/regression_client/`, `tests/regression_cli/`:
    Protocol-level cases in three binaries, each setting the process
    credential once (`regression/credential.rs`, which the other two
    include by path). `regression` is the relay against hand-rolled frames
    (`relay.rs`; `regression_cli` shares its `fixtures.rs`), and one case
    runs a real `connect` and `register` against it in process, through a
    publisher's retire-and-replace; it writes no environment and starts no
    process, so a relay closing a connection is seen to close it.
    `regression_client` is `register` and
    `connect` against fake relays (`fake.rs`), with client timings it sets
    once before any case starts a tunnel. `regression_cli` runs the
    `pb-mapper` binary, its child relays on port 0 announcing the port they
    took. It is separate because a Windows child inherited every Tokio
    socket of the test process under mio 1.1, which mio 1.2.4 no longer
    allows
  - `tests/lease_reclaim.rs`, `network_recovery.rs`, `quic_relay.rs`,
    `quic_retired_carrier.rs`, `stream_before_ack.rs`: Binaries of their own,
    each because it changes state the whole process shares;
    `tests/e2e/main.rs` says which
  - `examples/`: `compatibility_relay`, the loopback relay
    `scripts/test_version_compatibility.py` builds at each revision;
    `relay_transport_bench`, TCP against QUIC relay on loopback;
    `resubscribe_bench`, what subscribes waiting for a publisher's
    replacement cost the relay; and `buffer_retention_bench`, the memory
    idle tunnels hold after a burst

#### Flutter UI (`ui/`)

The design and its rules are in `docs/ui-design.md`.

- **`lib/src/app.dart`**: The root — owns the store, the preferences, the
  galaxy's state and the remote desktop's (`ViewerHost`, `RemoteDesktop`,
  `SharingChrome`); chooses first-run setup, the six pages, or the remote
  viewer in their place while `ViewerHost.showing`; runs the tray, which
  also says when this computer is being viewed

- **`lib/src/pages/`**: One file per place
  - `overview_page.dart`: The galaxy, with a floating summary and details panel
  - `tunnels_page.dart`: Shares and connections, and a count leading to Relay
  - `relay_page.dart`: The relay as a whole — whether it answers, who this
    computer is to it, and its sections, which live in `relay/`: services,
    connections, and (administrator key only) credentials and security, plus
    the relay this computer can run itself
  - `logs_page.dart`: The log stream, filtered by level and word
  - `settings_page.dart`: Relay, credential, setup string, *Try the local
    network first* and *LAN transport* QUIC / TCP (both saved at once,
    outside the relay form, and serialised in `AppStore`), theme, language;
    Test says what the relay answered, or why it did not
  - `setup_flow.dart`: First run — relay, then what to do, then that tunnel
  - `tunnel_editor.dart`: The one share/connect form, and `followTunnel`
  - `tunnel_actions.dart`: Start/stop, delete, copy — shared by list and panel
  - `remote/`: The Remote desktop page — `remote_page.dart` lays out
    `host_card.dart` (this computer as a host, its masked invitation and
    `invite_qr.dart`) and the remembered computers (`computer_list.dart`,
    or `computer_list_compact.dart` on a phone); `add_computer.dart` and
    `invite_scanner.dart` recognise a pasted or scanned invitation before
    connecting; `remote_actions.dart` holds the confirmations
  - `remote/viewer/`: The viewer, shown by `viewer_screen.dart` in place of
    the shell. `viewer_stage.dart` (picture, remote cursor, overlays) is
    shared by `desktop_viewer.dart` (title bar, floating toolbar, menus) and
    `touch_viewer.dart` (gutters, modifier bar, minimap, virtual mouse,
    gesture sheet). Input leaves through one door, `remote_input.dart`;
    touch is decided by the pure `touch_gestures.dart`, modifiers by
    `modifier_latch.dart`, hardware keys by `remote_keys.dart`, the soft
    keyboard by `remote_text_input.dart`. The gestures and key rules are in
    `docs/remote-desktop.md`

- **`lib/src/shell/`**: Navigation and window chrome
  - `app_scope.dart`: `AppPage` (six places; the phone bar has five, Logs
    opening from Settings) and the `AppScope` every page reads
  - `app_shell.dart`: Rail on the backdrop and a sheet, or a bottom bar
  - `navigation.dart`: The rail (with the relay's state at its foot and a
    dot on Remote desktop while sharing), the bar
  - `page_frame.dart`: A page's title bar, which is also the window's
  - `window_chrome.dart`: Drag areas and the Windows/Linux caption buttons
  - `sharing_indicator.dart`: While this computer is viewed, Disconnect
    viewer and Stop sharing — a capsule in the title bar, or a pill above
    every route where no capsule can be reached
  - `app_prefs.dart`: Theme, language, last page

- **`lib/src/state/`**: `AppStore` (every read and write, kept fresh by the
  change stream), `RelayStore` (the Relay page's reads and the administrator's
  changes, polled only while that page is on screen) and the `Tunnel` model.
  The remote desktop: `DesktopStore` (`desktop_store.dart`; its views —
  host, session, stats, the native decoder's stats — each notify only their
  own listeners, never `AppStore`), `RemoteDesktop` (`remote_desktop.dart`: which computer is
  open, Back pausing it, remembering at the first picture),
  `RemoteComputerStore` (`remote_computers.dart`: the remembered list and
  its thumbnails) and `ViewerPrefs` (`viewer_prefs.dart`)

- **`lib/src/galaxy/`**: The galaxy view — scene model, camera, frame buffer
  over `pb_mapper_galaxy_frame_v1` with its Dart mirror, painter, details panel

- **`lib/src/theme/`**: POCOX's design system — colours, type, glass layers,
  motion, Lucide icons (`app_icons.dart`: weight 400, four sizes; no
  Material `Icons.*`, which CI greps for), and the viewer's dark palette
  (`viewer_palette.dart`)

- **`lib/src/updates/`**: The self-update, after proxy-everything's — find the
  newest `vX.Y.Z-ui` release, check its Ed25519-signed manifest, download and
  hash the one package, apply it per platform. See "Updates" in
  `docs/ui-design.md`; `ui/pubspec.yaml`'s version must be bumped with the
  workspace's at every release

- **`lib/src/common/`**: Shared utilities
  - `polling.dart`: `waitUntilSettled` and `firstWhereOrNull`
  - `state_change.dart`: The change stream's events, and `ChangeSubscription`
  - `log_manager.dart`: Log collection
  - `app_toast.dart`: Toasts, as glass, raisable without a live context

- **`lib/src/ffi/`**: The Dart side of the boundary
  - `pb_mapper_ffi.dart`: Raw `dart:ffi` symbol lookups and library loading
  - `pb_mapper_service.dart`: FFI dispatch on a background isolate
  - `pb_mapper_api.dart`: `PbMapperApiClient`, the interface the views take,
    and `PbMapperApi`, the real implementation over the FFI
  - `relay_models.dart`: What the relay calls return, and `RelayCallException`
  - `desktop_api.dart`: `DesktopApiClient`, the remote desktop's interface,
    and `DesktopApi` over the FFI: lifecycle calls on a background isolate,
    the hot ops and the per-frame view on the UI isolate. Its failures carry
    the FFI `ErrorCode`; never match their text
  - `desktop_video.dart`: The native decoder's Flutter texture, over the
    `pb_mapper/desktop_video` channel. Dart binds it to a video source; the
    decoder pulls the video from Rust itself. The channel's `decoders`
    method says, without creating a decoder, how large H.264 and H.265 this
    device decodes in hardware and its fastest refresh; the store offers
    H.265 and the frame-rate choices from it
  - `remote_computer_vault.dart`: `RemoteComputerVault`, the `computers_*`
    ops below, and on Android the `pb_mapper/keystore` channel that unlocks
    them; records cross without their invitations

- **`native/pb_mapper_ffi/`**: The Rust C ABI crate (a workspace member)
  - Every call returns a JSON envelope: `{"success": bool, "message": …, "data": …}`
    — except the calls that return a plain integer:
    - `pb_mapper_galaxy_frame_v1`, made once per frame, and
      `pb_mapper_ed25519_verify_v1`, the update manifest's signature check,
      both leaf calls over caller-owned buffers;
    - `pb_mapper_spawn_hidden_powershell_v1`, which starts the Windows
      update's hand-off so that it outlives the app (a detached Dart child
      cannot: PowerShell without a console exits at once);
    - `pb_mapper_desktop_view_v1`, the remote desktop's per-frame view leaf:
      it fills a caller-owned 56-byte `PbDesktopViewV1` (mirrored in
      `pb_mapper_ffi.dart`; `size_of` and `offset_of` asserts guard it, as
      they guard `PbDesktopVideoMetaV1` on both sides) and returns 1, 0
      without a viewer, or -1;
    - the desktop video exports and their JNI twins, below
  - `desktop.rs`: The remote desktop. `pb_mapper_desktop_call_json` takes
    the lifecycle ops (sharing, the helper and background host, trust,
    `inspect_invitation`, `connect`, `disconnect`) under `block_on`, so only
    a background isolate calls it. `pb_mapper_desktop_viewer_call_json`
    takes the viewer's closed set of hot ops (`viewer_status`, `control`,
    `release`, `input`, `keyframe`, `quality`, `pause`, `resume`,
    `display_mode`): it never enters the runtime, awaits or takes a tokio
    lock, so the UI isolate calls it, as it calls the view leaf. Errors are
    typed `ErrorCode`s (`error.rs`), never text to match. `connect` takes
    `decoders` (`{h264, h265}`, each `[w, h]` or null) and an optional
    `resolution` (absent: the display's own size), and dials with the app
    config's `lan_first` and `lan_transport`; `viewer_status{local}`
    answers `modes.fit_local`, the size Fit local would pick;
    `display_mode` takes `{target}`; `host_restore_display_mode` is the
    host card's Restore. `desktop.rs` dispatches; `desktop/viewer.rs` holds
    `connect`, the viewer's state and the two hot exports, `desktop/host.rs`
    this app's own host, `desktop/background.rs` the Windows service's host
    as the app sees it, and `desktop/trust.rs` the remembered host grant
  - `desktop_video.rs`: Video goes from the viewer's `VideoSink` straight to
    its consumer. On Android, and on Windows where Media Foundation is
    installed, that is the native decoder (the runner's decoder thread in
    `windows/runner/desktop_video.cpp`, the Android feeder), so H.264 and
    H.265 never pass through Dart; elsewhere, Windows without Media
    Foundation included, it is Dart's JPEG isolate. A unit's
    `PbDesktopVideoMetaV1.flags` say its codec (`FLAG_H265`, bit 4). The runner and this DLL delay-load
    `mfplat.dll`, so the app starts without it. `connect` returns a
    `video_source`; a consumer attaches to it and pulls with
    `pb_mapper_desktop_video_{attach,next,presented,keyframe,failed,report,detach}_v1`;
    `failed` reports a decoder failure with its text.
    Ids, never pointers, cross; no id is reused, so a stale one reads as
    closed. The return codes are one space (`PbVideo` in Dart). Only `next`
    blocks, never on a UI thread. `pb_mapper_desktop_instance_token_v1`
    proves a native consumer loaded the library Dart loaded. The
    `desktop-test-source` feature adds a clip player
    (`desktop_video/test_source.rs`) for device runs; no release enables it
  - `desktop_jni.rs` (Android): The same exports as the static natives of
    `DesktopVideoNative.kt`, through which `VideoFeeder.kt` feeds
    `MediaCodec`. A JNI failure returns a code, never a throw
  - `desktop_computers.rs`: Remembered computers, the `computers_*` ops of
    `pb_mapper_desktop_call_json`. One vault per profile: DPAPI on Windows;
    on Android AES-256-GCM under a data key that an Android Keystore key
    wraps, which Dart unwraps through the `pb_mapper/keystore` channel
    (`ComputerKeystore.kt`) to unlock the vault once per process.
    Invitations never return to Dart: a computer is remembered from the
    live, pinned session, and `connect` takes its `computer_id`. The vault
    is version 2: a resolution only when chosen (a version-1 `1080p`
    follows the native default), and `display_mode: "fit_local"`.
    `desktop/trust.rs` keeps the sharing side's remembered identity
  - `relay.rs`: `pb_mapper_relay_call_json`, one export for every question
    and administrator request to the relay, dispatched on the request's
    `"op"` and answered with the SDK's `Client`/`Admin`. A new relay
    operation is a new `op` here, not a new export
  - `state/`: tunnels run on the SDK's `Registration`/`Connection` handles,
    built with `p2p` — the same workers the CLI runs, direct first. A
    tunnel's status and its `path` (`PathInfo`: sessions on each path, the
    attempt in flight, how the last one ended) are read from its handle,
    not probed; `watch_tunnel` emits the change events. Dropping a handle
    stops its tunnel. `pb_mapper_register_service` and
    `pb_mapper_connect_service` take `allow_direct` and `port_mapping`
    (0 off, 1 auto: once a punch fails, the default, 2 always), saved with
    the tunnel. By concern: `stored.rs` is what is saved (`config.json`,
    `services.json`, `clients.json`, as the bytes on disk, keeping at each
    level the keys a later build wrote), `configuration.rs` loads and saves
    it (atomically through core's `durable_file` where the file can be
    replaced; in place, as 0.11.0 did, for a file with other names, and on
    unix another user's or one in a group a replacement would not be in, a
    directory that takes no new file and a file the rename may not replace;
    a tunnel's entry is written only when it changed; refusing a
    file this user may not write; a store that cannot be read or parsed is
    an error, never an empty list, and one that holds nothing, empty or
    zero-filled, is an empty list), `tunnel.rs` is a tunnel's setup, SDK
    handle and view (a share or connection whose list cannot be read is
    refused before it starts), `views.rs` the JSON the window reads
    (`PathInfo` among it), `runtime.rs` the embedded relay and committing
    and stopping tunnels (a connection whose changed entry cannot be saved
    is refused, as a share is; stopping the embedded relay stops only the
    tunnels dialling its port at this computer's addresses), and
    `status.rs` the window's reads and its questions to the relay
  - `tunnel_events.rs`: `pb_mapper_tunnel_events_call_json`, the window's
    readers of a running tunnel's events (`subscribe`/`next`/`close`), each
    event carrying the tunnel's diagnostics and metrics as they are by then;
    with `metrics_ms`, `next` also answers `{type:"metrics"}` samples alone
  - The relay carrier (TCP or QUIC, `relay-quic`), LAN-first and the LAN
    transport (QUIC, or TCP for what this computer dials) are app config,
    set by `pb_mapper_update_config` with the address and credential; a
    tunnel keeps what it started with and reports its carrier as
    `relayTransport`. A change of any answers with the tunnels it reaches
    (`move`, `lanFirst`, `lanTransport`), only those on the saved relay
    under the saved credential, and the window offers to restart exactly
    those; a remote-desktop connection takes the new value when it next
    connects. Only
    a change of the relay settings
    (`AppConfig::same_relay`) makes the relay client again or asks the relay
    again about QUIC; the embedded relay's own status is kept.
    `pb_mapper_start_server` takes a `quic_port` (0: none). The window's
    own relay questions, and the remote desktop's connects and host starts,
    go through `relay_client()`, cached per config,
    which shares the process's QUIC connection with the tunnels. See
    "How this computer reaches the relay" in `docs/ui-design.md`
  - `unwrap_used` and `expect_used` are `deny` here — see its `Cargo.toml`

- **`native/pb_mapper_galaxy/`**: The galaxy's per-frame geometry. Its
  arithmetic is mirrored step for step by `computeFrameInDart`
  (`lib/src/galaxy/galaxy_frame.dart`); change both together and keep the
  parity test in `test/galaxy_test.dart` passing

### Key Components

1. **Message Protocol** (`crates/pb-mapper-protocol/`):
   - **Command Protocol** (`command.rs`): Defines request/response types:
     - `PbConnStatusReq`/`PbConnStatusResp`: Status checking
     - `PbConnRequest`/`PbConnResponse`: Connection management
     - `PbServerRequest`: Server operation requests
     - `LocalServer`: Service type definitions (TCP/UDP)
     - `AdminRequest`/`AdminResponse`: Administrator operations
   - **Secure sessions** (`secure.rs`): Protocol-v2 first flight — the initial
     frame carries a clear-text routing prefix plus an authenticated encrypted
     request, adding no extra round trip, and later frames on the connection use
     directional keys with monotonic counters
   - **Forward Protocol** (`forward.rs`): Data forwarding mechanisms
   - Uses JSON serialization with custom framing (checksum + length header)
   - Supports encryption/decryption for secure communication via ring crate

2. **Connection Management**:
   - Central server maintains mappings between service keys and connection IDs
   - Handles registration, subscription, and stream forwarding
   - Implements keep-alive and timeout mechanisms
   - Uses actor model for concurrent connection handling

3. **Stream Abstractions**: `StreamProvider` and `ListenerProvider` give TCP and
   UDP one interface. These live in the external `uni-stream` crate, not in this
   repository.

4. **Authentication** (`crates/pb-mapper-auth/`): An administrator key plus
   derived temporary credentials, persisted through a write-ahead log and
   snapshots, with expiry driven by a hierarchical timing wheel. See
   `docs/authentication-v2.md`.

5. **Configuration System**:
   - Environment variables (see Environment Variables below)
   - Command-line argument parsing with clap
   - Workspace-based dependency management

## UI Module Implementation

The UI module provides a complete graphical interface that replaces all CLI
functionality. It is Flutter calling into Rust over raw `dart:ffi`.

> The project used the Rinf framework — signals, actors, generated bindings —
> and no longer does. If you find a document, comment or memory describing
> `DartSignal`, `RustSignal`, `Notifiable` or `PbMapperActor` in this repo,
> it is describing an architecture that was removed. See `ui/README.md` for
> why the FFI layer returns a JSON envelope instead.

### How the UI talks to Rust

1. A view holds a `PbMapperApiClient` — an interface, taken as a constructor
   parameter and defaulting to the real `PbMapperApi()`.
2. `PbMapperApi` calls `PbMapperService`, which dispatches the FFI call on a
   background isolate so the UI thread stays responsive.
3. `pb_mapper_ffi.dart` looks the symbol up in the loaded library and passes
   JSON across the boundary.
4. Rust logs are pushed back through a `NativeCallable` and surface as
   `PbMapperService.logStream`.

**When adding a call**, change all four in step: the Rust export, the Dart
symbol lookup, `PbMapperApiClient`, and `PbMapperApi`. The interface is what
makes a missing implementation a compile error rather than a runtime crash,
and it is what lets a widget test substitute `FakePbMapperApi`
(`ui/test/fake_pb_mapper_api.dart`) instead of loading the native library.

### What the UI does

Six pages, a first-run flow and the remote viewer (`docs/ui-design.md` has
the reasoning):

- **Overview**: the galaxy — this computer and the relay as two stars, each
  tunnel and each service on the relay as a planet, live tunnels as arcs:
  into the relay's star, or for a direct path past it to the service's
  planet; click a body to start, stop, edit or connect, and its panel draws
  the route and says why when it is the relay
- **Tunnels**: what this computer shares and what it connects to, each
  running one with a chip for its path (`widgets/path.dart`), and a count of
  the relay's other services that leads to Relay. `desktop:` services are
  not tunnels: Tunnels and the galaxy leave them out
- **Remote desktop** (the **Remote** tab on a phone): this computer as a
  host — turn remote control on, the masked invitation with Copy and a QR
  code on request, who is connected, view-only or control, Stop sharing,
  the helper — and the computers it views, remembered in Rust's encrypted
  vault and opened by id, added by pasting or scanning an invitation. The
  viewer replaces the whole window: on a desktop a dark title bar and a
  floating toolbar; on a phone landscape gutters, a modifier bar and a
  minimap. Its Display menu chooses each computer's resolution (by default
  the remote display's own size, up to 4K and up to this device's
  decoder) and maximum frame rate and the device's codec, and turns on the
  quality monitor (speed, frame rate, delay, bitrate, codec, resolution).
  On a desktop, while controlling a host that offers it, **Remote
  resolution** (远程分辨率) changes the remote display's mode: Fit local,
  Original, or one of its modes; the host puts it back at the session's
  end, on revoke, or from Restore on its card. The remote cursor
  is the viewer's own (`DesktopCursor`, kept by the store for the session).
  Back keeps the session paused for up to ten minutes
- **Relay**: whether the relay answers and how fast, and who this computer is
  to it (administrator, temporary credential #n, or neither). Services, with
  Connect and the connections the relay holds; connections and direct
  sessions. With the administrator key, everything the `admin` command can
  do: issue, reveal, renew, revoke and clean up credentials, retire
  connections, allow older clients, rotate the key, reset all credentials.
  The administrator also sees and connects to every credential's services:
  a connection can carry a `namespace` (FFI `pb_mapper_connect_service`,
  `ClientConfigData.namespace`), and services are told apart by namespace and
  name (`RemoteService` in `lib/src/state/remote_service.dart`)
  Over QUIC, the link's round trip, loss and streams, measured locally.
  Also the relay this computer can run itself, with an optional QUIC port,
  and its key
- **Logs**: the Rust log, by minimum level and word, following new lines;
  on a phone it opens from Settings
- **Settings**: relay address, credential, the carrier (TCP or QUIC) with
  what the relay offers, default keep-alive, a copyable setup string for
  another computer; under Direct connections, LAN first and the LAN
  transport (QUIC by default, or TCP); theme and language
- **First run**: relay, then share-or-connect, then that one form

Sharing and connecting are one editor with a Share/Connect switch; its More
options hold the direct switch, off being `--relay-only`. The rail's
relay status and the galaxy's relay star both open Relay. An administrator
section is shown only when the relay confirms the administrator key — the
credential's own shape decides nothing. While someone views this computer,
Disconnect viewer and Stop sharing are in the title bar, above any dialog,
and in the tray.
## Key Features

### Core Networking
- **Protocol Support**: Full TCP and UDP support for local services
- **Security**: Optional encryption using ring crate for secure communication
- **Connection Stability**: Keep-alive and timeout handling for reliable connections
- **NAT Traversal**: Expose services behind firewalls and NAT devices

### Service Management
- **Service Registration**: Unique key-based service identification system
- **Dynamic Discovery**: Real-time service registration and discovery
- **Status Monitoring**: Comprehensive status checking and health monitoring
- **Multi-Protocol**: Unified interface for both TCP and UDP services

### Deployment & Operations
- **Docker Support**: Complete containerization with docker-compose setup
- **Systemd Integration**: Service files for Linux daemon deployment
- **Build System**: Makefile with multi-target builds (x86_64, musl)
- **Cross-Platform**: Support for Linux, macOS, Windows, Android, iOS

### User Interface
- **Flutter GUI**: Modern, responsive cross-platform interface
- **FFI Integration**: Direct `dart:ffi` calls into the `pb-mapper-ffi` crate
- **Real-time Updates**: Live status monitoring and log streaming
- **Configuration Management**: Persistent settings and environment variable management
- **Multi-platform**: Desktop and mobile. There is no web/wasm target — the UI
  loads a native library over `dart:ffi`, which the web cannot do.

## Development Notes

### Project Structure & Dependencies
- **Workspace Configuration**: Virtual manifest at the root; shared versions live
  in `[workspace.dependencies]`
- **Memory Optimization**: Uses mimalloc-rust for improved memory allocation performance
- **Error Handling**: snafu in core, protocol, server and client, each owning its
  error types and wrapping the layer below as a `source`; auth, the desktop and
  the FFI keep hand-written types
- **Async Runtime**: Built on Tokio with full async/await support
- **Serialization**: serde and serde_json for message serialization
- **Networking**: uni-stream for the stream/listener abstractions, hickory-resolver
  for DNS (custom resolvers on the async path only — the sync path uses `std`,
  since hickory has no blocking resolver)
- **Cryptography**: ring crate for encryption/decryption functionality

### Code Quality & Standards
- **Linting**: `unwrap_used` and `expect_used` are denied for the whole workspace
  via `[workspace.lints]`; `clippy.toml` exempts test code, and each `tests/` or
  `examples/` target that unwraps carries a file-level allow. A production
  `unwrap` needs a reason recorded at the site.
- **Formatting**: rustfmt.toml configuration for consistent code style
- **Toolchain**: rust-toolchain.toml for reproducible builds
- **Testing**: Unit tests live beside the code; integration tests are in
  `crates/pb-mapper-cli/tests/`, which is the crate that depends on every layer
  they exercise. A few crates keep their own: the CLI does not depend on
  `pb-mapper-desktop`, so the remote desktop's end to end test is that crate's
  `tests/session.rs`; and `pb-mapper-core`'s and `pb-mapper-auth`'s
  `tests/process_environment.rs` are single-threaded, harness-free binaries,
  because environment writes must not share a process with reader threads.
  The e2e scaffolding is `pb-mapper-testkit`, so each case stands up its own
  full `server` + `register` + `connect` flow; a new case is a module of
  `tests/e2e/` unless it changes state the whole process shares, which takes a
  binary of its own.

### UI Development Guidelines
- **Framework**: Flutter 3.44.9, Material 3. CI pins the same version.
- **CI gates**: From `ui/`, CI runs `flutter gen-l10n` and fails unless
  `l10n_untranslated.json` is `{}` (it lists each key `app_zh.arb` lacks), then
  `dart format --output=none --set-exit-if-changed lib test tool`, `flutter
  analyze --no-fatal-infos`, the `Icons.*` grep and `flutter test`. Run `dart
  format lib test tool` before committing Dart.
- **State Management**: Plain `StatefulWidget` state; no state-management package
- **Architecture**: `AppShell` owns navigation; every page reads one `AppStore`,
  which takes its `PbMapperApiClient` as a parameter so tests can pass a fake.
- **Real-time Updates**: The Rust change stream plus a slow relay check, both in
  `AppStore`; `waitUntilSettled` waits out a tunnel's retry loop.
- **Design**: POCOX's language (`lib/src/theme/`); see `docs/ui-design.md`.
  Render the pages headlessly with `test/preview_test.dart` to look at a change;
  the README posters are derived from the pages by `test/gen_poster_test.dart`.
- **Error Handling**: Graceful error handling with user-friendly feedback
- **Responsive Design**: Adaptive layouts for different screen sizes

### Environment Variables

The commonly used ones:

- **`PB_MAPPER_SERVER`**: Default remote server address for CLI tools
- **`PB_MAPPER_KEEP_ALIVE`**: TCP keep-alive ("ON", "1", "true", "yes" to enable).
  Read on every call, not cached — the UI's per-service toggle depends on that.
- **`MSG_HEADER_KEY`**: The process credential, administrator or temporary.
  Required; there is no insecure default.
- **`RUST_LOG`**: Tracing level configuration (supports env-filter)
- **`PB_MAPPER_LOG_FORMAT`**: Log output format

Timeouts, intervals, and pool sizes are also configurable, and there are more
than a dozen: the authoritative list is the `pub const PB_MAPPER_*` declarations
at the top of `crates/pb-mapper-core/src/config.rs`, each read by the accessor
named after it. Beyond those, the relay's routing limits
(`PB_MAPPER_MAX_*`, `PB_MAPPER_NEW_STREAMS_BURST`) are read by name in
`crates/pb-mapper-server/src/routing.rs`, and the authentication settings
(`PB_MAPPER_AUTH_STATE_DIR`, `PB_MAPPER_AUTH_MAX_TEMP_KEYS`,
`PB_MAPPER_AUTH_MAX_TEMP_TTL_SECS`, `PB_MAPPER_LEGACY_PROTOCOL`) in
`crates/pb-mapper-auth/src/config.rs`; each of those four is also a `server`
flag, which overrides it without writing the environment.

## Development Workflow

### Building the Project

```bash
# Build the unified CLI
make build-pb-mapper

# Build with musl for static linking
make build-pb-mapper-x86_64_musl

# Build and run Flutter UI
cd ui && flutter run
```

### Docker Deployment

```bash
# Build and release Docker images
make release-pb-mapper-docker-image
make release-pb-mapper-x86-64-musl-docker-image

# Run with docker-compose
docker-compose -f docker/docker-compose.yml up
```

### Testing

- **Unit Tests**: `cargo test` for Rust components
- **Widget Tests**: `flutter test` in ui/ directory
- **Integration Tests**: `crates/pb-mapper-cli/tests/` — end-to-end tests built
  on `pb-mapper-testkit`
- **Remote desktop (Rust)**: `cargo test -p pb-mapper-desktop -- --ignored
  --nocapture slow_link` runs the ignored acceptance cases: a synthetic encoder
  over a real relay and TLS on a shaped link. CI does not run them. The host's
  capture, encoder and helper compile only on Windows, where CI's
  `rust-windows-desktop` job lints and tests them
- **Android unit tests**: `./gradlew :app:testDebugUnitTest` from `ui/android`,
  JVM tests of `VideoFeeder`, the decoder rules, the presentation queue and
  `ComputerKeystore`. `gradlew` is git-ignored; `flutter build apk
  --config-only` in `ui/` writes it. CI's `android-check` job runs them
- **Device acceptance**: `scripts/test/desktop-ui-acceptance.md` (the viewer and
  its native decoder on devices, against a test host) and
  `scripts/test/windows-helper-acceptance.md` (the helper, UAC and Winlogon on an
  isolated Windows VM) — manual procedures. `cargo run -p pb-mapper-desktop
  --example display_mode_probe` changes the display it runs on and puts it
  back: run it only where that is allowed
- **Examples**: the SDK's in `crates/pb-mapper/examples/`, the remote
  desktop's probes and benchmarks in `crates/pb-mapper-desktop/examples/`,
  the protocol's and the auth store's benchmarks in
  `crates/pb-mapper-protocol/examples/` and `crates/pb-mapper-auth/examples/`,
  and the compatibility relay and the relay's benchmarks in
  `crates/pb-mapper-cli/examples/`

### Service Deployment

- **Systemd Services**: `services/` contains `.service` files for daemon deployment
- **Build Scripts**: `scripts/` contains automated build and release scripts
- **Cross-platform**: Support for Linux, macOS, Windows, Android, iOS, and web
