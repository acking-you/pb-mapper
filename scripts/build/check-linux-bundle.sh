#!/usr/bin/env bash
set -euo pipefail

# Checks a Linux bundle's native libraries (bundle/lib) before it ships:
# - the GStreamer decoder module is there, and loads against the system's
#   GStreamer, which is never bundled (a bundled core would load the system
#   plugins of another version, and those decide what decodes);
# - libpb_mapper_ffi.so loads with dlopen, as Dart loads it.
#
#   scripts/build/check-linux-bundle.sh ui/build/linux/x64/release/bundle

LIB="${1:?usage: $0 <bundle directory>}/lib"
MODULE="${LIB}/libpb_mapper_desktop_video.so"
FFI="${LIB}/libpb_mapper_ffi.so"

test -f "${MODULE}" || { echo "missing ${MODULE}: were the GStreamer development files installed?"; exit 1; }
test -f "${FFI}" || { echo "missing ${FFI}"; exit 1; }
if ls "${LIB}" | grep -E '^lib(gst|orc)'; then
  echo "GStreamer libraries are bundled in ${LIB}; they must come from the system"
  exit 1
fi
echo "The module links:"
readelf -d "${MODULE}" | grep NEEDED
python3 - "${FFI}" "${MODULE}" <<'PY'
import ctypes, sys
for library in sys.argv[1:]:
    ctypes.CDLL(library, mode=ctypes.RTLD_LOCAL)
    print("loads:", library)
PY
