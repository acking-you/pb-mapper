# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

This is a Rust-based network tunneling/proxy system called `pb-mapper` that allows exposing local services to clients over a public network. The system consists of three main components:

1. **pb-mapper-server**: Central server that manages connections between local services and clients
2. **pb-mapper-server-cli**: Registers local services with the central server
3. **pb-mapper-client-cli**: Connects to registered services through the central server

The system works by creating a bridge between local services and remote clients through a public server, enabling access to services behind NAT/firewalls.

## Code Architecture

### Core Modules

- `src/pb_server/`: Implements the central server logic that manages connections
- `src/local/server/`: Handles registration of local services with the central server
- `src/local/client/`: Handles client connections to registered services
- `src/common/`: Shared utilities, message protocols, and configuration
- `src/utils/`: Helper functions for address resolution, timeouts, etc.

### Key Components

1. **Message Protocol** (`src/common/message/`): 
   - Defines the communication protocol between components
   - Uses JSON serialization with custom framing (checksum + length header)
   - Supports encryption/decryption for secure communication

2. **Connection Management**:
   - Central server maintains mappings between service keys and connection IDs
   - Handles registration, subscription, and stream forwarding
   - Implements keep-alive and timeout mechanisms

## UI Module Requirements

The UI module needs to provide a graphical interface that replaces all functionality of the command-line tools. The UI should have the following features:

### 1. Server Management Interface
- Start/stop the pb-mapper-server
- Configure server port (default: 7666)
- Toggle IPv6 support
- Enable/disable TCP keep-alive
- Display server status and logs

### 2. Service Registration Interface (Server CLI Replacement)
- Register local TCP services:
  * Service key (unique identifier)
  * Local service address (ip:port)
  * Enable/disable encryption codec
  * Remote server address (PB_MAPPER_SERVER environment variable)
  * TCP keep-alive setting
- Register local UDP services:
  * Service key (unique identifier)
  * Local service address (ip:port)
  * Enable/disable encryption codec
  * Remote server address (PB_MAPPER_SERVER environment variable)
  * TCP keep-alive setting

### 3. Client Connection Interface (Client CLI Replacement)
- Connect to remote TCP services:
  * Service key to connect to
  * Local listening address (ip:port)
  * Remote server address (PB_MAPPER_SERVER environment variable)
  * TCP keep-alive setting
- Connect to remote UDP services:
  * Service key to connect to
  * Local listening address (ip:port)
  * Remote server address (PB_MAPPER_SERVER environment variable)
  * TCP keep-alive setting

### 4. Status Monitoring Interface
- View remote server status:
  * Show active remote connection IDs
  * Show registered service keys
  * Display server mapping information

### 5. Configuration Management
- Set and manage environment variables:
  * PB_MAPPER_SERVER: Remote server address
  * PB_MAPPER_KEEP_ALIVE: TCP keep-alive setting

## Key Features

- TCP/UDP support for local services
- Optional encryption for secure communication
- Keep-alive and timeout handling for connection stability
- Service registration with unique keys
- Status checking capabilities
- Docker deployment support
- Graphical UI interface (Dioxus-based) to replace CLI tools

## Development Notes

- The project uses a workspace structure with a separate UI crate
- Dependencies are managed through workspace configuration in Cargo.toml
- Uses mimalloc for memory allocation optimization
- Implements custom error handling with snafu crate
- Follows Rust async/await patterns with Tokio runtime
- UI development should focus on replicating CLI functionality with a user-friendly interface
- UI should provide form inputs for all CLI arguments and environment variables
- UI should display real-time status information and logs
- UI should handle error conditions gracefully and provide user feedback