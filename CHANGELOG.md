# Changelog

All notable changes to this project will be documented in this file.

## [0.1.1] - 2026-01-17
- Extracted stream/UDP logic into `deps/uni-stream` and switched core networking to use it.
- Fixed UDP forwarding by preserving datagram boundaries and adding explicit datagram APIs.
- Added `into_split()` owned halves for spawn-friendly IO split.
- Updated UI Rust bridge to pass correct UDP datagram mode to server/client.
- Added deep-dive docs on async Send/Sync/Pin and UDP datagram forwarding.

