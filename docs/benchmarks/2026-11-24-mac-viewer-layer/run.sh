#!/usr/bin/env bash
# One acceptance run on this Mac: desktop_ui_host replaying CLIP at SIZE and
# FPS, the bootstrap, and one viewer bundle from build-app.sh. While it runs,
# reads about every 0.6 s the viewer's and WindowServer's CPU time and the
# GPU's utilisation into usage.tsv. Build the host first with
# `cargo build --release -p pb-mapper --features desktop --example desktop_ui_host`.
# Usage:
#   run.sh NAME APPNAME CLIP WIDTHxHEIGHT FPS
set -u
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(git -C "$HERE" rev-parse --show-toplevel)
WORK=${WORK:-$HERE/work}
NAME=$1
APPNAME=$2
CLIP=$3
SIZE=$4
FPS=$5
DIR=$WORK/runs/$NAME
rm -rf "$DIR"
mkdir -p "$DIR"

"$ROOT/target/release/examples/desktop_ui_host" --clip "$CLIP" --size "$SIZE" --dir "$DIR" \
  --relay-only --view-only --fps "$FPS" --seconds 40 > "$DIR/host.log" 2>&1 &
HOST=$!
for _ in $(seq 1 100); do grep -q host_ready "$DIR/host.log" && break; sleep 0.2; done
grep -q host_ready "$DIR/host.log" || { echo "host not ready"; kill $HOST; exit 1; }

node "$ROOT/scripts/test/desktop-bootstrap.cjs" "$DIR" > "$DIR/bootstrap.log" 2>&1 &
BOOT=$!
for _ in $(seq 1 50); do grep -q bootstrap_ready "$DIR/bootstrap.log" && break; sleep 0.2; done
grep -q bootstrap_ready "$DIR/bootstrap.log" || { echo "bootstrap not ready"; kill $HOST $BOOT; exit 1; }

APPBIN=$WORK/app-$APPNAME/pb-mapper.app/Contents/MacOS/pb-mapper
"$APPBIN" > "$DIR/app.log" 2>&1 &
APP=$!
WS=$(pgrep -x WindowServer)
: > "$DIR/usage.tsv"
for _ in $(seq 1 240); do
  grep -q report_saved "$DIR/bootstrap.log" && break
  kill -0 $APP 2>/dev/null || break
  gpu=$(ioreg -r -d 1 -w 0 -c AGXAccelerator | grep -o '"Device Utilization %"=[0-9]*' | head -1 | cut -d= -f2)
  printf '%s\t%s\t%s\t%s\n' "$(python3 -I -c 'import time; print(time.time())')" \
    "$(ps -o cputime= -p $APP | tr -d ' ')" "$(ps -o cputime= -p $WS | tr -d ' ')" "$gpu" >> "$DIR/usage.tsv"
  sleep 0.5
done
for _ in $(seq 1 120); do grep -q report_saved "$DIR/bootstrap.log" && break; kill -0 $APP 2>/dev/null || break; sleep 0.25; done
grep -q report_saved "$DIR/bootstrap.log" || echo "no report"
sleep 1
kill $APP 2>/dev/null
touch "$DIR/stop"
for _ in $(seq 1 50); do kill -0 $HOST 2>/dev/null || break; sleep 0.2; done
kill $HOST 2>/dev/null
kill $BOOT 2>/dev/null
wait 2>/dev/null
echo "done $DIR"
