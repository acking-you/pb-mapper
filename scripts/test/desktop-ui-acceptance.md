# Native desktop UI performance acceptance

This uses the normal `DesktopApi`, `DesktopStore`, viewer widget and native
decoder with a separate test entry. Like the product UI, it automatically
requests control after a fresh presentation when the host permits it; the entry
does not itself generate pointer or keyboard actions. The host defaults to
view-only. Use `--control` only for a separate input check against a benign owned
target. The host uses the real Windows GPU capture/encoder and an existing
deployed pb-mapper service. A display with at least 90 Hz and moving content is needed to assess
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
the requested number of approximately one-second samples and disconnects. The
report contains counters and status, not images, invitations or relay credentials.

Check **every** steady-state sample for `state=streaming`, `path=direct`, H.264,
1920×1080, no error, and the SDK/native drop counts. `native_presented_fps` uses
the native texture-consumption counter delta and monotonic elapsed time after
the five-second warmup. The UI's sampled `present_fps` is also retained. Neither
decoder submissions nor encoded/received frames count as actual presentation.
Record source/receiver display refresh rates, machine/GPU, build revision and
both unique capture/receive rates alongside the result; a static image's
liveness repeats are not 90 unique desktop updates. Same-machine direct tests
do not establish independent-network or phone performance.

The local controller setup may override `relay_only` and `seconds` (15..1800)
without rebuilding. Preserve its private invitation and temporary credential,
restart the viewer, and save each report separately. Add `--control` to the
dedicated host only when testing against a benign owned input target; it defaults
to view-only. Reports also include decoder resets, recovery reasons, texture
identity, input grant state, last native-presented and transmitted-ACK frame IDs,
ACK/keyframe errors, native receive-to-presentation timings and Flutter frame
timings. Compare SDK compressed-frame drops separately from intentional native
decoded-picture skips; an unchanged timestamp/repaint is not a new presentation.

To exercise the relay separately, use `relay_only=true`. A lower adaptive frame
rate is allowed on that path; do not merge
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

## Recorded evidence and its limits

The older Windows revision's 89.58 FPS direct 1080p run and the reviewed 85.19 FPS
run retain their counters and binary hashes under `docs/validation/`; neither
certifies the current package. Build 922's recent 69–77 FPS self-view observation
was not a controlled 90 FPS benchmark.

The approximately 120-second Android 36 x86_64 / 60 Hz / RTX 5070 Ti host-OpenGL
baselines are `target/issue50-922/direct-window8-ack33.json` and
`target/issue50-922/relay-window8-ack33.json`. They measured 17.78 / 15.94 native
presented FPS, zero reconnects, SDK drops or native recoveries, and mean Surface
release-to-consume times of 48.42 / 50.53 ms. These used the eight-frame window
and 33 ms ACK interval before the later congestion-mean revision, so a final
revision needs its own measurements. They establish neither physical-phone
60 FPS nor a fixed emulator FPS limit.

Separate build 922 checks passed installed-UI exit/relaunch and Stop/Enable with
the service still Running/Automatic and the same freshly copied invitation.
The final protected installation subsequently passed hash matching, upgrade
restart, normal-UI direct/relay viewing and persistent Stop across UI restart.
A separate FFI viewer received direct/relay video while the test UIs were absent;
it used a TCP relay carrier and simulated H264 ACKs, not native presentation.
Android VM touch and trackpad gestures reached an owned Windows input target;
text and scrolling also passed. OS reboot, standalone SCM restart, physical-phone and
secure-desktop/UAC control remain unverified. See
[the current evidence summary](../../docs/remote-desktop.md#measurements-and-remaining-acceptance)
for the environment, provenance and remaining acceptance boundaries. Never
publish the private controller setup alongside these diagnostic reports.
