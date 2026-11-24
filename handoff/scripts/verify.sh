#!/bin/bash
# Full workspace verification of the integrated branch; run from the worktree root.
echo "== fmt"
cargo fmt --all -- --check && echo FMT_OK
echo "== clippy"
cargo clippy --workspace --all-targets --all-features -- -D warnings 2>&1 | tail -40
echo "CLIPPY_EXIT=${PIPESTATUS[0]}"
echo "== test"
cargo test --workspace --all-features > "$1.test" 2>&1
echo "TEST_EXIT=$?"
grep -E '^test result|FAILED|panicked|^error' "$1.test" | sort | uniq -c | sort -rn | head -40
echo "== doc"
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps 2>&1 | tail -5
echo "DOC_EXIT=${PIPESTATUS[0]}"
echo "== loopback"
cargo run -q -p pb-mapper --features desktop --example desktop_loopback 2>&1 | tail -3
echo "LOOPBACK_EXIT=${PIPESTATUS[0]}"
