# UI updates and mobile releases

The updater follows proxy-everything's flow: download inside the application,
show progress and cancellation, verify the Ed25519 release manifest and the
file's exact size/SHA-256, then hand it to the appropriate platform installer.
The system still asks for installation consent.

| Platform / installation | Update action |
| --- | --- |
| Android | Download to private cache, check package ID, certificate and increasing effective versionCode, request permission to install packages, then open Android's installer using a read-only content URI. Returning from permission settings continues automatically. Retry reuses and rechecks the downloaded APK. |
| Windows installer | Download and verify, quit, run Inno Setup, reopen. The next launch checks whether the requested version actually installed. |
| Windows portable | Download and verify the ZIP, select it in Explorer for replacement by the user. |
| Linux x64 AppImage | Download and verify, replace the AppImage, reopen; save to Downloads if its location cannot be written. |
| Linux x64 portable | Download and verify the ZIP, open Downloads. Other architectures report no package. |
| macOS Intel / Apple Silicon | Download the matching DMG, verify it, save it to Downloads and open the system disk-image installer. Drag the app into Applications. |
| iOS | Use a properly signed development, TestFlight or App Store distribution. An unsigned IPA is not an installable consumer update. |

macOS currently uses a sandboxed app and has no configured Developer ID / Apple
notarization release identity. Unlike CipherRelay's signed, notarized bundle
replacement, pb-mapper therefore opens the DMG for installation. It does not
silently replace bundles or disable Gatekeeper. An opener failure is reported
as failure, rather than saying the installation opened successfully.

## Android signing baseline

The published `v0.9.4-ui` and `v0.9.5-ui` arm64 APKs have different Android
debug certificates. Their SHA-256 fingerprints are respectively
`e6ababd7ed4a4af294c10e0a0f2fb1a1aa957dcfae0cfb5a94aea40e8e105204` and
`b47635fe08e83a3d841250bad740ec70c6c6f1d766684bc53fcf87ed52d91454`.
The 0.9.5 APK itself verifies, has versionName `0.9.5`, versionCode `2905`,
requires Android API 24+, and passes 16 KiB ZIP and native-library alignment.
This proves an upgrade-signing defect; it does not establish the cause of a
particular phone's browser failing to open its downloaded APK.

Like proxy-everything, distributed mobile packages are now built **locally**.
CI publishes desktop packages and their signed manifest. It never distributes
runner-signed APKs or unsigned IPAs. Android release Gradle tasks refuse CI,
missing keystores and certificates other than
[`android_signing_certificate.sha256`](../scripts/release/android_signing_certificate.sha256).
The existing maintainer key is `%USERPROFILE%\.android\debug.keystore` (or
`~/.android/debug.keystore`). Back it up securely. Never regenerate it or
change the application ID `com.pb_mapper.ui` to work around installation.
Only the public certificate fingerprint is checked in.

There is no in-place signing migration from the disposable keys used by old
CI releases without their private keys. Export the user's pb-mapper settings
before any manual uninstall/reinstall migration to the new signing baseline.
Do not claim the new key can cover-install those old packages. Once on this
baseline, later locally signed releases can update in place.

## Prepare a local Android release

Use Flutter 3.44.9, matching the desktop release and Flutter checks. Bump the
workspace/UI release version and pubspec build number for a real release;
never reuse a published build. Flutter split APKs add ABI-specific offsets to
the build number, so compare **effective** versionCodes in the APKs.
Build 906 was consumed by the local repair; release 0.9.6 uses build 907, 0.9.7
consumed 908 without shipping APKs, and 0.9.8 uses 909. The next distribution
must use at least 910. The build counter is independent of the public
version, as in proxy-everything.

1. Rebuild the Android Rust library with `make build-pb-mapper-ffi-android`
   (Android NDK r28+ recommended). On Windows, `cargo ndk -o ui/native/android
   -t arm64-v8a build -p pb-mapper-ffi --release` works with the Android target
   installed and `ANDROID_NDK_HOME` pointing to the installed NDK. Build only
   the ABIs that will be packaged. Never package stale libraries after Rust
   changes.
2. From `ui`, run `flutter pub get`, `flutter analyze --no-fatal-infos`, and
   `flutter test`. Then `flutter build apk --release --split-per-abi
   --target-platform android-arm64` (add other target platforms when needed).
3. Run the verifier on every resulting APK. It checks the real package ID,
   version, ABI, certificate, signature, FFI presence, and 16 KiB ZIP/ELF
   alignment. Pass the last locally signed release for upgrade comparison:

   ```sh
   python scripts/release/android_package.py ui/build/app/outputs/flutter-apk/app-arm64-v8a-release.apk --build-tools /path/to/Android/Sdk/build-tools/36.0.0 --version 0.9.8 --previous /path/to/previous/pb-mapper-arm64-v8a.apk
   ```

4. Copy the verified APK into an ignored staging directory as
   `pb-mapper-arm64-v8a.apk` (or `pb-mapper-armeabi-v7a.apk` /
   `pb-mapper-x86_64.apk`). Record the verifier's size, SHA-256, effective
   versionCode and certificate output with the local release.
5. After the desktop release workflow completes, upload the APKs without
   `--clobber`, then run **Publish verified Android updates**:

   ```sh
   gh release upload v0.9.8-ui release/pb-mapper-*.apk
   gh workflow run publish-android.yml -f tag=v0.9.8 -f previous_release=v0.9.6-ui
   ```

   `previous_release` names the last fixed-key release; only v0.9.6, the first,
   ran with `-F establish_signing_baseline=true` instead. CI verifies the
   existing manifest's signature and source commit, then the attached APKs'
   pinned certificate, identity, alignment and
   increasing versionCode. It signs the extended manifest using the existing
   `UPDATE_SIGNING_KEY` secret. The Android private key stays local. Re-running
   permits byte-identical APKs, never replacement of a published package.

For maintainers signing the manifest offline, the equivalent steps are:

1. Download the **same release's** desktop manifest and signature and verify
   them using `ui_release_manifest.py verify`. Add the local APKs:

   ```sh
   python scripts/release/ui_release_manifest.py add-android release/release-manifest.json release/pb-mapper-arm64-v8a.apk --build-tools /path/to/build-tools/36.0.0 --previous-directory /path/to/previous
   ```

   Only for the first fixed-key release, use `--establish-signing-baseline`
   instead of `--previous-directory`. This does not waive the pinned-key,
   package-identity or alignment checks. It acknowledges the migration above.
   Adding packages invalidates and removes the old local manifest signature.
2. Re-sign with the **existing** Ed25519 update key and verify again:

   ```sh
   python scripts/release/ui_release_manifest.py sign release/release-manifest.json --key /secure/path/private.pem
   python scripts/release/ui_release_manifest.py verify release/release-manifest.json --public-key scripts/release/update_signing_key.pub.pem
   ```

3. After release authorization, attach the APKs, then replace that release's
   manifest/signature pair. Until both are available, an update fails closed
   and can be retried. Never replace an existing APK or reuse its version.
   A release without APKs remains usable for desktop updates; Android shows
   that its package is not available yet.

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
