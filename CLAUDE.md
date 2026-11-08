# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

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
│   │                       # addr, codec, timeout, durable_file, DataLenType
│   ├── pb-mapper-auth/     # Credential lifecycle, persistence, timing wheel
│   ├── pb-mapper-protocol/ # Message framing, v2 secure sessions, forwarding
│   ├── pb-mapper-server/   # Central relay server, plus the task manager
│   ├── pb-mapper-client/   # Both tunnel ends: `register` and `connect`
│   ├── pb-mapper-desktop/  # Remote desktop: host, viewer, receipt credit, QoS,
│   │                       # Windows capture and the UAC/Winlogon helper
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
codec or input code. On Windows, `pb-mapper-desktop` also uses `pb-mapper-core`.

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
  - `checksum.rs`: The process credential, and the framing checksum over `datalen`
  - `config.rs`: Environment configuration and address resolution entry points
  - `conn_id.rs`: Connection ID types
  - `error.rs`: The shared error type, plus the `snafu_error_*` macros
  - `addr.rs`: Address resolution; custom DNS servers on the async path
  - `codec.rs`: AES-256-GCM encrypt/decrypt
  - `timeout.rs`: `RetryBackoff`
  - `durable_file.rs`: Atomic replace and parent-directory fsync
  - `test_support.rs`: `PROCESS_CREDENTIAL_TEST_LOCK`, shared across crates' tests
  - `lib.rs`: `DataLenType`, which lives here so `checksum` and `error` can name it

- **`pb-mapper-auth/`**: The credential subsystem, and the largest one
  - `lib.rs`: `AuthRuntime`, `AuthContext`, `AuthFailure`, `KeyId`
  - `runtime.rs`: Key derivation and authentication of a presented key
  - `actor/`: The lifecycle actor — `epoch.rs` for root rotation
  - `persistence/`: `snapshot.rs`, `wal.rs`, `blob.rs`, `admin_key.rs`, `fs.rs`,
    `namespaces.rs`
  - `timing_wheel.rs`: Hierarchical wheel driving credential expiry
  - `leases.rs`, `keys.rs`, `ids.rs`, `config.rs`: Leases, key material, platform dirs

- **`pb-mapper-protocol/`**: Framing and the authenticated session
  - `lib.rs`: The checksum + length framing, and the reader/writer traits
  - `command.rs`: Request/response types (`PbConnRequest`, `LocalServer`, `AdminRequest`, …)
  - `secure.rs`: Protocol-v2 single-flight sessions, client and server
  - `secure/`: `frame.rs`, `first_flight.rs`, `replay.rs`, `limiter.rs`
  - `forward.rs`: Stream and datagram forwarding
  - `buffer.rs`: Read buffers for the framing

- **`pb-mapper-server/`**: The central relay
  - `lib.rs`: `ManagerTask` / `ConnTask`, and the routing domain model
  - `runtime.rs`: Serialises the global routing maps and quotas (the largest file)
  - `connection.rs`: Per-socket authentication and dispatch
  - `server.rs`, `client.rs`: The service-side and subscriber-side loops
  - `admin.rs`: Administrator request handling
  - `status.rs`, `error.rs`, `manager.rs`: Status replies, errors, the task manager

- **`pb-mapper-client/`**: Both ends of a tunnel
  - `server/`: `register` — publishes a local service (`mod.rs`, `stream.rs`, `error.rs`)
  - `client/`: `connect` — subscribes and listens locally, plus `status.rs`
  - `direct/` (feature `p2p`): direct sessions. `lan.rs` ranks interfaces
    (netdev, read in the background) and judges from a peer's candidates
    whether it shares a LAN; `candidates.rs` orders what is offered, LAN
    first; `socket.rs` and `endpoint.rs` probe and dial each path as it
    answers; `provider.rs` is the publishing end. LAN-first (on by default,
    `lan_first` in the options, carried as the crate-private `DirectPolicy`)
    only orders probes and holds other paths back while a LAN is known to
    be there (`Assessment::holds`); off, the LAN is still detected and
    reported. A committed session's `DirectLink` lives in
    `diagnostics/link.rs`
  - `diagnostics.rs`, `diagnostics/metrics.rs`: a tunnel's diagnostics, its
    event stream and its `TunnelMetrics` (sessions and bytes per path, the
    relay round trip, the live link), all read from one state; each session
    holds a `SessionGuard` whose `ByteMeter` the forwarding fills

- **`pb-mapper-desktop/`**: Endpoint-owned remote desktop, reached as
  `pb_mapper::desktop`; how it works is in `docs/remote-desktop.md`
  - `host.rs`, `session.rs`, `shared.rs`: The host, and one session's network
    side, B-recv and B-send over TLS; `shared.rs` states the lock discipline
  - `worker.rs`, `pacer.rs`, `scheduler.rs`, `lane.rs`: Thread C, the one
    desktop-bound thread (capture, encode, input), its pacing, its one wait,
    and the input lane from B-recv
  - `timer.rs`: Fine Windows timers for the whole process (power-throttling
    opt-out and `timeBeginPeriod`), held while a session captures
  - `credit.rs`, `qos.rs`, `idr.rs`, `outgoing.rs`: Receipt credit, delay-based
    QoS, on-demand IDRs, and the hand-off from thread C to B-send
  - `protocol.rs`, `wire.rs`, `invite.rs`, `security.rs`, `auth.rs`: Protocol v2
    (ALPN `pb-mapper/desktop/2`), invitations, certificate pinning, and the
    input authorisation ledger
  - `viewer/`, `video_sink.rs`: The viewer, and its one hand-off to a native
    consumer, which is also where a presentation is validated
  - `platform.rs`, `windows.rs`: Capabilities, displays, and the Windows
    backend (xcap displays, enigo input, the desktop binding)
  - `video/windows.rs`: Desktop Duplication → D3D11 → Media Foundation H.265
    or H.264. H.265 only for a viewer that lists the `h265` capability, on a
    GPU whose hardware HEVC encoder passes a probe kept per adapter; one
    encoder failure turns it off for the process
  - `helper/` (Windows): The opt-in UAC/Winlogon service and the UI's client to
    it; the service side and the `pb-mapper-desktop-helper` binary need
    `--features windows-helper`. `HELPER_PROTOCOL` (`helper/mod.rs`) and the
    `.v<n>` pipe names change together; an installed helper of another
    protocol, or of another build (its executable differs from the one
    bundled with the app), is offered Update
  - `dpapi.rs` (Windows): The one DPAPI call every Windows store uses, current
    user or machine scope; the FFI's vault and trust files go through it
  - `tests.rs`, `tests/session.rs`: Tests over a real relay and TLS, the second
    end to end; `slow_link.rs`: the ignored acceptance cases
  - `examples/`: Capture, transport and viewer probes, and `video_bench`, the
    Windows capture and encode benchmark

- **`pb-mapper-testkit/`**: Test support only; nothing shipped depends on it
  - `relay.rs`: `Relay` — a live server that retains its `AuthRuntime`, so a case
    can issue, renew, and revoke credentials without the admin wire protocol
  - `tunnel.rs`: `TunnelSpec` / `Tunnel` / `TunnelHarness` — echo server plus
    `register` plus `connect`, each on reserved loopback ports
  - `echo.rs`, `traffic.rs`: Echo servers and the framed and raw traffic drivers
  - `shaped.rs`: `Shape` / `shaped_link` — a TCP hop with delay and rate
    adjustable while connections run, shared by the remote desktop's tests
  - A crate rather than `tests/common/mod.rs`: that module is compiled separately
    into every test binary, and whatever a binary does not use is reported as
    dead code — fatal under `-D warnings`

- **`pb-mapper-cli/`**: The binary, integration tests, and examples
  - `src/bin/pb-mapper.rs`: Argument parsing and the role commands
  - `src/bin/pb-mapper/admin.rs`: The `admin` subcommand
  - `tests/test_delay.rs`: The transport/codec matrix over the whole tunnel
  - `tests/temporary_credential_e2e.rs`: The credential lifecycle over the whole
    tunnel — namespace isolation, renew, expiry, revoke
  - `tests/key_namespaces.rs`: Several keys in one allocated namespace, over TCP,
    QUIC and direct setup
  - `tests/regression.rs`: Protocol-level cases against hand-rolled frames

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
    network first* (saved at once, outside the relay form), theme, language;
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
    `release`, `input`, `keyframe`, `quality`, `pause`, `resume`): it never
    enters the runtime, awaits or takes a tokio lock, so the UI isolate
    calls it, as it calls the view leaf. Errors are typed `ErrorCode`s
    (`error.rs`), never text to match
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
    `desktop-test-source` feature adds a clip player for device runs; no
    release enables it
  - `desktop_jni.rs` (Android): The same exports as the static natives of
    `DesktopVideoNative.kt`, through which `VideoFeeder.kt` feeds
    `MediaCodec`. A JNI failure returns a code, never a throw
  - `desktop_computers.rs`: Remembered computers, the `computers_*` ops of
    `pb_mapper_desktop_call_json`. One vault per profile: DPAPI on Windows;
    on Android AES-256-GCM under a data key that an Android Keystore key
    wraps, which Dart unwraps through the `pb_mapper/keystore` channel
    (`ComputerKeystore.kt`) to unlock the vault once per process.
    Invitations never return to Dart: a computer is remembered from the
    live, pinned session, and `connect` takes its `computer_id`.
    `desktop_trust.rs` keeps the sharing side's remembered identity
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
    the tunnel
  - `tunnel_events.rs`: `pb_mapper_tunnel_events_call_json`, the window's
    readers of a running tunnel's events (`subscribe`/`next`/`close`), each
    event carrying the tunnel's diagnostics and metrics as they are by then;
    with `metrics_ms`, `next` also answers `{type:"metrics"}` samples alone
  - The relay carrier (TCP or QUIC, `relay-quic`) and LAN-first are app
    config, set by `pb_mapper_update_config` with the address and
    credential; a tunnel keeps what it started with and reports its carrier
    as `relayTransport`. A change of either answers with the tunnels it
    reaches (`move`, `lanFirst`), only those on the saved relay under the
    saved credential, and the window offers to restart exactly those. Only
    a change of the relay settings
    (`AppConfig::same_relay`) resets the relay caches or asks the relay again.
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
  minimap. Its Display menu chooses each computer's resolution and maximum
  frame rate and the device's codec, and turns on the quality monitor
  (speed, frame rate, delay, bitrate, codec, resolution). The remote cursor
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
  another computer, theme and language
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
- **Workspace Configuration**: Virtual manifest at the root; versions are pinned
  once in `[workspace.dependencies]` and crates take them with `.workspace = true`
- **Memory Optimization**: Uses mimalloc-rust for improved memory allocation performance
- **Error Handling**: snafu, with each crate owning its own error type and wrapping
  the layer below as a `source` rather than sharing one workspace-wide enum
- **Async Runtime**: Built on Tokio with full async/await support
- **Serialization**: serde and serde_json for message serialization
- **Networking**: uni-stream for the stream/listener abstractions, hickory-resolver
  for DNS (custom resolvers on the async path only — the sync path uses `std`,
  since hickory has no blocking resolver)
- **Cryptography**: ring crate for encryption/decryption functionality

### Code Quality & Standards
- **Linting**: `unwrap_used` and `expect_used` are denied for the whole workspace
  via `[workspace.lints]`; `clippy.toml` exempts test code, and `tests/` and
  `examples/` targets carry a file-level allow. A production `unwrap` needs a
  reason recorded at the site.
- **Formatting**: rustfmt.toml configuration for consistent code style
- **Toolchain**: rust-toolchain.toml for reproducible builds
- **Testing**: Unit tests live beside the code; integration tests are in
  `crates/pb-mapper-cli/tests/`, which is the crate that depends on every layer
  they exercise. Two crates keep their own: the CLI does not depend on
  `pb-mapper-desktop`, so the remote desktop's end to end test is that crate's
  `tests/session.rs`; and `pb-mapper-core`'s `tests/process_environment.rs` is a
  single-threaded, harness-free binary, because environment writes must not
  share a process with reader threads. The e2e scaffolding is
  `pb-mapper-testkit`, so a new test file stands up its own full `server` +
  `register` + `connect` flow instead of everything accumulating in one file.

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
named after it. Beyond those, `PB_MAPPER_AUTH_STATE_DIR`,
`PB_MAPPER_LEGACY_PROTOCOL`, and `PB_MAPPER_NEW_STREAMS_PER_SECOND` are read by
name where they are used; the first two are also settable as `server` flags.

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
  isolated Windows VM) — manual procedures
- **Examples**: `examples/` directory provides working usage examples

### Service Deployment

- **Systemd Services**: `services/` contains `.service` files for daemon deployment
- **Build Scripts**: `scripts/` contains automated build and release scripts
- **Cross-platform**: Support for Linux, macOS, Windows, Android, iOS, and web
