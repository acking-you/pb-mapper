#!/bin/bash
# Merge branches in order into the current branch. Conflicts only in
# CHANGELOG.md / docs/README.md are resolved by keeping both sides; any other
# conflict stops the run.
S=/private/tmp/claude-501/-Users-lb7666--claude-spares-spare-vPIbq7/2de9709e-ea4b-442d-a692-7f9e8fd50cdd/scratchpad
for b in "$@"; do
  if git merge --no-ff --no-edit -q "$b" >/dev/null 2>&1; then
    echo "merged $b"; continue
  fi
  files=$(git diff --name-only --diff-filter=U)
  other=$(echo "$files" | grep -v -x -e CHANGELOG.md -e docs/README.md)
  if [ -z "$other" ]; then
    python3 -I "$S/union.py" $files && git add $files && git commit --no-edit -q && echo "merged $b (union: $(echo $files))"
  else
    echo "CONFLICT $b"; echo "$files"; exit 1
  fi
done
