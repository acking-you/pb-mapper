# UI updates and mobile releases

The updater follows proxy-everything's flow: download inside the application,
show progress and cancellation, verify the Ed25519 release manifest and the
file's exact size/SHA-256, then hand it to the appropriate platform installer.
The system still asks for installation consent.

| Platform / installation | Update action |
| --- | --- |
| Android | Download to private cache, check package ID, certificate and increasing effective versionCode, request permission to install packages, then open Android's installer using a read-only content URI. Returning from permission settings continues automatically. Retry reuses and rechecks the downloaded APK. |
| Windows installer | Download and verify, then quit: a hidden PowerShell waits for the app to exit, runs the Inno Setup installer with `/SILENT` (it elevates itself for a machine-wide install, so Windows asks for permission), writes `setup.log` beside the download and reopens the app. The FFI's `pb_mapper_spawn_hidden_powershell_v1` starts it with a windowless console of its own; as a detached Dart process it had no console and exited without running a line, so no update installed before 0.9.5. The next launch checks whether the requested version actually installed. |
| Windows portable | Download and verify the ZIP, select it in Explorer for replacement by the user. |
| Linux x64 AppImage | Download and verify, replace the AppImage, reopen; save to Downloads if its location cannot be written. |
| Linux x64 portable | Download and verify the ZIP, open Downloads. Other architectures report no package. |
| macOS Intel / Apple Silicon | Download the matching DMG, verify it, save it to Downloads and open the system disk-image installer. Drag the app into Applications. |
| iOS | Use a properly signed development, TestFlight or App Store distribution. An unsigned IPA is not an installable consumer update. |

Since 0.9.7 the macOS app and its DMG are signed with the project's Developer
ID, notarized and stapled, and the mounted DMG passes Gatekeeper before it is
uploaded ([macOS signing](macos-signing.zh-CN.md)). The app is sandboxed, so
unlike CipherRelay it cannot replace its own bundle in Applications: it opens
the verified DMG for installation instead, and never removes quarantine or
disables Gatekeeper. An opener failure is reported as failure, rather than
saying the installation opened successfully.

## Android signing baseline

The published `v0.9.4-ui` and `v0.9.5-ui` arm64 APKs have different Android
debug certificates. Their SHA-256 fingerprints are respectively
`e6ababd7ed4a4af294c10e0a0f2fb1a1aa957dcfae0cfb5a94aea40e8e105204` and
`b47635fe08e83a3d841250bad740ec70c6c6f1d766684bc53fcf87ed52d91454`.
The 0.9.5 APK itself verifies, has versionName `0.9.5`, versionCode `2905`,
requires Android API 24+, and passes 16 KiB ZIP and native-library alignment.
This proves an upgrade-signing defect; it does not establish the cause of a
particular phone's browser failing to open its downloaded APK.

The maintainer's existing key is `%USERPROFILE%\\.android\\debug.keystore` (or
`~/.android/debug.keystore`). The repository's `ANDROID_KEYSTORE_BASE64` Actions
secret contains this same key, not a runner-generated key. Keep a secure backup
of the original. Never regenerate it or change the application ID
`com.pb_mapper.ui` to work around installation. Only the public certificate
fingerprint is checked into the repository.

Release Gradle tasks require a keystore matching
[`android_signing_certificate.sha256`](../scripts/release/android_signing_certificate.sha256).
A CI release must also explicitly set `PB_MAPPER_ANDROID_KEYSTORE`; a missing,
wrong or runner-generated key fails the build. The existing key uses the Android
debug keystore's standard alias and passwords. The workflow decodes the secret
into a private temporary file, passes its path to Gradle and deletes it after the
build. Never store the private key in the repository, a workflow artifact or an
Actions variable.

There is no in-place signing migration from the disposable keys used by old
CI releases without their private keys. Export the user's pb-mapper settings
before any manual uninstall/reinstall migration to the new signing baseline.
Do not claim the new key can cover-install those old packages. Once on this
baseline, later releases signed with this key can update in place.

## Automated Android release

A `vX.Y.Z` tag builds ARMv7, ARM64 and x86_64 APKs using Flutter 3.44.9 and
fresh Rust FFI libraries. The workflow verifies the pinned certificate, package
identity, ABI, signature, 16 KiB alignment and actual FFI contents. It finds the
newest earlier release containing Android packages among the 100 most recent
releases, verifies that release's signed manifest, and requires each effective
APK versionCode to increase. A missing or incomplete previous Android release
fails closed. The APKs are uploaded without replacing an existing asset, then
the desktop and Android entries are published together in the Ed25519-signed
update manifest. A retry accepts an existing APK only if its size and SHA-256
match the verified build.

Bump the workspace/UI release version and pubspec build number for a real
release; never reuse a published build. Flutter split APKs add ABI-specific
offsets to the build number, so compare **effective** versionCodes in the APKs.
Build 906 was consumed by the local repair; release 0.9.6 uses build 907, 0.9.7
consumed 908 without shipping APKs, 0.9.8 uses 909, 0.10.0 uses 910 and
0.10.1 uses 911 and 0.10.2 uses 912. Build 913 is consumed by the local
issue #50 Windows/Android test distribution (version 0.10.2). The next
distribution must use at least 914. The build counter is independent of the
public version.

To verify the complete Android build without publishing a release, run
`release-ui.yml` with `android_verify_only=true` against the desired branch.
The verified packages are available as the `android-release` workflow artifact.
This check signs an unpublished test manifest using the previous release's
desktop packages; it never changes that release. The previous signing baseline
must be present. The existing `UPDATE_SIGNING_KEY` secret signs both test and
release manifests.

## Local Android build and recovery

Use Flutter 3.44.9 and rebuild the Android Rust library with
`make build-pb-mapper-ffi-android` (Android NDK r28+ recommended). On Windows,
`cargo ndk -o ui/native/android -t arm64-v8a build -p pb-mapper-ffi --release`
works with the Android target installed and `ANDROID_NDK_HOME` pointing to the
installed NDK. Build only the ABIs that will be packaged. Never package stale
libraries after Rust changes.

From `ui`, run `flutter pub get`, `flutter analyze --no-fatal-infos`, and
`flutter test`, then `flutter build apk --release --split-per-abi
--target-platform android-arm64` (add other target platforms when needed). Run
`scripts/release/android_package.py` on each package with `--version`,
`--build-tools`, and `--previous` pointing to the corresponding previous APK.
The verifier checks its identity, signature, certificate, ABI, native libraries,
alignment and upgrade versionCode.

For repair of an already-published desktop-only release, the existing
`publish-android.yml` workflow can verify separately uploaded APKs against a
specified previous release and re-sign the manifest. Do not replace existing APKs
or reuse a published build. `ui_release_manifest.py add-android` also accepts a
previous signed manifest using `--previous-manifest`; it authenticates that
manifest before comparing package identity and version codes. If re-signing a
manifest offline, use the existing Ed25519 update key and verify the signature
before attaching the manifest and its signature together.

## Acceptance checks

`python -m unittest discover -s scripts/release -p 'test_*.py' -v` covers release
composition, signing continuity and increasing APK versionCodes.
`flutter test test/update_test.dart test/update_io_test.dart test/version_test.dart`
covers download, integrity, cancellation, retry and platform selection; with a
built Windows FFI it also runs the actual hidden process handoff.

On Android, verify both a clean install and an upgrade from the preceding
**same-certificate** APK, first with install permission disabled and then
enabled. Confirm returning from Settings opens the installer, cancelling it
leaves the app usable, Retry avoids a second network download, and config
survives the upgrade. Also confirm a mismatched certificate is rejected.
Desktop installer execution and macOS/Linux acceptance require those actual
OS environments; a Dart test is not proof of successful system installation.

For a local Android system-installer smoke test without publishing anything,
`ui/tool/android_update_smoke.dart` is a debug-only target. Serve a verified,
newer, same-certificate APK at `http://127.0.0.1:8765/update.apk`, forward that
port with `adb reverse tcp:8765 tcp:8765`, and run the target with a lower build
number. Its Install fixture button exercises the real permission and content-URI
handoff; never distribute that test app.
