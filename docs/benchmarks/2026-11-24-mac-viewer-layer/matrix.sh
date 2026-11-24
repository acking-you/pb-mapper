#!/usr/bin/env bash
# The A/B matrix: each size and rate with both presenters, two rounds, the
# order reversed in the second, after build-app.sh made texture60, layer60,
# texture120 and layer120. CLIPS holds clip-1080p.h264, clip-1440p.h264 and
# clip-4k.h264 from clipgen.swift. Records the load average before each run.
set -u
HERE=$(cd "$(dirname "$0")" && pwd)
WORK=${WORK:-$HERE/work}
CLIPS=${CLIPS:-$WORK/clips}
mkdir -p "$WORK"
: > "$WORK/runs-load.txt"
one() {
  echo "$1 $(sysctl -n vm.loadavg)" >> "$WORK/runs-load.txt"
  bash "$HERE/run.sh" "$@"
}
for round in a b; do
  for case in "1080p 1920x1080 60" "1440p 2560x1440 60" "4k 3840x2160 60" "1080p 1920x1080 120" "4k 3840x2160 120"; do
    set -- $case
    clip=$CLIPS/clip-$1.h264
    if [ "$round" = a ]; then order="texture layer"; else order="layer texture"; fi
    for p in $order; do
      one "$p-$1-$3-$round" "$p$3" "$clip" "$2" "$3"
    done
  done
done
echo matrix-done
