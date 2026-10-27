#!/usr/bin/env bash
# Publish crates to crates.io in the order given, each version once.
#
#   scripts/release/publish-crates.sh <crate-dir>...
#
# A version already on crates.io is skipped when its published source matches
# this checkout, so a rerun picks up where a failed run stopped. A version
# published from different source fails the run instead: a pinned fork changed
# without a version bump would otherwise ship stale code to every SDK user.
set -euo pipefail

scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT

published() {
  cargo info --registry crates-io "$1@$2" >/dev/null 2>&1
}

for dir in "$@"; do
  manifest="$(realpath "$dir/Cargo.toml")"
  metadata="$(cargo metadata --no-deps --format-version 1 --manifest-path "$manifest")"
  read -r name version target < <(jq -r --arg manifest "$manifest" \
    '. as $m | .packages[] | select(.manifest_path == $manifest)
     | "\(.name) \(.version) \($m.target_directory)"' <<<"$metadata")

  if ! published "$name" "$version"; then
    cargo publish --locked --registry crates-io --manifest-path "$manifest"
    for attempt in $(seq 1 30); do
      published "$name" "$version" && continue 2
      echo "Waiting for $name@$version in the crates.io index ($attempt/30)"
      sleep 10
    done
    echo "::error::$name@$version did not reach the crates.io index"
    exit 1
  fi

  # Compare the normalized manifest too: it carries what a member inherits from
  # its workspace, such as dependency versions. Skip only the commit Cargo
  # records and the lockfile, which follows the registry at packaging time.
  cargo package --locked --no-verify --registry crates-io --manifest-path "$manifest"
  rm -rf "$scratch/local" "$scratch/published"
  mkdir -p "$scratch/local" "$scratch/published"
  tar -xzf "$target/package/$name-$version.crate" -C "$scratch/local"
  curl -fsSL "https://static.crates.io/crates/$name/$name-$version.crate" | tar -xz -C "$scratch/published"
  if ! diff -r --exclude=.cargo_vcs_info.json --exclude=Cargo.lock \
    "$scratch/local/$name-$version" "$scratch/published/$name-$version"; then
    echo "::error::$name@$version is already on crates.io with different source or manifest; bump its version"
    exit 1
  fi
  echo "$name@$version is already on crates.io with this source"
done
