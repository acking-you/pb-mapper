#!/usr/bin/env bash
set -euo pipefail

# Build the macOS FFI dynamic library and stage it for Flutter.
# - Copies to ui/native/macos for Xcode embed step
# - Copies into any existing .app bundle for quick local runs
#
# Builds for the host architecture only. Release artifacts are published per
# arch (one native runner each), so no universal binary is needed anywhere.

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
FFI_LIB_NAME="libpb_mapper_ffi.dylib"

cd "${ROOT_DIR}"

case "$(uname -m)" in
  arm64) HOST_TARGET="aarch64-apple-darwin" ;;
  x86_64) HOST_TARGET="x86_64-apple-darwin" ;;
  *) printf "Unsupported macOS arch: %s\n" "$(uname -m)" >&2; exit 1 ;;
esac

rustup target add "${HOST_TARGET}" >/dev/null
cargo build -p pb-mapper-ffi --release --target "${HOST_TARGET}"

STAGED_LIB="${ROOT_DIR}/target/${HOST_TARGET}/release/${FFI_LIB_NAME}"

FFI_TARGET_DIR="${ROOT_DIR}/ui/native/macos"
mkdir -p "${FFI_TARGET_DIR}"
cp "${STAGED_LIB}" "${FFI_TARGET_DIR}/${FFI_LIB_NAME}"

# arm64 binaries must carry a signature to run at all, so the linker ad-hoc
# signs them; x86_64 has no such rule and comes out unsigned. Xcode then fails
# the app's CodeSign step on the unsigned nested dylib ("code object is not
# signed at all"), so sign it here to make both architectures behave the same.
codesign --force --sign - --timestamp=none "${FFI_TARGET_DIR}/${FFI_LIB_NAME}"

for app in "${ROOT_DIR}/ui/build/macos/Build/Products"/*/*.app; do
  if [ -d "${app}" ]; then
    mkdir -p "${app}/Contents/Frameworks"
    cp "${FFI_TARGET_DIR}/${FFI_LIB_NAME}" "${app}/Contents/Frameworks/${FFI_LIB_NAME}"
  fi
done

printf "FFI ready (%s): %s\n" "$(lipo -archs "${FFI_TARGET_DIR}/${FFI_LIB_NAME}")" \
  "${FFI_TARGET_DIR}/${FFI_LIB_NAME}"
