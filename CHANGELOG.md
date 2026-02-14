# Changelog

All notable changes to this project will be documented in this file.

## [0.2.2] - 2026-02-15
- Added full Flutter UI + FFI support for `MSG_HEADER_KEY` configuration, including config persistence, validation, and runtime propagation.
- Made `MSG_HEADER_KEY` optional in UI config: empty value now falls back to the default header key behavior.
- Updated service registration/client connection setup guidance in UI to explicitly include `MSG_HEADER_KEY` consistency checks.
- Hardened UI release build flow to enforce FFI-first build order per platform (Windows/Linux/macOS/Android/iOS).
- Added workflow-level FFI integrity checks (hash verification between staged FFI artifacts and packaged UI outputs).

## [0.2.1] - 2026-02-09
- Added `pb-mapper-server --use-machine-msg-header-key` to derive `MSG_HEADER_KEY` from machine hostname + MAC addresses.
- Persisted the derived key to `/var/lib/pb-mapper-server/msg_header_key` for operator reuse in `pb-mapper-server-cli` and `pb-mapper-client-cli`.
- Added fallback MAC collection paths (`/sys/class/net`, `ip link`, `ifconfig`) to improve portability.
- Updated user guides (English and Chinese) with setup and usage instructions for machine-derived key mode.
- Added/expanded code documentation for public key-related APIs and derivation rationale.

## [0.1.1] - 2026-01-17
- Extracted stream/UDP logic into `deps/uni-stream` and switched core networking to use it.
- Fixed UDP forwarding by preserving datagram boundaries and adding explicit datagram APIs.
- Added `into_split()` owned halves for spawn-friendly IO split.
- Updated UI Rust bridge to pass correct UDP datagram mode to server/client.
- Added deep-dive docs on async Send/Sync/Pin and UDP datagram forwarding.
