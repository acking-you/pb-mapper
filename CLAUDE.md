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
│   ├── pb-mapper-testkit/  # Test support: a complete e2e tunnel, for any test file
│   └── pb-mapper-cli/      # The `pb-mapper` binary, integration tests, examples
├── ui/                    # Flutter UI, talking to Rust over dart:ffi
│   ├── lib/               # Flutter application code
│   │   ├── l10n/          # ARB sources and generated AppLocalizations
│   │   └── src/ffi/       # The Dart side of the FFI boundary
│   ├── native/pb_mapper_ffi/  # C ABI crate (a workspace member)
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

Note that the binary is still named `pb-mapper`, discovered from
`src/bin/pb-mapper.rs` inside `pb-mapper-cli`. The release workflows, both
Dockerfiles, and the install scripts hardcode that name, and `cargo build --bin
pb-mapper` resolves it from the workspace root regardless of the crate name.
Likewise `pb-mapper-ffi` keeps its package name, because it determines the
`libpb_mapper_ffi.{so,dylib,a}` / `pb_mapper_ffi.dll` filenames that the Dart
loader, two CMakeLists, four xcconfigs, and the release-ui hash checks expect.

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
  - `persistence/`: `snapshot.rs`, `wal.rs`, `blob.rs`, `admin_key.rs`, `fs.rs`
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

- **`pb-mapper-testkit/`**: Test support only; nothing shipped depends on it
  - `relay.rs`: `Relay` — a live server that retains its `AuthRuntime`, so a case
    can issue, renew, and revoke credentials without the admin wire protocol
  - `tunnel.rs`: `TunnelSpec` / `Tunnel` / `TunnelHarness` — echo server plus
    `register` plus `connect`, each on reserved loopback ports
  - `echo.rs`, `traffic.rs`: Echo servers and the framed and raw traffic drivers
  - A crate rather than `tests/common/mod.rs`: that module is compiled separately
    into every test binary, and whatever a binary does not use is reported as
    dead code — fatal under `-D warnings`

- **`pb-mapper-cli/`**: The binary, integration tests, and examples
  - `src/bin/pb-mapper.rs`: Argument parsing and the role commands
  - `src/bin/pb-mapper/admin.rs`: The `admin` subcommand
  - `tests/test_delay.rs`: The transport/codec matrix over the whole tunnel
  - `tests/temporary_credential_e2e.rs`: The credential lifecycle over the whole
    tunnel — namespace isolation, renew, expiry, revoke
  - `tests/regression.rs`: Protocol-level cases against hand-rolled frames

#### Flutter UI (`ui/`)
- **`lib/src/views/`**: One file per zone the shell can show
  - `main_landing_view.dart`: Home — pick a role, or head into ops
  - `setup_wizard_view.dart`: First-run guided setup
  - `service_registration_view.dart`: The register workspace (form / list / logs)
  - `client_connection_view.dart`: The connect workspace (form / list / logs)
  - `status_monitoring_view.dart`: Status dashboard
  - `configuration_view.dart`: Settings
  - `log_view_page.dart`: The log stream, shown inside both workspaces

- **`lib/src/common/`**: Shell and shared utilities
  - `app_section.dart` / `workspace_pane.dart`: Where the user is
  - `app_destination.dart`: The zone's destinations, described once and drawn
    by both the side rail and the bottom bar
  - `desktop_layout.dart`: The shell, and the animated move between the rail
    and the bottom bar
  - `responsive_layout.dart`: Breakpoints, and `usesBottomNav`
  - `nav_transitions.dart`: The rail/bar transition
  - `polling.dart`: `pollUntilSettled` and `firstWhereOrNull`
  - `log_manager.dart`: Log collection

- **`lib/src/ffi/`**: The Dart side of the boundary
  - `pb_mapper_ffi.dart`: Raw `dart:ffi` symbol lookups and library loading
  - `pb_mapper_service.dart`: FFI dispatch on a background isolate
  - `pb_mapper_api.dart`: `PbMapperApiClient`, the interface the views take,
    and `PbMapperApi`, the real implementation over the FFI

- **`native/pb_mapper_ffi/`**: The Rust C ABI crate (a workspace member)
  - Every call returns a JSON envelope: `{"success": bool, "message": …, "data": …}`
  - `unwrap_used` and `expect_used` are `deny` here — see its `Cargo.toml`

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

### Current UI Implementation Status

Every view under `ui/lib/src/views/`:

- **Main App** (`ui/lib/main.dart`): Entry point with navigation and theme management
- **Landing Page** (`main_landing_view.dart`): Central navigation hub
- **Setup Wizard** (`setup_wizard_view.dart`): First-run guided setup
- **Service Registration** (`service_registration_view.dart`): The register workspace
- **Registered Services** (`registered_services_view.dart`): What this process has registered
- **Client Connection** (`client_connection_view.dart`): The connect workspace
- **Status Monitoring** (`status_monitoring_view.dart`): Real-time status dashboard
- **Configuration** (`configuration_view.dart`): Environment and settings management
- **Logging** (`log_view_page.dart`, `src/common/log_manager.dart` (under `ui/lib/`)): The log stream

There is no separate server-management view: starting and stopping the relay is
part of the landing page and the setup wizard.

### UI Features Implemented

#### 1. Server Management Interface
- **Start/Stop Server**: Direct server process control
- **Port Configuration**: Configurable server port (default: 7666)
- **IPv6 Support**: Toggle between IPv4/IPv6 listening
- **Keep-Alive Control**: TCP keep-alive configuration
- **Real-time Status**: Live server status monitoring
- **Log Display**: Integrated log viewer with filtering

#### 2. Service Registration Interface (Server CLI Replacement)
- **TCP Service Registration**:
  - Service key input and validation
  - Local service address configuration (ip:port)
  - Encryption codec toggle
  - Remote server address management
  - Keep-alive settings
- **UDP Service Registration**: Same features as TCP with UDP-specific handling
- **Active Service Management**: View and manage currently registered services

#### 3. Client Connection Interface (Client CLI Replacement)
- **TCP Client Connections**:
  - Service key selection from available services
  - Local listening address configuration
  - Remote server connection management
  - Connection status monitoring
- **UDP Client Connections**: UDP-specific client interface
- **Connection History**: Track and manage previous connections

#### 4. Status Monitoring Interface
- **Server Status Dashboard**:
  - Active remote connection IDs display
  - Registered service keys listing
  - Server mapping information visualization
  - Real-time connection statistics
- **Service Health Monitoring**: Health checks and status indicators
- **Performance Metrics**: Connection latency and throughput monitoring

#### 5. Configuration Management
- **Environment Variables**:
  - `PB_MAPPER_SERVER`: Remote server address configuration
  - `PB_MAPPER_KEEP_ALIVE`: Global keep-alive setting
- **Application Settings**: UI preferences and configuration persistence
- **Profile Management**: Save and load configuration profiles

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
  they exercise. The e2e scaffolding is `pb-mapper-testkit`, so a new test file
  stands up its own full `server` + `register` + `connect` flow instead of
  everything accumulating in one file.

### UI Development Guidelines
- **Framework**: Flutter 3.44.9, Material 3. CI pins the same version.
- **State Management**: Plain `StatefulWidget` state; no state-management package
- **Architecture**: A shell (`desktop_layout.dart`) that owns navigation, and
  one view per zone. Views take their API as a parameter so they can be tested.
- **Real-time Updates**: Polled through `pollUntilSettled`, plus the log stream
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
- **Examples**: `examples/` directory provides working usage examples

### Service Deployment

- **Systemd Services**: `services/` contains `.service` files for daemon deployment
- **Build Scripts**: `scripts/` contains automated build and release scripts
- **Cross-platform**: Support for Linux, macOS, Windows, Android, iOS, and web
