# pb-mapper UI

**Remote desktop:** the **Remote desktop** page (the **Remote** tab on a phone)
shares a Windows desktop and views or controls other computers inside the app.
It uses the existing relay configuration, separate desktop permission, endpoint
encryption and native H.264 textures on Windows/Android. **Turn on remote
control** prepares the Windows helper, persistent service, input permission and
reusable invitation in one flow. Stop pauses sharing while retaining the
invitation; Revoke invalidates it. A viewer can remember the computers it
opens, in an encrypted vault on Windows and Android. The separately installed
UAC helper is implemented; actual secure-desktop prompt acceptance,
physical-phone/cross-network validation and sustained 60+ FPS UI acceptance
remain incomplete. Android development verification currently uses a VM. See
[setup, gestures, measurements and support matrix](../docs/remote-desktop.md).

The desktop and mobile app for pb-mapper: six places — an Overview drawn as a
galaxy, Tunnels, Remote desktop, Relay, Logs and Settings (on a phone, Logs
opens from Settings) — plus a three-step first run and the full-window remote
viewer. The design follows POCOX's language;
[`docs/ui-design.md`](../docs/ui-design.md) explains it.

Updates download and verify packages inside the app before opening the system
installer. Android releases use the maintainer's persistent signing key in CI;
see [updates and release verification](../docs/ui-updates.md) for platform
behavior and migration from older CI-signed APKs.

## Using Rust Inside Flutter

A tunnel's **More options** hold **Port mapping**, set under the direct
switch it depends on: Off, Auto or Always, saved with the tunnel and kept
while direct is off. Auto, the default, asks the router (PCP, NAT-PMP or
UPnP) for a mapping only once a direct attempt fails to punch through, so a
network where punching works never hears from it.

A running tunnel's **Diagnostics**, from its menu or its panel in the
overview, show its carrier, relay and sessions, its port mapping, and its
events as they happen; tap an event for its fields, or copy the whole as a
JSON report.

On the Dart side, `registerService` and `connectService` take a
`PortMappingMode`,
and `PbMapperApiClient.tunnelEvents(serviceKey:, share:)` is the live feed:
each update carries the tunnel's diagnostics as they are by then, and
cancelling it ends the reader in Rust, a read waiting on a quiet tunnel
included. Underneath, `pb_mapper_register_service` and
`pb_mapper_connect_service` take `port_mapping` (0 off, 1 auto, 2 always), and
`pb_mapper_tunnel_events_call_json` serves `subscribe {kind, key}`,
`next {id, timeout_ms}` and `close {id}` (see `native/pb_mapper_ffi/src/tunnel_events.rs`).

This project uses raw `dart:ffi` to call into a Rust FFI library (`pb-mapper-ffi`)
located under `ui/native`.

### Flutter UI Architecture

- **Pages** live in `ui/lib/src/pages` (overview, tunnels, relay, logs,
  settings, first-run setup, and the one share/connect editor), with the
  Remote desktop page in `pages/remote/` and its viewer in
  `pages/remote/viewer/`; the shell that frames them — rail or bottom bar,
  window chrome, the sharing capsule — in `ui/lib/src/shell`.
- **State**: every page reads one `AppStore` (`ui/lib/src/state`), which wraps
  the API and is refreshed by the Rust change stream. The remote desktop has
  its own `DesktopStore`, whose views notify only their own listeners;
  `RemoteDesktop` and `RemoteComputerStore` hold the open session and the
  remembered computers, and `ViewerPrefs` the viewer's settings.
- **Galaxy**: `ui/lib/src/galaxy` draws the overview. Its per-frame geometry is
  computed in Rust (`ui/native/pb_mapper_galaxy`, exported as
  `pb_mapper_galaxy_frame_v1`); without the library a Dart mirror draws the
  same frames.
- **Theme**: colours, type, glass surfaces, motion and icons in
  `ui/lib/src/theme`; shared widgets in `ui/lib/src/widgets`.
- **FFI layering**:
  - `pb_mapper_api.dart`: typed UI-facing API.
  - `pb_mapper_service.dart`: FFI dispatch + isolate execution (keeps UI thread
    responsive).
  - `pb_mapper_ffi.dart`: raw `dart:ffi` symbols and dynamic library loading.
  - `ui/native/pb_mapper_ffi`: Rust C ABI crate exposing JSON responses.
- **Logging**: Rust logs are pushed to Dart via `NativeCallable` and streamed
  through `PbMapperService.logStream`. The Logs page keeps the latest 1,000
  entries; native runtime diagnostics also persist independently under the
  configuration directory's `logs/` folder. Clearing or filtering the page
  does not change those files.

### Persistent runtime logs

Logs default to **the most recent hour**. A minute's records share a
`runtime-<Unix minute>.log` file, with `.1` through `.3` size archives (newest
first). Startup and a background task running every 15 seconds remove expired
files, including while the application is idle. The entire oldest minute shard
is removed when its start crosses the one-hour boundary; this can discard up to
59 seconds of younger records in that shard. Cleanup resumes at the next launch
when the application is closed, and after the OS resumes a suspended app.

Files rotate before exceeding 8 MiB. Each record is capped at 16 KiB with a
visible truncation marker; the nonblocking worker and startup buffer each hold
at most 512 records. Overflow is reported in the file. Simultaneous UI processes
use up to four locked directories (`runtime-0` through `runtime-3`), each with
a 32 MiB capacity limit as well as the one-hour limit, so all slots together
retain at most 128 MiB. Capacity pressure removes the oldest files first.
If every slot is occupied or storage fails, the live log reports the error.

The directory is `%APPDATA%\pb-mapper-ui\logs` on Windows,
`~/Library/Application Support/pb-mapper-ui/logs` on macOS, and
`$XDG_CONFIG_HOME/pb-mapper-ui/logs` (normally `~/.config/pb-mapper-ui/logs`)
on Linux. Android/iOS use
`<application documents>/pb-mapper-ui/logs`. Startup also emits the actual
resolved path. Existing files are appended across restarts.

Logging defaults to `info`, including transport-library warnings and errors;
set `RUST_LOG` before launching for a diagnostic session, for example
`info,pb_mapper_client::direct=debug`.
The worker flushes drained batches and is explicitly drained on normal exit,
with a bounded shutdown wait. Forced process termination, a stalled disk or
queue overflow can lose the newest records. File persistence currently covers
Rust runtime events; Flutter framework errors and Android system logs remain
in their platform diagnostics.

### Why the FFI API returns JSON

The Rust FFI control layer returns a JSON string (for example:
`{"success": true, "message": "...", "data": {...}}`). We intentionally keep
this JSON envelope because:

- **No generated bindings**: it avoids codegen and keeps the ABI stable even
  when Rust structs evolve.
- **Simple error handling**: every call can return `{success, message}` without
  maintaining extra error enums or parallel Dart structs.
- **Low maintenance**: adding new fields only affects the JSON payload, not the
  Dart FFI signatures.
- **Good enough performance**: UI interactions are dominated by network calls,
  so the JSON parse cost is negligible compared to keeping the API stable.

Desktop video never enters the JSON control envelope. On Windows and Android
the native decoder pulls H.264 from Rust itself, through plain C exports or
their JNI twins, and draws into a Flutter texture; Dart reads only status and
the per-frame view. Elsewhere JPEG comes to Dart as bytes and is decoded off
the UI isolate. See the desktop document above for queue bounds, presentation
acknowledgements and ownership.

To run and build this app, you need to have
[Flutter SDK](https://docs.flutter.dev/get-started/install)
and [Rust toolchain](https://www.rust-lang.org/tools/install)
installed on your system.
You can check that your system is ready with the commands below.
Note that all the Flutter subcomponents should be installed.

```shell
rustc --version
flutter doctor
```

Build the Rust FFI library for your target platform and place the output under
`ui/native/<platform>/` (see platform build scripts and CI for the expected paths).

Now you can run and build this app just like any other Flutter projects.

```shell
flutter run
```

For detailed instructions on using FFI with Flutter, see the Flutter docs.
