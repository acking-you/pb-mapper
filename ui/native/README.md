# Rust Crates

This folder contains Rust crates. The Flutter UI now integrates with Rust via a raw FFI library crate:

- `pb-mapper-ffi`: exposes a C ABI for Flutter `dart:ffi` (shared/static libs).
- `pb-mapper-galaxy`: the galaxy view's per-frame geometry, with no dependencies;
  `pb-mapper-ffi` exports it as the leaf call `pb_mapper_galaxy_frame_v1`.
