# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

This is a Rust-based network tunneling/proxy system called `pb-mapper` that allows exposing local services to clients over a public network. The project enables users to access their home services (like file transfer servers) from anywhere by creating secure tunnels through a public server.

The system uses one **pb-mapper** binary (`src/bin/pb-mapper.rs`) with explicit role commands:

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

5. **UI Module** (`ui/`): Flutter graphical interface
   - Replaces all CLI functionality with a user-friendly GUI
   - Calls into Rust through raw `dart:ffi` against the `pb-mapper-ffi` crate
   - Provides comprehensive service management interface

The system works by creating a bridge between local services and remote clients through a public server, enabling access to services behind NAT/firewalls.

## Code Architecture

### Project Structure

```
pb-mapper/
├── src/                    # Main Rust codebase
│   ├── bin/               # Unified pb-mapper CLI entry point
│   ├── pb_server/         # Central server implementation
│   ├── local/             # Local service handlers (server/client)
│   ├── common/            # Shared utilities and protocols
│   └── utils/             # Helper functions
├── ui/                    # Flutter UI, talking to Rust over dart:ffi
│   ├── lib/               # Flutter application code
│   │   ├── l10n/          # ARB sources and generated AppLocalizations
│   │   └── src/ffi/       # The Dart side of the FFI boundary
│   ├── native/pb_mapper_ffi/  # C ABI crate (a workspace member)
│   └── test/              # Widget tests
├── examples/              # Example implementations
├── tests/                 # Integration tests
├── docker/                # Docker deployment configuration
└── services/              # Systemd service files
```

### Core Modules

#### Rust Backend (`src/`)
- **`src/pb_server/`**: Central server implementation
  - `server.rs`: Main server logic with connection management
  - `client.rs`: Client connection handling
  - `status.rs`: Server status reporting
  - `mod.rs`: Server manager with ManagerTask and ConnTask enums

- **`src/local/server/`**: Local service registration (`register` functionality)
  - `stream.rs`: Stream handling for service registration
  - `mod.rs`: Registration logic and server-side CLI implementation
  - `error.rs`: Server-specific error handling

- **`src/local/client/`**: Client connection handling (`connect` functionality)
  - `stream.rs`: Stream management for client connections
  - `status.rs`: Status checking and reporting
  - `mod.rs`: Client-side CLI implementation
  - `error.rs`: Client-specific error handling

- **`src/common/`**: Shared utilities and protocols
  - `message/`: Protocol definitions (command.rs, forward.rs)
  - `config.rs`: Configuration management and environment variables
  - `stream.rs`: Stream abstractions (TcpStreamProvider, UdpStreamProvider)
  - `listener.rs`: Listener abstractions (TcpListenerProvider, UdpListenerProvider)
  - `manager.rs`: Connection management utilities
  - `buffer.rs`: Buffer management for data streaming
  - `checksum.rs`: Data integrity verification
  - `conn_id.rs`: Connection ID management
  - `error.rs`: Common error definitions

- **`src/utils/`**: Helper functions
  - `addr.rs`: Address resolution with OneOrMore enum for multiple addresses
  - `codec.rs`: Encryption/decryption utilities
  - `timeout.rs`: Timeout handling mechanisms
  - `udp.rs`: UDP-specific utilities

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

1. **Message Protocol** (`src/common/message/`): 
   - **Command Protocol** (`command.rs`): Defines request/response types:
     - `PbConnStatusReq`/`PbConnStatusResp`: Status checking
     - `PbConnRequest`/`PbConnResponse`: Connection management
     - `PbServerRequest`: Server operation requests
     - `LocalService`: Service type definitions (TCP/UDP)
   - **Forward Protocol** (`forward.rs`): Data forwarding mechanisms
   - Uses JSON serialization with custom framing (checksum + length header)
   - Supports encryption/decryption for secure communication via ring crate

2. **Connection Management**:
   - Central server maintains mappings between service keys and connection IDs
   - Handles registration, subscription, and stream forwarding
   - Implements keep-alive and timeout mechanisms
   - Uses actor model for concurrent connection handling

3. **Stream Abstractions**:
   - `StreamProvider` trait for TCP/UDP stream handling
   - `ListenerProvider` trait for TCP/UDP listener management
   - Unified interface for different transport protocols

4. **Configuration System**:
   - Environment variable support:
     - `PB_MAPPER_SERVER`: Remote server address
     - `PB_MAPPER_KEEP_ALIVE`: TCP keep-alive setting
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

The UI is fully implemented with the following structure:

- **Main App** (`ui/lib/main.dart`): Entry point with navigation and theme management
- **Landing Page** (`main_landing_view.dart`): Central navigation hub
- **Server Management** (`server_management_page.dart`, `server_management_view.dart`): Complete server control
- **Service Registration** (`service_registration_page.dart`, `service_registration_view.dart`): Service registration interface
- **Client Connection** (`client_connection_page.dart`, `client_connection_view.dart`): Client connection management
- **Status Monitoring** (`status_monitoring_view.dart`): Real-time status dashboard
- **Configuration** (`configuration_view.dart`): Environment and settings management
- **Logging** (`log_display_widget.dart`, `log_manager.dart`): Comprehensive log viewing

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
- **Multi-platform**: Desktop, mobile, and web support

## Development Notes

### Project Structure & Dependencies
- **Workspace Configuration**: Multi-crate workspace with shared dependencies in root `Cargo.toml`
- **Memory Optimization**: Uses mimalloc-rust for improved memory allocation performance
- **Error Handling**: Comprehensive error handling with snafu crate across all modules
- **Async Runtime**: Built on Tokio with full async/await support
- **Serialization**: serde and serde_json for message serialization
- **Networking**: socket2 for low-level socket operations, trust-dns-resolver for DNS
- **Cryptography**: ring crate for encryption/decryption functionality

### Code Quality & Standards
- **Linting**: Strict clippy rules in UI native hub (deny unwrap_used, expect_used, wildcard_imports)
- **Formatting**: rustfmt.toml configuration for consistent code style
- **Toolchain**: rust-toolchain.toml for reproducible builds
- **Testing**: Comprehensive test suite in `tests/` directory

### Build Profiles
- **wasm-dev**: Optimized for WebAssembly builds
- **server-dev**: Development profile for server components
- **android-dev**: Android-specific build optimizations

### UI Development Guidelines
- **Framework**: Flutter 3.44.9, Material 3. CI pins the same version.
- **State Management**: Plain `StatefulWidget` state; no state-management package
- **Architecture**: A shell (`desktop_layout.dart`) that owns navigation, and
  one view per zone. Views take their API as a parameter so they can be tested.
- **Real-time Updates**: Polled through `pollUntilSettled`, plus the log stream
- **Error Handling**: Graceful error handling with user-friendly feedback
- **Responsive Design**: Adaptive layouts for different screen sizes

### Environment Variables
- **`PB_MAPPER_SERVER`**: Default remote server address for CLI tools
- **`PB_MAPPER_KEEP_ALIVE`**: Global TCP keep-alive setting ("ON" to enable)
- **`RUST_LOG`**: Tracing level configuration (supports env-filter)

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
- **Integration Tests**: `tests/` directory contains end-to-end tests
- **Examples**: `examples/` directory provides working usage examples

### Service Deployment

- **Systemd Services**: `services/` contains `.service` files for daemon deployment
- **Build Scripts**: `scripts/` contains automated build and release scripts
- **Cross-platform**: Support for Linux, macOS, Windows, Android, iOS, and web
