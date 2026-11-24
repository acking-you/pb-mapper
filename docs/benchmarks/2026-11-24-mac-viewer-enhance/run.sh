#!/usr/bin/env bash
# Builds enhancecheck.swift with the runner's own DesktopVideo.swift (one
# file, so its file-private classes are in reach), release-optimised, and
# runs it. The output goes to stdout; the PNG crops to OUT (default:
# ./crops beside this script). ENHANCECHECK_ONLY=presenter skips the
# offscreen GPU time and picture checks.
# Usage: [ENHANCECHECK_ONLY=presenter] run.sh [OUT]
set -eu
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(git -C "$HERE" rev-parse --show-toplevel)
OUT=${1:-$HERE/crops}
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
FLUTTER_ROOT=${FLUTTER_ROOT:-$HOME/Developer/flutter}
FRAMEWORKS="$FLUTTER_ROOT/bin/cache/artifacts/engine/darwin-x64-release/FlutterMacOS.xcframework/macos-arm64_x86_64"
cat "$ROOT/ui/darwin/DesktopVideo.swift" "$HERE/enhancecheck.swift" > "$WORK/main.swift"
swiftc -O -swift-version 5 -target "$(uname -m)-apple-macosx14.0" -F "$FRAMEWORKS" \
  -framework FlutterMacOS "$WORK/main.swift" -o "$WORK/enhancecheck"
DYLD_FRAMEWORK_PATH="$FRAMEWORKS" "$WORK/enhancecheck" "$OUT"
