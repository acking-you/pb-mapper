---
name: pb-mapper-release-pipeline
description: Execute the pb-mapper release process end-to-end for the unified CLI, Flutter UI, Docker images, Rust SDK, and Node SDK, including changelog updates, strict local validation, semantic version tagging, registry publication, and release monitoring.
---

# Pb Mapper Release Pipeline

## Overview

Run the repository's official release flow in a deterministic way.  
Validate locally, update `CHANGELOG.md`, push commit and annotated tag, then monitor every GitHub and registry release until artifacts are published.

## Use This Workflow

Follow this workflow for all official releases in this repository:

- unified CLI release (`.github/workflows/release.yml`)
- UI release (`.github/workflows/release-ui.yml`)
- Docker release (`.github/workflows/docker-publish.yml`)
- Rust and Node SDK release (`.github/workflows/release-sdk.yml`)

Tagging `vX.Y.Z` triggers every release workflow. The UI workflow publishes to `vX.Y.Z-ui`.

## Preconditions

Satisfy these preconditions before releasing:

- Work on `master` (or the branch the project uses for release tags).
- Authenticate GitHub CLI (`gh auth status` must succeed).
- Keep working tree clean before creating the release commit/tag.
- Confirm the version does not already exist:
  - `git tag --list 'vX.Y.Z'`
  - `git ls-remote --tags origin 'vX.Y.Z'`
- Confirm the matching versions do not already exist on crates.io or npm.
- Configure `CARGO_REGISTRY_TOKEN` as a GitHub Actions secret.
- Configure npm Trusted Publishing for `release-sdk.yml` on `pb-mapper` and
  each published platform package. The SDK workflow uses GitHub OIDC and does
  not store an npm access token.

## Versioning Rule

Use semantic tags and never retag an existing version:

- normal release: `vX.Y.Z`
- failed release reroll: fix forward and release `vX.Y.(Z+1)`
- do not delete or move existing release tags

## Step 1: Update Release Content

Apply required code/workflow changes and update changelog.

In `CHANGELOG.md`, add a new section at the top using this exact heading style:

```md
## [X.Y.Z] - YYYY-MM-DD
```

Use concise bullets for user-visible release items.

## Step 2: Run Local Validation

Run the same strict checks used in CI:

```bash
cargo fmt --all
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
flutter analyze
cd js && bun install --frozen-lockfile && bun run build:release && bun test
```

Run `flutter analyze` from `ui/` or with `--project-dir ui`.

If release workflow logic changed, also validate syntax:

```bash
python - <<'PY'
import yaml
yaml.safe_load(open('.github/workflows/release.yml', 'r', encoding='utf-8'))
yaml.safe_load(open('.github/workflows/release-ui.yml', 'r', encoding='utf-8'))
yaml.safe_load(open('.github/workflows/release-sdk.yml', 'r', encoding='utf-8'))
print('release workflows OK')
PY
```

## Step 3: Commit and Push Release Commit

Stage only intended files, then commit:

```bash
git add <release-files>
git commit -m "Release X.Y.Z: <summary>"
git push origin master
```

## Step 4: Create and Push Tag

Create annotated tag and push:

```bash
git tag -a vX.Y.Z -m "Release vX.Y.Z"
git push origin vX.Y.Z
```

This push triggers:

- `Build and release pb-mapper CLI` (one CLI artifact per target)
- `Release pb-mapper UI` (UI artifacts and `vX.Y.Z-ui` release)
- `Build and Push pb-mapper Docker Images`
- `Publish pb-mapper SDKs` (crates.io and npm)

## Step 5: Monitor Workflows

Track every workflow run:

```bash
gh run list --workflow "Build and release pb-mapper CLI" --limit 5
gh run list --workflow "Release pb-mapper UI" --limit 5
gh run list --workflow "Build and Push pb-mapper Docker Images" --limit 5
gh run list --workflow "Publish pb-mapper SDKs" --limit 5
```

Inspect an active run:

```bash
gh run view <run-id>
```

Wait until both runs complete successfully.

## Step 6: Verify Published Releases

Confirm release pages and assets:

```bash
gh release view vX.Y.Z
gh release view vX.Y.Z-ui
```

Check that:

- one `pb-mapper` archive and checksum exist for every expected CLI target
- UI assets exist for Windows/Linux/macOS/Android/iOS jobs that succeeded
- UI release body contains the current version changelog section
- `cargo info pb-mapper@X.Y.Z` succeeds and a fresh project compiles
- `npm view pb-mapper@X.Y.Z version` succeeds and fresh Node installs load the native addon

## UI Changelog Notes Behavior

UI release notes are generated from `CHANGELOG.md` in `release-ui.yml`:

- `get-release` job extracts the `## [X.Y.Z]` section into `ui_release_notes.md`
- platform jobs publish with `body_path: ui_release_notes.md`

If changelog text is missing in UI release body, verify:

- the version heading format in `CHANGELOG.md` is exact
- the tag is `vX.Y.Z` and matches changelog section `[X.Y.Z]`

## Failure Recovery

If validation or workflow fails:

1. Fix code/workflow/changelog on `master`.
2. Re-run local checks.
3. Commit fix.
4. Create next patch tag (`vX.Y.(Z+1)`).
5. Push branch and new tag.

Do not reuse failed tags.
