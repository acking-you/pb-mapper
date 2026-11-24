#!/usr/bin/env bash
# Builds the release acceptance viewer with one presenter and frame-rate
# setup, and keeps a copy of the bundle in WORK (default: ./work beside this
# script). Run `make build-pb-mapper-ffi-macos` at the repository root first.
# Usage:
#   build-app.sh NAME PRESENTER MAXFPS QUALITY
# for example `build-app.sh layer60 layer 60 balanced` or
# `build-app.sh texture120 texture 120 smooth`.
set -eu
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(git -C "$HERE" rev-parse --show-toplevel)
WORK=${WORK:-$HERE/work}
NAME=$1
PRESENTER=$2
MAXFPS=$3
QUALITY=$4
mkdir -p "$WORK"
cd "$ROOT/ui"
FLUTTER_XCODE_ARCHS="$(uname -m)" FLUTTER_XCODE_ONLY_ACTIVE_ARCH=YES \
  ../scripts/build/flutter-macos.sh build macos --release \
  --target tool/desktop_viewer_acceptance.dart \
  --dart-define=setupUrl=http://127.0.0.1:18747/controller.json \
  --dart-define=maxFps="$MAXFPS" --dart-define=seconds=40 \
  --dart-define=resolution=native --dart-define=quality="$QUALITY" \
  --dart-define=presenter="$PRESENTER" \
  --dart-define="host=desktop_ui_host replay on this Mac, G24H2 at 144 Hz" \
  > "$WORK/app-$NAME.log" 2>&1
rm -rf "$WORK/app-$NAME"
mkdir -p "$WORK/app-$NAME"
cp -R build/macos/Build/Products/Release/pb-mapper.app "$WORK/app-$NAME/"
echo "built $WORK/app-$NAME"
