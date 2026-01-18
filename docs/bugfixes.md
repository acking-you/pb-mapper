# Bugfix Notes

This document records how we discovered and fixed common issues in pb-mapper.

## 1) Flutter UI freezes while checking pb-mapper availability

- Symptom: UI stutters or becomes unresponsive when probing server status.
- Discovery: Reproduced by opening the UI and watching the main thread stall during FFI calls.
- Root cause: FFI calls ran on the UI isolate and blocked the render thread.
- Fix: Move all FFI calls into a background isolate (via `compute`) and add a lightweight status cache with polling to keep the UI responsive.

## 2) Linux build fails to load `libpb_mapper_ffi.so`

- Symptom: `Failed to load dynamic library 'libpb_mapper_ffi.so'` at startup.
- Discovery: Reproduced after `flutter build linux` and launching the bundle.
- Root cause: FFI shared library was not built/copied into the app bundle.
- Fix: Add platform-specific build scripts and Makefile targets to compile and stage the FFI library into the correct bundle location (also reused by release jobs).

## 3) `Cannot start a runtime from within a runtime`

- Symptom: Panic from `trust-dns-resolver` when using custom domain resolution.
- Discovery: Reproduced by entering a custom domain and watching the runtime panic.
- Root cause: Sync resolver path called `block_on` inside an async runtime.
- Fix: Add an async DNS resolution path and guard sync resolution so it cannot execute inside a Tokio runtime.

## 4) Register/Connect status cards do not update immediately

- Symptom: Status cards remain stale until manual refresh.
- Discovery: Reproduced by registering/connecting and observing no UI update.
- Root cause: UI did not re-fetch status after actions and relied on stale cache.
- Fix: Trigger short polling after register/connect and add retry polling when re-checking availability.

## 5) Video downloads reset and forward logs spam errors

- Symptom: Streaming/downloading a specific video repeatedly resets connections; logs spam `read checksum`/`connection reset`.
- Discovery: Traced to `forward.rs` errors and frequent reconnects after stream termination.
- Root cause: Forwarding ended abruptly without half-close support; expected EOFs were logged as errors.
- Fix: Add half-close (`shutdown`) support in forward writers, allow both directions to finish, and downgrade expected disconnects (EOF/reset/broken pipe) to debug logs.
