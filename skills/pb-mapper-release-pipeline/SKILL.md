---
name: pb-mapper-release-pipeline
description: Execute the pb-mapper release process end-to-end for server binaries and Flutter UI, including changelog updates, strict local validation, semantic version tagging, GitHub workflow triggering, and release monitoring. Use when preparing a new `vX.Y.Z` release or rerolling a failed release as the next patch version.
---

# Pb Mapper Release Pipeline

## Overview

Run the repository's official release flow in a deterministic way.  
Validate locally, update `CHANGELOG.md`, push commit and annotated tag, then monitor both GitHub release workflows until artifacts are published.

## Use This Workflow

Follow this workflow for all official releases in this repository:

- server binary release (`.github/workflows/release.yml`)
- UI release (`.github/workflows/release-ui.yml`)

Tagging `vX.Y.Z` triggers both workflows. The UI workflow publishes to `vX.Y.Z-ui`.

## Preconditions

Satisfy these preconditions before releasing:

- Work on `master` (or the branch the project uses for release tags).
- Authenticate GitHub CLI (`gh auth status` must succeed).
- Keep working tree clean before creating the release commit/tag.
- Confirm the version does not already exist:
  - `git tag --list 'vX.Y.Z'`
  - `git ls-remote --tags origin 'vX.Y.Z'`

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
cargo clippy --workspace --lib --bins --all-features -- -D warnings
flutter analyze
```

Run `flutter analyze` from `ui/` or with `--project-dir ui`.

If release workflow logic changed, also validate syntax:

```bash
python - <<'PY'
import yaml
yaml.safe_load(open('.github/workflows/release-ui.yml', 'r', encoding='utf-8'))
print('release-ui.yml OK')
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

- `build and deploy` (server binaries)
- `Release pb-mapper UI` (UI artifacts and `vX.Y.Z-ui` release)

## Step 5: Monitor Workflows

Track both workflow runs:

```bash
gh run list --workflow "build and deploy" --limit 5
gh run list --workflow "Release pb-mapper UI" --limit 5
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

- binary archives exist for all expected targets
- UI assets exist for Windows/Linux/macOS/Android/iOS jobs that succeeded
- UI release body contains the current version changelog section

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
