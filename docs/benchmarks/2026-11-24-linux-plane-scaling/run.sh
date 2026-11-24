#!/usr/bin/env bash
# Builds planecheck.cc with the Linux runner's own desktop_video_gl.cc,
# unchanged, against Apple's OpenGL through the epoxy stand-in in macos-gl/,
# both as the app builds them (C++14, -Wall -Werror, -O3), and runs it. The
# output goes to stdout; the PNG crops to OUT (default: ./crops beside this
# script). macOS only.
# Usage: run.sh [OUT]
set -eu
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(git -C "$HERE" rev-parse --show-toplevel)
OUT=${1:-$HERE/crops}
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
mkdir -p "$OUT"
FLAGS=(-std=c++14 -Wall -Werror -O3 -DNDEBUG -I "$HERE/macos-gl" -I "$ROOT/ui/linux/runner" -I "$ROOT/ui/linux/test")
clang++ "${FLAGS[@]}" -c "$ROOT/ui/linux/runner/desktop_video_gl.cc" -o "$WORK/desktop_video_gl.o"
clang++ "${FLAGS[@]}" -c "$HERE/planecheck.cc" -o "$WORK/planecheck.o"
clang++ "$WORK/desktop_video_gl.o" "$WORK/planecheck.o" -framework OpenGL -framework ApplicationServices \
  -framework CoreText -framework ImageIO -framework CoreFoundation -o "$WORK/planecheck"
"$WORK/planecheck" "$OUT"
