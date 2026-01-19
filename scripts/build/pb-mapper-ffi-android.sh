#!/usr/bin/env bash
set -euo pipefail

# Build Android FFI libraries for Flutter (JNI libs).
# Requires Android NDK and cargo-ndk.

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

cd "${ROOT_DIR}"

if ! command -v cargo-ndk >/dev/null 2>&1; then
  cargo install cargo-ndk
fi

# Ensure Rust targets for the Android ABIs we build.
rustup target add armv7-linux-androideabi aarch64-linux-android x86_64-linux-android

# Build for common Android ABIs used by Flutter.
cargo ndk -o ui/native/android \
  -t armeabi-v7a -t arm64-v8a -t x86_64 \
  build -p pb-mapper-ffi --release

echo "FFI ready: ${ROOT_DIR}/ui/native/android"
