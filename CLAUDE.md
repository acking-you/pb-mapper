# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

This is a Rust-based network tunneling/proxy system called `pb-mapper` that allows exposing local services to clients over a public network. The project enables users to access their home services (like file transfer servers) from anywhere by creating secure tunnels through a public server.

The system consists of three main binary components:

1. **pb-mapper-server** (`src/bin/pb-mapper-server.rs`): Central server that manages connections between local services and clients
   - Runs on port 7666 by default
   - Supports IPv4/IPv6 configuration
   - Manages service registration and client subscription mappings
   - Handles connection forwarding and keep-alive mechanisms

2. **pb-mapper-server-cli** (`src/bin/pb-mapper-server-cli.rs`): Registers local services with the central server
   - Exposes local TCP/UDP services to the public server
   - Supports encryption codec for secure communication
   - Configurable via environment variables and command-line arguments

3. **pb-mapper-client-cli** (`src/bin/pb-mapper-client-cli.rs`): Connects to registered services through the central server
   - Subscribes to remote services and creates local listening endpoints
   - Supports both TCP and UDP protocols
   - Provides status checking capabilities

4. **UI Module** (`ui/`): Flutter-based graphical interface using Rinf framework
   - Replaces all CLI functionality with a user-friendly GUI
   - Built with Flutter and Rinf for Rust-Flutter integration
   - Provides comprehensive service management interface

The system works by creating a bridge between local services and remote clients through a public server, enabling access to services behind NAT/firewalls.

## Code Architecture

### Project Structure

```
pb-mapper/
├── src/                    # Main Rust codebase
│   ├── bin/               # Binary executables (server, server-cli, client-cli)
│   ├── pb_server/         # Central server implementation
│   ├── local/             # Local service handlers (server/client)
│   ├── common/            # Shared utilities and protocols
│   └── utils/             # Helper functions
├── ui/                    # Flutter UI with Rinf framework
│   ├── lib/               # Flutter application code
│   ├── native/hub/        # Rust-Flutter bridge (Rinf)
│   └── documentation/     # Rinf framework documentation
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

- **`src/local/server/`**: Local service registration (server-cli functionality)
  - `stream.rs`: Stream handling for service registration
  - `mod.rs`: Registration logic and server-side CLI implementation
  - `error.rs`: Server-specific error handling

- **`src/local/client/`**: Client connection handling (client-cli functionality)
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
- **`lib/src/views/`**: UI pages and components
  - `main_landing_view.dart`: Main application entry point
  - `server_management_view.dart` & `server_management_page.dart`: Server control interface
  - `service_registration_view.dart` & `service_registration_page.dart`: Service registration UI
  - `client_connection_view.dart` & `client_connection_page.dart`: Client connection interface
  - `status_monitoring_view.dart`: Status and monitoring dashboard
  - `configuration_view.dart`: Configuration management
  - `log_display_widget.dart`: Log viewing interface

- **`lib/src/common/`**: Shared UI utilities
  - `function.dart`: Common UI functions
  - `theme_change_button.dart`: Theme switching component
  - `log_manager.dart`: Log management system

- **`lib/src/bindings/`**: Rinf-generated Rust-Flutter bindings
  - `signals/`: Auto-generated signal handlers for Rust-Flutter communication
  - `serde/`: Serialization/deserialization utilities
  - `bincode/`: Binary encoding support

- **`native/hub/`**: Rust backend for Flutter (Rinf integration)
  - `src/actors/`: Actor-based concurrent processing
  - `src/signals/`: Signal handling for UI communication
  - Integration with main pb-mapper library

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

The UI module provides a complete graphical interface that replaces all CLI functionality. It's built with Flutter and uses the Rinf framework for seamless Rust-Flutter integration through message-passing.

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

### Rinf Integration Details

The UI uses Rinf framework for Rust-Flutter communication:

- **Signal-based Communication**: Type-safe message passing between Rust and Flutter
- **Auto-generated Bindings** (`lib/src/bindings/`): Rust types automatically exposed to Flutter
- **Actor System** (`native/hub/src/actors/`): Concurrent request processing
- **Signal Handlers** (`native/hub/src/signals/`): Business logic implementation

### Signal Types Available

- **Server Control**: `StartServerRequest`, `StopServerRequest`, `ServerStatusUpdate`
- **Service Management**: `RegisterServiceRequest`, `RegisteredServicesUpdate`, `RegisteredServiceInfo`
- **Client Operations**: `ConnectServiceRequest`, `DisconnectServiceRequest`, `ClientConnectionStatus`
- **Status Monitoring**: `ActiveConnectionsUpdate`, `ActiveConnectionInfo`
- **Configuration**: `UpdateConfigRequest`, `ConfigStatusUpdate`, `RequestConfig`
- **Logging**: `LogMessage` with integrated log collection and display

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
- **Rinf Integration**: Seamless Rust-Flutter communication
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
- **Framework**: Flutter 3.9+ with Material Design components
- **State Management**: Built-in Flutter state management with Rinf signal integration
- **Architecture**: Page/View pattern with clear separation of concerns
- **Real-time Updates**: Signal-based reactive UI updates from Rust backend
- **Error Handling**: Graceful error handling with user-friendly feedback
- **Responsive Design**: Adaptive layouts for different screen sizes

### Environment Variables
- **`PB_MAPPER_SERVER`**: Default remote server address for CLI tools
- **`PB_MAPPER_KEEP_ALIVE`**: Global TCP keep-alive setting ("ON" to enable)
- **`RUST_LOG`**: Tracing level configuration (supports env-filter)

## Rinf Framework Integration

The UI leverages the Rinf framework for seamless Rust-Flutter integration, enabling type-safe communication between the Rust backend and Flutter frontend.

### Rinf Architecture

- **Message-Passing System**: Bidirectional communication through signals
- **Type Safety**: Auto-generated Dart bindings from Rust types
- **Actor Model**: Concurrent processing in `native/hub/src/actors/`
- **Signal Handlers**: Business logic implementation in `native/hub/src/signals/`

### Actor Usage Pattern

The core business logic is implemented using an actor-based pattern with the `messages` crate:

#### 1. Actor Definition (`pb_mapper_actor.rs`)
```rust
use messages::prelude::{Context, Address, Notifiable};

pub struct PbMapperActor {
    // State fields
    config: AppConfig,
    server_handle: Option<JoinHandle<()>>,
    // ... other state
}

impl PbMapperActor {
    pub fn new(self_addr: Address<Self>) -> Self {
        let mut owned_tasks = OwnedTasks::new();
        
        // Spawn signal listeners for each DartSignal type
        owned_tasks.spawn(Self::listen_to_start_server(self_addr.clone()));
        owned_tasks.spawn(Self::listen_to_update_config(self_addr.clone()));
        // ... other listeners
        
        Self { /* initialize fields */ }
    }
}
```

#### 2. Signal Listeners
Each DartSignal type requires a dedicated listener method:
```rust
async fn listen_to_update_config(mut self_addr: Address<Self>) {
    let receiver = UpdateConfigRequest::get_dart_signal_receiver();
    while let Some(signal_pack) = receiver.recv().await {
        let _ = self_addr.notify(signal_pack.message).await;
    }
}
```

#### 3. Business Logic Implementation
Business logic is implemented via `Notifiable` trait:
```rust
#[async_trait]
impl Notifiable<UpdateConfigRequest> for PbMapperActor {
    async fn notify(&mut self, msg: UpdateConfigRequest, _: &Context<Self>) {
        // Update configuration in memory
        self.config.server_address = msg.server_address;
        self.config.keep_alive_enabled = msg.enable_keep_alive;
        
        // Save to file and send feedback
        match self.save_config() {
            Ok(_) => {
                ConfigSaveResult {
                    success: true,
                    message: "Configuration saved successfully".to_string(),
                }.send_signal_to_dart();
                
                // Trigger status update to refresh UI
                self.send_config_status().await;
            }
            Err(e) => {
                ConfigSaveResult {
                    success: false,
                    message: format!("Failed to save configuration: {}", e),
                }.send_signal_to_dart();
            }
        }
    }
}
```

#### 4. Actor Registration
The actor is created and started in `actors/mod.rs`:
```rust
pub async fn create_actors() {
    let start_receiver = CreateActors::get_dart_signal_receiver();
    start_receiver.recv().await;

    let pb_mapper_context = Context::new();
    let pb_mapper_addr = pb_mapper_context.address();
    
    let pb_mapper_actor = PbMapperActor::new(pb_mapper_addr);
    spawn(pb_mapper_context.run(pb_mapper_actor));
}
```

### Communication Flow

1. **Flutter → Rust**: UI sends signals (e.g., `UpdateConfigRequest`) to Rust
2. **Signal Reception**: Dedicated listener receives signal via `get_dart_signal_receiver()`
3. **Actor Notification**: Listener calls `self_addr.notify(message)` to forward to actor
4. **Business Logic**: Actor's `Notifiable` implementation processes the request
5. **Rust → Flutter**: Actor sends response signals (e.g., `ConfigSaveResult`) back to UI
6. **UI Updates**: Flutter rebuilds interface based on received signals

### Signal Types and Patterns

#### DartSignal (Flutter → Rust)
```rust
#[derive(Deserialize, DartSignal)]
pub struct UpdateConfigRequest {
    pub server_address: String,
    pub enable_keep_alive: bool,
}
```
- Requires listener method: `listen_to_update_config()`
- Requires `Notifiable` implementation for business logic
- Sent from Flutter: `UpdateConfigRequest(...).sendSignalToRust()`

#### RustSignal (Rust → Flutter)
```rust
#[derive(Serialize, RustSignal)]
pub struct ConfigSaveResult {
    pub success: bool,
    pub message: String,
}
```
- Sent from Rust: `ConfigSaveResult {...}.send_signal_to_dart()`
- Received in Flutter: `ConfigSaveResult.rustSignalStream.listen(...)`

### Key Integration Points

- **Server Operations**: Start/stop server with real-time status feedback
- **Service Management**: Register/unregister services with live updates
- **Client Connections**: Establish connections with status monitoring
- **Configuration**: Persist and sync settings between Rust and Flutter
- **Logging**: Real-time log streaming from Rust to Flutter UI

### Implementation Requirements

For each new DartSignal type, you must implement:

1. **Signal Definition** in `src/signals/server_signals.rs`:
   ```rust
   #[derive(Deserialize, DartSignal)]
   pub struct YourRequest {
       pub field: String,
   }
   ```

2. **Listener Method** in `pb_mapper_actor.rs`:
   ```rust
   async fn listen_to_your_request(mut self_addr: Address<Self>) {
       let receiver = YourRequest::get_dart_signal_receiver();
       while let Some(signal_pack) = receiver.recv().await {
           let _ = self_addr.notify(signal_pack.message).await;
       }
   }
   ```

3. **Spawn Listener** in actor's `new()` method:
   ```rust
   owned_tasks.spawn(Self::listen_to_your_request(self_addr.clone()));
   ```

4. **Business Logic** via `Notifiable` trait:
   ```rust
   #[async_trait]
   impl Notifiable<YourRequest> for PbMapperActor {
       async fn notify(&mut self, msg: YourRequest, _: &Context<Self>) {
           // Implement your business logic here
           // Send response signals as needed
       }
   }
   ```

### Error Handling Patterns

- Use `Result<(), String>` for methods that need Send trait compatibility
- Always send feedback signals for user-initiated actions
- Use `tracing::error!` for logging errors in actor methods
- Handle actor notification failures gracefully with `let _ = self_addr.notify(...).await;`

### State Management

- Actor state is mutable and thread-safe within the actor context
- Use `Arc<RwLock<T>>` for shared state between actor and spawned tasks
- Configuration persistence should be handled synchronously in the actor
- Status updates should be triggered after state changes

### Rinf Documentation

Comprehensive Rinf documentation is available in `ui/documentation/`:
- **Getting Started**: `ui/documentation/source/tutorial.md`
- **Complete Guide**: `ui/documentation/source/` (includes actor model, state management, etc.)
- **API Reference**: Auto-generated signal and type documentation

## Development Workflow

### Building the Project

```bash
# Build server components
make build-pb-mapper-server

# Build with musl for static linking
make build-pb-mapper-server-x86_64_musl

# Build and run Flutter UI
cd ui && flutter run
```

### Docker Deployment

```bash
# Build and release Docker images
make release-pb-mapper-server-docker-image
make release-pb-mapper-server-x86-64-musl-docker-image

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