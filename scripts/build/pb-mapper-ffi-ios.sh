#!/usr/bin/env bash
set -euo pipefail

# Build iOS FFI static library for Flutter.
# Requires Rust iOS target and Xcode toolchain.

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
FFI_LIB_NAME="libpb_mapper_ffi.a"

cd "${ROOT_DIR}"

rustup target add aarch64-apple-ios
cargo build -p pb-mapper-ffi --release --target aarch64-apple-ios

FFI_TARGET_DIR="${ROOT_DIR}/ui/native/ios"
mkdir -p "${FFI_TARGET_DIR}"
cp "${ROOT_DIR}/target/aarch64-apple-ios/release/${FFI_LIB_NAME}" \
  "${FFI_TARGET_DIR}/${FFI_LIB_NAME}"

echo "FFI ready: ${FFI_TARGET_DIR}/${FFI_LIB_NAME}"
