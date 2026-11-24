#!/usr/bin/env bash
# Writes es100check.html to OUT: the video plane's shaders as the Linux
# runner builds them for GL ES (GLSL ES 1.00, through shaderdump.cc), which
# the page compiles, links and draws once each in WebGL 1 and in WebGL 2
# (whose ES 3.00 contexts also take #version 100), printing the results.
# Open it in a Chromium-based browser, whose WebGL validates GLSL ES 1.00
# through ANGLE. macOS only, like run.sh.
# Usage: es100check.sh [OUT]
set -eu
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(git -C "$HERE" rev-parse --show-toplevel)
OUT=${1:-.}
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
clang++ -std=c++14 -Wall -Werror -I "$HERE/macos-gl" -I "$ROOT/ui/linux/runner" "$HERE/shaderdump.cc" \
  -framework OpenGL -o "$WORK/shaderdump"
"$WORK/shaderdump" > "$WORK/shaders.json"
python3 -I "$HERE/es100check.py" "$WORK/shaders.json" "$OUT/es100check.html"
