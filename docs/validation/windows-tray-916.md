# Windows tray stability (build 916)

2026-11-06, Windows 11 Enterprise 10.0.26300, Flutter 3.44.9.
Build 916 also includes the [helper identity repair](windows-helper-915.md).

The application store notifies the tray during background polling. Previously,
every notification set the icon and tooltip and rebuilt the entire menu, even
when none of those values changed. The vendored Windows plugin implements this
by removing and appending entries in the same HMENU; its popup message loop can
process these calls while the user is navigating that menu.

The tray now remembers rendered values and serializes native updates. Opening a
menu finishes any current update first, then defers further updates until the
native popup call returns. Repeated right clicks cannot nest popup loops. The
latest desired status and localized labels are applied once after closing;
neither native entries nor Dart click IDs change underneath an open menu.
Older status reads and completions after disposal are ignored.

Verification:

- Flutter analyzer: no issues.
- `flutter test --no-pub test/tray_service_test.dart`: two tests passed. They
  exercise the real tray service and plugin Dart event dispatch through a mock
  platform channel. Eighteen seconds of unchanged polling emits no native
  update; another eighteen seconds with an open popup, changed status and
  language still emits no menu/icon update. The original menu ID remains
  actionable. Closing applies only the latest values and reopening works.
- The second test holds a native tooltip update in flight while opening a
  popup, checks serialized ordering, and rejects stale/disposed status reads.
- The normal Windows x64 Release application compiled as `0.10.2+916`; bundled
  FFI and helper SHA-256 values match the freshly compiled release binaries.

These checks verify update and click ownership, not visual measurement of the
Windows shell. Actual secure-desktop acceptance remains outstanding as recorded
in the helper validation report. The build 914 Android APK remains compatible.
