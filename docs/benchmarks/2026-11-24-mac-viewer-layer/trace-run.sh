#!/usr/bin/env bash
# One run.sh run with a trace of the viewer: 12 s after the viewer starts,
# records it for 10 s with Animation Hitches plus the Points of Interest
# instrument, which takes the runner's `Decoded`, `Commit` and `Consumed`
# marks (timings are on throughout an acceptance run), then exports the
# marks to NAME-signposts.xml beside the trace. Build the bundle with
# build-app.sh first.
# Usage:
#   trace-run.sh NAME APPNAME CLIP WIDTHxHEIGHT FPS
set -u
HERE=$(cd "$(dirname "$0")" && pwd)
WORK=${WORK:-$HERE/work}
NAME=$1
APPNAME=$2
APPBIN=$WORK/app-$APPNAME/pb-mapper.app/Contents/MacOS/pb-mapper
TRACE=$WORK/traces/$NAME.trace
mkdir -p "$WORK/traces"
rm -rf "$TRACE"

bash "$HERE/run.sh" "$@" &
RUN=$!
APP=
for _ in $(seq 1 100); do
  APP=$(pgrep -f "^$APPBIN" | head -1)
  [ -n "$APP" ] && break
  sleep 0.2
done
[ -n "$APP" ] || { echo "viewer did not start"; wait $RUN; exit 1; }
sleep 12
xcrun xctrace record --template 'Animation Hitches' --instrument 'Points of Interest' \
  --attach "$APP" --time-limit 10s --output "$TRACE" > "$WORK/traces/$NAME.log" 2>&1
wait $RUN
xcrun xctrace export --input "$TRACE" \
  --xpath '/trace-toc/run[@number="1"]/data/table[@schema="os-signpost" and @category="PointsOfInterest"]' \
  > "$WORK/traces/$NAME-signposts.xml"
echo "traced $TRACE"
