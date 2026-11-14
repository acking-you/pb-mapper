# UI updates and mobile releases

The updater follows proxy-everything's flow: download inside the application,
show progress and cancellation, verify the Ed25519 release manifest and the
file's exact size/SHA-256, then hand it to the appropriate platform installer.
The system still asks for installation consent.

| Platform / installation | Update action |
| --- | --- |
| Android | Download to private cache, check package ID, certificate and increasing effective versionCode, request permission to install packages, then open Android's installer using a read-only content URI. Returning from permission settings continues automatically. Retry reuses and rechecks the downloaded APK. |
| Windows installer | Download and verify, then start the Inno Setup installer with `/SILENT /UPDATEPID=<the app's process>` while the app is still open and in front. For a machine-wide install Setup starts itself again elevated, so Windows asks for permission in front of the app; the app quits once Setup's log (`setup.log` beside the download) exists, which Setup starts only after that permission. Setup waits up to a minute for the app to close before it replaces a file, and when it ends, installed or not, opens the installed app again (`{app}`, as the user who started it). If Setup ends before that point (permission refused), the app stays open and Retry reuses the verified download. The next launch checks whether the requested version actually installed and removes the installer, and the log too when it did. |
| Windows portable | Download and verify the ZIP, select it in Explorer for replacement by the user. |
| Linux x64 AppImage | Download and verify, replace the AppImage, reopen; save to Downloads if its location cannot be written. |
| Linux x64 portable | Download and verify the ZIP, open Downloads. Other architectures report no package. |
| macOS Intel / Apple Silicon | Download the matching DMG, verify it, save it to Downloads and open the system disk-image installer. Drag the app into Applications. |
| iOS | Use a properly signed development, TestFlight or App Store distribution. An unsigned IPA is not an installable consumer update. |

### The Windows installer and the update

The installer's script is checked in as
[`ui/windows/packaging/exe/inno_setup.iss`](../ui/windows/packaging/exe/inno_setup.iss),
which fastforge renders from `make_config.yaml`. It is fastforge's default
script plus the update contract above in its `[Code]` section, and it writes a
log for every run (`%TEMP%\Setup Log <date> #<n>.txt` when started by hand).
Its AppId, default folder and `PrivilegesRequired=none` decide which earlier
installation an upgrade finds: an administrator's installs go to
`C:\Program Files\pb_mapper_ui` for all users after a permission prompt,
anyone else's to that user alone.

The script names the app, `pb_mapper_ui.exe`, itself. fastforge would take the
first `.exe` in the bundle, which since 0.11.0 is
`pb-mapper-desktop-helper.exe`, and 0.11.0's installer did: its Start menu,
desktop and startup shortcuts and its "Launch pb-mapper" box started the
helper, which shows nothing and exits, so pb-mapper seemed not installed and
never started. Installing 0.12.0 rewrites those shortcuts; one pinned to the
taskbar from 0.11.0 has to be pinned again.

The release workflow installs the built installer on its runner the way the
app runs it, and uploads it only if Setup waited for the app, installed the
bundle, opened the installed app again and left shortcuts that open it.

Before 0.12.0 the app quit first and left a hidden PowerShell to run Setup.
Setup's permission prompt then came from a process with no window after the
app had gone, which Windows may leave behind other windows as a flashing
taskbar button; PowerShell waited for Setup without a limit, so while the
prompt went unseen nothing was installed or opened again. That PowerShell
also reopened the executable the app ran from, which since 0.11.0 can be the
desktop helper's protected copy of the older release under
`C:\Program Files\pb-mapper-desktop`. A copy of 0.11.0 or older still updates
that way: install 0.12.0 by hand once, and updates from it on take the new
path.

Setup neither closes nor replaces that protected copy: an app running from it
during an upgrade by hand stays the old version until it is quit. pb-mapper
opened from the Start menu afterwards, or reopened by the installer, is the
new one. Its Remote desktop page shows the sharing as the older service's,
which keeps sharing as it was set up, and offers to update the service: that
installs a new protected copy and keeps the service's settings and
invitation, which the page shows again once the app has restarted into it.

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
issue #50 Windows/Android test distribution (version 0.10.2); build 914 is its
mobile interaction and recovery repair. Build 915 is the Windows service
identity-query permission repair. Build 916 includes that repair and stable
tray menus during background status updates. Build 917 also fixes native popup
reopening and preserves late menu clicks. Build 918 repairs Windows session
privilege adjustment and distinguishes session-query failures from an absent
owner login. Build 919 replaces the separate installed-app launch with an
acknowledged, automatic restart after helper installation. Builds 920 to 927
were consumed by later issue #50 test distributions (version 0.10.2), and
0.11.0 uses 928. The next distribution must use at least 929. The build counter
is independent of the public version.

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
covers download, integrity, cancellation, retry, platform selection, and when
the app counts the Windows installer as started. The installer's own side
(waiting for the app, installing, opening it again) is checked by the release
workflow on a real install.

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
