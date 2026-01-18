#!/usr/bin/env bash
set -euo pipefail

# Build the macOS FFI dynamic library and stage it for Flutter.
# - Copies to ui/native/macos for Xcode embed step
# - Copies into any existing .app bundle for quick local runs

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
FFI_LIB_NAME="libpb_mapper_ffi.dylib"

cd "${ROOT_DIR}"

cargo build -p pb-mapper-ffi --release

FFI_TARGET_DIR="${ROOT_DIR}/ui/native/macos"
mkdir -p "${FFI_TARGET_DIR}"
cp "${ROOT_DIR}/target/release/${FFI_LIB_NAME}" "${FFI_TARGET_DIR}/${FFI_LIB_NAME}"

for app in "${ROOT_DIR}/ui/build/macos/Build/Products"/*/*.app; do
  if [ -d "${app}" ]; then
    mkdir -p "${app}/Contents/Frameworks"
    cp "${ROOT_DIR}/target/release/${FFI_LIB_NAME}" "${app}/Contents/Frameworks/${FFI_LIB_NAME}"
  fi
done

printf "FFI ready: %s\n" "${FFI_TARGET_DIR}/${FFI_LIB_NAME}"
