#!/usr/bin/env bash
set -euo pipefail

# Recent SDKs reject the Runner's 10.15 target and even older CocoaPods
# targets. Override both for this build only when the SDK requires it.
sdk=$(xcrun --sdk macosx --show-sdk-path)
minimum=$(plutil -extract SupportedTargets.macosx.MinimumDeploymentTarget raw -o - "$sdk/SDKSettings.plist")
IFS=. read -r major minor _ <<< "$minimum"
if (( major > 10 || (major == 10 && minor > 15) )); then
  export FLUTTER_XCODE_MACOSX_DEPLOYMENT_TARGET="${FLUTTER_XCODE_MACOSX_DEPLOYMENT_TARGET:-$minimum}"
fi
exec flutter "$@"
