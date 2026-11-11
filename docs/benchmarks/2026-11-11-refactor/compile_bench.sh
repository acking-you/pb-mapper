#!/usr/bin/env bash
# Wall-clock compile times of a pb-mapper checkout, each scenario from a clean
# target directory unless it says otherwise.
#
#   compile_bench.sh <src-dir> <label> <out-dir> [scenario ...]
#
# Scenarios: check build incr test sdk release-cli release-all
# Appends `label,scenario,seconds,exit` rows to <out-dir>/results.csv and keeps
# each command's log (and cargo's timing report) beside it.
set -u

SRC=$1
LABEL=$2
OUT=$3
shift 3
SCENARIOS=${*:-"check build incr test release-cli release-all"}

TD="$SRC/target/bench"
mkdir -p "$OUT"
cd "$SRC" || exit 2

export CARGO_TARGET_DIR="$TD"
export CARGO_TERM_COLOR=never
# An empty value overrides a `build.rustc-wrapper` (sccache) in the user's cargo
# config, so every run compiles for real.
export RUSTC_WRAPPER=

cargo fetch --locked >"$OUT/$LABEL-fetch.log" 2>&1

now() { date +%s.%N; }

clean() { rm -rf "$TD"; }

# timed <scenario> <command...>
timed() {
    local name=$1
    shift
    local t0 t1 rc
    t0=$(now)
    "$@" >"$OUT/$LABEL-$name.log" 2>&1
    rc=$?
    t1=$(now)
    awk -v l="$LABEL" -v n="$name" -v a="$t0" -v b="$t1" -v rc="$rc" \
        'BEGIN { printf "%s,%s,%.1f,%s\n", l, n, b - a, rc }' | tee -a "$OUT/results.csv"
}

keep_timings() {
    local name=$1
    local report="$TD/cargo-timings/cargo-timing.html"
    [ -f "$report" ] && cp "$report" "$OUT/$LABEL-$name-timing.html"
}

for s in $SCENARIOS; do
    case "$s" in
    check)
        clean
        timed check cargo check --workspace --all-targets --all-features --locked
        ;;
    build)
        clean
        timed build cargo build --workspace --all-features --locked --timings
        keep_timings build
        ;;
    incr)
        # Rebuild fan-out after an edit, in the directory `build` left.
        for c in pb-mapper-core pb-mapper-protocol pb-mapper-client pb-mapper-desktop; do
            touch "crates/$c/src/lib.rs"
            timed "incr-$c" cargo build --workspace --all-features --locked
        done
        ;;
    test)
        clean
        timed test cargo test --workspace --all-features --no-run --locked
        ;;
    sdk)
        # What a user of the published SDK compiles: the facade with its default features.
        clean
        timed sdk cargo build -p pb-mapper --locked
        ;;
    release-cli)
        clean
        timed release-cli cargo build --release --bin pb-mapper --locked --timings
        keep_timings release-cli
        ;;
    release-all)
        clean
        timed release-all cargo build --release --workspace --all-features --locked --timings
        keep_timings release-all
        ;;
    *)
        echo "unknown scenario: $s" >&2
        ;;
    esac
done
clean
