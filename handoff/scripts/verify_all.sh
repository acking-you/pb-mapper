#!/bin/bash
# Rust workspace checks plus the Flutter app checks; run from the worktree root.
S=/private/tmp/claude-501/-Users-lb7666--claude-spares-spare-vPIbq7/2de9709e-ea4b-442d-a692-7f9e8fd50cdd/scratchpad
bash "$S/verify.sh" "$S/${VERIFY_TAG:-verify4}"
echo "== flutter"
cd ui || exit 1
F=~/Developer/flutter/bin
$F/flutter gen-l10n >/dev/null 2>&1; echo "GEN_L10N_EXIT=$?"; echo "untranslated: $(cat l10n_untranslated.json 2>/dev/null)"
$F/flutter analyze --no-fatal-infos > "$S/analyze.txt" 2>&1; echo "ANALYZE_EXIT=$?"
grep -E ' error | warning ' "$S/analyze.txt" | grep -v deprecated_member_use | head -10
$F/flutter test > "$S/flutter_test.txt" 2>&1; echo "FLUTTER_TEST_EXIT=$?"
tail -3 "$S/flutter_test.txt"
grep -E '\[E\]|FAILED|Some tests failed' "$S/flutter_test.txt" | head -10
cd ..
git checkout -- ui/pubspec.lock ui/analysis_options.yaml ui/macos/Flutter 2>/dev/null
rm -f ui/macos/Podfile
git status --short
