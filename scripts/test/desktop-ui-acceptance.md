# Desktop viewer acceptance

The acceptance entry `ui/tool/desktop_viewer_acceptance.dart` runs the normal
`DesktopApi`, `DesktopStore`, viewer widget and native decoder against a
dedicated test host. Like the product UI, it requests control after a fresh
presentation when the host permits it; the entry itself generates no pointer or
keyboard input. It records one sample per second for the requested number of
seconds, disconnects with a thumbnail, and sends its report (schema 2, below)
back to the loopback bootstrap. The report holds counters and status, never
images, invitations or relay credentials.

The host defaults to view-only. Pass `--control` only against a benign target
you own. Never run the entry as the packaged application.

## Host setup

On the Windows host, from the repository root, in three terminals:

```powershell
cargo build --release -p pb-mapper-desktop --example deployed --example video_bench
target/release/examples/deployed.exe APP_CONFIG_PATH target/desktop-acceptance serve 600 --tcp --control --feedback-log
node scripts/test/desktop-bootstrap.cjs target/desktop-acceptance        # after "host_ready"
target/release/examples/video_bench.exe 600 60 --pattern-only            # moving content
```

- `APP_CONFIG_PATH` is an existing local app configuration that may issue a
  dedicated expiring relay scope. Do not paste or log its contents. Only the
  generated temporary scope enters `controller.json`, and the host revokes it
  when it exits. Use a new output directory without a stale `stop` file.
- `--tcp` is a test override of the carrier and leaves the configuration
  unchanged. For a QUIC run, restart `deployed` **without** `--tcp`: it then
  uses the carrier of `APP_CONFIG_PATH`, which must be QUIC, so check that
  before the run. `controller.json` hands the carrier to the viewer.
- `--feedback-log` installs its own filter over the desktop crate's diagnostic
  targets, which carry no secret, and deliberately ignores `RUST_LOG`. Setting
  `RUST_LOG` has no effect.
- `video_bench --pattern-only` keeps its animation window topmost so the
  viewer cannot cover the source. It injects no input. A static desktop's
  liveness repeats are not unique desktop updates.

The bootstrap listens on `127.0.0.1:18747` only:

- `GET /controller.json` returns the host's setup to the viewer.
- `PUT /report` takes the viewer's report: at most 8 MiB, and it must parse as
  JSON. The bootstrap picks the name, writes `report-<n>.json` into the
  directory it serves, and prints `report_saved <path>`.

## Phone viewer

First build the Android FFI for the phone's ABI, from the repository root
([prerequisites](../../docs/ui-updates.md#local-android-build-and-recovery)).
`ui/native/android` is git-ignored and the APK packages whatever it holds: in a
fresh checkout the tool would ship without the library, and in an older one
with a stale library.

```powershell
cargo ndk -o ui/native/android -t arm64-v8a build -p pb-mapper-ffi --release
```

From `ui/`, build and install the tool:

```powershell
flutter build apk --release --split-per-abi --target-platform android-arm64 `
  --target tool/desktop_viewer_acceptance.dart `
  --dart-define=setupUrl=http://127.0.0.1:18747/controller.json --dart-define=seconds=120 `
  --dart-define=quality=balanced
adb install -r build\app\outputs\flutter-apk\app-arm64-v8a-release.apk
adb reverse tcp:18747 tcp:18747
```

Then start the installed app.

- The x86_64 emulator builds the FFI with `-t x86_64`, takes
  `--target-platform android-x64` and installs `app-x86_64-release.apk`.
- Build split per ABI, as the release does: a split APK's versionCode is the
  build number plus 2000 on arm64 or 4000 on x86_64 (2925 or 4925 at `+925`),
  as the product's is. A universal build carries the bare build number, and
  `adb install -r` refuses it over the product as a downgrade.
- Relay-only runs add `--dart-define=relayOnly=true`. `deployed` writes no
  `relay_only` into `controller.json`, so the define decides. A local
  `controller.json` may still override `relay_only` and `seconds` (15..1800)
  without a rebuild.
- `quality` (`quality`, `balanced` or `smooth`) is a report schema 2 define;
  the Stage 1 tool, built from tag `ovh-g1`, ignores it.
- Two optional schema 2 defines label the report: `case` (for example
  `--dart-define=case=A`) and `host`, the host's GPU and display refresh,
  which a viewer cannot see (for example
  `--dart-define="host=RTX 5070 Ti, 60 Hz"`).
- At the end of the run the tool sends its report with
  `PUT http://127.0.0.1:18747/report` through the same `adb reverse`. Nothing
  is read from the app's private storage, so a release APK, which is not
  debuggable and cannot use `run-as`, works.
- The release build needs the pinned signing key
  (`ui/android/app/build.gradle.kts`). If it does not resolve, restore it (the
  build forbids generating a replacement), or build the acceptance APK with
  `--profile` (`app-arm64-v8a-profile.apk`). Only if that APK's signature
  differs from the installed product's does `adb install -r` need
  `adb uninstall com.pb_mapper.ui` first. Never answer any other failed
  `install -r`, a downgrade above all, with an uninstall: it deletes the
  phone's app data, and the relay credential and remembered computers with it.

Never install the tool as the product entry. Reinstall the product APK
afterwards.

## Windows viewer

Build the FFI library and helper with `scripts/build/pb-mapper-ffi-windows.ps1`,
then from `ui/`:

```powershell
flutter build windows --release --target tool/desktop_viewer_acceptance.dart `
  --dart-define=setupUrl=http://127.0.0.1:18747/controller.json --dart-define=seconds=120 `
  --dart-define=quality=balanced
```

Launch it from its build directory. The report reaches the bootstrap directly
over loopback. Restore the normal entry before producing any product artifact:
`flutter build windows --release --target lib/main.dart`.

## After a run

Create `target/desktop-acceptance/stop` and let the host exit normally. It
revokes its scope and removes `controller.json`. Stop the bootstrap and close
the viewer. Keep each `report-<n>.json` with the host `feedback` log of its
case. Never publish the bootstrap directory or a controller setup alongside
the reports.

Record with every case the build, quality preset, both refresh rates, the host
GPU, the phone, its decoder, temperature and the network. A schema 2 report
carries all of them but the host's, which the `host` define supplies. Where
the report's battery temperature is null (Android may refuse an app the
sensor), read it before and after the run with
`adb shell dumpsys battery` (`temperature` is in tenths of a degree).

## Report schema 2

One JSON object:

- `schema` (2), `case`, `quality`, `relay_only`, `decoder_hints`,
  `transport`, `seconds`, `warmup_seconds` (5).
- `build`: the app version, build number and mode (`release`, `profile` or
  `debug`).
- `device`: the OS and its version, processors, the display refresh rate
  and, on Android, the manufacturer, model and SDK level.
- `network`: the interface names (never addresses) and `type`, the first of
  `wifi`, `ethernet` and `cellular` among them.
- `host`: the `host` define, plus the `rate_model` (and `gop` when the
  viewer status carries it) from the last sample. The host's
  `desktop_feedback_interval` log has `gop` and stays the authority for
  host-side values.
- `decoder`: the viewer status's decoder (`name`, `hardware`, `state`) at
  the end.
- `battery_celsius`: `start` and `end`, or null.
- `connect_ms`: from the connect to the first live picture.
- `disconnect`: whether the picture was `fresh`, `disconnect_ms` (the
  thumbnail and the disconnect together), and whether the thumbnail was
  written and its size (D23: at most 300 ms, never blocking).
- `samples`, one per second: `elapsed_us`, the store's `phase`,
  `reconnect_attempt`, `failure`, the app `lifecycle`, `status` (the
  `viewer_status` exactly as Rust reported it: state, `stats` with
  `presented_fps`, `received_fps`, `picture_latency_ms`, `rtt_ms`,
  `adaptation`, `rate_model`, `fps_cap`, `keyframes`, `keyframes_requested`,
  `backlog_drops`, `present_interval` with its p50/p95/p99/max and the session
  totals `gaps_100`/`gaps_250`, and `decoder`), `native` (the native
  decoder's own counters, where there is one) and `flutter`: the frame
  timings Flutter reported in that second (it batches them about once a
  second), with the frames whose build or raster took longer than one
  refresh interval as `over_budget`.
- `events`: each phase change (with its failure code), each app lifecycle
  change, and after each return from the background `resume_picture_ms`, the
  time to the first new presented picture read from the view leaf (null
  when none came within 10 s): case E.
- `summary`: the perf 11.4 measures over the samples after the warm-up, with
  no verdict. Presented and received fps, latency and RTT as p50/min/max over
  live seconds; the worst `present_interval` percentiles; the rise of
  `gaps_100` (also per minute), `gaps_250`, `keyframes`,
  `keyframes_requested`, `backlog_drops` and `dropped_frames` (counted across
  reconnects); the seconds in each `adaptation` and `fps_cap`, and degraded;
  the resolutions seen; reconnects; the resume times; and the Flutter frame
  totals.

## Stages

Stage 1 needs the report schema 1 tool and the old app pipeline, which exist
only at tag `ovh-g1`. Build its tool in a checkout of that tag with the
commands above, the FFI build first, so the APK carries that tag's library;
the tag's build number is also 925, so it installs with `-r` over a product of
build 925. Stages 2 and 3 use this tree's schema 2 tool, which replaces it.

### Stage 1: the old pipeline on the new core

The phone runs the old app pipeline, with the schema 1 tool, against the
rewritten Rust core.

- Case A: direct Wi-Fi, 60 Hz phone.
- Case B: relay only (`relayOnly=true`), over TCP (`deployed --tcp`), then over
  QUIC (`deployed` restarted without `--tcp`).

Look for: no "burst, stall, burst" (no gaps over 250 ms while the RTT is
stable), skipped capture ticks without a pause, and a resolution that never
changes. This stage is evidence, not pass or fail.

### Stage 2: smoke on both viewers

On the phone and on a Windows viewer:

- connect;
- take control, and move, click and type;
- background or minimise for 10 s, then resume: a picture within 1 s and no
  reconnect. On Windows also hide to the tray with the close button and
  restore;
- on the phone, background for 60 s and for 5 min and note whether Android
  froze the app (it then reconnects once on resume);
- toggle Wi-Fi: it reconnects, and the last picture stays grey under the
  reconnect pill;
- disconnect while `video_bench` animates: the thumbnail file is written on
  both platforms.

Also confirm that the native single-instance tokens match in the debug log.

### Stage 3: full acceptance

The pass criteria are proposals; the owner decides, and the merge-block
statements stay until the owner accepts fluidity.

1. Performance:
   - A: direct Wi-Fi, Balanced, 60 Hz: presented fps p50 ≥ 55, interval p95
     ≤ 25 ms and p99 ≤ 50 ms, no gap over 250 ms, latency p50 ≤ 80 ms, plus
     one camera check (20 readings against the host's "Unique desktop
     presentation N").
   - B: relay over TCP and over QUIC: ≥ 30 fps, no burst-stall.
   - C: cellular.
   - D: Smooth and Quality, and a Windows viewer at the display refresh.
   - E: background 10 s, then resume, five times, then once each for 60 s and
     5 min. Without a freeze: a picture within 1 s and no reconnect. After a
     freeze: one immediate reconnect and a picture within 3 s. Always at most
     two IDRs per resume, and no input replay.
   - F: decoder hints on versus off: A, then A again with `deployed … serve
     --no-decoder-hints`, which writes `decoder_hints: false` into
     `controller.json`; no rebuild. The report's `decoder_hints` is the arm
     asked for and each sample's `native.lowLatency` the one that ran. A
     hinted start that fails for any reason is retried once without the hint,
     so a false `lowLatency` on the hints-on arm needs logcat's
     `PbDesktopVideo` warning to say why.
   - G: UAC prompt, lock screen and Win+L through the helper.
   - H: RustDesk side by side.
   - I: `video_bench --pace 60,30,15,5` and `video_bench 60 60` on each host
     GPU.
2. Windows chrome: at 100, 125 and 150 % scaling, maximised and restored, the
   close button sits flush in the window's top-right corner, as tall as the
   page's title bar; its hover red reaches both edges with no fringe, no
   backdrop shows around the three buttons, and the glyphs are crisp. With a
   dialog open the buttons stay undimmed and still respond; switching pages
   never moves or fades them; their glyphs dim while another window is
   active. At the narrowest window (720 px) while viewed, the capsule's dot
   menu opens over the buttons, no hover grey paints over it, and Disconnect
   viewer responds right up to its top edge. A restored window resizes from
   its top edge, over the buttons too, and still drags just below it. The
   icons look right on every page.
3. Navigation: six rail entries; five bar entries on the phone; Logs is
   reachable from Settings, and its back button returns.
4. Host (Windows):
   - enable remote control, through UAC and the helper;
   - view only versus view and control;
   - copy the invite; the QR shows and hides, and hides when you leave the
     page;
   - Make view-only takes effect live;
   - the sharing capsule (Disconnect viewer, Stop sharing) appears while
     viewed and stays reachable over a dialog and in a full-screen viewer;
     the tray offers both;
   - hide the window to the tray and connect from the phone: the tray shows
     the active icon and "Someone is viewing this computer", and the window
     comes back without taking focus, showing the capsule;
   - while the phone is on "Back to list", the capsule, tray and host card say
     "Connected · paused"; after 10 min the phone ends the session and a
     second viewer can connect;
   - with the background host configured, the display picker and rename are
     disabled with "Stop background sharing to change this";
   - an outdated helper offers "Update".
5. Add a computer:
   - phone: scan the QR; deny the camera once (paste still works);
   - desktop: paste; a setup string is rejected with its message;
   - Remember is on; connect, disconnect (the thumbnail is written), restart
     the app: the computer is still listed with its online dot and thumbnail,
     on Android and on Windows;
   - Forget removes it and its thumbnail;
   - Android backup excludes the secrets: with the local transport
     (`adb shell bmgr transport com.android.localtransport/.LocalTransport`),
     `adb shell bmgr backupnow com.pb_mapper.ui`, then uninstall, reinstall and
     restore (`adb shell bmgr restore`): the restored app has no relay
     credential, no tunnels and no remembered computers.
6. Desktop viewer:
   - the toolbar pins, and unpinned it collapses 2 s after the pointer leaves;
   - Display: fit or original, quality presets live, no reconnect;
   - Shortcuts: Win+D, Alt+Tab, Ctrl+Shift+Esc, Win; Win+L locks;
     Ctrl+Alt+Del is shown as unsupported;
   - the six overlays: Wi-Fi off gives reconnecting; a host lock gives the
     secure desktop;
   - a German keyboard layout keeps AltGr characters; Ctrl+S, Alt+F and Win+E
     work;
   - with a Russian host layout, Ctrl+C, Ctrl+V and Win+E still work and type
     no stray letters; the same for a latched Ctrl+C from the phone's
     modifier bar.
7. Mobile viewer:
   - landscape lock; gutters;
   - trackpad mode by default: a tap clicks at once; a long press
     right-clicks; double-tap-and-hold drags (an Explorer icon moves, it does
     not open); three fingers scroll; two fingers pinch and pan locally;
   - modifier bar: Ctrl one-shot, lock and release; the latches clear when the
     app goes to the background;
   - the soft keyboard keeps zoom and offset;
   - the minimap shows; the virtual mouse works in direct mode; the gesture
     sheet shows once;
   - the IME works on Samsung and Gboard; deleting a long word or a selection,
     and pasting a paragraph, keep control.
8. Compatibility (ALPN `pb-mapper/desktop/2`): the new viewer against a PR #51
   host ends on the "update pb-mapper" message; a PR #51 viewer against the
   new host fails at the TLS handshake, never mid-session.

Secure-desktop and UAC acceptance of the helper is a separate matrix in
[windows-helper-acceptance.md](windows-helper-acceptance.md).

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
Build 922's Android VM run of the PR #51 touch and trackpad gestures, since
replaced, reached an owned Windows input target; text and scrolling also
passed. The current gestures are Stage 3, item 7. OS reboot, standalone SCM
restart, physical-phone and secure-desktop/UAC control remain unverified. See
[the current evidence summary](../../docs/remote-desktop.md#measurements-and-remaining-acceptance)
for the environment, provenance and remaining acceptance boundaries.
