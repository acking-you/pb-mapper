# Native desktop UI performance acceptance

This uses the normal `DesktopApi`, `DesktopStore`, viewer widget and native
decoder with a separate test entry. It never requests input control. The host
uses the real Windows GPU capture/encoder and an existing deployed pb-mapper
service. A display with at least 90 Hz and moving content is needed to assess
the 1080p direct-viewing target. A 60 Hz Android VM is useful for compatibility,
but cannot establish 90 presented FPS.

Run commands from the repository root unless indicated. Build the Windows FFI
and helper with `scripts/build/pb-mapper-ffi-windows.ps1`, using the existing
permitted script environment. Build the probes:

```powershell
cargo build --release -p pb-mapper-desktop --example deployed --example video_bench
```

In `ui/`, compile the explicit test entry. Replace `ABSOLUTE_REPORT_PATH` with
an absolute path under a dedicated local test directory, using forward slashes.
The defines contain no invitation or credential. `relayOnly=false` uses the
product's normal direct-first selection; it does not prove the chosen path.

```powershell
flutter build windows --release --target tool/desktop_viewer_acceptance.dart `
  --dart-define=setupUrl=http://127.0.0.1:18747/controller.json `
  --dart-define=reportPath=ABSOLUTE_REPORT_PATH `
  --dart-define=seconds=30 --dart-define=relayOnly=false
```

Start the host in a separate terminal. `APP_CONFIG_PATH` is an existing local
app configuration with permission to issue a dedicated expiring relay scope;
do not paste or log its contents. Only the generated temporary scope enters the
controller setup. TCP is an explicit test override and does not modify the app
configuration. Use a new dedicated output directory without a stale `stop` file.

```powershell
target/release/examples/deployed.exe APP_CONFIG_PATH target/desktop-acceptance serve 600 --tcp
```

Once it prints `host_ready`, start the loopback bootstrap in another terminal:

```powershell
node scripts/test/desktop-bootstrap.cjs target/desktop-acceptance
```

Run `target/release/examples/video_bench.exe 60 90 --pattern-only` to supply
moving content, then launch the newly built UI from its build directory. The
pattern program keeps its own animation window topmost so the viewer cannot
occlude the source; it does not inject input. Avoid
other builds and system changes during the measurement. The viewer records
30 one-second samples and disconnects. The report contains counters and status,
not images, invitations or relay credentials.

Check **every** steady-state sample for `state=streaming`, `path=direct`, H.264,
1920×1080, no error, and the SDK/native drop counts. `native_presented_fps` uses
the native texture-consumption counter delta and monotonic elapsed time after
the five-second warmup. The UI's sampled `present_fps` is also retained. Neither
decoder submissions nor encoded/received frames count as actual presentation.
Record source/receiver display refresh rates, machine/GPU, build revision and
both unique capture/receive rates alongside the result; a static image's
liveness repeats are not 90 unique desktop updates. Same-machine direct tests
do not establish independent-network or phone performance.

To exercise the relay separately, rebuild with `relayOnly=true` and a different
report path. A lower adaptive frame rate is allowed on that path; do not merge
its results with the direct samples. For an Android VM, the corresponding
explicit APK entry can use ADB's loopback bootstrap forwarding, subject to the
device's existing installation/debugging authorization. A blocked installation
is not a performance result.

Afterward create `target/desktop-acceptance/stop` and allow the host to exit
normally. It revokes its scope and removes `controller.json`. Stop the bootstrap
and close the test UI. Do not publish the bootstrap directory or its contents.
Finally restore the normal package entry before producing any product artifact:

```powershell
# From ui/
flutter build windows --release --target lib/main.dart
```

These results cover ordinary-desktop video only. Secure-desktop/UAC acceptance
is a separate matrix in [windows-helper-acceptance.md](windows-helper-acceptance.md).
