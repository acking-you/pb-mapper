# Changelog

All notable changes to this project will be documented in this file.

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
