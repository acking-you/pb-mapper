#!/usr/bin/env bash
set -euo pipefail

# Build the Linux FFI shared library and place it where Flutter expects it.
# - Copies to ui/native/linux/x64 for packaging during flutter build
# - Copies into any existing Linux bundle (debug/release) for immediate runs

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
FFI_LIB_NAME="libpb_mapper_ffi.so"

cd "${ROOT_DIR}"

# Build the FFI library (release for reuse in release bundles).
cargo build -p pb-mapper-ffi --release

# Stage into the native assets directory used by Flutter CMake.
FFI_TARGET_DIR="${ROOT_DIR}/ui/native/linux/x64"
mkdir -p "${FFI_TARGET_DIR}"
cp "${ROOT_DIR}/target/release/${FFI_LIB_NAME}" "${FFI_TARGET_DIR}/${FFI_LIB_NAME}"

# If a Linux bundle already exists, copy the library into its lib folder.
for bundle in "${ROOT_DIR}/ui/build/linux/x64"/*/bundle; do
  if [ -d "${bundle}/lib" ]; then
    cp "${ROOT_DIR}/target/release/${FFI_LIB_NAME}" "${bundle}/lib/${FFI_LIB_NAME}"
  fi
done

printf "FFI ready: %s\n" "${FFI_TARGET_DIR}/${FFI_LIB_NAME}"
